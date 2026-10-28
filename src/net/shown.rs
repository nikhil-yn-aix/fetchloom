//! links as fetchloom shows them. a query value that grants access by itself, such as a token or
//! the signature of a presigned storage link, never reaches a message or a log.

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

/// the first query key of `url` whose value grants access, when it has one.
pub(crate) fn secret(url: &Url) -> Option<String> {
    url.query_pairs()
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
        if secret(self.0).is_none() {
            return f.write_str(self.0.as_str());
        }
        let pairs: Vec<(String, String)> = self
            .0
            .query_pairs()
            .map(|(key, value)| {
                let value = if is_secret(&key) {
                    "***".to_owned()
                } else {
                    value.into_owned()
                };
                (key.into_owned(), value)
            })
            .collect();
        let mut url = self.0.clone();
        url.query_pairs_mut().clear().extend_pairs(pairs);
        f.write_str(url.as_str())
    }
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
}
