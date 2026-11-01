//! S3: ListObjectsV2 over anonymous HTTPS, following a bucket to the region it lives in.

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Object, Prefix, is_region, object_url, query, s3_host, xml};
use crate::digest::Md5;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};
use crate::source::MOST_FILES;

/// where a bucket's requests go: the bucket's own host, or for a name with dots, which the
/// service's wildcard certificate does not cover, a path on the service's host.
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
        let base = if bucket.contains('.') {
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
pub(super) async fn list(prefix: &Prefix, api: &impl Api) -> Result<Vec<Object>, Error> {
    let mut endpoint = Endpoint::new(prefix, None)?;
    let mut moved = false;
    let mut token: Option<String> = None;
    let mut objects = Vec::new();
    loop {
        let response = api
            .get(&endpoint.listing(&prefix.key, token.as_deref()))
            .await?;
        let document = xml::parse(&response.text());
        if response.status != 200 {
            let said = document.as_ref().ok();
            if matches!(response.status, 301 | 400)
                && !moved
                && let Some(region) = region(&response, said)
            {
                endpoint = Endpoint::new(prefix, Some(&region))?;
                moved = true;
                continue;
            }
            if response.status == 301 {
                return Err(Error::new(
                    Kind::NetworkStatus,
                    format!("S3 moved {prefix} to a region it did not name"),
                )
                .help("try again later, or report it at https://github.com/nikhil-yn-aix/fetchloom/issues if it keeps happening"));
            }
            let code = said.and_then(|said| said.text_of("Code"));
            let message = said.and_then(|said| said.text_of("Message"));
            return Err(prefix.refused(&response, code, message));
        }
        let page = document.map_err(|reason| prefix.invalid(&reason))?;
        if page.name != "ListBucketResult" {
            return Err(prefix.invalid(&format!("the answer is <{}>", page.name)));
        }
        let escaped = page.text_of("EncodingType") == Some("url");
        for contents in page.children("Contents") {
            objects.push(object(prefix, &endpoint, contents, escaped)?);
            if objects.len() > MOST_FILES {
                return Err(prefix.too_many());
            }
        }
        match page.text_of("IsTruncated") {
            Some("false") => return Ok(objects),
            Some("true") => {}
            _ => return Err(prefix.invalid("a page does not say whether more follow")),
        }
        let next = page
            .text_of("NextContinuationToken")
            .filter(|next| !next.is_empty())
            .ok_or_else(|| prefix.invalid("a page says more follow but gives no token for them"))?;
        if token.as_deref() == Some(next) {
            return Err(prefix.invalid("a page repeated its continuation token"));
        }
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

fn object(
    prefix: &Prefix,
    endpoint: &Endpoint,
    contents: &xml::Element,
    escaped: bool,
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
    let size = contents
        .text_of("Size")
        .and_then(|size| size.parse().ok())
        .ok_or_else(|| prefix.invalid(&format!("the size of \"{key}\" is not a number")))?;
    Ok(Object {
        url: object_url(&endpoint.base, &key),
        md5: contents.text_of("ETag").and_then(etag_md5),
        key,
        size,
    })
}

/// the MD5 an ETag holds. an object uploaded in parts has an ETag with a `-` and a part count,
/// which is no MD5 of its bytes.
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
