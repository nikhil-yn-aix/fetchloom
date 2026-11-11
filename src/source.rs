//! sources: where datasets live, how a person names one, and how each lists its files. a source
//! says what to download and how to check it, and leaves file bodies to the downloader; the one
//! exception is kaggle, which reads a competition's smallest file to learn whether its rules
//! were accepted.

mod bucket;
mod dataverse;
mod doi;
mod drive;
mod dropbox;
mod figshare;
mod hf;
mod kaggle;
pub(crate) mod local;
mod osf;
#[cfg(test)]
pub(crate) mod recorded;
pub(crate) mod search;
mod zenodo;

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use tracing::Instrument as _;
use url::Url;

pub(crate) use bucket::{Bucket, Keyed, Signing, unversioned, with_version};

use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::{self, Api, Credential, Fetch, Response};

/// the most files one dataset lists when a run says nothing else, and the least it ever allows.
/// a run sizes its own from its memory with [`set_most_files`].
pub(crate) const MOST_FILES: usize = 100_000;

/// about the memory one listed file takes while a run works, from its listing to its placing,
/// which sizes [`most_files`] from the memory a run may use.
pub(crate) const PER_FILE: u64 = 2 << 10;

/// the most files one dataset may list in this run.
static MOST: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(MOST_FILES);

/// sets the most files one dataset may list, for every later listing in the run.
pub(crate) fn set_most_files(most: usize) {
    MOST.store(most.max(1), std::sync::atomic::Ordering::Relaxed);
}

/// the most files one dataset may list in this run, so memory stays bounded.
pub(crate) fn most_files() -> usize {
    MOST.load(std::sync::atomic::Ordering::Relaxed)
}

/// the error for `subject`, a dataset that lists more than [`most_files`] files, with `instead`
/// saying how to need fewer.
pub(crate) fn too_many(subject: impl fmt::Display, instead: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "{subject} lists more than {} files, the most this run lists for one dataset",
            most_files()
        ),
    )
    .help(format!(
        "{instead}, or raise FETCHLOOM_MOST_FILES, or most_files in the settings file; each listed file takes about 2 KiB of memory"
    ))
}

/// reads the environment, so tests can give each run its own.
pub(crate) type Var<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// the environment a source reads to tell whether its requests go signed in, as the network
/// that sends them found its credentials there. unit tests read an empty one instead, so a key
/// saved on the machine running them changes nothing, and pass their own where it matters.
fn env(name: &str) -> Option<OsString> {
    if cfg!(test) {
        None
    } else {
        crate::auth::env(name)
    }
}

/// where a dataset comes from, parsed once where it enters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reference {
    Url(Url),
    Doi(doi::Doi),
    Zenodo(zenodo::Record),
    Figshare(figshare::Article),
    Dataverse(dataverse::Dataset),
    Osf(osf::Node),
    HuggingFace(hf::Repo),
    Kaggle(kaggle::Dataset),
    KaggleCompetition(kaggle::Competition),
    Drive(drive::Item),
    Dropbox(dropbox::Link),
    Bucket(bucket::Prefix),
    /// files imported from a machine with `fetchloom add <path>`, which no source lists: data.lock
    /// pins their bytes, never where they were.
    Local,
}

/// a reference, and the one file or folder inside it when a pasted link pointed at one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) reference: Reference,
    pub(crate) file: Option<String>,
    pub(crate) folder: Option<String>,
}

impl Parsed {
    /// the select patterns the pasted link implies: its one file, or everything in its folder.
    pub(crate) fn select(&self) -> Vec<String> {
        let file = self.file.iter().map(|file| crate::glob::escape(file));
        let folder = self
            .folder
            .iter()
            .map(|folder| format!("{}/**", crate::glob::escape(folder)));
        file.chain(folder).collect()
    }
}

/// what a source says a dataset holds, at one exact version.
#[derive(Clone, Debug)]
pub(crate) struct Listing {
    /// the reference to resolve again, pinned to a version when the source provides one.
    pub(crate) resolved: Reference,
    pub(crate) title: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) doi: Option<String>,
    pub(crate) access: Access,
    pub(crate) files: Vec<Remote>,
    pub(crate) unavailable: Vec<Unavailable>,
    /// what a person should know about the listing, told as warnings once it resolves, such as
    /// files it leaves out.
    pub(crate) warnings: Vec<String>,
    /// the folders listed when only those were, as they held every file wanted; empty when the
    /// listing covers the whole dataset.
    pub(crate) within: Vec<String>,
}

impl Listing {
    /// the title a generated name may come from. a hugging face repository is known by its id,
    /// such as `multi_nli`, more than by its card's title, so its name comes from the id.
    pub(crate) fn naming_title(&self) -> Option<&str> {
        match self.resolved {
            Reference::HuggingFace(_) => None,
            _ => self.title.as_deref(),
        }
    }

    /// the paths select and exclude can match, including entries without downloadable bytes.
    pub(crate) fn paths(&self) -> impl Iterator<Item = &str> {
        self.files
            .iter()
            .map(|file| file.path.as_str())
            .chain(self.unavailable.iter().map(|entry| entry.path.as_str()))
    }
}

/// what a source said about who may read a dataset: a hint for sharing it, never a check of a
/// licence. a source that says nothing leaves it unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Access {
    /// anyone may read it without an account.
    Public,
    /// reading it takes an account, as a private repository does.
    Login,
    /// reading it takes accepted terms, as a gated repository or a competition's rules do.
    Terms,
    #[default]
    Unknown,
}

impl Access {
    /// the word data.lock and `--json` use for it.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Access::Public => "public",
            Access::Login => "login",
            Access::Terms => "terms",
            Access::Unknown => "unknown",
        }
    }
}

/// a listed entry that cannot be fetched, retained so selection can explicitly leave it out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Unavailable {
    pub(crate) path: String,
    pub(crate) reason: String,
}

/// one file a source lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Remote {
    /// where the file goes inside the dataset, with `/` between folders.
    pub(crate) path: String,
    pub(crate) size: Option<u64>,
    pub(crate) sha256: Option<Sha256>,
    pub(crate) md5: Option<Md5>,
    /// the link project commands write to data.lock. redirects and source credentials are not
    /// part of that record.
    pub(crate) url: Url,
    /// the path is only a guess from the link, and the name the server sends replaces it.
    pub(crate) named_by_server: bool,
}

impl Reference {
    /// reads a reference as a person writes it: an id such as `zenodo:3608990`, a DOI, or a link
    /// pasted from a browser.
    pub(crate) fn parse(text: &str) -> Result<Parsed, Error> {
        let text = text.trim();
        // a link parser drops tabs and line breaks, so what is written and what is fetched could
        // differ, and data.lock's record of the entry could match another one
        if text.contains(char::is_control) {
            let text = net::mask_query(text);
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!(
                    "\"{}\" holds a line break or another control character",
                    text.escape_debug()
                ),
            )
            .help("write the reference on one line, without tabs"));
        }
        let whole = |reference| {
            Ok(Parsed {
                reference,
                file: None,
                folder: None,
            })
        };
        if local::is_path(text) {
            return Err(local::refused(text));
        }
        if text == local::IMPORTED {
            return whole(Reference::Local);
        }
        if let Some(doi) = doi::Doi::bare(text) {
            return whole(Reference::Doi(doi));
        }
        let Some((scheme, rest)) = text.split_once(':') else {
            return Err(unknown(text));
        };
        match scheme.to_ascii_lowercase().as_str() {
            "http" | "https" => link(text),
            "doi" => whole(Reference::Doi(doi::Doi::parse(rest)?)),
            "zenodo" => whole(Reference::Zenodo(zenodo::Record::parse(rest)?)),
            "figshare" => whole(Reference::Figshare(figshare::Article::parse(rest)?)),
            "dataverse" => whole(Reference::Dataverse(dataverse::Dataset::parse(rest)?)),
            "osf" => whole(Reference::Osf(osf::Node::parse(rest)?)),
            "hf" => whole(Reference::HuggingFace(hf::Repo::parse(rest)?)),
            "kaggle" => whole(kaggle::parse(rest)?),
            "drive" => whole(Reference::Drive(drive::Item::parse(rest)?)),
            "s3" => whole(Reference::Bucket(bucket::Prefix::parse(
                bucket::Store::S3,
                rest,
            )?)),
            "gs" => whole(Reference::Bucket(bucket::Prefix::parse(
                bucket::Store::Gcs,
                rest,
            )?)),
            _ => Err(unknown(text)),
        }
    }

    /// the origin a person names by writing this reference, when it is not one of the fixed
    /// public services. it may be on a private network, and only it: another port or scheme of
    /// its host is not named.
    pub(crate) fn written_origin(&self) -> Option<url::Origin> {
        match self {
            Reference::Url(url) => Some(url.origin()),
            Reference::Dataverse(dataset) => dataset.origin(),
            Reference::Bucket(prefix) => prefix.written_origin(),
            _ => None,
        }
    }

    /// whether the links of this dataset's files may pin the object version their bytes were,
    /// as S3's may.
    pub(crate) fn keeps_versions(&self) -> bool {
        matches!(self, Reference::Bucket(prefix) if prefix.keeps_versions())
    }

    /// the reference as messages and logs show it, a secret in a link masked.
    pub(crate) fn shown(&self) -> String {
        match self {
            Reference::Url(url) => crate::net::Shown(url).to_string(),
            other => net::mask_query(&other.to_string()).into_owned(),
        }
    }

    /// refuses a link that carries a value granting access, such as a token or the signature of
    /// a presigned storage link: data.toml and data.lock would keep it, git would share it, and a
    /// signature stops working within days anyway.
    pub(crate) fn check_shareable(&self) -> Result<(), Error> {
        let url = match self {
            Reference::Url(url) => url,
            Reference::Dropbox(link) => link.url(),
            _ => return Ok(()),
        };
        match crate::net::secret(url) {
            None => Ok(()),
            Some(key) => Err(Error::new(
                Kind::ReferenceInvalid,
                format!("this link carries {key}, a secret that data.toml and data.lock would keep and share"),
            )
            .help("use a link that works without it, or download the file once with `fetchloom get`")),
        }
    }

    /// asks the source what the dataset holds: at the exact version this reference names, or at
    /// the newest one when it names none.
    pub(crate) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        self.resolve_in(api, None).await
    }

    /// [`Reference::resolve`], listing only `folders` where the source can and they hold every
    /// file wanted, as [`crate::glob::folders`] finds them. the listing names the folders it
    /// covers.
    pub(crate) async fn resolve_in(
        &self,
        api: &impl Api,
        folders: Option<&[String]>,
    ) -> Result<Listing, Error> {
        let span = tracing::debug_span!("resolve", reference = %self.shown());
        let listing = async {
            match self {
                Reference::Doi(doi) => {
                    let target = doi.locate(api).await?;
                    let mut listing = target.resolve_direct(api, folders).await?;
                    listing.doi.get_or_insert_with(|| doi.name().to_owned());
                    Ok(listing)
                }
                other => other.resolve_direct(api, folders).await,
            }
        }
        .instrument(span)
        .await
        .map_err(|error: Error| {
            if error.kind == Kind::OfflineMissing {
                Error::new(
                    Kind::OfflineMissing,
                    format!("looking up {} needs the internet", self.shown()),
                )
                .help("in a project prepared online, fetchloom sync --offline works without it")
            } else {
                error
            }
        })?;
        check(self, listing)
    }

    async fn resolve_direct(
        &self,
        api: &impl Api,
        folders: Option<&[String]>,
    ) -> Result<Listing, Error> {
        match self {
            Reference::Url(url) => Ok(single(url)),
            Reference::Doi(doi) => Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{doi} leads to another DOI, which fetchloom does not follow"),
            )
            .help("open the DOI in a browser and use the dataset page or a direct file link")),
            Reference::Zenodo(record) => record.resolve(api).await,
            Reference::Figshare(article) => article.resolve(api).await,
            Reference::Dataverse(dataset) => dataset.resolve(api).await,
            Reference::Osf(node) => node.resolve(api).await,
            Reference::HuggingFace(repo) => repo.resolve(api, folders).await,
            Reference::Kaggle(dataset) => dataset.resolve(api).await,
            Reference::KaggleCompetition(competition) => competition.resolve(api).await,
            Reference::Drive(item) => item.resolve(api).await,
            Reference::Dropbox(link) => Ok(link.listing()),
            Reference::Bucket(prefix) => prefix.resolve(api).await,
            Reference::Local => Err(local::unlisted()),
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reference::Url(url) => write!(f, "{url}"),
            Reference::Doi(doi) => write!(f, "{doi}"),
            Reference::Zenodo(record) => write!(f, "{record}"),
            Reference::Figshare(article) => write!(f, "{article}"),
            Reference::Dataverse(dataset) => write!(f, "{dataset}"),
            Reference::Osf(node) => write!(f, "{node}"),
            Reference::HuggingFace(repo) => write!(f, "{repo}"),
            Reference::Kaggle(dataset) => write!(f, "{dataset}"),
            Reference::KaggleCompetition(competition) => write!(f, "{competition}"),
            Reference::Drive(item) => write!(f, "{item}"),
            Reference::Dropbox(link) => write!(f, "{link}"),
            Reference::Bucket(prefix) => write!(f, "{prefix}"),
            Reference::Local => f.write_str(local::IMPORTED),
        }
    }
}

/// a download request with the source's response handling.
pub(crate) fn fetch_plan(url: &Url) -> Fetch {
    let (from, divert) = drive::fetch_plan(url)
        .or_else(|| dropbox::fetch_plan(url))
        .unwrap_or_else(|| (url.clone(), None));
    Fetch {
        from,
        divert,
        expired: osf::expiry_check(url),
        ..Fetch::new(url.clone())
    }
}

/// whether the source behind `url` may send the file wrapped in a zip of its own, `train.csv` as
/// `train.csv.zip`, as kaggle does with large competition files. such a file unpacks once its
/// bytes show the wrapper, and its listed size is no expectation for them.
pub(crate) fn may_wrap(url: &Url) -> bool {
    kaggle::may_wrap(url)
}

/// a source's own error for a download it refused, when it says more than the status.
pub(crate) fn refused_download(url: &Url) -> Option<Error> {
    kaggle::refused_download(url)
}

impl Remote {
    /// the size the downloaded bytes must have: the listed size, unless the source may send the
    /// file wrapped in a zip.
    pub(crate) fn expected_size(&self) -> Option<u64> {
        self.size.filter(|_| !may_wrap(&self.url))
    }
}

/// kaggle's key, as an api token or a legacy key, found where the kaggle tools keep it.
pub(crate) fn kaggle_credentials(var: Var<'_>) -> Vec<Credential> {
    kaggle::credentials(var)
}

/// the kaggle key downloads send, and where it was found.
pub(crate) fn kaggle_key(var: Var<'_>) -> Option<crate::auth::Found> {
    kaggle::locate(var)
}

/// where the kaggle CLI keeps `kaggle.json`.
pub(crate) fn kaggle_folder(var: Var<'_>) -> Option<PathBuf> {
    kaggle::config_folder(var)
}

/// the `authorization` value for a kaggle key a person pasted, and the host it goes to.
pub(crate) fn kaggle_pasted(secret: &str) -> Option<(&'static str, String)> {
    kaggle::pasted(secret)
}

/// standard base64 with padding, which basic authentication sends.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let [a, b, c] = [0, 1, 2].map(|at| chunk.get(at).copied().unwrap_or(0));
        let sextets = [
            a >> 2,
            ((a & 0b11) << 4) | (b >> 4),
            ((b & 0b1111) << 2) | (c >> 6),
            c & 0b11_1111,
        ];
        for (at, sextet) in sextets.into_iter().enumerate() {
            text.push(if at <= chunk.len() {
                char::from(ALPHABET[usize::from(sextet)])
            } else {
                '='
            });
        }
    }
    text
}

pub(crate) fn home(var: Var<'_>) -> Option<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        set("USERPROFILE").or_else(|| set("HOME"))
    } else {
        set("HOME")
    }
}

/// a link pasted from a browser, matched to the source that serves it by its host.
fn link(text: &str) -> Result<Parsed, Error> {
    let url = Url::parse(text).map_err(|err| {
        Error::new(
            Kind::ReferenceInvalid,
            format!("\"{}\" is not a valid link: {err}", net::mask_query(text)),
        )
        .help("copy the whole link again from the browser's address bar, and keep it in double quotes on the command line")
    })?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Error::new(
            Kind::ReferenceInvalid,
            "this link carries a user name or password, which would end up in files and logs",
        )
        .help("remove the user name and password from the link. for a private dataset, sign in with `fetchloom auth login`"));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(Error::new(
            Kind::ReferenceInvalid,
            format!("\"{}\" names no host", net::mask_query(text)),
        )
        .help(
            "copy the complete link from your browser, including https:// and the website name",
        ));
    }
    let parsed = |reference, file| Parsed {
        reference,
        file,
        folder: None,
    };
    if let Some(versioned) = osf::versioned(&url) {
        return versioned.map(|url| parsed(Reference::Url(url), None));
    }
    if let Some(error) = osf::stored_item(&url) {
        return Err(error);
    }
    let routed = doi::Doi::from_link(&url)
        .map(|doi| parsed(Reference::Doi(doi), None))
        .or_else(|| {
            zenodo::Record::from_link(&url)
                .map(|(record, file)| parsed(Reference::Zenodo(record), file))
        })
        .or_else(|| {
            figshare::Article::from_link(&url)
                .map(|article| parsed(Reference::Figshare(article), None))
        });
    if let Some(routed) = routed {
        return Ok(routed);
    }
    if let Some(dataset) = dataverse::Dataset::from_link(&url) {
        return dataset.map(|dataset| parsed(Reference::Dataverse(dataset), None));
    }
    let routed = osf::Node::from_link(&url)
        .map(|node| parsed(Reference::Osf(node), None))
        .or_else(|| {
            hf::Repo::from_link(&url).map(|(repo, file, folder)| Parsed {
                reference: Reference::HuggingFace(repo),
                file,
                folder,
            })
        })
        .or_else(|| kaggle::from_link(&url).map(|(reference, file)| parsed(reference, file)))
        .or_else(|| drive::Item::from_link(&url).map(|item| parsed(Reference::Drive(item), None)))
        .or_else(|| {
            dropbox::Link::from_link(&url).map(|link| parsed(Reference::Dropbox(link), None))
        })
        .or_else(|| {
            bucket::Prefix::from_link(&url).map(|prefix| parsed(Reference::Bucket(prefix), None))
        });
    let Some(routed) = routed else {
        if let Some(source) = known_host(&url) {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!(
                    "fetchloom does not know this kind of {source} link: {}",
                    net::Shown(&url)
                ),
            )
            .help("paste the link of the dataset's own page, as in `fetchloom add https://zenodo.org/records/3608990`, or a name to search for"));
        }
        return Ok(parsed(Reference::Url(raw_link(&url).unwrap_or(url)), None));
    };
    Ok(routed)
}

/// repository file pages have a stable raw endpoint, unlike their HTML view.
fn raw_link(url: &Url) -> Option<Url> {
    let parts: Vec<_> = url.path_segments()?.collect();
    if parts.last().is_none_or(|part| part.is_empty()) {
        return None;
    }
    let mut raw = url.clone();
    match url.host_str()? {
        "github.com" if parts.len() >= 5 && matches!(parts[2], "blob" | "raw") => {
            raw.set_scheme("https").ok()?;
            raw.set_host(Some("raw.githubusercontent.com")).ok()?;
            raw.set_path(&format!(
                "/{}/{}/{}",
                parts[0],
                parts[1],
                parts[3..].join("/")
            ));
        }
        "gitlab.com" => {
            let marker = parts.windows(2).position(|pair| pair == ["-", "blob"])?;
            if marker < 2 || parts.len() < marker + 4 {
                return None;
            }
            let mut parts = parts;
            parts[marker + 1] = "raw";
            raw.set_path(&format!("/{}", parts.join("/")));
        }
        _ => return None,
    }
    Some(raw)
}

/// the source a host belongs to, for links to its pages that are not datasets. downloading such a
/// page would only give its HTML.
fn known_host(url: &Url) -> Option<&'static str> {
    Some(match url.host_str()? {
        "zenodo.org" | "www.zenodo.org" => "zenodo",
        host if figshare::is_portal(host) && !url.path().starts_with("/ndownloader/") => "figshare",
        "osf.io" | "www.osf.io" => "osf",
        "huggingface.co" | "www.huggingface.co" | "hf.co" => "hugging face",
        "kaggle.com" | "www.kaggle.com" => "kaggle",
        "drive.google.com" | "docs.google.com" => "google drive",
        "dropbox.com" | "www.dropbox.com" => "dropbox",
        "doi.org" | "www.doi.org" | "dx.doi.org" => "doi",
        _ => return None,
    })
}

/// a plain link: one file, named by its last path segment until the server names it.
fn single(url: &Url) -> Listing {
    Listing {
        within: Vec::new(),
        access: Access::Unknown,
        unavailable: Vec::new(),
        warnings: Vec::new(),
        resolved: Reference::Url(url.clone()),
        title: None,
        license: None,
        doi: None,
        files: vec![Remote {
            path: name::from_url(url).unwrap_or_else(|| "download".to_owned()),
            size: None,
            sha256: None,
            md5: None,
            url: url.clone(),
            named_by_server: true,
        }],
    }
}

/// a listing is hostile input: every path must stay inside the dataset, and no path may appear
/// twice. a dataset with nothing but its readme and git files is told, since its data has
/// usually moved elsewhere.
fn check(reference: &Reference, mut listing: Listing) -> Result<Listing, Error> {
    let only_about = !matches!(reference, Reference::Url(_) | Reference::Dropbox(_))
        && !listing.files.is_empty()
        && listing.files.iter().all(|file| is_about(&file.path));
    let reference = reference.shown();
    if only_about {
        let mut names: Vec<&str> = listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        let last = names.pop().unwrap_or_default();
        let names = if names.is_empty() {
            last.to_owned()
        } else {
            format!("{} and {last}", names.join(", "))
        };
        listing.warnings.push(format!(
            "{reference} holds only {names}, no data files; its README may say where the data went"
        ));
    }
    let count = listing.files.len() + listing.unavailable.len();
    if count > most_files() {
        return Err(too_many(
            reference,
            "select the folders you need with --select",
        ));
    }
    let mut seen = HashSet::with_capacity(count);
    for path in listing.paths() {
        if let Some(reason) = name::escapes(path) {
            let path = path.escape_debug();
            return Err(Error::new(
                Kind::SourceInvalid,
                format!(
                    "{reference} lists a file at \"{path}\", which fetchloom refuses because {reason}"
                ),
            )
            .help("ask the dataset's owner to rename that file, then run the same command again"));
        }
        if !seen.insert(path) {
            return Err(Error::new(
                Kind::SourceInvalid,
                format!("{reference} lists \"{path}\" twice"),
            )
            .help("ask the dataset's owner to give the files distinct names, then run the same command again"));
        }
    }
    Ok(listing)
}

/// whether a top-level file only describes a dataset: a readme, a license or citation, or git's
/// own files.
fn is_about(path: &str) -> bool {
    if path.contains('/') {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    matches!(lower.as_str(), ".gitattributes" | ".gitignore" | ".gitkeep")
        || ["readme", "license", "licence", "citation", "copying"]
            .iter()
            .any(|stem| {
                lower
                    .strip_prefix(stem)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
            })
}

/// what to do about an id its source does not have, as a project or a command line gives ids.
/// `get`, which keeps no project, words its own.
pub(crate) const UNKNOWN_HELP: &str = "check the id in a browser, then fix it on the command line or as ref in data.toml. a dataset that was removed cannot be fetched";

/// reads a JSON answer from a source's API. a missing dataset and a refusal become errors that
/// name the reference, so every source words them the same way.
pub(crate) fn json<T: serde::de::DeserializeOwned>(
    reference: &dyn fmt::Display,
    response: &Response,
) -> Result<T, Error> {
    let reference = reference.to_string();
    let reference = net::mask_query(&reference);
    match response.status {
        200..=299 => response.json(),
        404 | 410 => Err(Error::new(
            Kind::ReferenceUnresolved,
            format!("{reference} does not exist, or is not public"),
        )
        .help(UNKNOWN_HELP)),
        401 | 403 => Err(refused(net::host(&response.url), &reference)),
        status => Err(Error::new(
            Kind::NetworkStatus,
            format!(
                "{} answered {status} for {reference}",
                net::host(&response.url)
            ),
        )),
    }
}

/// a refusal from the source at `host`, naming the account that could lift it.
pub(crate) fn refused(host: &str, reference: &str) -> Error {
    let error = Error::new(
        Kind::AuthRequired,
        format!("{host} refused access to {reference}"),
    );
    match crate::auth::Account::from_host(host) {
        Some(account) => error
            .help(format!(
                "if the dataset is private or restricted, sign in with `{}`, then run the same command again",
                account.command()
            ))
            .needs(account, None),
        None => error.help("if the dataset is private or restricted, ask its owner for a public link"),
    }
}
fn unknown(text: &str) -> Error {
    let text = net::mask_query(text);
    Error::new(Kind::ReferenceInvalid, format!("fetchloom cannot read \"{text}\" as a dataset"))
        .help("use a name to search for, the link of the dataset's page, a DOI, or an id such as kaggle:uciml/iris, as in `fetchloom add https://huggingface.co/datasets/scikit-learn/iris`")
}

/// the error for a malformed id after a known prefix, such as `zenodo:abc`.
fn malformed(text: &str, expected: &str) -> Error {
    let text = net::mask_query(text);
    Error::new(
        Kind::ReferenceInvalid,
        format!("\"{text}\" is not a valid reference"),
    )
    .help(format!("write it as {expected}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_names_the_account_that_lifts_it() {
        use crate::auth::Account;
        for (host, account, command) in [
            ("zenodo.org", Account::Zenodo, "fetchloom auth login zenodo"),
            (
                "api.figshare.com",
                Account::Figshare,
                "fetchloom auth login figshare",
            ),
            ("api.osf.io", Account::Osf, "fetchloom auth login osf"),
            (
                "huggingface.co",
                Account::HuggingFace,
                "fetchloom auth login hf",
            ),
            (
                "www.kaggle.com",
                Account::Kaggle,
                "fetchloom auth login kaggle",
            ),
        ] {
            let error = refused(host, "x");
            assert_eq!(error.need.map(|need| need.account), Some(account), "{host}");
            assert!(error.help.unwrap_or_default().contains(command), "{host}");
        }
        let unknown = refused("example.org", "x");
        assert!(unknown.need.is_none());
        assert!(unknown.help.unwrap_or_default().contains("public link"));
    }

    fn parse(text: &str) -> Parsed {
        Reference::parse(text).unwrap()
    }

    #[test]
    fn local_never_resolves() {
        let parsed = parse("local");
        assert_eq!(parsed.reference, Reference::Local);
        assert_eq!(parsed.reference.to_string(), "local");
        let api = recorded::Recorded::default();
        let error = recorded::block_on(parsed.reference.resolve(&api)).unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceUnresolved);
        assert_eq!(
            error.help.as_deref(),
            Some("run `fetchloom add <folder> --as <name>` where the files are")
        );
        assert!(api.asked().is_empty(), "nothing is asked of anyone");
        assert!(
            Reference::parse("Local").is_err(),
            "only data.lock's own word"
        );
    }

    #[test]
    fn repository_file_pages_resolve_to_raw_references() {
        for (typed, raw) in [
            (
                "https://github.com/mwaskom/seaborn-data/blob/master/iris.csv",
                "https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv",
            ),
            (
                "http://github.com/org/repo/raw/main/folder/data%20set.csv?download=1",
                "https://raw.githubusercontent.com/org/repo/main/folder/data%20set.csv?download=1",
            ),
            (
                "https://gitlab.com/group/sub/repo/-/blob/main/data.csv",
                "https://gitlab.com/group/sub/repo/-/raw/main/data.csv",
            ),
        ] {
            let parsed = parse(typed);
            assert_eq!(parsed.reference.to_string(), raw);
            assert!(parsed.file.is_none());
            let Reference::Url(url) = parsed.reference else {
                panic!("plain URL expected")
            };
            let listing = single(&url);
            assert_eq!(listing.resolved.to_string(), raw);
            assert_eq!(listing.files[0].url.as_str(), raw);
        }
        for unchanged in [
            "https://github.com/org/repo/tree/main",
            "https://github.com/org/repo/blob/main",
            "https://github.com/org/repo/blob/main/",
            "https://github.com.example.org/org/repo/blob/main/data.csv",
            "https://gitlab.com/group/repo/-/blob/main",
            "https://example.org/org/repo/blob/main/data.csv",
        ] {
            assert_eq!(parse(unchanged).reference.to_string(), unchanged);
        }
    }

    #[test]
    fn ids_links_and_dois_find_their_source() {
        let cases = [
            ("zenodo:3608990", "zenodo:3608990"),
            ("https://zenodo.org/records/3608990", "zenodo:3608990"),
            ("https://zenodo.org/record/3608990", "zenodo:3608990"),
            ("doi:10.5281/zenodo.3608990", "doi:10.5281/zenodo.3608990"),
            ("10.5281/zenodo.3608990", "doi:10.5281/zenodo.3608990"),
            (
                "https://doi.org/10.5281/zenodo.3608990",
                "doi:10.5281/zenodo.3608990",
            ),
            ("figshare:34744689", "figshare:34744689"),
            ("figshare:34744689@2", "figshare:34744689@2"),
            (
                "https://figshare.com/articles/dataset/some_title/34744689/2",
                "figshare:34744689@2",
            ),
            (
                "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
                "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
            ),
            (
                "https://dataverse.harvard.edu/dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP&version=2.0",
                "dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP@2.0",
            ),
            ("osf:abc12", "osf:abc12"),
            ("https://osf.io/abc12/", "osf:abc12"),
            (
                "hf:datasets/stanfordnlp/imdb",
                "hf:datasets/stanfordnlp/imdb",
            ),
            (
                "hf:stanfordnlp/imdb@main",
                "hf:datasets/stanfordnlp/imdb@main",
            ),
            (
                "https://huggingface.co/datasets/stanfordnlp/imdb",
                "hf:datasets/stanfordnlp/imdb",
            ),
            ("kaggle:uciml/iris", "kaggle:uciml/iris"),
            (
                "https://www.kaggle.com/datasets/uciml/iris/versions/2",
                "kaggle:uciml/iris@2",
            ),
            ("kaggle:competitions/titanic", "kaggle:competitions/titanic"),
            (
                "https://www.kaggle.com/c/titanic/data",
                "kaggle:competitions/titanic",
            ),
            (
                "https://drive.google.com/file/d/1AbCdEfGhIjK/view?usp=sharing",
                "drive:1AbCdEfGhIjK",
            ),
            (
                "https://drive.google.com/drive/folders/1AbCdEfGhIjK",
                "drive:folders/1AbCdEfGhIjK",
            ),
            (
                "https://www.dropbox.com/s/abc123/data.zip?dl=0",
                "https://www.dropbox.com/s/abc123/data.zip?dl=1",
            ),
            (
                "https://example.org/data.csv",
                "https://example.org/data.csv",
            ),
        ];
        for (text, canonical) in cases {
            assert_eq!(parse(text).reference.to_string(), canonical, "{text}");
            assert_eq!(
                parse(canonical).reference,
                parse(text).reference,
                "{canonical} reads back the same"
            );
        }
    }

    #[test]
    fn links_to_one_file_say_which() {
        let zenodo = parse("https://zenodo.org/records/3608990/files/iris.csv?download=1");
        assert_eq!(zenodo.reference.to_string(), "zenodo:3608990");
        assert_eq!(zenodo.file.as_deref(), Some("iris.csv"));
        let hf = parse(
            "https://huggingface.co/datasets/stanfordnlp/imdb/blob/main/plain_text/train-00000-of-00001.parquet",
        );
        assert_eq!(
            hf.reference.to_string(),
            "hf:datasets/stanfordnlp/imdb@main"
        );
        assert_eq!(
            hf.file.as_deref(),
            Some("plain_text/train-00000-of-00001.parquet")
        );
        let raw = parse("https://huggingface.co/datasets/org/name/raw/main/data.csv");
        assert_eq!(raw.reference.to_string(), "hf:datasets/org/name@main");
        assert_eq!(raw.file.as_deref(), Some("data.csv"));
        let model = parse("https://huggingface.co/gpt2/raw/main/config.json");
        assert_eq!(model.reference.to_string(), "hf:models/gpt2@main");
        assert_eq!(model.file.as_deref(), Some("config.json"));
        let history = parse("https://huggingface.co/gpt2/commits/main");
        assert_eq!(history.reference.to_string(), "hf:models/gpt2");
        // a repo may be named like a page
        let named = parse("https://huggingface.co/datasets/acme/tree");
        assert_eq!(named.reference.to_string(), "hf:datasets/acme/tree");
        let inside = parse("https://huggingface.co/datasets/acme/tree/blob/main/file.csv");
        assert_eq!(inside.reference.to_string(), "hf:datasets/acme/tree@main");
        assert_eq!(inside.file.as_deref(), Some("file.csv"));
    }

    #[test]
    fn folder_links_select_their_folder() {
        let folder =
            parse("https://huggingface.co/datasets/HuggingFaceFW/fineweb/tree/main/sample/10BT");
        assert_ne!(folder, parse("hf:datasets/HuggingFaceFW/fineweb@main"));
        assert_eq!(
            folder.reference.to_string(),
            "hf:datasets/HuggingFaceFW/fineweb@main"
        );
        assert_eq!(folder.file, None);
        assert_eq!(folder.select(), ["sample/10BT/**"]);
        let glob = crate::glob::Glob::new(&folder.select()[0]).unwrap();
        assert!(glob.matches("sample/10BT/000_00000.parquet"));
        assert!(glob.matches("sample/10BT/deeper/000_00000.parquet"));
        assert!(!glob.matches("sample/100BT/000_00000.parquet"));
        assert!(!glob.matches("data/CC-MAIN-2013-20/000_00000.parquet"));
        let marked = parse("https://huggingface.co/datasets/org/name/tree/v1/data%5B1%5D/*");
        assert_eq!(marked.select(), ["data[[]1]/[*]/**"]);
        let glob = crate::glob::Glob::new(&marked.select()[0]).unwrap();
        assert!(glob.matches("data[1]/*/x.csv"));
        assert!(!glob.matches("data1/a/x.csv"));
        let file = parse("https://huggingface.co/datasets/org/name/blob/main/data[1].csv");
        assert_eq!(file.select(), ["data[[]1].csv"]);
        for whole in [
            "https://huggingface.co/datasets/org/name/tree/main",
            "https://huggingface.co/datasets/org/name/tree/main/",
            "hf:datasets/org/name",
        ] {
            assert_eq!(parse(whole).select(), Vec::<String>::new(), "{whole}");
        }
        let inside =
            Reference::parse("https://osf.io/ezcuj/files/osfstorage/553e69248c5e4a219919ea54");
        let error = inside.unwrap_err();
        assert_eq!(error.kind, Kind::ReferenceInvalid);
        assert!(error.help.unwrap_or_default().contains("osf:ezcuj"));
        for whole in [
            "https://osf.io/ezcuj/files/osfstorage",
            "https://osf.io/ezcuj/files/",
        ] {
            assert_eq!(parse(whole).reference.to_string(), "osf:ezcuj", "{whole}");
        }
    }

    #[test]
    fn nonsense_is_refused_with_a_way_forward() {
        for text in [
            "zenodo is great",
            "zenodo:abc",
            "kaggle:onlyowner",
            "https://user:secret@example.org/x",
            "ftp://example.org/x",
            "https://",
        ] {
            let err = Reference::parse(text).unwrap_err();
            assert_eq!(err.kind, Kind::ReferenceInvalid, "{text}");
        }
    }

    #[test]
    fn reference_errors_mask_query_secrets() {
        for text in [
            "https://huggingface.co/settings?token=abc",
            "https://huggingface.co/settings?%74oken=abc&download=1#readme",
            "https://[invalid]/data?token=abc",
            "ftp://example.org/data?token=abc",
            "zenodo:bad?token=abc",
        ] {
            let error = Reference::parse(text).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid);
            assert!(
                error.message.contains("token=***"),
                "reference error exposed its query: {}",
                error.message
            );
            assert!(!error.message.contains("abc"), "{}", error.message);
        }
        let ordinary = "https://huggingface.co/settings?download=1#readme";
        assert!(
            Reference::parse(ordinary)
                .unwrap_err()
                .message
                .contains(ordinary)
        );
    }

    #[test]
    fn listing_errors_mask_query_secrets() {
        for text in [
            "https://example.org/data?token=abc",
            "https://www.dropbox.com/s/id/data.csv?token=abc",
        ] {
            let reference = parse(text).reference;
            assert!(reference.to_string().contains("token=abc"));
            let mut listing = single(&Url::parse(text).unwrap());
            listing.files[0].path = "../outside".to_owned();
            let error = check(&reference, listing).unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(
                error.message.contains("token=***"),
                "listing error exposed its query: {}",
                error.message
            );
            assert!(!error.message.contains("abc"), "{}", error.message);
        }
    }

    #[test]
    fn a_hugging_face_dataset_is_named_after_its_id_not_its_title() {
        let titled = |text: &str| {
            let mut listing = single(&Url::parse("https://example.org/data.csv").unwrap());
            listing.resolved = parse(text).reference;
            listing.title = Some("Multi-Genre Natural Language Inference".to_owned());
            listing
        };
        assert_eq!(titled("hf:datasets/nyu-mll/multi_nli").naming_title(), None);
        assert_eq!(
            titled("zenodo:3608990").naming_title(),
            Some("Multi-Genre Natural Language Inference")
        );
    }

    #[test]
    fn response_errors_mask_query_secrets() {
        let url = Url::parse("https://example.org/data?token=abc").unwrap();
        let reference = Reference::Url(url.clone());
        for status in [404, 403, 500, 200] {
            let response = Response {
                status,
                url: url.clone(),
                next: None,
                region: None,
                keyed: None,
                body: b"not json".to_vec(),
            };
            let error = json::<serde_json::Value>(&reference, &response).unwrap_err();
            let text = format!("{} {}", error.message, error.details.join(" "));
            assert!(
                text.contains("token=***"),
                "response error exposed its query: {text}"
            );
            assert!(!text.contains("abc"), "{text}");
        }
    }

    #[test]
    fn bidi_controls_are_refused_in_listings() {
        let url = Url::parse("https://example.org/x").unwrap();
        let reference = Reference::Url(url.clone());
        for unavailable in [false, true] {
            let mut listing = single(&url);
            let path = "report\u{202e}txt.exe".to_owned();
            if unavailable {
                listing.files.clear();
                listing.unavailable.push(Unavailable {
                    path,
                    reason: "not downloadable".to_owned(),
                });
            } else {
                listing.files[0].path = path;
            }
            let result = check(&reference, listing);
            assert!(result.is_err(), "accepted bidi listed path");
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::SourceInvalid);
            assert!(
                error.message.contains("control character"),
                "{}",
                error.message
            );
            assert!(
                error.message.contains("report\\u{202e}txt.exe"),
                "{}",
                error.message
            );
            assert!(!error.message.contains('\u{202e}'), "{}", error.message);
        }
    }

    #[test]
    fn listings_cannot_escape_their_folder_or_repeat_a_path() {
        let url = Url::parse("https://example.org/x").unwrap();
        let listing = |paths: &[&str]| Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable: Vec::new(),
            warnings: Vec::new(),
            files: paths
                .iter()
                .map(|path| Remote {
                    path: (*path).to_owned(),
                    url: url.clone(),
                    ..single(&url).files[0].clone()
                })
                .collect(),
            ..single(&url)
        };
        let reference = Reference::Url(url.clone());
        assert!(check(&reference, listing(&["a/b.csv", "c.csv"])).is_ok());
        for bad in [
            &["../x"][..],
            &["/etc/passwd"],
            &["a//b"],
            &["a\\b"],
            &["C:/x"],
            &["x", "x"],
        ] {
            assert_eq!(
                check(&reference, listing(bad)).unwrap_err().kind,
                Kind::SourceInvalid,
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_dataset_holding_only_its_readme_says_so() {
        let url = Url::parse("https://example.org/x").unwrap();
        let listing = |paths: &[&str]| Listing {
            within: Vec::new(),
            access: Access::Unknown,
            files: paths
                .iter()
                .map(|path| Remote {
                    path: (*path).to_owned(),
                    ..single(&url).files[0].clone()
                })
                .collect(),
            ..single(&url)
        };
        let repository = parse("hf:datasets/mozilla-foundation/common_voice_17_0").reference;
        let checked = check(&repository, listing(&[".gitattributes", "README.md"])).unwrap();
        assert_eq!(
            checked.warnings,
            [
                "hf:datasets/mozilla-foundation/common_voice_17_0 holds only .gitattributes and README.md, no data files; its README may say where the data went"
            ]
        );
        for fine in [
            &[".gitattributes", "README.md", "data/train.parquet"][..],
            &["docs/README.md", "LICENSE"],
        ] {
            assert!(
                check(&repository, listing(fine))
                    .unwrap()
                    .warnings
                    .is_empty(),
                "{fine:?}"
            );
        }
        let one_file = Reference::Url(Url::parse("https://example.org/README.md").unwrap());
        assert!(
            check(&one_file, listing(&["README.md"]))
                .unwrap()
                .warnings
                .is_empty(),
            "a link to one readme asked for it"
        );
    }
}
