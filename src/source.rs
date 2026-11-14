//! sources: how a person names a dataset and how each source lists its files, saying what to
//! download and how to check it. file bodies are left to the downloader, but kaggle reads a
//! competition's smallest file to learn whether its rules were accepted.

mod arxiv;
mod bucket;
mod dandi;
mod dataverse;
mod doi;
pub(crate) mod drive;
mod dropbox;
mod dryad;
mod ena;
mod figshare;
mod geo;
mod github;
mod gitlab;
mod hf;
pub(crate) mod kaggle;
pub(crate) mod local;
mod mendeley;
mod openneuro;
mod osf;
#[cfg(test)]
pub(crate) mod recorded;
mod s2;
pub(crate) mod search;

pub(crate) use s2::Relink;
mod xml;
mod zenodo;

use std::collections::HashSet;
use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use tracing::Instrument as _;
use url::Url;

pub(crate) use bucket::{
    AzureContainer, Bucket, GcsBucket, Keyed, Sas, Signer, Signing, VERSION as AZURE_VERSION,
    account_of as azure_account, is_account as is_azure_account,
    is_container as is_azure_container, unversioned, with_version,
};
pub(crate) use drive::DriveId;

use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;
use crate::net::{self, Api, Fetch, Response};

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
    too_many_of(subject, most_files(), instead)
}

/// the pages of one listing, named by a token, a link or a number. a page named again, more pages
/// than its most files need, or a long run of pages that bring nothing new is refused, in one
/// wording every source's error carries.
pub(crate) struct Pages<K> {
    asked: HashSet<K>,
    most: usize,
    /// the entries the listing held when the page before, in this chain of pages, was asked for.
    held: Option<usize>,
    /// the pages in a row of this chain that brought no entry.
    idle: usize,
}

/// pages in a row that bring no entry before a listing counts as broken. cloud storage may
/// pause a listing with empty pages, which must never fail it, so only a long run counts.
const MOST_IDLE: usize = 100;

impl<K: Eq + std::hash::Hash> Pages<K> {
    /// the pages of a listing of at most [`most_files`] files.
    pub(crate) fn new() -> Pages<K> {
        Pages::most(most_files())
    }

    /// the pages of a listing of at most `most` files.
    pub(crate) fn most(most: usize) -> Pages<K> {
        Pages {
            asked: HashSet::new(),
            most,
            held: None,
            idle: 0,
        }
    }

    /// the pages of a listing that follows each page's token, whose first page, which no token
    /// names, is asked for before the guard hears of the next.
    pub(crate) fn after_first() -> Pages<K> {
        Pages {
            held: Some(0),
            ..Pages::new()
        }
    }

    /// starts another chain of pages, as a walk does for each folder it lists: an empty folder
    /// is one page with nothing in it and no next, which leaves no listing broken.
    pub(crate) fn begin(&mut self) {
        self.held = None;
        self.idle = 0;
    }

    /// counts the page `key` names before it is asked for, the listing holding `entries` from
    /// the pages before it, or says why it is not.
    pub(crate) fn next(&mut self, key: K, entries: usize) -> Result<(), String> {
        if let Some(held) = self.held {
            self.idle = if entries > held { 0 } else { self.idle + 1 };
        }
        self.held = Some(entries);
        // a loop is the plainer cause, so it is told first when pages also bring nothing new
        if !self.asked.insert(key) {
            return Err("it repeated an earlier page".to_owned());
        }
        if self.idle >= MOST_IDLE {
            return Err(format!(
                "it sent {MOST_IDLE} pages in a row that held nothing new"
            ));
        }
        if self.asked.len() > self.most {
            return Err(format!(
                "it sent more than {} pages",
                crate::units::grouped(self.most as u64)
            ));
        }
        Ok(())
    }
}

/// [`too_many`] for a listing that allows `most` files.
pub(crate) fn too_many_of(subject: impl fmt::Display, most: usize, instead: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "{subject} lists more than {} files, the most this run lists for one dataset",
            crate::units::grouped(most as u64)
        ),
    )
    .help(format!(
        "{instead}, or raise FETCHLOOM_MOST_FILES, or most_files in the settings file; each listed file takes about 2 KiB of memory"
    ))
}

/// reads the environment, so tests can give each run its own.
pub(crate) type Var<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// the environment a source reads to tell whether its requests go signed in. unit tests read an
/// empty one, so a key saved on the machine running them changes nothing.
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
    Dryad(dryad::Dataset),
    Mendeley(mendeley::Dataset),
    GitHub(github::Release),
    GitLab(gitlab::Release),
    Ena(ena::Accession),
    Geo(geo::Accession),
    Dandi(dandi::Dandiset),
    OpenNeuro(openneuro::Dataset),
    Figshare(figshare::Article),
    Dataverse(dataverse::Dataset),
    Osf(osf::Node),
    HuggingFace(hf::Repo),
    Kaggle(kaggle::Dataset),
    KaggleCompetition(kaggle::Competition),
    Drive(drive::Item),
    Dropbox(dropbox::Link),
    Bucket(bucket::Prefix),
    /// one arXiv paper, at a version or its newest.
    Arxiv(arxiv::Paper),
    /// the arXiv papers of a month, or of one category in a month.
    ArxivMonth(arxiv::Month),
    /// one paper semantic scholar knows, at the open-access pdf it links.
    S2Paper(s2::Paper),
    /// a dataset of a semantic scholar release, at that release or the newest.
    S2Dataset(s2::Dataset),
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
    /// the key of a value granting access, such as a token, that the reference's link carried,
    /// even where the source's own reference leaves the value out.
    pub(crate) secret: Option<String>,
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

    /// refuses a link that carries a value granting access, such as a token or a presigned link's
    /// signature: data.toml keeps a reference as written, data.lock copies it, and git shares both.
    pub(crate) fn check_shareable(&self) -> Result<(), Error> {
        let Some(key) = &self.secret else {
            return Ok(());
        };
        let error = Error::new(
            Kind::ReferenceInvalid,
            format!(
                "this link carries {key}, a secret that data.toml and data.lock would keep and share"
            ),
        );
        let without =
            "use a link that works without it, or download the file once with `fetchloom get`";
        Err(match &self.reference {
            Reference::Url(url) if url.host_str().and_then(bucket::account_of).is_some() => error
                .help(
                    "keep the SAS out of the link: list the container in azure_containers and set AZURE_STORAGE_SAS_TOKEN to the SAS, or download the file once with `fetchloom get`",
                ),
            Reference::Url(url) if key == "key" => {
                error.help(signing_in(url).unwrap_or_else(|| without.to_owned()))
            }
            Reference::Url(_) | Reference::Dropbox(_) => error.help(without),
            read => error.help(format!(
                "leave {key} out of the link: fetchloom reads {read} without it, with your saved key where its source asks for one"
            )),
        })
    }
}

/// how to read a link to a dataverse installation's api without the key it carries: a key saved
/// for the installation goes with each request to it.
fn signing_in(url: &Url) -> Option<String> {
    let host = match url.port() {
        Some(port) => format!("{}:{port}", url.host_str()?),
        None => url.host_str()?.to_owned(),
    };
    let host = crate::auth::dataverse_host(&host)?;
    let command = crate::account::Account::Dataverse(host.clone()).command();
    Some(format!(
        "leave key out of the link: sign in once with `{command}`, and fetchloom sends your key to {host} itself"
    ))
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
    /// the folders listed when only those were, as [`Glob::scope`] writes them: `sub-*/anat` stands
    /// for each folder it matches and an empty one for the top's files. empty for a whole listing.
    pub(crate) within: Vec<String>,
}

impl Listing {
    /// the title a generated name may come from. a hugging face repository is known by its id,
    /// such as `multi_nli`, more than by its card's title, so its name comes from the id.
    pub(crate) fn naming_title(&self) -> Option<&str> {
        match self.resolved {
            Reference::HuggingFace(_) | Reference::ArxivMonth(_) | Reference::S2Dataset(_) => None,
            _ => self.title.as_deref(),
        }
    }

    /// whether a generated name may come from the one file the listing holds. a paper is known by
    /// its title, and its file only by an id, as `1706.03762v7.pdf`; an arXiv month and a
    /// semantic scholar dataset by their reference, and their files by ids and times; a bucket's
    /// folder by its path, since folders often hold objects of one name, as `part_0000.gz`.
    pub(crate) fn names_its_file(&self) -> bool {
        match &self.resolved {
            Reference::Arxiv(_)
            | Reference::ArxivMonth(_)
            | Reference::S2Paper(_)
            | Reference::S2Dataset(_) => false,
            Reference::Bucket(prefix) => !prefix.names_a_folder(),
            _ => true,
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
    /// the `ETag` the listing gives, as the service sends it with the bytes: a relock reuses an
    /// earlier download of the link while the listing gives the `ETag` and size the cache recorded.
    pub(crate) etag: Option<String>,
}

/// the prefixes [`Reference::parse`] reads before a `:`, which a guess at a typo picks from.
pub(crate) const SCHEMES: [&str; 23] = [
    "http",
    "https",
    "doi",
    "zenodo",
    "dryad",
    "mendeley",
    "github",
    "gitlab",
    "ena",
    "geo",
    "dandi",
    "openneuro",
    "figshare",
    "dataverse",
    "osf",
    "hf",
    "kaggle",
    "arxiv",
    "s2",
    "drive",
    "s3",
    "gs",
    "azure",
];

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
                secret: None,
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
            "dryad" => whole(Reference::Dryad(dryad::Dataset::parse(rest)?)),
            "mendeley" => whole(Reference::Mendeley(mendeley::Dataset::parse(rest)?)),
            "github" => whole(Reference::GitHub(github::Release::parse(rest)?)),
            "gitlab" => whole(Reference::GitLab(gitlab::Release::parse(rest)?)),
            "ena" => whole(Reference::Ena(ena::Accession::parse(rest)?)),
            "geo" => whole(Reference::Geo(geo::Accession::parse(rest)?)),
            "dandi" => whole(Reference::Dandi(dandi::Dandiset::parse(rest)?)),
            "openneuro" => whole(Reference::OpenNeuro(openneuro::Dataset::parse(rest)?)),
            "figshare" => whole(Reference::Figshare(figshare::Article::parse(rest)?)),
            "dataverse" => whole(Reference::Dataverse(dataverse::Dataset::parse(rest)?)),
            "osf" => whole(Reference::Osf(osf::Node::parse(rest)?)),
            "hf" => whole(Reference::HuggingFace(hf::Repo::parse(rest)?)),
            "kaggle" => whole(kaggle::parse(rest)?),
            "arxiv" => whole(arxiv::parse(rest)?),
            "s2" => whole(s2::parse(rest)?),
            "drive" => whole(Reference::Drive(drive::Item::parse(rest)?)),
            "s3" => whole(Reference::Bucket(bucket::Prefix::parse(
                bucket::Store::S3,
                rest,
            )?)),
            "gs" => whole(Reference::Bucket(bucket::Prefix::parse(
                bucket::Store::Gcs,
                rest,
            )?)),
            "azure" => whole(Reference::Bucket(bucket::Prefix::parse(
                bucket::Store::Azure,
                rest,
            )?)),
            _ => Err(unknown(text)),
        }
    }

    /// the origin a person names by writing this reference, when it is no fixed public service. it
    /// alone may be on a private network: another port or scheme of its host is not named.
    pub(crate) fn written_origin(&self) -> Option<url::Origin> {
        match self {
            Reference::Url(url) => Some(url.origin()),
            Reference::Dataverse(dataset) => dataset.origin(),
            Reference::Bucket(prefix) => prefix.written_origin(),
            _ => None,
        }
    }

    /// whether this names a paper, as arXiv's and semantic scholar's do, rather than a dataset.
    pub(crate) fn is_paper(&self) -> bool {
        matches!(self, Reference::Arxiv(_) | Reference::S2Paper(_))
    }

    /// whether the links of this dataset's files may pin the object version their bytes were,
    /// as S3's may.
    pub(crate) fn keeps_versions(&self) -> bool {
        matches!(self, Reference::Bucket(prefix) if prefix.keeps_versions())
    }

    /// whether this resolved reference names bytes nobody can change, as a hugging face commit, a
    /// kaggle dataset's version or an openneuro snapshot does, even for files no checksum pins.
    pub(crate) fn fixes_bytes(&self) -> bool {
        match self {
            Reference::HuggingFace(repo) => repo.is_at_commit(),
            Reference::Kaggle(dataset) => dataset.is_versioned(),
            Reference::OpenNeuro(dataset) => dataset.is_snapshot(),
            _ => false,
        }
    }

    /// the reference as messages and logs show it, a secret in a link masked.
    pub(crate) fn shown(&self) -> String {
        match self {
            Reference::Url(url) => crate::net::Shown(url).to_string(),
            other => net::mask_query(&other.to_string()).into_owned(),
        }
    }

    /// asks the source what the dataset holds: at the exact version this reference names, or at
    /// the newest one when it names none.
    pub(crate) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        self.resolve_in(api, &[]).await
    }

    /// [`Reference::resolve`], listing only the folders that may hold a file the `select`
    /// patterns match, where the source can. the listing names the folders it covers.
    pub(crate) async fn resolve_in(
        &self,
        api: &impl Api,
        select: &[Glob],
    ) -> Result<Listing, Error> {
        let span = tracing::debug_span!("resolve", reference = %self.shown());
        let listing = async {
            match self {
                Reference::Doi(doi) => {
                    let target = doi.locate(api).await?;
                    let mut listing = target.resolve_direct(api, select).await?;
                    listing.doi.get_or_insert_with(|| doi.name().to_owned());
                    Ok(listing)
                }
                other => other.resolve_direct(api, select).await,
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

    async fn resolve_direct(&self, api: &impl Api, select: &[Glob]) -> Result<Listing, Error> {
        match self {
            Reference::Url(url) => Ok(single(url)),
            Reference::Doi(doi) => Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{doi} leads to another DOI, which fetchloom does not follow"),
            )
            .help("open the DOI in a browser and use the dataset page or a direct file link")),
            Reference::Zenodo(record) => record.resolve(api).await,
            Reference::Dryad(dataset) => dataset.resolve(api).await,
            Reference::Mendeley(dataset) => dataset.resolve(api).await,
            Reference::GitHub(release) => release.resolve(api).await,
            Reference::GitLab(release) => release.resolve(api).await,
            Reference::Ena(accession) => accession.resolve(api).await,
            Reference::Geo(accession) => accession.resolve(api).await,
            Reference::Dandi(dandiset) => dandiset.resolve(api).await,
            Reference::OpenNeuro(dataset) => dataset.resolve(api, select).await,
            Reference::Figshare(article) => article.resolve(api).await,
            Reference::Dataverse(dataset) => dataset.resolve(api).await,
            Reference::Osf(node) => node.resolve(api).await,
            Reference::HuggingFace(repo) => {
                repo.resolve(api, crate::glob::folders(select).as_deref())
                    .await
            }
            Reference::Kaggle(dataset) => dataset.resolve(api).await,
            Reference::KaggleCompetition(competition) => competition.resolve(api).await,
            Reference::Drive(item) => item.resolve(api).await,
            Reference::Dropbox(link) => Ok(link.listing()),
            Reference::Bucket(prefix) => prefix.resolve(api).await,
            Reference::Arxiv(paper) => paper.resolve(api).await,
            Reference::ArxivMonth(month) => month.resolve(api).await,
            Reference::S2Paper(paper) => paper.resolve(api).await,
            Reference::S2Dataset(dataset) => dataset.resolve(api).await,
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
            Reference::Dryad(dataset) => write!(f, "{dataset}"),
            Reference::Mendeley(dataset) => write!(f, "{dataset}"),
            Reference::GitHub(release) => write!(f, "{release}"),
            Reference::GitLab(release) => write!(f, "{release}"),
            Reference::Ena(accession) => write!(f, "{accession}"),
            Reference::Geo(accession) => write!(f, "{accession}"),
            Reference::Dandi(dandiset) => write!(f, "{dandiset}"),
            Reference::OpenNeuro(dataset) => write!(f, "{dataset}"),
            Reference::Figshare(article) => write!(f, "{article}"),
            Reference::Dataverse(dataset) => write!(f, "{dataset}"),
            Reference::Osf(node) => write!(f, "{node}"),
            Reference::HuggingFace(repo) => write!(f, "{repo}"),
            Reference::Kaggle(dataset) => write!(f, "{dataset}"),
            Reference::KaggleCompetition(competition) => write!(f, "{competition}"),
            Reference::Drive(item) => write!(f, "{item}"),
            Reference::Dropbox(link) => write!(f, "{link}"),
            Reference::Bucket(prefix) => write!(f, "{prefix}"),
            Reference::Arxiv(paper) => write!(f, "{paper}"),
            Reference::ArxivMonth(month) => write!(f, "{month}"),
            Reference::S2Paper(paper) => write!(f, "{paper}"),
            Reference::S2Dataset(dataset) => write!(f, "{dataset}"),
            Reference::Local => f.write_str(local::IMPORTED),
        }
    }
}

/// a paper or a dataset a search found, with what a person tells it by.
pub(crate) struct Found {
    pub(crate) reference: Reference,
    pub(crate) title: String,
    /// its first author and year, `Vaswani et al. 2017`, which tells it apart.
    pub(crate) byline: Option<String>,
    /// what to know before adding it, as `no open-access pdf` for a paper add would refuse.
    pub(crate) note: Option<String>,
}

/// a paper's first author's last name and its `year`, as `Vaswani et al. 2017`.
pub(crate) fn byline(authors: &[String], year: Option<&str>) -> Option<String> {
    let first = authors.first()?;
    let surname = first.rsplit(' ').next().unwrap_or(first);
    let who = if authors.len() > 1 {
        format!("{surname} et al.")
    } else {
        surname.to_owned()
    };
    let year =
        year.filter(|year| year.len() == 4 && year.bytes().all(|byte| byte.is_ascii_digit()));
    Some(match year {
        Some(year) => format!("{who} {year}"),
        None => who,
    })
}

/// a download request with the source's response handling.
pub(crate) fn fetch_plan(url: &Url) -> Fetch {
    let (from, divert) = drive::fetch_plan(url)
        .or_else(|| dropbox::fetch_plan(url))
        .unwrap_or_else(|| (url.clone(), None));
    Fetch {
        from,
        divert,
        expired: osf::expiry_check(url).or_else(|| s2::expiry_check(url)),
        ..Fetch::new(url.clone())
    }
}

/// a file of a source's dataset whose download link the source gives only for a while, as
/// semantic scholar's, by the stable link data.lock keeps: the request for a fresh link to it.
pub(crate) fn relink(url: &Url) -> Option<Relink> {
    s2::relink(url)
}

/// whether a signed link a source gave for a while expires before `now`, or too soon after it.
pub(crate) fn link_expired(link: &Url, now: std::time::SystemTime) -> bool {
    s2::link_expired(link, now)
}

/// whether the source behind `url` may send the file wrapped in a zip, `train.csv` as
/// `train.csv.zip`, as kaggle does with large competition files, so its listed size binds nothing.
pub(crate) fn may_wrap(url: &Url) -> bool {
    kaggle::may_wrap(url)
}

impl Reference {
    /// the hosts a search match's files download from, as its source knows before listing them,
    /// the usual one first; none where they vary by file, as a paper's pdf from semantic scholar,
    /// or for a source search does not ask.
    pub(crate) fn download_hosts(&self) -> &'static [&'static str] {
        match self {
            Reference::HuggingFace(_) => &["huggingface.co"],
            Reference::Kaggle(_) | Reference::KaggleCompetition(_) => &["api.kaggle.com"],
            Reference::Zenodo(_) => &["zenodo.org"],
            Reference::Ena(_) => &["ftp.sra.ebi.ac.uk", "ftp.ebi.ac.uk"],
            Reference::Geo(_) => &["ftp.ncbi.nlm.nih.gov"],
            Reference::Dandi(_) => &["dandiarchive.s3.amazonaws.com"],
            Reference::OpenNeuro(_) => &["s3.amazonaws.com"],
            // arXiv's bucket holds most papers; the rest come from arXiv itself
            Reference::Arxiv(_) | Reference::ArxivMonth(_) => {
                &["storage.googleapis.com", "arxiv.org"]
            }
            _ => &[],
        }
    }
}

/// the name requests to `host` are paced and counted under: the host, or for hosts whose source
/// limits them together, as arXiv's terms do its hosts, the name they share.
pub(crate) fn paced_as(host: &str) -> String {
    arxiv::paced_as(host).map_or_else(|| host.to_owned(), str::to_owned)
}

/// how far apart requests paced as `name` start, for a host that asks for that, as NCBI's,
/// arXiv's and semantic scholar's do.
pub(crate) fn spacing(name: &str) -> Option<std::time::Duration> {
    geo::spacing(name)
        .or_else(|| arxiv::spacing(name))
        .or_else(|| s2::spacing(name))
}

/// how many requests paced as `name` may run at once, for hosts that ask for fewer than the
/// network allows, as arXiv's and semantic scholar's ask for one.
pub(crate) fn most_at_once(name: &str) -> Option<usize> {
    arxiv::most_at_once(name).or_else(|| s2::most_at_once(name))
}

/// a source's own error for a download it refused, when it says more than the status.
pub(crate) fn refused_download(url: &Url, status: u16) -> Option<Error> {
    match status {
        401 => dryad::refused_download(url),
        403 => kaggle::refused_download(url).or_else(|| dryad::refused_download(url)),
        _ => None,
    }
}

impl Remote {
    /// the size the downloaded bytes must have: the listed size, unless the source may send the
    /// file wrapped in a zip.
    pub(crate) fn expected_size(&self) -> Option<u64> {
        self.size.filter(|_| !may_wrap(&self.url))
    }
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
    let secret = net::secret(&url);
    let parsed = |reference, file| Parsed {
        reference,
        file,
        folder: None,
        secret: secret.clone(),
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
            repository_link(&url).map(|(reference, folder)| Parsed {
                reference,
                file: None,
                folder,
                secret: secret.clone(),
            })
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
                secret: secret.clone(),
            })
        })
        .or_else(|| kaggle::from_link(&url).map(|(reference, file)| parsed(reference, file)))
        .or_else(|| arxiv::from_link(&url).map(|paper| parsed(Reference::Arxiv(paper), None)))
        .or_else(|| s2::from_link(&url).map(|(reference, file)| parsed(reference, file)))
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
            .help(other_page_help(source)));
        }
        return Ok(parsed(Reference::Url(raw_link(&url).unwrap_or(url)), None));
    };
    Ok(routed)
}

/// a pasted link to a page of a repository whose links name no single file: the dataset the
/// page shows, and the folder inside it a link to one names, as a GEO folder's does.
fn repository_link(url: &Url) -> Option<(Reference, Option<String>)> {
    let whole = |reference| (reference, None);
    dryad::Dataset::from_link(url)
        .map(|dataset| whole(Reference::Dryad(dataset)))
        .or_else(|| {
            mendeley::Dataset::from_link(url).map(|dataset| whole(Reference::Mendeley(dataset)))
        })
        .or_else(|| {
            github::Release::from_link(url).map(|release| whole(Reference::GitHub(release)))
        })
        .or_else(|| {
            gitlab::Release::from_link(url).map(|release| whole(Reference::GitLab(release)))
        })
        .or_else(|| {
            ena::Accession::from_link(url).map(|accession| whole(Reference::Ena(accession)))
        })
        .or_else(|| {
            geo::Accession::from_link(url)
                .map(|(accession, folder)| (Reference::Geo(accession), folder))
        })
        .or_else(|| {
            dandi::Dandiset::from_link(url)
                .map(|(dandiset, folder)| (Reference::Dandi(dandiset), folder))
        })
        .or_else(|| {
            openneuro::Dataset::from_link(url).map(|dataset| whole(Reference::OpenNeuro(dataset)))
        })
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

/// the error for a link that answered with a git-lfs pointer, the few lines a repository keeps
/// in place of a file git-lfs stores apart, which say nothing of the data itself.
pub(crate) fn lfs_pointer(url: &Url) -> Error {
    let error = Error::new(
        Kind::SourceInvalid,
        format!(
            "{} sent a git-lfs pointer, not the file it stands for",
            net::Shown(url)
        ),
    );
    match lfs_link(url) {
        Some(link) => error.help(format!(
            "git-lfs keeps that file apart from the repository: use {link} instead"
        )),
        None => error.help(
            "git-lfs keeps that file apart from the repository: use the link its site gives to download the file itself",
        ),
    }
}

/// the link that serves the file git-lfs keeps for a raw repository link: github's media host,
/// or gitlab's api asked for the file. it leaves out the link's query, which may hold a token.
fn lfs_link(url: &Url) -> Option<String> {
    match url.host_str()? {
        "raw.githubusercontent.com" => Some(format!(
            "https://media.githubusercontent.com/media{}",
            url.path()
        )),
        "gitlab.com" => {
            let (project, rest) = url.path().trim_start_matches('/').split_once("/-/raw/")?;
            let (reference, path) = rest.split_once('/')?;
            let encoded = |text: &str| {
                let decoded = percent_decode_str(text).decode_utf8_lossy();
                utf8_percent_encode(&decoded, NON_ALPHANUMERIC).to_string()
            };
            Some(format!(
                "https://gitlab.com/api/v4/projects/{}/repository/files/{}/raw?ref={}&lfs=true",
                encoded(project),
                encoded(path),
                encoded(reference)
            ))
        }
        _ => None,
    }
}

/// what to give instead of a link to a page of `source` that names no dataset or paper: a paper
/// site's own forms, else a dataset's page.
fn other_page_help(source: &str) -> &'static str {
    match source {
        "arXiv" => arxiv::PAGES,
        "semantic scholar" => s2::PAGES,
        _ => {
            "paste the link of the dataset's own page, as in `fetchloom add https://zenodo.org/records/3608990`, or a name to search for"
        }
    }
}

/// the source a host belongs to, for links to its pages that are not datasets. downloading such a
/// page would only give its HTML.
fn known_host(url: &Url) -> Option<&'static str> {
    Some(match url.host_str()? {
        "zenodo.org" | "www.zenodo.org" => "zenodo",
        "datadryad.org" | "www.datadryad.org" => "dryad",
        "data.mendeley.com" if !url.path().starts_with("/public-files/") => "mendeley data",
        "www.ncbi.nlm.nih.gov" | "ncbi.nlm.nih.gov" if url.path().starts_with("/geo/") => "geo",
        "dandiarchive.org" | "www.dandiarchive.org" => "dandi",
        "openneuro.org" | "www.openneuro.org" if !url.path().starts_with("/crn/") => "openneuro",
        "www.ebi.ac.uk" | "ebi.ac.uk"
            if ["/ena/browser/view/", "/ena/data/view/"]
                .iter()
                .any(|page| url.path().starts_with(page)) =>
        {
            "ena"
        }
        host if figshare::is_portal(host) && !url.path().starts_with("/ndownloader/") => "figshare",
        "osf.io" | "www.osf.io" => "osf",
        "huggingface.co" | "www.huggingface.co" | "hf.co" => "hugging face",
        "kaggle.com" | "www.kaggle.com" => "kaggle",
        "drive.google.com" | "docs.google.com" => "google drive",
        "dropbox.com" | "www.dropbox.com" => "dropbox",
        "doi.org" | "www.doi.org" | "dx.doi.org" => "doi",
        "arxiv.org" | "www.arxiv.org" | "export.arxiv.org" => "arXiv",
        "www.semanticscholar.org" | "semanticscholar.org" | "api.semanticscholar.org" => {
            "semantic scholar"
        }
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
            etag: None,
        }],
    }
}

/// a listing is hostile input: every path must stay inside the dataset and appear once. a dataset
/// of only its readme and git files is told, since its data has usually moved elsewhere.
fn check(reference: &Reference, mut listing: Listing) -> Result<Listing, Error> {
    let only_about = !matches!(reference, Reference::Url(_) | Reference::Dropbox(_))
        && !listing.files.is_empty()
        && listing.files.iter().all(|file| is_about(&file.path));
    let reference = reference.shown();
    if only_about {
        let names: Vec<&str> = listing
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        let names = crate::units::listed(&names, "and");
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
            return Err(refused_name(&reference, path, reason));
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

/// the error for a file or folder `reference` lists at `path`, a name fetchloom refuses
/// `because` of what it holds, which only the dataset's owner can change.
pub(crate) fn refused_name(reference: &str, path: &str, because: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "{reference} lists \"{}\", which fetchloom refuses because {because}",
            path.escape_debug()
        ),
    )
    .help("ask the dataset's owner to rename it, then run the same command again")
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
    match crate::account::Account::from_host(host) {
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

    /// every prefix a guess may offer is one the parser reads, so a guess never names another
    /// unknown one.
    #[test]
    fn every_scheme_a_guess_offers_is_read() {
        for scheme in SCHEMES {
            if let Err(error) = Reference::parse(&format!("{scheme}:x")) {
                assert!(
                    !error.message.starts_with("fetchloom cannot read"),
                    "{scheme}: {}",
                    error.message
                );
            }
        }
    }

    #[test]
    fn a_refusal_names_the_account_that_lifts_it() {
        use crate::account::Account;
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

    /// data.toml keeps the link as written and data.lock copies it. the help names the reference
    /// fetchloom reads without it.
    #[test]
    fn a_secret_in_a_link_is_refused_whichever_source_reads_it() {
        for (link, read) in [
            (
                "https://zenodo.org/records/3608990?token=abc",
                "zenodo:3608990",
            ),
            (
                "https://doi.org/10.5281/zenodo.3608990#access_token=abc",
                "doi:10.5281/zenodo.3608990",
            ),
        ] {
            let error = parse(link).check_shareable().unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{link}");
            assert!(!error.message.contains("abc"), "{link}");
            let help = error.help.unwrap_or_default();
            assert!(help.contains(read), "{link}: {help}");
        }
        let plain = parse("https://zenodo.org/records/3608990?download=1");
        assert!(plain.check_shareable().is_ok());
        let sas = parse(
            "https://lab.blob.core.windows.net/raw/a.csv?sv=2022-11-02&sr=c&sp=rl&se=2026-12-31&sig=abc",
        );
        let error = sas.check_shareable().unwrap_err();
        assert!(!error.message.contains("abc"), "{}", error.message);
        let help = error.help.unwrap_or_default();
        assert!(
            help.contains("AZURE_STORAGE_SAS_TOKEN") && help.contains("azure_containers"),
            "{help}"
        );
        let keyed = parse("https://demo.dataverse.org/api/access/datafile/42?key=abc");
        let error = keyed.check_shareable().unwrap_err();
        assert_eq!(
            error.message,
            "this link carries key, a secret that data.toml and data.lock would keep and share"
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "leave key out of the link: sign in once with `fetchloom auth login dataverse --host demo.dataverse.org`, and fetchloom sends your key to demo.dataverse.org itself"
            )
        );
        // a host no real installation has, as one holding `$(`, gets no command to paste
        let hostile = parse("https://data.$(id).invalid/api/access/datafile/1?key=abc");
        let error = hostile.check_shareable().unwrap_err();
        assert_eq!(
            error.help.as_deref(),
            Some(
                "use a link that works without it, or download the file once with `fetchloom get`"
            )
        );
    }

    /// a long run of empty pages is refused too, in the words every source's error carries.
    #[test]
    fn pages_refuse_a_page_named_again_and_more_than_a_listing_needs() {
        let mut pages = Pages::most(2);
        assert_eq!(pages.next("a", 0), Ok(()));
        assert_eq!(
            pages.next("a", 1),
            Err("it repeated an earlier page".to_owned())
        );
        assert_eq!(pages.next("b", 1), Ok(()));
        assert_eq!(
            pages.next("c", 2),
            Err("it sent more than 2 pages".to_owned())
        );
        let mut idle = Pages::most(1_000);
        for page in 0..100 {
            assert_eq!(idle.next(page, 7), Ok(()), "{page}");
        }
        // a page that brings an entry starts the run again
        for page in 100..200 {
            assert_eq!(idle.next(page, 8), Ok(()), "{page}");
        }
        assert_eq!(
            idle.next(200, 8),
            Err("it sent 100 pages in a row that held nothing new".to_owned())
        );
        // each empty folder of a walk is a chain of its own
        let mut walk = Pages::most(1_000);
        for folder in 0..200 {
            walk.begin();
            assert_eq!(walk.next(folder, 0), Ok(()), "{folder}");
        }
    }

    /// a commit, a dataset's version and a snapshot fix their bytes. a branch, a dataset without
    /// its version and a link do not.
    #[test]
    fn only_a_revision_nobody_can_change_fixes_its_bytes() {
        for (text, fixed) in [
            (
                "hf:datasets/scikit-learn/iris@0123456789abcdef0123456789abcdef01234567",
                true,
            ),
            ("hf:datasets/scikit-learn/iris@main", false),
            ("hf:datasets/scikit-learn/iris", false),
            ("kaggle:uciml/iris@2", true),
            ("kaggle:uciml/iris", false),
            ("openneuro:ds000001@1.0.0", true),
            ("openneuro:ds000001", false),
            ("https://example.org/data.csv", false),
            ("geo:GSE1000", false),
        ] {
            assert_eq!(parse(text).reference.fixes_bytes(), fixed, "{text}");
        }
    }

    /// the help gives the file's own link where the site has one, never repeating the query.
    #[test]
    fn a_git_lfs_pointer_is_answered_with_the_link_of_its_file() {
        for (link, help) in [
            (
                "https://raw.githubusercontent.com/lab/scans/main/data/model.bin?token=abc",
                "git-lfs keeps that file apart from the repository: use https://media.githubusercontent.com/media/lab/scans/main/data/model.bin instead",
            ),
            (
                "https://gitlab.com/lab/vision/scans/-/raw/v1.0/data/model%20one.bin",
                "git-lfs keeps that file apart from the repository: use https://gitlab.com/api/v4/projects/lab%2Fvision%2Fscans/repository/files/data%2Fmodel%20one%2Ebin/raw?ref=v1%2E0&lfs=true instead",
            ),
            (
                "https://example.org/model.bin",
                "git-lfs keeps that file apart from the repository: use the link its site gives to download the file itself",
            ),
        ] {
            let error = lfs_pointer(&Url::parse(link).unwrap());
            assert_eq!(error.kind, Kind::SourceInvalid, "{link}");
            assert_eq!(error.help.as_deref(), Some(help), "{link}");
            assert!(!format!("{error:?}").contains("abc"), "{link}");
        }
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

    /// accessions of the read archives and of GEO, and the pages that show them, find their
    /// source, written as each archive writes them.
    #[test]
    fn archive_accessions_and_pages_find_their_source() {
        for (text, canonical) in [
            ("ena:SRR390728", "ena:SRR390728"),
            ("ena:srr390728", "ena:SRR390728"),
            ("ena:submitted/ERR1160846", "ena:submitted/ERR1160846"),
            ("ena:fastq/PRJNA172563", "ena:PRJNA172563"),
            (
                "https://www.ebi.ac.uk/ena/browser/view/SRX079566",
                "ena:SRX079566",
            ),
            (
                "https://www.ebi.ac.uk/ena/browser/view/PRJNA172563?show=reads",
                "ena:PRJNA172563",
            ),
            ("geo:GSE1000", "geo:GSE1000"),
            ("geo:gsm2666939", "geo:GSM2666939"),
            (
                "https://www.ncbi.nlm.nih.gov/geo/query/acc.cgi?acc=GPL96",
                "geo:GPL96",
            ),
            (
                "https://ftp.ncbi.nlm.nih.gov/geo/series/GSE1nnn/GSE1000/",
                "geo:GSE1000",
            ),
        ] {
            assert_eq!(parse(text).reference.to_string(), canonical, "{text}");
        }
    }

    /// a container is written with its account, and a pasted link to a container or a folder
    /// in one is read as that. a link to one blob stays a link, as one to an S3 object does.
    #[test]
    fn azure_containers_and_their_links_find_their_source() {
        for (text, canonical) in [
            (
                "azure:azureopendatastorage/mnist/",
                "azure:azureopendatastorage/mnist/",
            ),
            (
                "azure:AzureOpenDataStorage/mnist",
                "azure:azureopendatastorage/mnist/",
            ),
            (
                "azure:azureopendatastorage/mnist/t10k-labels",
                "azure:azureopendatastorage/mnist/t10k-labels",
            ),
            (
                "https://azureopendatastorage.blob.core.windows.net/mnist/",
                "azure:azureopendatastorage/mnist/",
            ),
            (
                "https://azureopendatastorage.blob.core.windows.net/mnist",
                "azure:azureopendatastorage/mnist/",
            ),
            (
                "https://azureopendatastorage.blob.core.windows.net/mnist/processedManifest/",
                "azure:azureopendatastorage/mnist/processedManifest/",
            ),
            (
                "https://azureopendatastorage.blob.core.windows.net/mnist/t10k-labels-idx1-ubyte.gz",
                "https://azureopendatastorage.blob.core.windows.net/mnist/t10k-labels-idx1-ubyte.gz",
            ),
        ] {
            assert_eq!(parse(text).reference.to_string(), canonical, "{text}");
        }
        for bad in [
            "azure:mnist",
            "azure:azureopendatastorage/",
            "azure:open_data/mnist/",
            "azure:azureopendatastorage/MNIST/",
            "azure:azureopendatastorage/mn--ist/",
        ] {
            let error = Reference::parse(bad).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{bad}");
        }
    }

    /// a dandiset is its number, at a version or the newest, and a pasted page of one, or of a
    /// folder in one, is read as that, the folder as what to select.
    #[test]
    fn dandisets_and_their_pages_find_their_source() {
        for (text, canonical, select) in [
            ("dandi:000027", "dandi:000027", &[][..]),
            ("DANDI:000027", "dandi:000027", &[]),
            (
                "dandi:000027@0.210831.2033",
                "dandi:000027@0.210831.2033",
                &[],
            ),
            ("dandi:000027@draft", "dandi:000027@draft", &[]),
            (
                "https://dandiarchive.org/dandiset/000027",
                "dandi:000027",
                &[],
            ),
            (
                "https://dandiarchive.org/dandiset/000027/0.210831.2033",
                "dandi:000027@0.210831.2033",
                &[],
            ),
            (
                "https://dandiarchive.org/dandiset/000027/draft/files?location=sub-RAT123%2F",
                "dandi:000027@draft",
                &["sub-RAT123/**"],
            ),
        ] {
            let parsed = parse(text);
            assert_eq!(parsed.reference.to_string(), canonical, "{text}");
            assert_eq!(parsed.select(), select, "{text}");
        }
        for bad in [
            "dandi:27",
            "dandi:0000271",
            "dandi:000027@",
            "dandi:000027@1.0",
        ] {
            let error = Reference::parse(bad).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{bad}");
        }
    }

    /// an openneuro dataset is its accession, at a snapshot or the newest, and a pasted page of
    /// it or of one of its snapshots, where its DOI leads, is read as that.
    #[test]
    fn openneuro_datasets_and_their_pages_find_their_source() {
        for (text, canonical) in [
            ("openneuro:ds000001", "openneuro:ds000001"),
            ("openneuro:DS000001@1.0.0", "openneuro:ds000001@1.0.0"),
            (
                "https://openneuro.org/datasets/ds000001",
                "openneuro:ds000001",
            ),
            (
                "https://openneuro.org/datasets/ds000001/versions/1.0.0",
                "openneuro:ds000001@1.0.0",
            ),
            (
                "https://openneuro.org/datasets/ds000001/versions/1.0.0/download",
                "openneuro:ds000001@1.0.0",
            ),
        ] {
            assert_eq!(parse(text).reference.to_string(), canonical, "{text}");
        }
        for bad in [
            "openneuro:ds1",
            "openneuro:ds000001@",
            "openneuro:ds000001@1.0\"0",
            "openneuro:sub000001",
        ] {
            let error = Reference::parse(bad).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{bad}");
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
            ("dryad:10.5061/DRYAD.J1FD7@2", "dryad:10.5061/dryad.j1fd7@2"),
            (
                "https://datadryad.org/dataset/doi:10.5061/dryad.j1fd7",
                "dryad:10.5061/dryad.j1fd7",
            ),
            (
                "https://datadryad.org/stash/dataset/doi:10.5061/dryad.j1fd7",
                "dryad:10.5061/dryad.j1fd7",
            ),
            ("mendeley:rscbjbr9sj@2", "mendeley:rscbjbr9sj@2"),
            (
                "https://data.mendeley.com/datasets/rscbjbr9sj/2",
                "mendeley:rscbjbr9sj@2",
            ),
            (
                "https://data.mendeley.com/datasets/rscbjbr9sj",
                "mendeley:rscbjbr9sj",
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
                error.message.contains("oken=***"),
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

    /// a bucket's folder is named by its path even when it holds one object, so folders whose
    /// objects share a name, as openalex's date folders' `part_0000.gz`, stay apart; a reference
    /// to one object is named after it.
    #[test]
    fn a_bucket_folder_is_named_by_its_path_not_its_one_object() {
        let holding_one = |text: &str| {
            let mut listing = single(&Url::parse("https://example.org/part_0000.gz").unwrap());
            listing.resolved = parse(text).reference;
            listing
        };
        for folder in [
            "s3://openalex/data/jsonl/works/updated_date=2016-06-24/",
            "gs://lab-data/",
            "s3:https://minio.lab.example:9000/scans/2026/",
        ] {
            assert!(!holding_one(folder).names_its_file(), "{folder}");
        }
        assert!(holding_one("s3://openalex/data/jsonl/works/part_0000.gz").names_its_file());
        assert_eq!(
            crate::name::reference_name("s3://openalex/data/jsonl/works/updated_date=2016-06-24/")
                .as_deref(),
            Some("updated_date-2016-06-24")
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
                total: None,
                region: None,
                keyed: None,
                authorized: None,
                drive: None,
                body: std::sync::Arc::new(b"not json".to_vec()),
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
