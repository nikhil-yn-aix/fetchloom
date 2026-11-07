//! links as fetchloom shows them. a query or fragment value that grants access, such as a token or
//! the signature of a presigned storage link, never reaches a message or a log.

use std::borrow::Cow;
use std::fmt;

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

/// the first query or fragment key of `url` whose value grants access, when it has one.
pub(crate) fn secret(url: &Url) -> Option<String> {
    url.query_pairs()
        .chain(url::form_urlencoded::parse(
            url.fragment().unwrap_or_default().as_bytes(),
        ))
        .map(|(key, _)| key)
        .find(|key| is_secret(key))
        .map(|key| key.to_ascii_lowercase())
}

/// whether a query key's value grants access: any token, secret, password, signature, credential
/// or api key however it is spelled, and the short names azure (`sig`) and cloudfront
/// (`Key-Pair-Id`) sign links with.
fn is_secret(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    SECRET_ENDINGS.iter().any(|ending| key.ends_with(ending))
        || matches!(key.as_str(), "sig" | "keypairid")
}

/// a link as messages and logs show it, each value that grants access replaced by `***`.
pub(crate) struct Shown<'a>(pub(crate) &'a Url);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&mask_query(self.0.as_str()))
    }
}

/// masks a login and secret query or fragment values, even before a reference is a valid URL.
pub(crate) fn mask_query(text: &str) -> Cow<'_, str> {
    let text = mask_login(text);
    if let Cow::Owned(masked) = mask_secret_values(&text) {
        return Cow::Owned(masked);
    }
    text
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

fn mask_secret_values(text: &str) -> Cow<'_, str> {
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

fn mask_pairs(text: &str) -> Cow<'_, str> {
    let pairs = url::form_urlencoded::parse(text.as_bytes());
    if !pairs.clone().any(|(key, _)| is_secret(&key)) {
        return Cow::Borrowed(text);
    }
    let pairs = pairs.map(|(key, value)| {
        let value = if is_secret(&key) {
            Cow::Borrowed("***")
        } else {
            value
        };
        (key, value)
    });
    let mut encoded = url::form_urlencoded::Serializer::new(String::new());
    encoded.extend_pairs(pairs);
    Cow::Owned(encoded.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_storage_links_show_without_their_secrets() {
        let url = Url::parse(
            "https://bucket.s3.amazonaws.com/data.csv?X-Amz-Credential=AKIA1&X-Amz-Signature=abc123&X-Amz-Expires=600",
        )
        .unwrap();
        assert_eq!(secret(&url).as_deref(), Some("x-amz-credential"));
        let shown = Shown(&url).to_string();
        assert!(
            !shown.contains("AKIA1") && !shown.contains("abc123"),
            "{shown}"
        );
        assert!(shown.contains("X-Amz-Expires=600"), "{shown}");
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
            "auth-token",
            "refresh_token",
            "client_secret",
            "x-api-key",
            "apiKey",
            "Sig",
        ] {
            let url = Url::parse(&format!("https://example.org/a.csv?{key}=abc123")).unwrap();
            assert!(secret(&url).is_some(), "{key}");
            assert!(!Shown(&url).to_string().contains("abc123"), "{key}");
        }
        for key in ["rlkey", "resourcekey", "dl", "raw", "key"] {
            let url = Url::parse(&format!("https://example.org/a.csv?{key}=1")).unwrap();
            assert_eq!(secret(&url), None, "{key}");
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
                "https://example.org/a?download=1#token=***",
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
    fn fragment_navigation_without_credentials_is_unchanged() {
        for text in [
            "https://example.org/a#chapter-2",
            "https://example.org/a?download=1#page=2",
            "https://example.org/a#/route?index=2",
            "https://example.org/a?#",
        ] {
            assert!(matches!(mask_query(text), Cow::Borrowed(_)));
            assert_eq!(Shown(&Url::parse(text).unwrap()).to_string(), text);
            assert_eq!(secret(&Url::parse(text).unwrap()), None);
        }
    }
}
