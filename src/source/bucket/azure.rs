//! azure blob storage: a container's `List Blobs` over HTTPS, up to 5,000 blobs a page, without a
//! SAS unless `azure_containers` lists the container and it refused a request without one.

use std::collections::HashSet;

use url::Url;

use super::{Listed, Object, Prefix, object_url, query};
use crate::auth::azure::BLOB_HOST;
use crate::error::Error;
use crate::net::{Api, Response};
use crate::source::invalid;
use crate::xml;

/// the container's link, ending in `/`, which its blobs' links extend.
fn container_url(prefix: &Prefix) -> Result<Url, Error> {
    Url::parse(&format!(
        "https://{}{BLOB_HOST}/{}/",
        prefix.account(),
        prefix.bucket
    ))
    .map_err(|_| invalid(prefix, "the container makes no valid link"))
}

/// every blob under the prefix, a page at a time, without the empty blobs that only mark a
/// folder other blobs are in.
pub(super) async fn list(prefix: &Prefix, api: &impl Api) -> Result<Listed, Error> {
    let blobs_at = container_url(prefix)?;
    let mut listing = blobs_at.clone();
    listing.set_path(blobs_at.path().trim_end_matches('/'));
    let mut marker: Option<String> = None;
    let mut pages = super::Pages::new(api.most_files());
    let (mut objects, mut empty) = (Vec::new(), 0);
    let mut credentials = false;
    loop {
        let mut pairs = vec![
            ("restype", "container"),
            ("comp", "list"),
            ("prefix", prefix.key.as_str()),
        ];
        pairs.extend(marker.as_deref().map(|marker| ("marker", marker)));
        let mut url = listing.clone();
        url.set_query(Some(&query(&pairs)));
        let response = api.get(&url).await?;
        let document = xml::parse(response.text().trim_start_matches('\u{feff}'));
        if response.status != 200 {
            return Err(refused(prefix, &response, document.as_ref().ok()));
        }
        let page = document.map_err(|reason| invalid(prefix, &reason))?;
        if page.name != "EnumerationResults" {
            return Err(invalid(prefix, &format!("the answer is <{}>", page.name)));
        }
        credentials |= super::with_credentials(&response);
        for blob in page
            .children("Blobs")
            .flat_map(|blobs| blobs.children("Blob"))
        {
            let object = object(prefix, &blobs_at, blob)?;
            // an empty blob may mark a folder, which only the whole listing tells
            empty += usize::from(object.size == 0);
            objects.push(object);
            if super::past_most(objects.len(), empty, api.most_files()) {
                return Err(prefix.too_many(api.most_files()));
            }
        }
        match page.text_of("NextMarker").filter(|next| !next.is_empty()) {
            None => {
                let objects = without_folder_marks(objects);
                if objects.len() > api.most_files() {
                    return Err(prefix.too_many(api.most_files()));
                }
                return Ok(Listed {
                    objects,
                    credentials,
                });
            }
            Some(next) => {
                pages.follow(prefix, next, objects.len())?;
                marker = Some(next.to_owned());
            }
        }
    }
}

/// a blob of the listing, with the MD5 azure keeps for it when it keeps one. its `ETag` is
/// quoted as a GET sends it.
fn object(prefix: &Prefix, blobs_at: &Url, blob: &xml::Element) -> Result<Object, Error> {
    let name = blob
        .text_of("Name")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid(prefix, "a blob has no name"))?;
    prefix.check_key(name)?;
    let properties = blob
        .children("Properties")
        .next()
        .ok_or_else(|| invalid(prefix, &format!("the blob \"{name}\" has no properties")))?;
    let size = properties
        .text_of("Content-Length")
        .and_then(|size| size.parse().ok())
        .ok_or_else(|| invalid(prefix, &format!("the size of \"{name}\" is not a number")))?;
    let md5 = match properties
        .text_of("Content-MD5")
        .filter(|md5| !md5.is_empty())
    {
        Some(md5) => Some(
            super::gcs::base64_md5(md5)
                .ok_or_else(|| invalid(prefix, &format!("the MD5 of \"{name}\" is not base64")))?,
        ),
        None => None,
    };
    let etag = properties
        .text_of("Etag")
        .filter(|etag| !etag.is_empty())
        .map(|etag| {
            if etag.starts_with('"') {
                etag.to_owned()
            } else {
                format!("\"{etag}\"")
            }
        });
    Ok(Object {
        key: name.to_owned(),
        size,
        md5,
        etag,
        url: object_url(blobs_at, name),
    })
}

/// `objects` without the empty blobs named as a folder other blobs are in, which tools that
/// treat a container as folders leave behind, so no file takes the place of its own folder.
fn without_folder_marks(objects: Vec<Object>) -> Vec<Object> {
    let folders: HashSet<&str> = objects
        .iter()
        .flat_map(|object| {
            object
                .key
                .match_indices('/')
                .map(|(at, _)| &object.key[..at])
        })
        .collect();
    let marks: HashSet<String> = objects
        .iter()
        .filter(|object| object.size == 0 && folders.contains(object.key.as_str()))
        .map(|object| object.key.clone())
        .collect();
    objects
        .into_iter()
        .filter(|object| !marks.contains(&object.key))
        .collect()
}

/// the error for an answer other than a listing, with the code and message azure gives. the rest
/// of its error document, which for a refused SAS repeats what it signs, never shows.
fn refused(prefix: &Prefix, response: &Response, said: Option<&xml::Element>) -> Error {
    let code = said.and_then(|said| said.text_of("Code"));
    let message = said
        .and_then(|said| said.text_of("Message"))
        .and_then(|message| message.lines().next());
    prefix.refused(response, code, message, None)
}

#[cfg(test)]
mod tests;
