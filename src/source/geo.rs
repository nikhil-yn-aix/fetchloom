//! GEO, NCBI's gene expression omnibus: the files GEO keeps for a series, a sample or a platform,
//! as the indexes of NCBI's file server list them over https, where `GEOquery` finds them too. NCBI
//! states no hashes for these files, and its indexes round their sizes, so the lock's SHA-256 is
//! all that names a file's bytes, and the size comes from the download. GEO remakes a series'
//! SOFT, `MINiML` and matrix files from time to time, so a pin can stop matching them.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Access, Listing, Reference, Remote, malformed};
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

const FILES: &str = "https://ftp.ncbi.nlm.nih.gov/geo";

/// the most folders one listing reads: the accession's own and those inside it, as a series
/// keeps `matrix/`, `miniml/`, `soft/` and `suppl/`.
const MOST_FOLDERS: usize = 64;

/// NCBI asks a program without a key to send no more than 3 requests a second, so requests to
/// its hosts start this far apart.
const SPACING: Duration = Duration::from_millis(334);

/// how far apart requests to `host` start, for a host that asks them to: NCBI's.
pub(super) fn spacing(host: &str) -> Option<Duration> {
    (host == "ncbi.nlm.nih.gov" || host.ends_with(".ncbi.nlm.nih.gov")).then_some(SPACING)
}

/// a GEO series, sample or platform, by its accession.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Accession {
    /// the accession, uppercase, as `GSE1000`.
    id: String,
}

impl Accession {
    /// reads the part after `geo:`: a series (GSE), sample (GSM) or platform (GPL) accession.
    pub(super) fn parse(text: &str) -> Result<Accession, Error> {
        Accession::named(text.trim()).ok_or_else(|| {
            malformed(
                &format!("geo:{text}"),
                "geo:GSE1000, geo:GSM2666939 or geo:GPL96",
            )
        })
    }

    fn named(text: &str) -> Option<Accession> {
        let id = text.to_ascii_uppercase();
        let digits = id.get(3..)?;
        let valid = ["GSE", "GSM", "GPL"].contains(&&id[..3])
            && (1..=12).contains(&digits.len())
            && digits.bytes().all(|b| b.is_ascii_digit())
            && !digits.starts_with('0');
        valid.then_some(Accession { id })
    }

    /// a pasted link to an accession's page on GEO's site, or to its folder on NCBI's file
    /// server, and the folder inside it the link named, as `suppl`. a link to one file stays a
    /// plain link.
    pub(super) fn from_link(url: &Url) -> Option<(Accession, Option<String>)> {
        match url.host_str()? {
            "www.ncbi.nlm.nih.gov" | "ncbi.nlm.nih.gov" if url.path() == "/geo/query/acc.cgi" => {
                let named = url
                    .query_pairs()
                    .find(|(key, _)| key.eq_ignore_ascii_case("acc"))?;
                Some((Accession::named(&named.1)?, None))
            }
            "ftp.ncbi.nlm.nih.gov" => {
                let parts: Vec<&str> = url.path_segments()?.collect();
                let (accession, rest) = match parts.as_slice() {
                    ["geo", kind, stub, id, rest @ ..] => {
                        let accession = Accession::named(id)?;
                        let fits = *kind == accession.kind() && *stub == accession.stub();
                        (fits.then_some(accession)?, rest)
                    }
                    _ => return None,
                };
                let folder = match rest {
                    [] | [""] => None,
                    [folder] | [folder, ""] if is_name(folder) => Some((*folder).to_owned()),
                    _ => return None,
                };
                // a link to a file inside the accession's own folder ends without `/`
                if rest.len() == 1 && !rest[0].is_empty() && rest[0].contains('.') {
                    return None;
                }
                Some((accession, folder))
            }
            _ => None,
        }
    }

    /// lists every file in the accession's folder and the folders inside it, by their paths below
    /// it, as `suppl/GSE1000_RAW.tar`.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let top = self.folder()?;
        let mut folders = VecDeque::from([(String::new(), top.clone())]);
        let mut files = Vec::new();
        let mut read = 0;
        while let Some((path, url)) = folders.pop_front() {
            read += 1;
            if read > MOST_FOLDERS {
                return Err(self.invalid(&format!("it holds more than {MOST_FOLDERS} folders")));
            }
            let response = api.get(&url).await?;
            let html = self.index(&response, path.is_empty())?;
            for entry in entries(&html) {
                let (name, folder) = match entry.strip_suffix('/') {
                    Some(name) => (name, true),
                    None => (entry.as_str(), false),
                };
                let decoded = percent_decode_str(name)
                    .decode_utf8()
                    .ok()
                    .filter(|decoded| is_name(decoded))
                    .ok_or_else(|| {
                        self.invalid(&format!(
                            "its index names an entry that is no file name: {}",
                            name.escape_debug()
                        ))
                    })?;
                let at = if path.is_empty() {
                    decoded.into_owned()
                } else {
                    format!("{path}/{decoded}")
                };
                let link = url
                    .join(&entry)
                    .ok()
                    .filter(|link| link.as_str().starts_with(top.as_str()))
                    .ok_or_else(|| {
                        self.invalid(&format!("its index links away: {}", entry.escape_debug()))
                    })?;
                if folder {
                    folders.push_back((at, link));
                    continue;
                }
                files.push(Remote {
                    path: at,
                    size: None,
                    sha256: None,
                    md5: None,
                    url: link,
                    named_by_server: false,
                    etag: None,
                });
                if files.len() > super::most_files() {
                    return Err(super::too_many(
                        self,
                        &format!("download the files you need from {top} one at a time"),
                    ));
                }
            }
        }
        if files.is_empty() {
            return Err(self.missing());
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Listing {
            within: Vec::new(),
            access: Access::Public,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            resolved: Reference::Geo(self.clone()),
            title: None,
            license: None,
            doi: None,
            files,
        })
    }

    /// the text of an index of GEO's file server. the accession's own folder is missing for an
    /// accession GEO keeps no files for.
    fn index(&self, response: &Response, own: bool) -> Result<String, Error> {
        match response.status {
            200..=299 => Ok(response.text().into_owned()),
            404 | 410 if own => Err(self.missing()),
            404 | 410 => Err(self.invalid("a folder its index lists is not there")),
            401 | 403 => Err(super::refused(
                crate::net::host(&response.url),
                &self.to_string(),
            )),
            status => Err(Error::new(
                Kind::NetworkStatus,
                format!(
                    "{} answered {status} for {self}",
                    crate::net::host(&response.url)
                ),
            )),
        }
    }

    /// the accession's folder on NCBI's file server, as
    /// `https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/`.
    fn folder(&self) -> Result<Url, Error> {
        Url::parse(&format!(
            "{FILES}/{}/{}/{}/",
            self.kind(),
            self.stub(),
            self.id
        ))
        .map_err(|_| self.invalid("the folder's URL is invalid"))
    }

    /// the folder of GEO's file server for the accession's kind.
    fn kind(&self) -> &'static str {
        match &self.id[..3] {
            "GSE" => "series",
            "GSM" => "samples",
            _ => "platforms",
        }
    }

    /// the folder of a thousand accessions the accession's own sits in: its last three digits
    /// as `nnn`, so `GSE1000` is in `GSE1nnn` and `GPL96` in `GPLnnn`.
    fn stub(&self) -> String {
        let keep = self.id.len().saturating_sub(3).max(3);
        format!("{}nnn", &self.id[..keep])
    }

    fn missing(&self) -> Error {
        Error::new(
            Kind::ReferenceUnresolved,
            format!("GEO keeps no files for {}", self.id),
        )
        .help(format!(
            "check https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc={}: the accession may not exist or not be public yet, and a sample's own table is on that page rather than in a file GEO keeps",
            self.id
        ))
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("check the accession on GEO's site and report the invalid listing at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }
}

/// the links of an index of GEO's file server that name its entries: files, and folders ending
/// in `/`. the link to the folder above, and links to other sites, name none.
fn entries(html: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = html;
    while let Some(at) = rest.find("<a href=\"") {
        rest = &rest[at + "<a href=\"".len()..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let link = &rest[..end];
        rest = &rest[end..];
        if !link.is_empty() && !link.starts_with(['/', '?', '#', '.']) && !link.contains(':') {
            found.push(link.to_owned());
        }
    }
    found
}

/// a name one entry of a folder may have: not empty, not a step up, with no separator.
fn is_name(name: &str) -> bool {
    !matches!(name, "" | "." | "..")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

impl fmt::Display for Accession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "geo:{}", self.id)
    }
}

#[cfg(test)]
mod tests;
