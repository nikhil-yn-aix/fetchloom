//! links as fetchloom shows them, and the secrets it never keeps in a link. a query or fragment
//! value shows only under a name known to be harmless, so a token under a name no list knows,
//! such as dataverse's `key`, never reaches a message, a log or a trace.

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

/// whole names of keys whose values grant access, once case and separators are ignored: azure's
/// `sig`, with the expiry `se` and permissions `sp` of its SAS, cloudfront's `Key-Pair-Id`, and
/// the names a login, a session or a web token goes by.
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

/// names whose values show as written, ignoring case: switches, formats and versions, including
/// the object versions S3 (`versionId`) and cloud storage (`generation`) links pin, the ids
/// dataverse's pages take, and the keys dropbox (`rlkey`) and drive (`resourcekey`) share links
/// need to work, which data.toml keeps anyway.
const HARMLESS: [&str; 10] = [
    "dl",
    "raw",
    "download",
    "format",
    "version",
    "versionid",
    "generation",
    "persistentid",
    "rlkey",
    "resourcekey",
];

/// the first query or fragment key of `url` whose value grants access, when it has one: a name
/// [`is_secret`] knows, or `key` on a path of dataverse's api, which takes a person's api key
/// there.
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

/// whether `name`, a query or fragment key as a link writes it, is a secret on some paths only,
/// as dataverse's `key` is, so that what [`secret`] says of one link says nothing of another
/// with the same keys.
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
    if let Some(masked) = mask_normalized(text) {
        return Cow::Owned(masked);
    }
    // pasted, wholly encoded URLs are still sensitive when rejected as arguments.
    if !text.contains(':') && text.contains('%') {
        let decoded = percent_decode_str(text).decode_utf8_lossy();
        if let Some(masked) = mask_normalized(&decoded) {
            return Cow::Owned(masked);
        }
    }
    Cow::Borrowed(text)
}

fn mask_normalized(text: &str) -> Option<String> {
    // URL parsing removes these characters, even from the scheme or authority.
    let normalized = if text.contains(['\t', '\r', '\n']) {
        Cow::Owned(text.replace(['\t', '\r', '\n'], ""))
    } else {
        Cow::Borrowed(text)
    };
    let login = mask_login(&normalized);
    if let Cow::Owned(masked) = mask_values(&login) {
        return Some(masked);
    }
    match login {
        Cow::Owned(masked) => Some(masked),
        Cow::Borrowed(_) => None,
    }
}

/// replaces the `user:password@` of a web link's authority with `***@`. the authority starts
/// after any number of slashes, as a URL parser reads `https:host` and `https:///host` too, and
/// only web schemes count, so `figshare:1@2` keeps its version.
fn mask_login(text: &str) -> Cow<'_, str> {
    let Some((scheme, rest)) = text.split_once(':') else {
        return Cow::Borrowed(text);
    };
    if !["http", "https", "ftp"]
        .iter()
        .any(|web| scheme.trim().eq_ignore_ascii_case(web))
    {
        return Cow::Borrowed(text);
    }
    let start = text.len() - rest.trim_start_matches(['/', '\\']).len();
    let end = text[start..]
        .find(['/', '\\', '?', '#'])
        .map_or(text.len(), |at| start + at);
    match text[start..end].rfind('@') {
        Some(at) => Cow::Owned(format!("{}***{}", &text[..start], &text[start + at..])),
        None => Cow::Borrowed(text),
    }
}

fn mask_values(text: &str) -> Cow<'_, str> {
    let (head, fragment) = text.split_once('#').unwrap_or((text, ""));
    let (address, query) = head.split_once('?').unwrap_or((head, ""));
    let masked_query = mask_pairs(query);
    let masked_fragment = mask_pairs(fragment);
    if matches!(masked_query, Cow::Borrowed(_)) && matches!(masked_fragment, Cow::Borrowed(_)) {
        return Cow::Borrowed(text);
    }
    let query_separator = if head.contains('?') { "?" } else { "" };
    let fragment_separator = if text.contains('#') { "#" } else { "" };
    Cow::Owned(format!(
        "{address}{query_separator}{masked_query}{fragment_separator}{masked_fragment}"
    ))
}

/// `text`, a query or a fragment, with each value under a name not known to be harmless replaced
/// by `***` and the rest as written.
fn mask_pairs(text: &str) -> Cow<'_, str> {
    let hidden = |pair: &'_ str| {
        pair.split_once('=')
            .is_some_and(|(name, value)| !value.is_empty() && !is_harmless(name))
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
        ] {
            assert_eq!(mask_query(text), shown, "{text}");
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

    /// dataverse's api takes a person's api key as `key`, which elsewhere names a file or a
    /// setting.
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

    /// a value shows only under a name known to be harmless, since any other may grant access
    /// under a name no list knows. a name without a value shows as written.
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
                "https://example.org/a#access_token=***&page=***",
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
