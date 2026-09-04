//! Signing a request with a secret that is never sent.

use std::fmt::Write as _;

use fetchloom_engine::credential::SigningKeys;
use sha2::{Digest, Sha256};

const DIGEST_LEN: usize = 32;

const BLOCK_LEN: usize = 64;

fn keyed_hash(key: &[u8], message: &[u8]) -> [u8; DIGEST_LEN] {
    let mut block = [0_u8; BLOCK_LEN];
    if key.len() > BLOCK_LEN {
        let reduced = Sha256::digest(key);
        block[..DIGEST_LEN].copy_from_slice(&reduced);
    } else {
        block[..key.len()].copy_from_slice(key);
    }

    let mut inner_key = [0x36_u8; BLOCK_LEN];
    let mut outer_key = [0x5c_u8; BLOCK_LEN];
    for index in 0..BLOCK_LEN {
        inner_key[index] ^= block[index];
        outer_key[index] ^= block[index];
    }

    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(message);
    let inner = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner);
    let mut answer = [0_u8; DIGEST_LEN];
    answer.copy_from_slice(&outer.finalize());
    answer
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

#[must_use]
pub fn payload_digest(payload: &[u8]) -> String {
    hex(&Sha256::digest(payload))
}

pub const EMPTY_PAYLOAD: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningTime {
    pub stamp: String,
    pub day: String,
}

impl SigningTime {
    #[must_use]
    pub fn now() -> Self {
        Self::at(fetchloom_engine::timestamp::Timestamp::now())
    }

    #[must_use]
    pub fn at(timestamp: fetchloom_engine::timestamp::Timestamp) -> Self {
        let rendered = timestamp.to_string();
        let compact: String = rendered
            .chars()
            .filter(|letter| letter.is_ascii_digit() || *letter == 'T' || *letter == 'Z')
            .collect();
        let day = compact.chars().take(8).collect();
        Self {
            stamp: compact,
            day,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Request<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub host: &'a str,
    pub payload: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signed {
    pub authorization: String,
    pub date: String,
    pub content_digest: String,
    pub security_token: Option<String>,
}

fn canonical_request(request: &Request<'_>, time: &SigningTime, token: Option<&str>) -> String {
    let mut headers = format!(
        "host:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
        request.host, request.payload, time.stamp
    );
    let mut signed = String::from("host;x-amz-content-sha256;x-amz-date");
    if let Some(token) = token {
        let _ = writeln!(headers, "x-amz-security-token:{token}");
        signed.push_str(";x-amz-security-token");
    }
    format!(
        "{}\n{}\n{}\n{headers}\n{signed}\n{}",
        request.method,
        request.path,
        canonical_query(request.query),
        request.payload
    )
}

fn canonical_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<(&str, &str)> = query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| pair.split_once('=').unwrap_or((pair, "")))
        .collect();
    pairs.sort_unstable();
    pairs
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

#[must_use]
pub fn sign(
    keys: &SigningKeys,
    service: &str,
    request: &Request<'_>,
    time: &SigningTime,
) -> Signed {
    let token = keys
        .session_token
        .as_ref()
        .map(|held| held.expose().as_str());
    let scope = format!("{}/{}/{service}/aws4_request", time.day, keys.region);

    let canonical = canonical_request(request, time, token);
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{}\n{scope}\n{}",
        time.stamp,
        hex(&Sha256::digest(canonical.as_bytes()))
    );

    let mut signing_key = keyed_hash(
        format!("AWS4{}", keys.secret_key.expose()).as_bytes(),
        time.day.as_bytes(),
    );
    signing_key = keyed_hash(&signing_key, keys.region.as_bytes());
    signing_key = keyed_hash(&signing_key, service.as_bytes());
    signing_key = keyed_hash(&signing_key, b"aws4_request");
    let signature = hex(&keyed_hash(&signing_key, to_sign.as_bytes()));

    let mut signed_headers = String::from("host;x-amz-content-sha256;x-amz-date");
    if token.is_some() {
        signed_headers.push_str(";x-amz-security-token");
    }

    Signed {
        authorization: format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            keys.access_key
        ),
        date: time.stamp.clone(),
        content_digest: request.payload.to_owned(),
        security_token: token.map(str::to_owned),
    }
}

#[cfg(test)]
mod tests {
    use super::{EMPTY_PAYLOAD, Request, SigningTime, hex, keyed_hash, payload_digest, sign};
    use fetchloom_engine::credential::SigningKeys;
    use fetchloom_engine::redact::Secret;

    #[test]
    fn the_keyed_hash_matches_the_published_test_vectors() {
        let one = keyed_hash(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex(&one),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );

        let two = keyed_hash(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex(&two),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );

        let three = keyed_hash(&[0xaa; 20], &[0xdd; 50]);
        assert_eq!(
            hex(&three),
            "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
        );

        let long = keyed_hash(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex(&long),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn the_empty_payload_digest_is_the_one_that_is_named() {
        assert_eq!(payload_digest(b""), EMPTY_PAYLOAD);
    }

    fn keys() -> SigningKeys {
        SigningKeys {
            access_key: "AKIAIOSFODNN7EXAMPLE".to_owned(),
            secret_key: Secret::new("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_owned()),
            session_token: None,
            region: "us-east-1".to_owned(),
        }
    }

    fn moment() -> SigningTime {
        SigningTime {
            stamp: "20130524T000000Z".to_owned(),
            day: "20130524".to_owned(),
        }
    }

    #[test]
    fn the_canonical_request_is_the_documented_form() {
        let canonical = super::canonical_request(
            &Request {
                method: "GET",
                path: "/test.txt",
                query: "",
                host: "examplebucket.s3.amazonaws.com",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
            None,
        );
        assert_eq!(
            canonical,
            format!(
                "GET\n/test.txt\n\nhost:examplebucket.s3.amazonaws.com\nx-amz-content-sha256:{EMPTY_PAYLOAD}\nx-amz-date:20130524T000000Z\n\nhost;x-amz-content-sha256;x-amz-date\n{EMPTY_PAYLOAD}"
            )
        );
    }

    #[test]
    fn the_credential_scope_names_the_day_the_region_and_the_service() {
        let signed = sign(
            &keys(),
            "s3",
            &Request {
                method: "GET",
                path: "/test.txt",
                query: "",
                host: "examplebucket.s3.amazonaws.com",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        assert!(
            signed.authorization.starts_with(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, SignedHeaders=host;x-amz-content-sha256;x-amz-date, Signature="
            ),
            "{}",
            signed.authorization
        );
        let signature = signed
            .authorization
            .rsplit("Signature=")
            .next()
            .unwrap_or_default();
        assert_eq!(signature.len(), 64, "a signature is one SHA-256 in hex");
        assert!(signature.chars().all(|letter| letter.is_ascii_hexdigit()));
    }

    #[test]
    fn a_different_day_produces_a_different_signature() {
        let request = Request {
            method: "GET",
            path: "/object",
            query: "",
            host: "bucket.example",
            payload: EMPTY_PAYLOAD,
        };
        let one = sign(&keys(), "s3", &request, &moment());
        let other = sign(
            &keys(),
            "s3",
            &request,
            &SigningTime {
                stamp: "20130525T000000Z".to_owned(),
                day: "20130525".to_owned(),
            },
        );
        assert_ne!(
            one.authorization, other.authorization,
            "the day a signing key is derived for did not change the signature"
        );
    }

    #[test]
    fn a_signature_never_carries_the_secret_key() {
        let signed = sign(
            &keys(),
            "s3",
            &Request {
                method: "GET",
                path: "/object",
                query: "",
                host: "bucket.example",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        let everything = format!("{signed:?}");
        assert!(
            !everything.contains("wJalrXUtnFEMI"),
            "the secret key reached the headers: {everything}"
        );
        assert!(
            everything.contains("AKIAIOSFODNN7EXAMPLE"),
            "the access key names which credential signed, and travels in the clear"
        );
    }

    #[test]
    fn a_session_token_is_signed_over_and_carried() {
        let mut with_token = keys();
        with_token.session_token = Some(Secret::new("the-session-token".to_owned()));
        let signed = sign(
            &with_token,
            "s3",
            &Request {
                method: "GET",
                path: "/object",
                query: "",
                host: "bucket.example",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        assert_eq!(
            signed.security_token.as_deref(),
            Some("the-session-token"),
            "a temporary credential's token was not carried"
        );
        assert!(
            signed.authorization.contains(
                "SignedHeaders=host;x-amz-content-sha256;x-amz-date;x-amz-security-token"
            ),
            "the token was carried without being signed over: {}",
            signed.authorization
        );
    }

    #[test]
    fn a_different_region_produces_a_different_signature() {
        let one = sign(
            &keys(),
            "s3",
            &Request {
                method: "GET",
                path: "/object",
                query: "",
                host: "bucket.example",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        let mut elsewhere = keys();
        elsewhere.region = "eu-west-1".to_owned();
        let other = sign(
            &elsewhere,
            "s3",
            &Request {
                method: "GET",
                path: "/object",
                query: "",
                host: "bucket.example",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        assert_ne!(
            one.authorization, other.authorization,
            "the region a signature is computed over did not change it"
        );
    }

    #[test]
    fn a_query_and_a_range_change_the_signature() {
        let plain = sign(
            &keys(),
            "s3",
            &Request {
                method: "GET",
                path: "/object",
                query: "",
                host: "bucket.example",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        let with_query = sign(
            &keys(),
            "s3",
            &Request {
                method: "GET",
                path: "/object",
                query: "versionId=2",
                host: "bucket.example",
                payload: EMPTY_PAYLOAD,
            },
            &moment(),
        );
        assert_ne!(plain.authorization, with_query.authorization);
    }

    #[test]
    fn the_query_is_signed_in_byte_order_whatever_order_it_was_given_in() {
        let signed_over = |query: &str| {
            sign(
                &keys(),
                "s3",
                &Request {
                    method: "GET",
                    path: "/object",
                    query,
                    host: "bucket.example",
                    payload: EMPTY_PAYLOAD,
                },
                &moment(),
            )
            .authorization
        };

        assert_eq!(
            signed_over("versionId=2&partNumber=3"),
            signed_over("partNumber=3&versionId=2"),
            "the same query in two orders signed differently, so a source that reorders it \
             refuses the signature"
        );
        assert_eq!(
            signed_over("uploads"),
            signed_over("uploads="),
            "a parameter with no value signed differently from the same parameter with an empty \
             one"
        );
        assert_ne!(
            signed_over("versionId=2&partNumber=3"),
            signed_over("versionId=3&partNumber=2"),
            "two different queries signed the same"
        );
    }
}
