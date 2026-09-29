//! References: where the data of a dataset comes from.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::ErrorKind;
use crate::name::Name;
use crate::text;

/// A reference as written by a person, with what it points at.
///
/// The original text is kept and displayed back unchanged, so a reference round trips through
/// parse and display.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Reference {
    text: String,
    target: Target,
}

/// What a reference points at.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Target {
    /// A URL of a network location: a host or bucket and a path, which may be empty.
    Url {
        /// The URL scheme.
        scheme: UrlScheme,
        /// The host, with any user and port, or the bucket.
        host: String,
        /// Everything after the host, starting with `/` when present.
        path: String,
    },
    /// A `file://` URL.
    File {
        /// The path after `file://`.
        path: String,
    },
    /// A local path, relative or absolute, in the platform's own form.
    Path(String),
    /// A DOI, resolved through its landing page.
    Doi {
        /// `10.` and the registrant code.
        prefix: String,
        /// Everything after the first `/`.
        suffix: String,
    },
    /// A path inside a remote named in the machine configuration, written `@name/path`.
    Remote {
        /// The remote's name.
        name: String,
        /// The path inside it, empty for its root.
        path: String,
    },
    /// Any other `scheme:body`: repository adapters, declarative sources and plugins. The body is
    /// left to the adapter of that scheme.
    Scheme {
        /// The scheme, lowercase.
        scheme: String,
        /// Everything after the first colon.
        body: String,
    },
    /// A bare name, to be searched for.
    Name(String),
}

/// A scheme of network URLs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum UrlScheme {
    /// `http://`
    Http,
    /// `https://`
    Https,
    /// `ftp://`
    Ftp,
    /// `ftps://`
    Ftps,
    /// `sftp://`
    Sftp,
    /// `s3://`, object storage.
    S3,
    /// `gs://`, object storage.
    Gs,
    /// `az://`, object storage.
    Az,
}

impl UrlScheme {
    const ALL: [Self; 8] = [
        Self::Http,
        Self::Https,
        Self::Ftp,
        Self::Ftps,
        Self::Sftp,
        Self::S3,
        Self::Gs,
        Self::Az,
    ];

    /// The scheme as written before `://`.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Ftp => "ftp",
            Self::Ftps => "ftps",
            Self::Sftp => "sftp",
            Self::S3 => "s3",
            Self::Gs => "gs",
            Self::Az => "az",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scheme| scheme.name() == name)
    }
}

impl Reference {
    /// The reference exactly as written.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// What the reference points at.
    #[must_use]
    pub const fn target(&self) -> &Target {
        &self.target
    }
}

impl FromStr for Reference {
    type Err = ReferenceError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            text: text.to_owned(),
            target: parse(text)?,
        })
    }
}

fn parse(text: &str) -> Result<Target, ReferenceError> {
    let owned = || text.to_owned();
    if text.is_empty() {
        return Err(ReferenceError::Empty);
    }
    if text.chars().any(char::is_control) {
        return Err(ReferenceError::Control(owned()));
    }
    if text.trim() != text {
        return Err(ReferenceError::Surrounding(owned()));
    }
    if is_local_path(text) {
        return Ok(Target::Path(owned()));
    }
    let scheme = split_scheme(text)?;
    if scheme.is_none() && !text.starts_with('@') && text.contains(['/', '\\']) {
        return Ok(Target::Path(owned()));
    }
    if text.chars().any(char::is_whitespace) {
        return Err(ReferenceError::Whitespace(owned()));
    }
    if let Some(rest) = text.strip_prefix('@') {
        return remote(text, rest);
    }
    match scheme {
        Some((scheme, body)) => scheme_target(text, scheme, body),
        None => Ok(Target::Name(owned())),
    }
}

fn is_local_path(text: &str) -> bool {
    let relative = ["./", "../", ".\\", "..\\", "/", "\\", "~/", "~\\"];
    let drive = matches!(text.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    drive
        || matches!(text, "." | ".." | "~")
        || relative.iter().any(|start| text.starts_with(start))
}

fn split_scheme(text: &str) -> Result<Option<(&str, &str)>, ReferenceError> {
    let Some((scheme, body)) = text.split_once(':') else {
        return Ok(None);
    };
    let mut chars = scheme.chars();
    let starts_with_letter = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let rest_allowed = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    if !(starts_with_letter && rest_allowed && scheme.len() >= 2) {
        return Ok(None);
    }
    if scheme.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(ReferenceError::UppercaseScheme {
            scheme: scheme.to_owned(),
            text: text.to_owned(),
        });
    }
    Ok(Some((scheme, body)))
}

fn scheme_target(text: &str, scheme: &str, body: &str) -> Result<Target, ReferenceError> {
    if let Some(url_scheme) = UrlScheme::from_name(scheme) {
        return url(text, url_scheme, body);
    }
    match scheme {
        "file" => match body.strip_prefix("//") {
            Some("") => Err(ReferenceError::NoPath(text.to_owned())),
            Some(path) => Ok(Target::File {
                path: path.to_owned(),
            }),
            None => Err(ReferenceError::Slashes {
                scheme: scheme.to_owned(),
                text: text.to_owned(),
            }),
        },
        "doi" => doi(text, body),
        _ if body.is_empty() => Err(ReferenceError::EmptyBody {
            scheme: scheme.to_owned(),
            text: text.to_owned(),
        }),
        "croissant" => match parse(body) {
            Ok(Target::Url {
                scheme: UrlScheme::Http | UrlScheme::Https,
                ..
            }) => Ok(scheme_parts(scheme, body)),
            _ => Err(ReferenceError::Croissant(text.to_owned())),
        },
        _ => Ok(scheme_parts(scheme, body)),
    }
}

fn scheme_parts(scheme: &str, body: &str) -> Target {
    Target::Scheme {
        scheme: scheme.to_owned(),
        body: body.to_owned(),
    }
}

fn url(text: &str, scheme: UrlScheme, body: &str) -> Result<Target, ReferenceError> {
    let rest = body
        .strip_prefix("//")
        .ok_or_else(|| ReferenceError::Slashes {
            scheme: scheme.name().to_owned(),
            text: text.to_owned(),
        })?;
    let (host, path) = rest.find('/').map_or((rest, ""), |at| rest.split_at(at));
    if host.is_empty() {
        return Err(ReferenceError::NoHost(text.to_owned()));
    }
    Ok(Target::Url {
        scheme,
        host: host.to_owned(),
        path: path.to_owned(),
    })
}

fn doi(text: &str, body: &str) -> Result<Target, ReferenceError> {
    let invalid = || ReferenceError::Doi(text.to_owned());
    let (prefix, suffix) = body.split_once('/').ok_or_else(invalid)?;
    let registrant = prefix.strip_prefix("10.").ok_or_else(invalid)?;
    let registrant_valid = registrant.starts_with(|c: char| c.is_ascii_digit())
        && registrant.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !registrant_valid || suffix.is_empty() {
        return Err(invalid());
    }
    Ok(Target::Doi {
        prefix: prefix.to_owned(),
        suffix: suffix.to_owned(),
    })
}

fn remote(text: &str, rest: &str) -> Result<Target, ReferenceError> {
    let (name, path) = rest.split_once('/').unwrap_or((rest, ""));
    if name.is_empty() {
        return Err(ReferenceError::NoRemote(text.to_owned()));
    }
    if Name::new(name).is_err() {
        return Err(ReferenceError::RemoteName {
            name: name.to_owned(),
            text: text.to_owned(),
        });
    }
    Ok(Target::Remote {
        name: name.to_owned(),
        path: path.to_owned(),
    })
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl Serialize for Reference {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text)
    }
}

impl<'de> Deserialize<'de> for Reference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        text::deserialize(deserializer)
    }
}

/// Why text is not a valid reference.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceError {
    /// The text is empty.
    #[error("a reference cannot be empty")]
    Empty,
    /// The text starts or ends with whitespace.
    #[error("reference `{0}` starts or ends with whitespace")]
    Surrounding(String),
    /// The text holds a control character.
    #[error("reference {0:?} contains a control character")]
    Control(String),
    /// Whitespace inside a reference that is not a local path.
    #[error("reference `{0}` contains whitespace")]
    Whitespace(String),
    /// A scheme written with uppercase letters.
    #[error("scheme `{scheme}` in `{text}` must be lowercase")]
    UppercaseScheme {
        /// The scheme as written.
        scheme: String,
        /// The whole reference.
        text: String,
    },
    /// A URL scheme not followed by `//`.
    #[error("reference `{text}` needs `{scheme}://`")]
    Slashes {
        /// The scheme.
        scheme: String,
        /// The whole reference.
        text: String,
    },
    /// A URL with an empty host or bucket.
    #[error("reference `{0}` names no host")]
    NoHost(String),
    /// A `file://` URL with nothing after it.
    #[error("reference `{0}` names no path")]
    NoPath(String),
    /// A `doi:` reference that is not `10.` digits, `/` and a suffix.
    #[error("`{0}` is not a DOI, expected `doi:10.NNNN/suffix`")]
    Doi(String),
    /// A scheme with nothing after its colon.
    #[error("reference `{text}` has nothing after `{scheme}:`")]
    EmptyBody {
        /// The scheme.
        scheme: String,
        /// The whole reference.
        text: String,
    },
    /// `@` without a remote name.
    #[error("reference `{0}` names no remote")]
    NoRemote(String),
    /// A remote name outside `a-z0-9-_`.
    #[error("remote `{name}` in `{text}` may hold only a-z, 0-9, `-` and `_`")]
    RemoteName {
        /// The remote name as written.
        name: String,
        /// The whole reference.
        text: String,
    },
    /// `croissant:` not followed by an http or https URL.
    #[error("reference `{0}` needs an http or https URL after `croissant:`")]
    Croissant(String),
}

impl ReferenceError {
    /// The kind this error is reported as.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        ErrorKind::ReferenceInvalid
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn target(text: &str) -> Target {
        let reference: Reference = text.parse().unwrap();
        assert_eq!(reference.as_str(), text);
        assert_eq!(reference.to_string(), text);
        reference.target().clone()
    }

    fn check(cases: &[(&str, Target)]) {
        for (text, expected) in cases {
            assert_eq!(&target(text), expected, "{text}");
        }
    }

    fn url(scheme: UrlScheme, host: &str, path: &str) -> Target {
        Target::Url {
            scheme,
            host: host.to_owned(),
            path: path.to_owned(),
        }
    }

    fn scheme(scheme: &str, body: &str) -> Target {
        Target::Scheme {
            scheme: scheme.to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn parses_urls_and_local_paths() {
        let cases = [
            (
                "https://host/path/file.tar.gz",
                url(UrlScheme::Https, "host", "/path/file.tar.gz"),
            ),
            (
                "http://host:8080/path/?a=1",
                url(UrlScheme::Http, "host:8080", "/path/?a=1"),
            ),
            ("https://host", url(UrlScheme::Https, "host", "")),
            (
                "s3://bucket/prefix/",
                url(UrlScheme::S3, "bucket", "/prefix/"),
            ),
            ("gs://bucket/p", url(UrlScheme::Gs, "bucket", "/p")),
            ("az://container/p", url(UrlScheme::Az, "container", "/p")),
            ("ftp://host/pub/", url(UrlScheme::Ftp, "host", "/pub/")),
            ("ftps://host/pub/", url(UrlScheme::Ftps, "host", "/pub/")),
            (
                "sftp://user@host/data",
                url(UrlScheme::Sftp, "user@host", "/data"),
            ),
            (
                "file:///D:/raw",
                Target::File {
                    path: "/D:/raw".to_owned(),
                },
            ),
            ("./raw", Target::Path("./raw".to_owned())),
            ("../raw", Target::Path("../raw".to_owned())),
            ("/abs/raw", Target::Path("/abs/raw".to_owned())),
            ("~/raw", Target::Path("~/raw".to_owned())),
            ("D:\\raw", Target::Path("D:\\raw".to_owned())),
            ("D:/raw", Target::Path("D:/raw".to_owned())),
            (".\\raw", Target::Path(".\\raw".to_owned())),
            ("raw/data", Target::Path("raw/data".to_owned())),
            ("./my data", Target::Path("./my data".to_owned())),
            (".", Target::Path(".".to_owned())),
        ];
        check(&cases);
    }

    #[test]
    fn parses_dois_and_opaque_schemes() {
        let cases = [
            (
                "doi:10.5281/zenodo.3242074",
                Target::Doi {
                    prefix: "10.5281".to_owned(),
                    suffix: "zenodo.3242074".to_owned(),
                },
            ),
            (
                "doi:10.1000.10/a/b",
                Target::Doi {
                    prefix: "10.1000.10".to_owned(),
                    suffix: "a/b".to_owned(),
                },
            ),
            (
                "hf:datasets/org/name@rev",
                scheme("hf", "datasets/org/name@rev"),
            ),
            ("zenodo:3242074", scheme("zenodo", "3242074")),
            ("inveniordm:host/id", scheme("inveniordm", "host/id")),
            ("figshare:1234567", scheme("figshare", "1234567")),
            (
                "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/OMV93V",
                scheme("dataverse", "dataverse.harvard.edu/doi:10.7910/DVN/OMV93V"),
            ),
            ("osf:abc12", scheme("osf", "abc12")),
            (
                "dryad:doi:10.5061/dryad.xyz",
                scheme("dryad", "doi:10.5061/dryad.xyz"),
            ),
            ("github:owner/repo@tag", scheme("github", "owner/repo@tag")),
            ("kaggle:owner/dataset", scheme("kaggle", "owner/dataset")),
            ("openml:61", scheme("openml", "61")),
            (
                "ckan:demo.ckan.org/dataset-id",
                scheme("ckan", "demo.ckan.org/dataset-id"),
            ),
            (
                "croissant:https://host/metadata.json",
                scheme("croissant", "https://host/metadata.json"),
            ),
            ("sra:SRR000001", scheme("sra", "SRR000001")),
            (
                "openneuro:ds003061@1.1.0",
                scheme("openneuro", "ds003061@1.1.0"),
            ),
            (
                "ensembl:homo_sapiens/GRCh38@110",
                scheme("ensembl", "homo_sapiens/GRCh38@110"),
            ),
            ("x-y.z+1:a", scheme("x-y.z+1", "a")),
        ];
        check(&cases);
    }

    #[test]
    fn parses_remotes_and_bare_names() {
        let cases = [
            (
                "@labdrive/path/x",
                Target::Remote {
                    name: "labdrive".to_owned(),
                    path: "path/x".to_owned(),
                },
            ),
            (
                "@labdrive",
                Target::Remote {
                    name: "labdrive".to_owned(),
                    path: String::new(),
                },
            ),
            ("ham10000", Target::Name("ham10000".to_owned())),
            ("HAM10000", Target::Name("HAM10000".to_owned())),
        ];
        check(&cases);
    }

    #[test]
    fn refuses_invalid_references_naming_the_problem() {
        let cases = [
            ("", "a reference cannot be empty"),
            (
                " zenodo:1",
                "reference ` zenodo:1` starts or ends with whitespace",
            ),
            (
                "zenodo:1 ",
                "reference `zenodo:1 ` starts or ends with whitespace",
            ),
            ("a\tb", "reference \"a\\tb\" contains a control character"),
            ("ham 10000", "reference `ham 10000` contains whitespace"),
            ("zenodo:12 3", "reference `zenodo:12 3` contains whitespace"),
            (
                "https://host/a b",
                "reference `https://host/a b` contains whitespace",
            ),
            (
                "DOI:10.1/x",
                "scheme `DOI` in `DOI:10.1/x` must be lowercase",
            ),
            ("https:host/x", "reference `https:host/x` needs `https://`"),
            ("https:///x", "reference `https:///x` names no host"),
            ("s3://", "reference `s3://` names no host"),
            ("file://", "reference `file://` names no path"),
            ("doi:", "`doi:` is not a DOI, expected `doi:10.NNNN/suffix`"),
            (
                "doi:11.1/x",
                "`doi:11.1/x` is not a DOI, expected `doi:10.NNNN/suffix`",
            ),
            (
                "doi:10.1",
                "`doi:10.1` is not a DOI, expected `doi:10.NNNN/suffix`",
            ),
            (
                "doi:10./x",
                "`doi:10./x` is not a DOI, expected `doi:10.NNNN/suffix`",
            ),
            (
                "doi:10.12a/x",
                "`doi:10.12a/x` is not a DOI, expected `doi:10.NNNN/suffix`",
            ),
            (
                "doi:10.1/",
                "`doi:10.1/` is not a DOI, expected `doi:10.NNNN/suffix`",
            ),
            ("zenodo:", "reference `zenodo:` has nothing after `zenodo:`"),
            ("@", "reference `@` names no remote"),
            ("@/x", "reference `@/x` names no remote"),
            (
                "@Lab/x",
                "remote `Lab` in `@Lab/x` may hold only a-z, 0-9, `-` and `_`",
            ),
            (
                "croissant:ftp://host/m.json",
                "reference `croissant:ftp://host/m.json` needs an http or https URL after `croissant:`",
            ),
            (
                "croissant:meta.json",
                "reference `croissant:meta.json` needs an http or https URL after `croissant:`",
            ),
        ];
        for (text, message) in cases {
            let err = text.parse::<Reference>().unwrap_err();
            assert_eq!(err.to_string(), message, "{text}");
        }
    }

    #[test]
    fn a_single_letter_before_a_colon_is_a_drive_not_a_scheme() {
        assert_eq!(target("C:"), Target::Path("C:".to_owned()));
        assert_eq!(target("c:data"), Target::Path("c:data".to_owned()));
    }

    fn valid_reference() -> impl Strategy<Value = String> {
        let word = "[a-z0-9][a-z0-9._-]{0,11}";
        prop_oneof![
            (
                prop_oneof![Just("https"), Just("http"), Just("s3"), Just("sftp")],
                word,
                word
            )
                .prop_map(|(s, h, p)| format!("{s}://{h}/{p}")),
            ("[0-9]{4,5}", word).prop_map(|(r, s)| format!("doi:10.{r}/{s}")),
            ("[a-z][a-z0-9]{1,8}", word, word)
                .prop_filter("not a known scheme", |(s, _, _)| !KNOWN
                    .contains(&s.as_str()))
                .prop_map(|(s, a, b)| format!("{s}:{a}/{b}")),
            ("[a-z0-9_-]{1,8}", word).prop_map(|(n, p)| format!("@{n}/{p}")),
            word.prop_map(|p| format!("./{p}")),
            "[A-Za-z0-9][A-Za-z0-9._-]{0,15}".prop_filter("not a path", |n| n != "." && n != ".."),
        ]
    }

    const KNOWN: [&str; 11] = [
        "http",
        "https",
        "ftp",
        "ftps",
        "sftp",
        "s3",
        "gs",
        "az",
        "file",
        "doi",
        "croissant",
    ];

    proptest! {
        #[test]
        fn valid_references_round_trip_through_parse_and_display(text in valid_reference()) {
            let reference: Reference = text.parse().unwrap();
            prop_assert_eq!(reference.to_string(), text.clone());
            prop_assert_eq!(reference.to_string().parse::<Reference>().unwrap(), reference);
        }

        #[test]
        fn parsing_never_panics(text in "\\PC{0,40}") {
            let _ = text.parse::<Reference>();
        }
    }
}
