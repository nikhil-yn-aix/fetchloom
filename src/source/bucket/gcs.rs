//! google cloud storage: the JSON API's object list over HTTPS, without a token unless the
//! `gcs_buckets` setting lists the bucket and it refused a request without one.

use serde::Deserialize;
use url::Url;

use super::{Listed, Object, Prefix, object_url, query};
use crate::digest::Md5;
use crate::error::Error;
use crate::net::Api;
use crate::source::invalid;

/// the object fields a listing needs, so pages stay small.
const FIELDS: &str = "items(name,size,md5Hash,generation),nextPageToken";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    #[serde(default)]
    items: Vec<Item>,
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Item {
    name: String,
    size: String,
    md5_hash: Option<String>,
    generation: Option<String>,
}

/// every object under the prefix, a page of up to 1,000 at a time.
pub(super) async fn list(prefix: &Prefix, api: &impl Api) -> Result<Listed, Error> {
    let bad = || invalid(prefix, "the bucket makes no valid link");
    let listing = Url::parse(&format!(
        "https://storage.googleapis.com/storage/v1/b/{}/o",
        prefix.bucket
    ))
    .map_err(|_| bad())?;
    let objects_at = Url::parse(&format!(
        "https://storage.googleapis.com/{}/",
        prefix.bucket
    ))
    .map_err(|_| bad())?;
    let mut token: Option<String> = None;
    let mut pages = super::Pages::new(api.most_files());
    let mut objects = Vec::new();
    let mut credentials = false;
    loop {
        let mut pairs = vec![("prefix", prefix.key.as_str()), ("fields", FIELDS)];
        pairs.extend(token.as_deref().map(|token| ("pageToken", token)));
        let mut url = listing.clone();
        url.set_query(Some(&query(&pairs)));
        let response = api.get(&url).await?;
        if response.status != 200 {
            let said: Option<serde_json::Value> = serde_json::from_slice(&response.body).ok();
            let error = said.as_ref().map(|said| &said["error"]);
            let code = error.and_then(|error| error["errors"][0]["reason"].as_str());
            let message = error.and_then(|error| error["message"].as_str());
            return Err(prefix.refused(&response, code, message, None));
        }
        let page: Page = response.json()?;
        credentials |= super::with_credentials(&response);
        for item in page.items {
            objects.push(object(prefix, &objects_at, item)?);
            if objects.len() > api.most_files() {
                return Err(prefix.too_many(api.most_files()));
            }
        }
        match page.next_page_token.filter(|next| !next.is_empty()) {
            None => {
                return Ok(Listed {
                    objects,
                    credentials,
                });
            }
            Some(next) => {
                pages.follow(prefix, &next, objects.len())?;
                token = Some(next);
            }
        }
    }
}

/// an object, linked at its generation, so a later upload under the same name is not taken
/// for it.
fn object(prefix: &Prefix, objects_at: &Url, item: Item) -> Result<Object, Error> {
    prefix.check_key(&item.name)?;
    let size = item.size.parse().map_err(|_| {
        invalid(
            prefix,
            &format!("the size of \"{}\" is not a number", item.name),
        )
    })?;
    let md5 = item
        .md5_hash
        .as_deref()
        .map(|hash| {
            base64_md5(hash).ok_or_else(|| {
                invalid(
                    prefix,
                    &format!("the MD5 of \"{}\" is not base64", item.name),
                )
            })
        })
        .transpose()?;
    let mut url = object_url(objects_at, &item.name);
    let generation = item.generation.as_deref().ok_or_else(|| {
        invalid(
            prefix,
            &format!(
                "the object \"{}\" has no generation",
                item.name.escape_debug()
            ),
        )
    })?;
    if generation.is_empty() || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(
            prefix,
            &format!(
                "the generation of \"{}\" is not a number",
                item.name.escape_debug()
            ),
        ));
    }
    url.set_query(Some(&format!("generation={generation}")));
    Ok(Object {
        key: item.name,
        size,
        md5,
        etag: None,
        url,
    })
}

/// the 16 bytes of an MD5 written in padded standard base64, as cloud storage and azure state
/// them.
pub(super) fn base64_md5(text: &str) -> Option<Md5> {
    let bytes = crate::auth::from_base64(text).filter(|bytes| bytes.len() == 16)?;
    Md5::from_hex(&crate::digest::hex(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_hashes_are_read_from_base64() {
        assert_eq!(
            base64_md5("1B2M2Y8AsgTpgAmY7PhCfg=="),
            Md5::from_hex("d41d8cd98f00b204e9800998ecf8427e")
        );
        assert_eq!(
            base64_md5("upfBewFSlvnN3ecTWMrHTg=="),
            Md5::from_hex("ba97c17b015296f9cddde71358cac74e")
        );
        for bad in [
            "",
            "1B2M2Y8AsgTpgAmY7PhCfg",
            "1B2M2Y8AsgTpgAmY7PhCfh==",
            "1B2M2Y8AsgTpgAmY7PhC*g==",
            "1B2M2Y8AsgTpgAmY7PhCfgAA",
            "AAAA",
        ] {
            assert_eq!(base64_md5(bad), None, "{bad}");
        }
    }
}
