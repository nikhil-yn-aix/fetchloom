//! google cloud storage: the JSON API's object list over anonymous HTTPS.

use std::fmt::Write as _;

use serde::Deserialize;
use url::Url;

use super::{Object, Prefix, object_url, query};
use crate::digest::Md5;
use crate::error::Error;
use crate::net::Api;
use crate::source::MOST_FILES;

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
pub(super) async fn list(prefix: &Prefix, api: &impl Api) -> Result<Vec<Object>, Error> {
    let bad = || prefix.invalid("the bucket makes no valid link");
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
    let mut pages = super::Pages::default();
    let mut objects = Vec::new();
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
            return Err(prefix.refused(&response, code, message));
        }
        let page: Page = response.json()?;
        for item in page.items {
            objects.push(object(prefix, &objects_at, item)?);
            if objects.len() > MOST_FILES {
                return Err(prefix.too_many());
            }
        }
        match page.next_page_token.filter(|next| !next.is_empty()) {
            None => return Ok(objects),
            Some(next) => {
                pages.follow(prefix, &next)?;
                token = Some(next);
            }
        }
    }
}

/// an object, linked at its generation, so a later upload under the same name is not taken
/// for it.
fn object(prefix: &Prefix, objects_at: &Url, item: Item) -> Result<Object, Error> {
    prefix.check_key(&item.name)?;
    let size = item
        .size
        .parse()
        .map_err(|_| prefix.invalid(&format!("the size of \"{}\" is not a number", item.name)))?;
    let md5 = item
        .md5_hash
        .as_deref()
        .map(|hash| {
            base64_md5(hash).ok_or_else(|| {
                prefix.invalid(&format!("the MD5 of \"{}\" is not base64", item.name))
            })
        })
        .transpose()?;
    let mut url = object_url(objects_at, &item.name);
    let generation = item.generation.as_deref().ok_or_else(|| {
        prefix.invalid(&format!(
            "the object \"{}\" has no generation",
            item.name.escape_debug()
        ))
    })?;
    if generation.is_empty() || !generation.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(prefix.invalid(&format!(
            "the generation of \"{}\" is not a number",
            item.name.escape_debug()
        )));
    }
    url.set_query(Some(&format!("generation={generation}")));
    Ok(Object {
        key: item.name,
        size,
        md5,
        url,
    })
}

/// the 16 bytes of an MD5 written in padded standard base64, as cloud storage states them.
fn base64_md5(text: &str) -> Option<Md5> {
    let digits = text.strip_suffix("==")?;
    if digits.len() != 22 {
        return None;
    }
    let mut bits: u32 = 0;
    let mut held = 0;
    let mut bytes = Vec::with_capacity(16);
    for byte in digits.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from((bits >> held) & 0xff).ok()?);
        }
    }
    // the last digit carries four bits past the sixteenth byte, which must be zero
    if bits & ((1 << held) - 1) != 0 {
        return None;
    }
    let hex = bytes
        .iter()
        .fold(String::with_capacity(32), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    Md5::from_hex(&hex)
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
