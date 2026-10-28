//! links as fetchloom shows them. a query value that grants access by itself, such as a token or
//! the signature of a presigned storage link, never reaches a message or a log.

use std::fmt;

use url::Url;

/// query keys whose values grant access to whoever holds them: tokens, keys, and the signatures
/// of presigned links on aws, google cloud, azure and cloudfront.
const SECRET: &[&str] = &[
    "token",
    "access_token",
    "api_key",
    "apikey",
    "password",
    "secret",
    "sig",
    "signature",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
    "x-goog-signature",
    "x-goog-credential",
    "key-pair-id",
];

/// the first query key of `url` whose value grants access, when it has one.
pub(crate) fn secret(url: &Url) -> Option<String> {
    url.query_pairs()
        .map(|(key, _)| key)
        .find(|key| is_secret(key))
        .map(|key| key.to_ascii_lowercase())
}

fn is_secret(key: &str) -> bool {
    SECRET.iter().any(|secret| key.eq_ignore_ascii_case(secret))
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
}
