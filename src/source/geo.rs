//! GEO, NCBI's gene expression omnibus: the files of a series, a sample or a platform, as NCBI's
//! file server indexes list them. NCBI states no hashes and its indexes round sizes, so the lock's
//! SHA-256 names the bytes, and only a series' `suppl/filelist.txt` states sizes, its tars'; GEO
//! remakes a series' SOFT, `MINiML` and matrix files, so a pin can go stale. NCBI refuses a
//! program the listing of its folders at times while it serves their files, so a refused folder
//! is listed by the names GEO gives every accession's files.

use std::collections::VecDeque;
use std::fmt;
use std::time::Duration;

use percent_encoding::percent_decode_str;
use url::Url;

use super::{Access, Listing, Reference, Remote, invalid, malformed};
use crate::error::{Error, Kind};
use crate::net::{Api, Extra, Response};

const FILES: &str = "https://ftp.ncbi.nlm.nih.gov/geo";

/// the most folders one listing reads: the accession's own and those inside it, as a series
/// keeps `matrix/`, `miniml/`, `soft/` and `suppl/`.
const MOST_FOLDERS: usize = 64;

/// NCBI asks a program without a key to send no more than 3 requests a second, so requests to
/// its hosts start this far apart.
pub(super) const SPACING: Duration = Duration::from_millis(334);

/// the list GEO keeps beside a series' tar of every sample's files, naming it with its size.
const FILELIST: &str = "suppl/filelist.txt";

/// the header of a filelist, whose rows name a tar as `Archive` and each file in it as `File`.
const FILELIST_HEADER: &str = "#Archive/File\tName\tTime\tSize\tType";

/// a request for a file's first byte alone, whose answer says whether the file is there without
/// sending it.
const FIRST_BYTE: Extra = &[("range", "bytes=0-0")];

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

    /// a pasted link to an accession's page on GEO's site or its folder on NCBI's file server, and
    /// the folder inside it the link named, as `suppl`. a link to one file stays a plain link.
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
    /// it, as `suppl/GSE1000_RAW.tar`. a folder whose listing NCBI refuses gives the files GEO
    /// names alike for every accession of its kind, each listed once NCBI serves its first byte.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let top = self.folder()?;
        let (mut files, refused) = self.walk(api, &top).await?;
        let blocked = |folder: &str| refused.iter().any(|path| path.is_empty() || path == folder);
        for (folder, name) in self.named_by_rule() {
            if blocked(folder) {
                let link = self.link(&top, folder, &name)?;
                if self.serves(api, &link).await? {
                    files.push(remote(format!("{folder}/{name}"), link));
                }
            }
        }
        self.filelisted(
            api,
            &top,
            &mut files,
            self.kind() == "series" && blocked("suppl"),
        )
        .await?;
        if files.len() > api.most_files() {
            return Err(self.too_many(api, &top));
        }
        if files.is_empty() {
            return Err(if refused.is_empty() {
                self.missing()
            } else {
                self.unlisted()
            });
        }
        let warnings = if refused.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "NCBI refused to list the folders of {self} for now, so it holds the files GEO names alike for every {}: one named otherwise, such as a matrix for each of several platforms, is left out until NCBI lists them again",
                self.noun()
            )]
        };
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Listing {
            within: Vec::new(),
            access: Access::Public,
            unavailable: Vec::new(),
            warnings,
            resolved: Reference::Geo(self.clone()),
            title: None,
            license: None,
            doi: None,
            files,
        })
    }

    /// the files the indexes of the accession's folder `top` and the folders inside it list, and
    /// the folders whose listing NCBI refused, the accession's own as "".
    async fn walk(&self, api: &impl Api, top: &Url) -> Result<(Vec<Remote>, Vec<String>), Error> {
        let mut folders = VecDeque::from([(String::new(), top.clone())]);
        let mut files = Vec::new();
        let mut refused = Vec::new();
        let mut read = 0;
        while let Some((path, url)) = folders.pop_front() {
            read += 1;
            if read > MOST_FOLDERS {
                return Err(invalid(
                    self,
                    &format!("it holds more than {MOST_FOLDERS} folders"),
                ));
            }
            let response = api.get(&url).await?;
            if matches!(response.status, 401 | 403) {
                refused.push(path);
                continue;
            }
            let html = self.index(&response, path.is_empty())?;
            let parent = url.join("..").map(|parent| parent.path().to_owned());
            let found = parent.ok().and_then(|parent| entries(&html, &parent));
            let Some(found) = found else {
                return Err(invalid(
                    self,
                    "it sent a page that is no index of its folder",
                ));
            };
            for entry in found {
                let (name, folder) = match entry.strip_suffix('/') {
                    Some(name) => (name, true),
                    None => (entry.as_str(), false),
                };
                let decoded = percent_decode_str(name)
                    .decode_utf8()
                    .ok()
                    .filter(|decoded| is_name(decoded))
                    .ok_or_else(|| {
                        invalid(
                            self,
                            &format!(
                                "its index names an entry that is no file name: {}",
                                name.escape_debug()
                            ),
                        )
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
                        invalid(
                            self,
                            &format!("its index links away: {}", entry.escape_debug()),
                        )
                    })?;
                if folder {
                    folders.push_back((at, link));
                    continue;
                }
                files.push(remote(at, link));
                if files.len() > api.most_files() {
                    return Err(self.too_many(api, top));
                }
            }
        }
        Ok((files, refused))
    }

    /// reads the series' `suppl/filelist.txt` where `files` lists it, or where NCBI refused to
    /// list its folder, as `unlisted` says, and lists the filelist and each tar it names then:
    /// the filelist and those tars take the sizes it states.
    async fn filelisted(
        &self,
        api: &impl Api,
        top: &Url,
        files: &mut Vec<Remote>,
        unlisted: bool,
    ) -> Result<(), Error> {
        if !unlisted && files.iter().all(|file| file.path != FILELIST) {
            return Ok(());
        }
        let Some(archives) = self.filelist(api, top).await? else {
            return Ok(());
        };
        if unlisted {
            files.push(remote(
                FILELIST.to_owned(),
                self.link(top, "suppl", "filelist.txt")?,
            ));
            for (name, _) in &archives {
                files.push(remote(
                    format!("suppl/{name}"),
                    self.link(top, "suppl", name)?,
                ));
            }
        }
        for file in files {
            let stated = archives
                .iter()
                .find(|(name, _)| file.path.strip_prefix("suppl/") == Some(name.as_str()))
                .map(|(_, size)| *size);
            if stated.is_some() {
                file.size = stated;
            }
        }
        Ok(())
    }

    /// the files GEO names alike for every accession of this kind, by their folder and name.
    fn named_by_rule(&self) -> Vec<(&'static str, String)> {
        let id = &self.id;
        let family = [
            ("miniml", format!("{id}_family.xml.tgz")),
            ("soft", format!("{id}_family.soft.gz")),
        ];
        match self.kind() {
            "series" => [("matrix", format!("{id}_series_matrix.txt.gz"))]
                .into_iter()
                .chain(family)
                .collect(),
            "platforms" => [("annot", format!("{id}.annot.gz"))]
                .into_iter()
                .chain(family)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// the link of the file `name` in the accession's folder `folder`.
    fn link(&self, top: &Url, folder: &str, name: &str) -> Result<Url, Error> {
        let mut link = top.clone();
        link.path_segments_mut()
            .map_err(|()| invalid(self, "the folder's URL has no path"))?
            .pop_if_empty()
            .extend([folder, name]);
        Ok(link)
    }

    /// whether NCBI's file server serves the file at `link`, asked for its first byte alone.
    async fn serves(&self, api: &impl Api, link: &Url) -> Result<bool, Error> {
        let response = api.get_with(link, FIRST_BYTE).await?;
        match response.status {
            200..=299 => Ok(true),
            401 | 403 | 404 | 410 => Ok(false),
            status => Err(self.status(&response, status)),
        }
    }

    /// each tar the series' `suppl/filelist.txt` names, with its size; none when GEO keeps no
    /// filelist or refuses it. an answer in another form, as a sign-in page, is no filelist.
    async fn filelist(
        &self,
        api: &impl Api,
        top: &Url,
    ) -> Result<Option<Vec<(String, u64)>>, Error> {
        let response = api.get(&self.link(top, "suppl", "filelist.txt")?).await?;
        match response.status {
            200..=299 => archives(&response.text()).map(Some).ok_or_else(|| {
                invalid(self, "its suppl/filelist.txt is in another form than GEO's")
            }),
            401 | 403 | 404 | 410 => Ok(None),
            status => Err(self.status(&response, status)),
        }
    }

    /// the text of an index of GEO's file server. the accession's own folder is missing for an
    /// accession GEO keeps no files for.
    fn index(&self, response: &Response, own: bool) -> Result<String, Error> {
        match response.status {
            200..=299 => Ok(response.text().into_owned()),
            404 | 410 if own => Err(self.missing()),
            404 | 410 => Err(invalid(self, "a folder its index lists is not there")),
            status => Err(self.status(response, status)),
        }
    }

    fn status(&self, response: &Response, status: u16) -> Error {
        Error::new(
            Kind::NetworkStatus,
            format!(
                "{} answered {status} for {self}",
                crate::net::host(&response.url)
            ),
        )
    }

    fn too_many(&self, api: &impl Api, top: &Url) -> Error {
        super::too_many(
            self,
            api.most_files(),
            &format!("download the files you need from {top} one at a time"),
        )
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
        .map_err(|_| invalid(self, "the folder's URL is invalid"))
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

    /// NCBI's refusal to list the accession's folders, with no file GEO names by rule served
    /// instead: never a private dataset's, since GEO's file server holds public records alone.
    fn unlisted(&self) -> Error {
        Error::new(
            Kind::NetworkStatus,
            format!("ftp.ncbi.nlm.nih.gov refused to list the files of {self}"),
        )
        .help("NCBI's file server refuses programs a listing of its folders at times, though GEO's files are public: run the same command again later")
    }

    /// the kind of accession, as a person names it.
    fn noun(&self) -> &'static str {
        match self.kind() {
            "series" => "series",
            "samples" => "sample",
            _ => "platform",
        }
    }
}

/// a file of the accession at `path` below its folder.
fn remote(path: String, url: Url) -> Remote {
    Remote {
        path,
        size: None,
        sha256: None,
        md5: None,
        url,
        named_by_server: false,
        etag: None,
    }
}

/// the links of an index of GEO's file server that name its entries, files and folders ending in
/// `/`: those inside its `<pre>` block, past the link to the folder above, `parent`. a link with
/// a raw `?` or `#` names none; any other is an entry for the listing's checks to judge, so a
/// name no file may have is refused rather than left out. none for a page that is no such index.
fn entries(html: &str, parent: &str) -> Option<Vec<String>> {
    let (_, block) = html.split_once("<pre>")?;
    let (block, _) = block.split_once("</pre>")?;
    let mut links = Vec::new();
    let mut rest = block;
    while let Some(at) = rest.find("<a href=\"") {
        rest = &rest[at + "<a href=\"".len()..];
        let end = rest.find('"')?;
        links.push(&rest[..end]);
        rest = &rest[end..];
    }
    if !links.contains(&parent) {
        return None;
    }
    Some(
        links
            .into_iter()
            .filter(|link| *link != parent && !link.contains(['?', '#']))
            .map(str::to_owned)
            .collect(),
    )
}

/// a name one entry of a folder may have: not empty, not a step up, with no separator.
fn is_name(name: &str) -> bool {
    !matches!(name, "" | "." | "..")
        && !name.contains(['/', '\\'])
        && !name.chars().any(char::is_control)
}

/// the tars a filelist names, each once with its size in bytes: its `Archive` rows below the
/// header GEO writes. a row that names no file or no whole size names none, nor does one a line
/// break does not end, as an answer cut short leaves its last; a text in another form is no
/// filelist.
fn archives(text: &str) -> Option<Vec<(String, u64)>> {
    let mut lines = text
        .split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
        .map(|line| line.strip_suffix('\r').unwrap_or(line));
    if lines.next() != Some(FILELIST_HEADER) {
        return None;
    }
    let mut found: Vec<(String, u64)> = Vec::new();
    for line in lines {
        let fields: Vec<&str> = line.split('\t').collect();
        let ["Archive", name, _, size, _] = fields.as_slice() else {
            continue;
        };
        let Ok(size) = size.parse() else {
            continue;
        };
        if is_name(name) && !found.iter().any(|(known, _)| known == name) {
            found.push(((*name).to_owned(), size));
        }
    }
    Some(found)
}

impl fmt::Display for Accession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "geo:{}", self.id)
    }
}

#[cfg(test)]
mod tests;
