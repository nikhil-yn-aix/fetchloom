//! AWS signature version 4, which S3 and the services that copy its API check on a request made
//! with a key: a hash of the request's method, link and chosen headers, signed with a key that
//! the secret yields for one day, region and service. the secret itself never leaves the machine.

use std::fmt::{self, Write as _};
use std::time::SystemTime;

use hmac::{Hmac, KeyInit, Mac};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, percent_encode};
use sha2::Sha256;
use url::Url;

/// the hash of an empty body, which every GET states and signs.
pub(crate) const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// what a signature leaves unescaped in a path segment or a query: the unreserved characters.
/// everything else is escaped with uppercase hex, a space as `%20` and never `+`.
pub(crate) const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// an AWS access key: its id, its secret and, for temporary keys, the session they belong to.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Keys {
    pub(crate) id: String,
    pub(crate) secret: String,
    pub(crate) token: Option<String>,
}

impl Keys {
    /// the key id as messages show it, its last four characters only: `…WXYZ`.
    pub(crate) fn shown(&self) -> String {
        let skip = self.id.chars().count().saturating_sub(4);
        format!("\u{2026}{}", self.id.chars().skip(skip).collect::<String>())
    }
}

impl fmt::Debug for Keys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Keys")
            .field("id", &self.shown())
            .finish_non_exhaustive()
    }
}

/// where a signature holds: the time it was made, as `20130524T000000Z`, the region and the
/// service.
pub(crate) struct Scope<'a> {
    pub(crate) time: &'a str,
    pub(crate) region: &'a str,
    pub(crate) service: &'a str,
}

impl Scope<'_> {
    fn date(&self) -> &str {
        self.time.get(..8).unwrap_or(self.time)
    }

    /// `20130524/us-east-1/s3/aws4_request`, as the key id is sent with.
    fn credential(&self) -> String {
        format!(
            "{}/{}/{}/aws4_request",
            self.date(),
            self.region,
            self.service
        )
    }
}

/// one request as it is signed: its method, its link as sent, the headers signed, each a
/// lowercase name and its value with `host` among them, and the hash of its body.
pub(crate) struct Request<'a> {
    pub(crate) method: &'a str,
    pub(crate) url: &'a Url,
    pub(crate) headers: &'a [(&'a str, &'a str)],
    pub(crate) payload: &'a str,
}

impl Request<'_> {
    /// the signed headers, sorted by name, with their values trimmed and inner runs of spaces
    /// folded to one.
    fn sorted_headers(&self) -> Vec<(String, String)> {
        let mut headers: Vec<(String, String)> = self
            .headers
            .iter()
            .map(|(name, value)| {
                let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
                (name.to_ascii_lowercase(), value)
            })
            .collect();
        headers.sort();
        headers
    }

    fn signed_headers(&self) -> String {
        let names: Vec<String> = self
            .sorted_headers()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        names.join(";")
    }
}

/// the `authorization` header that signs `request` with `keys` for `scope`.
pub(crate) fn authorization(keys: &Keys, scope: &Scope<'_>, request: &Request<'_>) -> String {
    let hashed = crate::digest::sha256(canonical(request).as_bytes()).hex();
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{}\n{hashed}",
        scope.time,
        scope.credential()
    );
    // the signing key: the secret narrowed to one day, region and service
    let mut key = format!("AWS4{}", keys.secret).into_bytes();
    for part in [scope.date(), scope.region, scope.service, "aws4_request"] {
        key = hmac(&key, part.as_bytes()).to_vec();
    }
    let signature = crate::digest::hex(&hmac(&key, to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={signature}",
        keys.id,
        scope.credential(),
        request.signed_headers()
    )
}

/// the canonical request: the request reduced to the form both ends hash. the path and query
/// are escaped once, as S3 reads them, whatever escaping the link already had.
pub(crate) fn canonical(request: &Request<'_>) -> String {
    let path: Vec<String> = request.url.path().split('/').map(encode).collect();
    let mut pairs: Vec<(String, String)> = request
        .url
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (encode(name), encode(value))
        })
        .collect();
    pairs.sort();
    let query: Vec<String> = pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let headers =
        request
            .sorted_headers()
            .into_iter()
            .fold(String::new(), |mut text, (name, value)| {
                let _ = writeln!(text, "{name}:{value}");
                text
            });
    format!(
        "{}\n{}\n{}\n{headers}\n{}\n{}",
        request.method,
        path.join("/"),
        query.join("&"),
        request.signed_headers(),
        request.payload
    )
}

/// the time as `x-amz-date` states it, such as `20130524T000000Z`.
pub(crate) fn amz_date(now: SystemTime) -> String {
    crate::project::lock::timestamp(now).replace(['-', ':'], "")
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    // hmac takes a key of any length, so making one cannot fail
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!("hmac takes a key of any length"));
    mac.update(data);
    mac.finalize().into_bytes().into()
}

/// `text` escaped once: what it holds escaped already is read as the byte it stands for.
fn encode(text: &str) -> String {
    let bytes: Vec<u8> = percent_decode_str(text).collect();
    percent_encode(&bytes, UNRESERVED).to_string()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    /// a key from AWS's examples.
    fn example(id: &str, secret: &str, token: Option<&str>) -> Keys {
        Keys {
            id: id.to_owned(),
            secret: secret.to_owned(),
            token: token.map(str::to_owned),
        }
    }

    /// S3's own examples of signed requests, from its guide to signing with the authorization
    /// header: each request's canonical request hash and signature.
    #[test]
    fn s3s_examples_sign_as_aws_documents() {
        let keys = example(
            "AKIAIOSFODNN7EXAMPLE",
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            None,
        );
        let scope = Scope {
            time: "20130524T000000Z",
            region: "us-east-1",
            service: "s3",
        };
        for (link, range, hashed, signature) in [
            (
                "https://examplebucket.s3.amazonaws.com/test.txt",
                Some("bytes=0-9"),
                "7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972",
                "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
            ),
            (
                "https://examplebucket.s3.amazonaws.com/?max-keys=2&prefix=J",
                None,
                "df57d21db20da04d7fa30298dd4488ba3a2b47ca3a489c74750e0f1e7df1b9b7",
                "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7",
            ),
            (
                "https://examplebucket.s3.amazonaws.com/?lifecycle",
                None,
                "9766c798316ff2757b517bc739a67f6213b4ab36dd5da2f94eaebf79c77395ca",
                "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543",
            ),
        ] {
            let url = Url::parse(link).unwrap();
            let mut headers = vec![("host", "examplebucket.s3.amazonaws.com")];
            headers.extend(range.map(|range| ("range", range)));
            headers.extend([
                ("x-amz-content-sha256", EMPTY),
                ("x-amz-date", "20130524T000000Z"),
            ]);
            let request = Request {
                method: "GET",
                url: &url,
                headers: &headers,
                payload: EMPTY,
            };
            assert_eq!(
                crate::digest::sha256(canonical(&request).as_bytes()).hex(),
                hashed,
                "{link}"
            );
            let signed: Vec<_> = headers.iter().map(|(name, _)| *name).collect();
            assert_eq!(
                authorization(&keys, &scope, &request),
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders={}, Signature={signature}",
                    signed.join(";")
                ),
                "{link}"
            );
        }
    }

    /// cases from the signing test suite AWS publishes with its common runtime, in
    /// awslabs/aws-c-auth: a session token, a space in the path signed escaped once, as S3
    /// takes it, and a query sorted by its escaped names.
    #[test]
    fn the_signing_suite_signs_as_aws_publishes() {
        let token = "6e86291e8372ff2a2260956d9b8aae1d763fbf315fa00fa31553b73ebf194267";
        let scope = Scope {
            time: "20150830T123600Z",
            region: "us-east-1",
            service: "service",
        };
        for (link, token, canonical_request, signature) in [
            (
                "https://example.amazonaws.com/",
                Some(token),
                format!(
                    "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\nx-amz-security-token:{token}\n\nhost;x-amz-date;x-amz-security-token\n{EMPTY}"
                ),
                "07ec1639c89043aa0e3e2de82b96708f198cceab042d4a97044c66dd9f74e7f8",
            ),
            (
                "https://example.amazonaws.com/example space/",
                None,
                format!(
                    "GET\n/example%20space/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\n{EMPTY}"
                ),
                "652487583200325589f1fba4c7e578f72c47cb61beeca81406b39ddec1366741",
            ),
            (
                "https://example.amazonaws.com/?Param-3=Value3&Param=Value2&%E1%88%B4=Value1",
                None,
                format!(
                    "GET\n/\n%E1%88%B4=Value1&Param=Value2&Param-3=Value3\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\n{EMPTY}"
                ),
                "371d3713e185cc334048618a97f809c9ffe339c62934c032af5a0e595648fcac",
            ),
        ] {
            let keys = example(
                "AKIDEXAMPLE",
                "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
                token,
            );
            let url = Url::parse(link).unwrap();
            let mut headers = vec![
                ("host", "example.amazonaws.com"),
                ("x-amz-date", "20150830T123600Z"),
            ];
            headers.extend(token.map(|token| ("x-amz-security-token", token)));
            let request = Request {
                method: "GET",
                url: &url,
                headers: &headers,
                payload: EMPTY,
            };
            assert_eq!(canonical(&request), canonical_request, "{link}");
            let signed: Vec<_> = headers.iter().map(|(name, _)| *name).collect();
            assert_eq!(
                authorization(&keys, &scope, &request),
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders={}, Signature={signature}",
                    signed.join(";")
                ),
                "{link}"
            );
        }
    }

    #[test]
    fn keys_are_escaped_once_whether_the_link_escaped_them_or_not() {
        for link in [
            "https://b.s3.amazonaws.com/a%20b%2Bc/%C3%A9.txt",
            "https://b.s3.amazonaws.com/a b%2Bc/é.txt",
            "https://b.s3.amazonaws.com/a%20b%2bc/%c3%a9.txt",
        ] {
            let url = Url::parse(link).unwrap();
            let request = Request {
                method: "GET",
                url: &url,
                headers: &[("host", "b.s3.amazonaws.com")],
                payload: EMPTY,
            };
            assert!(
                canonical(&request).starts_with("GET\n/a%20b%2Bc/%C3%A9.txt\n\n"),
                "{link}: {}",
                canonical(&request)
            );
        }
    }

    #[test]
    fn header_values_are_trimmed_and_their_spaces_folded() {
        let url = Url::parse("https://b.s3.amazonaws.com/").unwrap();
        let request = Request {
            method: "GET",
            url: &url,
            headers: &[("x-amz-date", " 1  2 "), ("host", "b.s3.amazonaws.com")],
            payload: EMPTY,
        };
        assert_eq!(
            canonical(&request),
            format!(
                "GET\n/\n\nhost:b.s3.amazonaws.com\nx-amz-date:1 2\n\nhost;x-amz-date\n{EMPTY}"
            )
        );
    }

    #[test]
    fn the_date_comes_from_the_given_time() {
        let time = UNIX_EPOCH + Duration::from_hours(380_376);
        assert_eq!(amz_date(time), "20130524T000000Z");
        assert_eq!(
            amz_date(time + Duration::from_secs(86_399)),
            "20130524T235959Z"
        );
    }

    #[test]
    fn only_the_last_four_characters_of_a_key_id_ever_show() {
        let keys = Keys {
            id: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned(),
            token: Some("session-secret".to_owned()),
        };
        assert_eq!(keys.shown(), "\u{2026}MPLE");
        let debug = format!("{keys:?}");
        assert!(debug.contains("\u{2026}MPLE"), "{debug}");
        for hidden in ["AKIAIOSFODNN7", "wJalrXUtnFEMI", "session-secret"] {
            assert!(!debug.contains(hidden), "{debug}");
        }
    }
}
