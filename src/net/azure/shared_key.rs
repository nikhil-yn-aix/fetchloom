//! azure storage's shared key: a request signed with its account's key, as azure defines it for
//! the blob service from version 2015-02-21. the key only derives the signature and is never sent.

use std::fmt::Write as _;

use percent_encoding::percent_decode_str;
use reqwest::header::HeaderMap;
use url::Url;

use crate::auth::azure::AccountKey;

/// the standard headers whose values a signature holds, in its order, each on a line of its
/// own, empty when the request has none.
const SIGNED: [&str; 11] = [
    "content-encoding",
    "content-language",
    "content-length",
    "content-md5",
    "content-type",
    "date",
    "if-modified-since",
    "if-match",
    "if-none-match",
    "if-unmodified-since",
    "range",
];

/// the `Authorization` value that signs a request of `method` with `headers` for `url`, a link
/// to `account`'s blob host, with `key`: `SharedKey <account>:<signature>`.
pub(crate) fn authorization(
    account: &str,
    key: &AccountKey,
    method: &str,
    headers: &HeaderMap,
    url: &Url,
) -> String {
    let text = signed_text(account, method, headers, url);
    let signature = crate::auth::base64(&crate::net::sign::hmac(key.bytes(), text.as_bytes()));
    format!("SharedKey {account}:{signature}")
}

/// what a signature signs: the method, the standard headers in their order, every `x-ms-`
/// header by name, then the account, the path as the link writes it and each query parameter,
/// decoded, by name.
fn signed_text(account: &str, method: &str, headers: &HeaderMap, url: &Url) -> String {
    let value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    };
    let mut text = format!("{method}\n");
    for name in SIGNED {
        // a body of no bytes signs as no length at all
        let said = match value(name) {
            "0" if name == "content-length" => "",
            said => said,
        };
        text.push_str(said);
        text.push('\n');
    }
    let mut own: Vec<(&str, &str)> = headers
        .keys()
        .map(reqwest::header::HeaderName::as_str)
        .filter(|name| name.starts_with("x-ms-"))
        .map(|name| (name, value(name).trim()))
        .collect();
    own.sort_unstable();
    for (name, said) in own {
        let _ = writeln!(text, "{name}:{said}");
    }
    let _ = write!(text, "/{account}{}", url.path());
    let mut pairs: Vec<(String, String)> = url
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, said) = pair.split_once('=').unwrap_or((pair, ""));
            let decoded = |part: &str| percent_decode_str(part).decode_utf8_lossy().into_owned();
            (decoded(name).to_ascii_lowercase(), decoded(said))
        })
        .collect();
    pairs.sort();
    let mut previous: Option<String> = None;
    for (name, said) in pairs {
        if previous.as_deref() == Some(name.as_str()) {
            let _ = write!(text, ",{said}");
        } else {
            let _ = write!(text, "\n{name}:{said}");
            previous = Some(name);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use reqwest::header::HeaderValue;

    use super::*;

    /// a key made for these tests, the base64 of an english sentence.
    const KEY: &str = "ZmV0Y2hsb29tIHRlc3Qga2V5LCBub3QgYSBzZWNyZXQgZm9yIGFueSBhY2NvdW50ISE=";

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(*name, HeaderValue::from_static(value));
        }
        headers
    }

    /// the cases are azure's own example, line for line, and what azure-storage-blob computes for
    /// the same requests, or the page's rules for a `Range` that library sends otherwise.
    #[test]
    fn a_signature_is_the_one_azure_computes() {
        let key = AccountKey::parse(KEY).unwrap();
        let example = Url::parse(
            "https://myaccount.blob.core.windows.net/mycontainer?restype=container&comp=metadata&timeout=20",
        )
        .unwrap();
        let dated = headers(&[
            ("x-ms-date", "Fri, 26 Jun 2015 23:39:12 GMT"),
            ("x-ms-version", "2015-02-21"),
        ]);
        assert_eq!(
            signed_text("myaccount", "GET", &dated, &example),
            "GET\n\n\n\n\n\n\n\n\n\n\n\nx-ms-date:Fri, 26 Jun 2015 23:39:12 GMT\nx-ms-version:2015-02-21\n/myaccount/mycontainer\ncomp:metadata\nrestype:container\ntimeout:20"
        );
        assert_eq!(
            authorization("myaccount", &key, "GET", &dated, &example),
            "SharedKey myaccount:RZ7gvPiXaS+3qVyAF/ZlBC/AllufBqAjznHCO09yT60="
        );
        let now = [
            ("x-ms-date", "Thu, 12 Nov 2026 12:00:00 GMT"),
            ("x-ms-version", "2020-10-02"),
        ];
        let listing = Url::parse(
            "https://lab.blob.core.windows.net/raw?restype=container&comp=list&prefix=scans%2Fa%20b",
        )
        .unwrap();
        assert_eq!(
            authorization("lab", &key, "GET", &headers(&now), &listing),
            "SharedKey lab:LV2vV4serNpUlTyyaQb/XMxKdXDgr2poy6jKhuV9MBQ="
        );
        let mut resumed = headers(&now);
        resumed.insert("range", HeaderValue::from_static("bytes=100-"));
        resumed.insert("if-none-match", HeaderValue::from_static("\"0x8DC\""));
        resumed.insert("if-range", HeaderValue::from_static("\"0x8DC\""));
        resumed.insert("accept", HeaderValue::from_static("*/*"));
        let blob = Url::parse("https://lab.blob.core.windows.net/raw/scans/a%20b.csv").unwrap();
        assert_eq!(
            authorization("lab", &key, "GET", &resumed, &blob),
            "SharedKey lab:hBvCK120Fr4kLQlEZr64AppmFsvHImVXh0OM+XLKV3Y="
        );
    }

    /// names match whatever their case, and the values sign sorted, parted by commas.
    #[test]
    fn a_repeated_parameter_signs_its_values_together() {
        let url = Url::parse("https://lab.blob.core.windows.net/raw?include=b&comp=list&Include=a")
            .unwrap();
        assert!(
            signed_text("lab", "GET", &HeaderMap::new(), &url)
                .ends_with("/lab/raw\ncomp:list\ninclude:a,b")
        );
    }
}
