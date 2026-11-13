//! S3: `ListObjectsV2` over anonymous HTTPS, following a bucket to the region it lives in.

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Keyed, Listed, Object, Prefix, is_region, object_url, query, s3_host, xml};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

/// where a bucket's requests go: its own host, or a path on the service's host for a dotted name
/// the wildcard certificate does not cover, as always for a bucket at another service.
struct Endpoint {
    base: Url,
}

impl Endpoint {
    fn new(prefix: &Prefix, region: Option<&str>) -> Result<Endpoint, Error> {
        let service = match region {
            Some(region) => format!("s3.{region}.amazonaws.com"),
            None => "s3.amazonaws.com".to_owned(),
        };
        let bucket = &prefix.bucket;
        let base = if let Some(origin) = &prefix.endpoint {
            format!("{}/{bucket}/", origin.ascii_serialization())
        } else if bucket.contains('.') {
            format!("https://{service}/{bucket}/")
        } else {
            format!("https://{bucket}.{service}/")
        };
        let base =
            Url::parse(&base).map_err(|_| prefix.invalid("the bucket makes no valid link"))?;
        Ok(Endpoint { base })
    }

    fn listing(&self, prefix: &str, token: Option<&str>) -> Url {
        let mut pairs = vec![
            ("list-type", "2"),
            ("prefix", prefix),
            ("encoding-type", "url"),
        ];
        pairs.extend(token.map(|token| ("continuation-token", token)));
        let mut url = self.base.clone();
        url.set_query(Some(&query(&pairs)));
        url
    }
}

/// every object under the prefix, a page of up to 1,000 at a time.
pub(super) async fn list(prefix: &Prefix, api: &impl Api) -> Result<Listed, Error> {
    let mut endpoint = Endpoint::new(prefix, None)?;
    let mut moved = false;
    // whether the endpoint names the bucket's region, which a signed request must name too
    let mut regional = false;
    let mut token: Option<String> = None;
    let mut pages = super::Pages::default();
    let mut objects = Vec::new();
    let mut credentials = false;
    loop {
        let response = api
            .get(&endpoint.listing(&prefix.key, token.as_deref()))
            .await?;
        let document = xml::parse(&response.text());
        if response.status != 200 {
            let said = document.as_ref().ok();
            if matches!(response.status, 301 | 400)
                && !moved
                && prefix.endpoint.is_none()
                && let Some(region) = region(&response, said)
            {
                endpoint = Endpoint::new(prefix, Some(&region))?;
                moved = true;
                regional = true;
                continue;
            }
            if response.status == 301 {
                let what = if moved {
                    format!("S3 moved {prefix} a second time")
                } else {
                    format!("S3 moved {prefix} to a region it did not name")
                };
                return Err(Error::new(Kind::NetworkStatus, what).help(format!(
                    "try again later, or report it at {} if it keeps happening",
                    crate::error::ISSUES
                )));
            }
            let code = said.and_then(|said| said.text_of("Code"));
            let message = said.and_then(|said| said.text_of("Message"));
            let named = said.and_then(|said| said.text_of("Region"));
            return Err(prefix.refused(&response, code, message, named));
        }
        let page = document.map_err(|reason| prefix.invalid(&reason))?;
        if page.name != "ListBucketResult" {
            return Err(prefix.invalid(&format!("the answer is <{}>", page.name)));
        }
        credentials |= super::with_credentials(&response);
        let signed = match &response.keyed {
            Some(Keyed::Signed { region, .. }) => Some(region.as_str()),
            _ => None,
        };
        // a signed listing names where the bucket is, so its links can carry the region a
        // later run signs them for
        if let Some(region) = signed
            && !regional
            && prefix.endpoint.is_none()
            && is_region(region)
        {
            endpoint = Endpoint::new(prefix, Some(region))?;
            regional = true;
        }
        let escaped = page.text_of("EncodingType") == Some("url");
        for contents in page.children("Contents") {
            objects.push(object(
                prefix,
                &endpoint,
                contents,
                escaped,
                signed.is_some(),
            )?);
            if objects.len() > crate::source::most_files() {
                return Err(prefix.too_many());
            }
        }
        match page.text_of("IsTruncated") {
            Some("false") => {
                return Ok(Listed {
                    objects,
                    credentials,
                });
            }
            Some("true") => {}
            _ => return Err(prefix.invalid("a page does not say whether more follow")),
        }
        let next = page
            .text_of("NextContinuationToken")
            .filter(|next| !next.is_empty())
            .ok_or_else(|| prefix.invalid("a page says more follow but gives no token for them"))?;
        pages.follow(prefix, next, objects.len())?;
        token = Some(next.to_owned());
    }
}

/// the region an answer sends the bucket to: its `x-amz-bucket-region` header, else the region
/// or regional endpoint the error names.
fn region(response: &Response, said: Option<&xml::Element>) -> Option<String> {
    let named = || {
        let said = said?;
        said.text_of("Region")
            .or_else(|| s3_host(said.text_of("Endpoint")?).and_then(|(_, region)| region))
            .map(str::to_owned)
    };
    response
        .region
        .clone()
        .or_else(named)
        .filter(|region| is_region(region))
}

/// an object of the listing. its `ETag` stands for an MD5 only in an unsigned AWS listing: a
/// bucket that needs a key may encrypt with its own, and other services promise nothing.
fn object(
    prefix: &Prefix,
    endpoint: &Endpoint,
    contents: &xml::Element,
    escaped: bool,
    signed: bool,
) -> Result<Object, Error> {
    let raw = contents
        .text_of("Key")
        .ok_or_else(|| prefix.invalid("an object has no key"))?;
    // with encoding-type=url, S3 escapes keys as a form does, a space as +
    let key = if escaped {
        percent_decode_str(&raw.replace('+', " "))
            .decode_utf8()
            .map_err(|_| prefix.invalid(&format!("the key \"{raw}\" is not UTF-8")))?
            .into_owned()
    } else {
        raw.to_owned()
    };
    prefix.check_key(&key)?;
    let size = contents
        .text_of("Size")
        .and_then(|size| size.parse().ok())
        .ok_or_else(|| prefix.invalid(&format!("the size of \"{key}\" is not a number")))?;
    let mut url = object_url(&endpoint.base, &key);
    // s3 reserves the exact key soap on a bucket host for its legacy SOAP endpoint
    if key == "soap" && prefix.endpoint.is_none() && !prefix.bucket.contains('.') {
        let service = endpoint
            .base
            .host_str()
            .and_then(|host| host.strip_prefix(&format!("{}.", prefix.bucket)))
            .ok_or_else(|| prefix.invalid("the bucket makes no valid link"))?;
        url.set_host(Some(service))
            .map_err(|_| prefix.invalid("the bucket makes no valid link"))?;
        url.set_path(&format!("/{}/soap", prefix.bucket));
    }
    let etag = contents.text_of("ETag");
    let md5 = if prefix.endpoint.is_none() && !signed {
        etag.and_then(etag_md5)
    } else {
        None
    };
    Ok(Object {
        key,
        size,
        md5,
        etag: etag.map(str::to_owned),
        url,
    })
}

/// the MD5 an `ETag` holds. an object uploaded in parts has an `ETag` with a `-` and a part
/// count, which is no MD5 of its bytes.
fn etag_md5(etag: &str) -> Option<Md5> {
    let etag = etag.trim().trim_matches('"');
    if etag.contains('-') {
        return None;
    }
    Md5::from_hex(etag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_part_etag_is_an_md5() {
        assert_eq!(
            etag_md5("\"1beeac62f1ded905f45c6d6d20a4688b\""),
            Md5::from_hex("1beeac62f1ded905f45c6d6d20a4688b")
        );
        assert_eq!(etag_md5("\"1beeac62f1ded905f45c6d6d20a4688b-12\""), None);
        assert_eq!(etag_md5("\"W/abc\""), None);
        assert_eq!(etag_md5(""), None);
    }
}
