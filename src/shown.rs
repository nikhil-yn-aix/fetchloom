//! links as fetchloom shows them, and the secrets it never keeps in one. a query or fragment value
//! shows only under a name known to be harmless, so no unknown token reaches a message or a log.

use std::borrow::Cow;
use std::fmt;

use percent_encoding::percent_decode_str;
use url::Url;

/// how the names of keys whose values grant access end, once case and separators are ignored:
/// `private_token`, `X-Amz-Signature`, `client_secret` or `x-api-key`.
const SECRET_ENDINGS: [&str; 7] = [
    "token",
    "secret",
    "password",
    "passwd",
    "signature",
    "credential",
    "apikey",
];

/// whole names of keys whose values grant access, once case and separators are ignored, such as
/// azure's SAS `sig`, `se` and `sp`, cloudfront's `Key-Pair-Id` and a login's or session's names.
const SECRET_NAMES: [&str; 12] = [
    "sig",
    "se",
    "sp",
    "keypairid",
    "auth",
    "jwt",
    "accesskey",
    "pass",
    "pw",
    "pwd",
    "session",
    "sessionid",
];

/// names whose values show as written, ignoring case. dropbox's `rlkey` and drive's `resourcekey`
/// are in data.toml anyway; a paging cursor stays masked, since some apis hand back a token there.
const HARMLESS: [&str; 17] = [
    "dl",
    "raw",
    "download",
    "format",
    "version",
    "versionid",
    "datasetversionnumber",
    "generation",
    "persistentid",
    "rlkey",
    "resourcekey",
    "page",
    "per_page",
    "page_size",
    "limit",
    "offset",
    "size",
];

/// the first query or fragment key of `url` whose value grants access: a name [`is_secret`]
/// knows, or `key` on a path of dataverse's api, which takes a person's api key there.
pub(crate) fn secret(url: &Url) -> Option<String> {
    url.query_pairs()
        .chain(url::form_urlencoded::parse(
            url.fragment().unwrap_or_default().as_bytes(),
        ))
        .map(|(key, _)| key)
        .find(|key| {
            let name = normalized(key);
            is_secret(&name) || name == "key" && is_dataverse_api(url)
        })
        .map(|key| key.to_ascii_lowercase())
}

/// whether `name` is a secret on some paths only, as dataverse's `key` is, so what [`secret`]
/// says of one link says nothing of another with the same keys.
pub(crate) fn depends_on_path(name: &str) -> bool {
    normalized(&percent_decode_str(name).decode_utf8_lossy()) == "key"
}

/// whether a key's [`normalized`] name grants access: any token, secret, password, signature,
/// credential or api key however it is spelled, and the short names of [`SECRET_NAMES`].
fn is_secret(name: &str) -> bool {
    SECRET_ENDINGS.iter().any(|ending| name.ends_with(ending)) || SECRET_NAMES.contains(&name)
}

/// `key` in lowercase without separators, so `X-Amz-Signature` is `xamzsignature`.
fn normalized(key: &str) -> String {
    key.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// whether `url` asks dataverse's api for a file or a dataset: its path holds `api/access` or
/// `api/datasets`, with `v1` between them or not.
fn is_dataverse_api(url: &Url) -> bool {
    let Some(segments) = url.path_segments() else {
        return false;
    };
    let segments: Vec<Cow<'_, str>> = segments
        .map(|segment| percent_decode_str(segment).decode_utf8_lossy())
        .collect();
    segments.iter().enumerate().any(|(at, segment)| {
        let mut rest = segments[at + 1..].iter();
        let mut next = rest.next();
        if next.is_some_and(|word| word.eq_ignore_ascii_case("v1")) {
            next = rest.next();
        }
        segment.eq_ignore_ascii_case("api")
            && next.is_some_and(|word| {
                word.eq_ignore_ascii_case("access") || word.eq_ignore_ascii_case("datasets")
            })
    })
}

/// a link as messages and logs show it, each query or fragment value under a name not known to
/// be harmless replaced by `***`.
pub(crate) struct Shown<'a>(pub(crate) &'a Url);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&mask_query(self.0.as_str()))
    }
}

/// masks a login, and each query or fragment value under a name not known to be harmless, even
/// before a reference is a valid URL.
pub(crate) fn mask_query(text: &str) -> Cow<'_, str> {
    masked(text, |name| !is_harmless(name))
}

/// masks a login, and each query or fragment value under a name that grants access, as a token,
/// a signature or a `key` does, for `list`, where two links that differ in a harmless value must
/// look different. messages and logs mask more, with [`mask_query`].
pub(crate) fn mask_secrets(text: &str) -> Cow<'_, str> {
    masked(text, |name| {
        let name = normalized(&percent_decode_str(name).decode_utf8_lossy());
        is_secret(&name) || name == "key"
    })
}

/// `text` with its login and each value under a name `hides` masked.
fn masked(text: &str, hides: fn(&str) -> bool) -> Cow<'_, str> {
    if let Some(masked) = mask_normalized(text, hides) {
        return Cow::Owned(masked);
    }
    // pasted, wholly encoded URLs are still sensitive when rejected as arguments
    if !text.contains(':') && text.contains('%') {
        let decoded = percent_decode_str(text).decode_utf8_lossy();
        if let Some(masked) = mask_normalized(&decoded, hides) {
            return Cow::Owned(masked);
        }
    }
    Cow::Borrowed(text)
}

/// `typed`, a link or a word a person typed, as a refusal shows it: a link's login and query values
/// masked, as [`mask_query`] masks them, and so is whatever comes before an `@` without a link
/// around it, as the password in `ada:pw@data.example.org` or `//ada:pw@lab.local`.
pub(crate) fn mask_typed(typed: &str) -> String {
    let masked = mask_query(typed);
    let start = masked.find("://").map_or(0, |at| at + 3);
    match masked[start..].rfind('@') {
        Some(at) => format!("{}***{}", &masked[..start], &masked[start + at..]),
        None => masked.into_owned(),
    }
}

/// `typed`, a reference, a name or a command a person typed, as a refusal or a did-you-mean shows
/// it: masked as [`mask_query`] masks it, and with a login before a host in its first part masked
/// too, as the password in `ada:pw@data.example.org/x.csv`, which a URL parser reads as no login
/// without a scheme. an `@` before no host, as `figshare:1@2` has, or past a `/`, as
/// `github:owner/repo@v1` has, names a version, so it stays.
pub(crate) fn mask_reference(typed: &str) -> Cow<'_, str> {
    let masked = mask_query(typed);
    let end = masked.find(['/', '\\', '?', '#']).unwrap_or(masked.len());
    let Some(at) = masked[..end].rfind('@').filter(|at| *at > 0) else {
        return masked;
    };
    let host = &masked[at + 1..end];
    let (name, port) = host.split_once(':').unwrap_or((host, ""));
    let hostlike = name.contains('.')
        || name.eq_ignore_ascii_case("localhost")
        || (!port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()));
    if hostlike {
        Cow::Owned(format!("***{}", &masked[at..]))
    } else {
        masked
    }
}

fn mask_normalized(text: &str, hides: fn(&str) -> bool) -> Option<String> {
    // URL parsing removes these characters, even from the scheme or authority
    let normalized = if text.contains(['\t', '\r', '\n']) {
        Cow::Owned(text.replace(['\t', '\r', '\n'], ""))
    } else {
        Cow::Borrowed(text)
    };
    let login = mask_login(&normalized);
    if let Cow::Owned(masked) = mask_values(&login, hides) {
        return Some(masked);
    }
    match login {
        Cow::Owned(masked) => Some(masked),
        Cow::Borrowed(_) => None,
    }
}

/// replaces the `user:password@` of a link's authority with `***@`: after a web scheme and any
/// number of slashes, as a URL parser reads them, after the `//` of any other scheme wherever the
/// link starts, as in `scp://` or `list:scp://`, or after a `//` that starts the text. a reference
/// with no authority keeps its `@`, so `figshare:1@2` keeps its version.
fn mask_login(text: &str) -> Cow<'_, str> {
    let Some(start) = authority(text) else {
        return Cow::Borrowed(text);
    };
    let end = text[start..]
        .find(['/', '\\', '?', '#'])
        .map_or(text.len(), |at| start + at);
    match text[start..end].rfind('@') {
        Some(at) => Cow::Owned(format!("{}***{}", &text[..start], &text[start + at..])),
        None => Cow::Borrowed(text),
    }
}

/// where the authority [`mask_login`] masks starts in `text`, if it has one.
fn authority(text: &str) -> Option<usize> {
    let past_slashes = |rest: &str| text.len() - rest.trim_start_matches(['/', '\\']).len();
    if let Some((scheme, rest)) = text.split_once(':')
        && ["http", "https", "ftp"]
            .iter()
            .any(|web| scheme.trim().eq_ignore_ascii_case(web))
    {
        return Some(past_slashes(rest));
    }
    if text.starts_with("//") || text.starts_with("\\\\") {
        return Some(past_slashes(text));
    }
    let colon = text.find("://").or_else(|| text.find(":\\\\"))?;
    Some(past_slashes(&text[colon + 1..]))
}

fn mask_values(text: &str, hides: fn(&str) -> bool) -> Cow<'_, str> {
    let (head, fragment) = text.split_once('#').unwrap_or((text, ""));
    let (address, query) = head.split_once('?').unwrap_or((head, ""));
    let masked_query = mask_pairs(query, hides);
    let masked_fragment = mask_pairs(fragment, hides);
    if matches!(masked_query, Cow::Borrowed(_)) && matches!(masked_fragment, Cow::Borrowed(_)) {
        return Cow::Borrowed(text);
    }
    let query_separator = if head.contains('?') { "?" } else { "" };
    let fragment_separator = if text.contains('#') { "#" } else { "" };
    Cow::Owned(format!(
        "{address}{query_separator}{masked_query}{fragment_separator}{masked_fragment}"
    ))
}

/// `text`, a query or a fragment, with each value under a name `hides` replaced by `***` and the
/// rest as written.
fn mask_pairs(text: &str, hides: fn(&str) -> bool) -> Cow<'_, str> {
    let hidden = |pair: &'_ str| {
        pair.split_once('=')
            .is_some_and(|(name, value)| !value.is_empty() && hides(name))
    };
    if !text.split('&').any(hidden) {
        return Cow::Borrowed(text);
    }
    let pairs: Vec<Cow<'_, str>> = text
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) if hidden(pair) => Cow::Owned(format!("{name}=***")),
            _ => Cow::Borrowed(pair),
        })
        .collect();
    Cow::Owned(pairs.join("&"))
}

/// whether `name`, a query or fragment key as a link writes it, is one of [`HARMLESS`].
fn is_harmless(name: &str) -> bool {
    let name = percent_decode_str(name).decode_utf8_lossy();
    HARMLESS
        .iter()
        .any(|harmless| name.eq_ignore_ascii_case(harmless))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_cannot_hide_userinfo_from_masking() {
        for text in [
            "ht\ttps://reader:mask_canary@example.org/path",
            "http\rs://reader:mask_canary@example.org/path",
            "https:\n//reader:mask_canary@example.org/path",
            "https%3A%2F%2Freader%3Amask_canary%40example.org/path",
            "https%3A%2F%2Fexample.org/path%3Ftoken%3Dmask_canary",
        ] {
            let shown = mask_query(text);
            assert!(!shown.contains("mask_canary"), "{shown}");
            assert!(shown.contains("***"), "{shown}");
        }
        let plain = "http\rs://example.org/path";
        assert_eq!(mask_query(plain), plain);
    }

    /// list tells apart links that differ in a harmless value, and still hides what grants access.
    #[test]
    fn a_list_shows_harmless_values_and_masks_secrets() {
        let link = "https://lab.example/x.csv?n=2&api_token=abc&X-Amz-Signature=def&key=ghi#v=3";
        assert_eq!(
            mask_secrets(link),
            "https://lab.example/x.csv?n=2&api_token=***&X-Amz-Signature=***&key=***#v=3"
        );
        assert_eq!(
            mask_query(link),
            "https://lab.example/x.csv?n=***&api_token=***&X-Amz-Signature=***&key=***#v=***"
        );
        assert_eq!(
            mask_secrets("https://ada:pw@lab.example/x.csv?n=2"),
            "https://***@lab.example/x.csv?n=2"
        );
    }

    #[test]
    fn signed_storage_links_show_without_their_secrets() {
        let url = Url::parse(
            "https://bucket.s3.amazonaws.com/data.csv?X-Amz-Credential=AKIA1&X-Amz-Signature=abc123&X-Amz-Expires=600",
        )
        .unwrap();
        assert_eq!(secret(&url).as_deref(), Some("x-amz-credential"));
        assert_eq!(
            Shown(&url).to_string(),
            "https://bucket.s3.amazonaws.com/data.csv?X-Amz-Credential=***&X-Amz-Signature=***&X-Amz-Expires=***"
        );
    }

    #[test]
    fn a_login_never_shows_even_in_a_link_that_does_not_parse() {
        for (text, shown) in [
            (
                "https://ada:pw@example.org/a.csv?token=t",
                "https://***@example.org/a.csv?token=***",
            ),
            (
                "https://ada:pw@example.org:bad/a.csv",
                "https://***@example.org:bad/a.csv",
            ),
            ("https://example.org/a@b.csv", "https://example.org/a@b.csv"),
            (
                "https:ada:pw@example.org/a.csv",
                "https:***@example.org/a.csv",
            ),
            (
                "https:///ada:pw@example.org:bad/a.csv",
                "https:///***@example.org:bad/a.csv",
            ),
            (
                "HTTP:\\\\ada:pw@example.org\\a.csv",
                "HTTP:\\\\***@example.org\\a.csv",
            ),
            ("figshare:34744689@1", "figshare:34744689@1"),
            ("zenodo:1", "zenodo:1"),
            // any scheme's authority, wherever its link starts, and a `//` that starts the text
            (
                "scp://ada:pw@example.org/a.csv",
                "scp://***@example.org/a.csv",
            ),
            (
                "<smb://ada:pw@example.org/a.csv>",
                "<smb://***@example.org/a.csv>",
            ),
            (
                "list:scp://ada:pw@example.org/a.txt",
                "list:scp://***@example.org/a.txt",
            ),
            (
                "s3:https://ada:pw@minio.example.org/b",
                "s3:https://***@minio.example.org/b",
            ),
            ("//ada:pw@example.org/a.csv", "//***@example.org/a.csv"),
            ("hf://datasets/x@main", "hf://datasets/x@main"),
        ] {
            assert_eq!(mask_query(text), shown, "{text}");
        }
    }

    #[test]
    fn a_typed_reference_hides_a_login_before_a_host_and_keeps_a_version() {
        for (typed, shown) in [
            (
                "ada:pw@data.example.org/x.csv",
                "***@data.example.org/x.csv",
            ),
            ("zenodoo:ada:pw@localhost:8000", "***@localhost:8000"),
            ("ada@lab:8000/x", "***@lab:8000/x"),
            (
                "scp://ada:pw@example.org/a.csv",
                "scp://***@example.org/a.csv",
            ),
            ("figshare:34744689@1", "figshare:34744689@1"),
            ("github:jqlang/jq@jq-1.8.2", "github:jqlang/jq@jq-1.8.2"),
            (
                "hf:datasets/scikit-learn/iris@main",
                "hf:datasets/scikit-learn/iris@main",
            ),
        ] {
            assert_eq!(mask_reference(typed), shown, "{typed}");
        }
    }

    #[test]
    fn ordinary_links_show_as_they_are() {
        let url = Url::parse("https://example.org/files/a.csv?download=1&version=2").unwrap();
        assert_eq!(secret(&url), None);
        assert_eq!(Shown(&url).to_string(), url.as_str());
    }

    #[test]
    fn any_token_or_secret_counts_however_it_is_spelled() {
        for key in [
            "private_token",
            "PRIVATE_TOKEN",
            "%74oken",
            "auth-token",
            "refresh_token",
            "client_secret",
            "x-api-key",
            "apiKey",
            "Sig",
            "se",
            "SP",
            "X-Amz-Credential",
            "Key-Pair-Id",
        ] {
            let url = Url::parse(&format!("https://example.org/a.csv?{key}=abc123")).unwrap();
            assert!(secret(&url).is_some(), "{key}");
            assert!(!Shown(&url).to_string().contains("abc123"), "{key}");
        }
        for key in ["rlkey", "resourcekey", "dl", "raw"] {
            let url = Url::parse(&format!("https://example.org/a.csv?{key}=1")).unwrap();
            assert_eq!(secret(&url), None, "{key}");
            assert_eq!(Shown(&url).to_string(), url.as_str(), "{key}");
        }
    }

    #[test]
    fn names_for_a_login_a_session_or_a_web_token_are_secrets() {
        for key in [
            "auth",
            "jwt",
            "access_key",
            "accesskey",
            "pass",
            "pw",
            "pwd",
            "session",
            "sessionid",
        ] {
            let url = Url::parse(&format!("https://example.org/a.csv?{key}=abc123")).unwrap();
            assert_eq!(secret(&url).as_deref(), Some(key), "{key}");
        }
    }

    /// dataverse's api takes a person's api key as `key`, which elsewhere names a file or setting.
    #[test]
    fn a_key_is_a_secret_on_dataverse_api_paths() {
        for link in [
            "https://dataverse.harvard.edu/api/access/datafile/4123456?key=abc123",
            "https://dataverse.example.edu:8443/api/v1/access/datafile/1#Key=abc123",
            "https://demo.dataverse.org/api/datasets/:persistentId/versions/:latest/files?persistentId=doi:10.70122/FK2/ABC&key=abc123",
        ] {
            let url = Url::parse(link).unwrap();
            assert_eq!(secret(&url).as_deref(), Some("key"), "{link}");
        }
        for link in [
            "https://example.org/files/a.csv?key=abc123",
            "https://example.org/api/accessible/a.csv?key=abc123",
        ] {
            assert_eq!(secret(&Url::parse(link).unwrap()), None, "{link}");
        }
    }

    /// any other name may grant access. a name without a value shows as written.
    #[test]
    fn values_show_only_under_names_known_to_be_harmless() {
        for (text, shown) in [
            (
                "https://www.dropbox.com/scl/fi/abc/a.csv?rlkey=r1&dl=1",
                "https://www.dropbox.com/scl/fi/abc/a.csv?rlkey=r1&dl=1",
            ),
            (
                "https://example.org/a.csv?key=k1&Format=csv&verify=v1#chapter-2",
                "https://example.org/a.csv?key=***&Format=csv&verify=***#chapter-2",
            ),
            (
                "https://example.org/a.csv?version=1&persistentId=doi:10.7910/DVN/X#view=v2",
                "https://example.org/a.csv?version=1&persistentId=doi:10.7910/DVN/X#view=***",
            ),
            (
                "https://api.example.org/files?page=2&per_page=100&page_size=50&limit=10&offset=20&size=5&cursor=c1",
                "https://api.example.org/files?page=2&per_page=100&page_size=50&limit=10&offset=20&size=5&cursor=***",
            ),
            (
                "https://api.kaggle.com/v1/datasets/download/a/b/c.csv?datasetVersionNumber=2&raw=true&token=t1",
                "https://api.kaggle.com/v1/datasets/download/a/b/c.csv?datasetVersionNumber=2&raw=true&token=***",
            ),
        ] {
            assert_eq!(mask_query(text), shown, "{text}");
            assert_eq!(Shown(&Url::parse(text).unwrap()).to_string(), shown);
        }
    }

    #[test]
    fn fragment_credentials_are_masked_and_not_shareable() {
        for (text, expected) in [
            (
                "https://example.org/a#access_token=hidden&page=2",
                "https://example.org/a#access_token=***&page=2",
            ),
            (
                "https://example.org/a?download=1#%74oken=hidden",
                "https://example.org/a?download=1#%74oken=***",
            ),
            (
                "https://example.org/a?token=hidden#client_secret=hidden",
                "https://example.org/a?token=***#client_secret=***",
            ),
            (
                "https://example.org/a?#TOKEN=hidden",
                "https://example.org/a?#TOKEN=***",
            ),
        ] {
            let url = Url::parse(text).unwrap();
            assert!(secret(&url).is_some(), "{text}");
            assert_eq!(mask_query(text), expected);
            assert_eq!(Shown(&url).to_string(), expected);
        }
        assert_eq!(
            mask_query("https://example.org:bad/a#token=hidden"),
            "https://example.org:bad/a#token=***"
        );
    }

    #[test]
    fn fragment_navigation_without_values_is_unchanged() {
        for text in [
            "https://example.org/a#chapter-2",
            "https://example.org/a?download=1#results",
            "https://example.org/a#/route",
            "https://example.org/a?#",
        ] {
            assert!(matches!(mask_query(text), Cow::Borrowed(_)));
            assert_eq!(Shown(&Url::parse(text).unwrap()).to_string(), text);
            assert_eq!(secret(&Url::parse(text).unwrap()), None);
        }
    }
}
