//! azure blob storage: a container's `List Blobs` over HTTPS, a page of up to 5,000 blobs at a
//! time, without a SAS unless the `azure_containers` setting lists the container and it refused
//! a request without one, as [`crate::net`] decides. every request to a blob host asks for
//! [`VERSION`] of azure's API.

use std::collections::HashSet;
use std::fmt;

use url::Url;

use super::{Object, Prefix, object_url, query, xml};
use crate::error::Error;
use crate::net::{Api, Response};

/// the version of azure's API every request to a blob host asks for. an anonymous request that
/// names none gets the account's default, as old as 2009-09-19, under which a container holding
/// newer kinds of blob refuses to list. later versions escape some blob names in a way the
/// listing's reader cannot see.
pub(crate) const VERSION: &str = "2020-10-02";
/// the end of every blob host's name in azure's public cloud.
pub(crate) const BLOB_HOST: &str = ".blob.core.windows.net";

/// whether `name` names a storage account: 3 to 24 lowercase letters and digits.
pub(crate) fn is_account(name: &str) -> bool {
    (3..=24).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

/// whether `name` names a container: 3 to 63 lowercase letters, digits and single hyphens,
/// starting and ending with a letter or digit.
pub(crate) fn is_container(name: &str) -> bool {
    let plain = |byte: &u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let bytes = name.as_bytes();
    (3..=63).contains(&name.len())
        && bytes.first().is_some_and(plain)
        && bytes.last().is_some_and(plain)
        && !name.contains("--")
        && bytes.iter().all(|byte| plain(byte) || *byte == b'-')
}

/// the account a blob host names, such as `lab` for lab.blob.core.windows.net.
pub(crate) fn account_of(host: &str) -> Option<&str> {
    host.strip_suffix(BLOB_HOST)
        .filter(|account| is_account(account))
}

/// a shared access signature: the query that lets a request in, which never shows.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Sas(String);

impl Sas {
    /// reads a SAS as the azure portal and the az cli give one, with or without a leading `?`:
    /// a query with a signature, `sig`, and the version it was signed for, `sv`.
    pub(crate) fn parse(text: &str) -> Option<Sas> {
        let text = text.trim();
        let text = text.strip_prefix('?').unwrap_or(text);
        let plain = !text.is_empty()
            && text
                .bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'#' | b'?'));
        let keys: HashSet<String> = url::form_urlencoded::parse(text.as_bytes())
            .map(|(key, _)| key.into_owned())
            .collect();
        (plain && keys.contains("sig") && keys.contains("sv")).then(|| Sas(text.to_owned()))
    }

    /// the query a request carries it in.
    pub(crate) fn query(&self) -> &str {
        &self.0
    }

    /// when it stops letting requests in, as its `se` says.
    pub(crate) fn expiry(&self) -> Option<String> {
        url::form_urlencoded::parse(self.0.as_bytes())
            .find(|(key, _)| key == "se")
            .map(|(_, value)| value.into_owned())
    }
}

impl fmt::Debug for Sas {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sas(***)")
    }
}

/// a container as the `azure_containers` setting names it, with the SAS its entry gives, if any.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AzureContainer {
    account: String,
    name: String,
    sas: Option<Sas>,
}

impl AzureContainer {
    /// reads `azure:<account>/<container>`, or the container's link,
    /// `https://<account>.blob.core.windows.net/<container>`, which may carry the container's
    /// SAS as the azure portal gives its SAS link. either may end in `/`.
    pub(crate) fn parse(text: &str) -> Option<AzureContainer> {
        let text = text.trim();
        if let Some(rest) = text.strip_prefix("azure:") {
            let rest = rest.strip_suffix('/').unwrap_or(rest);
            let (account, name) = rest.split_once('/')?;
            let account = account.to_ascii_lowercase();
            return (is_account(&account) && is_container(name)).then(|| AzureContainer {
                account,
                name: name.to_owned(),
                sas: None,
            });
        }
        let url = Url::parse(text).ok()?;
        let account = account_of(url.host_str()?)?.to_owned();
        let path = url.path().strip_prefix('/')?;
        let name = path.strip_suffix('/').unwrap_or(path);
        let sas = match url.query() {
            Some(query) => Some(Sas::parse(query)?),
            None => None,
        };
        let plain = url.scheme() == "https"
            && url.port().is_none()
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none();
        (plain && is_container(name)).then(|| AzureContainer {
            account,
            name: name.to_owned(),
            sas,
        })
    }

    pub(crate) fn account(&self) -> &str {
        &self.account
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// the SAS the setting's entry gives the container.
    pub(crate) fn sas(&self) -> Option<&Sas> {
        self.sas.as_ref()
    }
}

impl fmt::Display for AzureContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "azure:{}/{}", self.account, self.name)
    }
}

impl fmt::Debug for AzureContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sas = if self.sas.is_some() {
            " with a SAS"
        } else {
            ""
        };
        write!(f, "AzureContainer({self}{sas})")
    }
}

/// the container's link, ending in `/`, which its blobs' links extend.
fn container_url(prefix: &Prefix) -> Result<Url, Error> {
    Url::parse(&format!(
        "https://{}{BLOB_HOST}/{}/",
        prefix.account(),
        prefix.bucket
    ))
    .map_err(|_| prefix.invalid("the container makes no valid link"))
}

/// every blob under the prefix, a page at a time, without the empty blobs that only mark a
/// folder other blobs are in.
pub(super) async fn list(prefix: &Prefix, api: &impl Api) -> Result<Vec<Object>, Error> {
    let blobs_at = container_url(prefix)?;
    let mut listing = blobs_at.clone();
    listing.set_path(blobs_at.path().trim_end_matches('/'));
    let mut marker: Option<String> = None;
    let mut pages = super::Pages::default();
    let mut objects = Vec::new();
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
        let page = document.map_err(|reason| prefix.invalid(&reason))?;
        if page.name != "EnumerationResults" {
            return Err(prefix.invalid(&format!("the answer is <{}>", page.name)));
        }
        for blob in page
            .children("Blobs")
            .flat_map(|blobs| blobs.children("Blob"))
        {
            objects.push(object(prefix, &blobs_at, blob)?);
            if objects.len() > crate::source::most_files() {
                return Err(prefix.too_many());
            }
        }
        match page.text_of("NextMarker").filter(|next| !next.is_empty()) {
            None => return Ok(without_folder_marks(objects)),
            Some(next) => {
                pages.follow(prefix, next)?;
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
        .ok_or_else(|| prefix.invalid("a blob has no name"))?;
    prefix.check_key(name)?;
    let properties = blob
        .children("Properties")
        .next()
        .ok_or_else(|| prefix.invalid(&format!("the blob \"{name}\" has no properties")))?;
    let size = properties
        .text_of("Content-Length")
        .and_then(|size| size.parse().ok())
        .ok_or_else(|| prefix.invalid(&format!("the size of \"{name}\" is not a number")))?;
    let md5 = match properties
        .text_of("Content-MD5")
        .filter(|md5| !md5.is_empty())
    {
        Some(md5) => Some(
            super::gcs::base64_md5(md5)
                .ok_or_else(|| prefix.invalid(&format!("the MD5 of \"{name}\" is not base64")))?,
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

/// the error for an answer other than a listing, with the code and message azure's error
/// document gives. the rest of that document, which for a refused SAS repeats what it signs,
/// never shows.
fn refused(prefix: &Prefix, response: &Response, said: Option<&xml::Element>) -> Error {
    let code = said.and_then(|said| said.text_of("Code"));
    let message = said
        .and_then(|said| said.text_of("Message"))
        .and_then(|message| message.lines().next());
    prefix.refused(response, code, message, None)
}

#[cfg(test)]
mod tests;
