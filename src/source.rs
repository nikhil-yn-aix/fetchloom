//! sources: how a person names a dataset and how each source lists its files, saying what to
//! download and how to check it. file bodies are left to the downloader, but kaggle reads a
//! competition's smallest file to learn whether its rules were accepted.

pub(crate) mod access;
mod ahead;
mod arxiv;
mod bucket;
#[cfg(test)]
mod conformance;
mod dandi;
mod dataverse;
mod doi;
pub(crate) mod drive;
mod dropbox;
mod dryad;
mod ena;
mod figshare;
pub(crate) mod folder;
mod geo;
mod git;
mod github;
mod gitlab;
mod hf;
mod ia;
mod invenio;
pub(crate) mod kaggle;
pub(crate) mod list;
pub(crate) mod local;
mod mendeley;
mod openneuro;
mod osf;
#[cfg(test)]
pub(crate) mod recorded;
mod s2;
pub(crate) mod search;
pub(crate) mod share;

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use tracing::Instrument as _;
use url::Url;

use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::name;
use crate::net::{self, Api, Fetch, Response};

pub(crate) use git::Blob;
pub(crate) use git::is_unserved;
pub(crate) use github::PRIVATE_LFS;

/// about the memory one listed file takes while a run works, from its listing to its placing,
/// which sizes the most files a dataset may list from the memory a run may use.
pub(crate) const PER_FILE: u64 = 2 << 10;

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
    /// the pages of a listing of at most `most` files.
    pub(crate) fn most(most: usize) -> Pages<K> {
        Pages {
            asked: HashSet::new(),
            most,
            held: None,
            idle: 0,
        }
    }

    /// the pages of a listing of at most `most` files that follows each page's token, whose first
    /// page, which no token names, is asked for before the guard hears of the next.
    fn after_first(most: usize) -> Pages<K> {
        Pages {
            held: Some(0),
            ..Pages::most(most)
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

/// the error for `subject`, a dataset that lists more than the `most` files a run allows, with
/// `instead` saying how to need fewer.
pub(crate) fn too_many(subject: impl fmt::Display, most: usize, instead: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "{subject} lists more than {} files, the most this run lists for one dataset",
            crate::units::grouped(most as u64)
        ),
    )
    .help(format!(
        "{instead}, or raise the limit with `fetchloom settings set most_files <count>` or FETCHLOOM_MOST_FILES; each listed file takes about 2 KiB of memory"
    ))
}

/// the environment a source reads to tell whether its requests go signed in. unit tests read the
/// variables their own thread sets in `TEST_VARIABLES`, none unless one does, so a key saved on
/// the machine running them changes nothing.
fn env(name: &str) -> Option<OsString> {
    #[cfg(test)]
    {
        TEST_VARIABLES.with_borrow(|set| {
            set.iter()
                .find(|(set_name, _)| *set_name == name)
                .map(|(_, value)| OsString::from(value))
        })
    }
    #[cfg(not(test))]
    {
        crate::machine::env(name)
    }
}

#[cfg(test)]
thread_local! {
    /// the variables a unit test gives the sources on its own thread, as a key a run has.
    pub(crate) static TEST_VARIABLES: std::cell::RefCell<Vec<(&'static str, &'static str)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// where a dataset comes from, parsed once where it enters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Reference {
    Url(Url),
    Doi(doi::Doi),
    /// a record of an invenioRDM install, zenodo's among them.
    Invenio(invenio::Record),
    Dryad(dryad::Dataset),
    Mendeley(mendeley::Dataset),
    GitHub(github::Release),
    GitLab(gitlab::Release),
    /// a folder of a github or gitlab repository, at a branch, a tag or a commit.
    GitFolder(git::Folder),
    Ena(ena::Accession),
    Geo(geo::Accession),
    Dandi(dandi::Dandiset),
    OpenNeuro(openneuro::Dataset),
    Figshare(figshare::Article),
    Dataverse(dataverse::Dataset),
    Osf(osf::Node),
    HuggingFace(hf::Repo),
    /// an internet archive item, by its identifier.
    InternetArchive(ia::Item),
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
    /// the files a list names, a checksum list or a list of links, at a link or in the project.
    List(list::List),
    /// the files a web server lists in a folder and the folders inside it, by the folder's link.
    Folder(folder::Folder),
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
            Reference::Url(url) if url.host_str().and_then(crate::auth::azure::account_of).is_some() => error
                .help(
                    "keep the SAS out of the link: list the container in azure_containers and set AZURE_STORAGE_SAS_TOKEN to the SAS, or download the file once with `fetchloom get`",
                ),
            Reference::Url(url) if key == "key" => {
                error.help(signing_in(url).unwrap_or_else(|| without.to_owned()))
            }
            Reference::Url(_) | Reference::Dropbox(_) | Reference::List(_) => error.help(without),
            read => error.help(format!(
                "leave {key} out of the link: fetchloom reads {read} without it, with your saved key where its source asks for one"
            )),
        })
    }
}

/// how to read a link to a dataverse installation's api without the key it carries: a key saved
/// for the installation goes with each request to it.
fn signing_in(url: &Url) -> Option<String> {
    let host = crate::auth::installation(url)?;
    let command = crate::auth::site::key_command(&host, &crate::account::DATAVERSE_KEY);
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
    /// about how many bytes each file is, by its path, whose size a folder listing gives only
    /// rounded to a unit, as apache's `1.3M`: what plans its download, binding none of its bytes.
    /// none where the listing rounds no size.
    pub(crate) rounded: Option<HashMap<String, u64>>,
}

impl Listing {
    /// the title a generated name may come from. a hugging face repository is known by its id,
    /// such as `multi_nli`, more than by its card's title, and a release by its repository, as
    /// `ripgrep`, since its title is often its tag alone, so their names come from the reference.
    pub(crate) fn naming_title(&self) -> Option<&str> {
        match self.resolved {
            Reference::HuggingFace(_)
            | Reference::ArxivMonth(_)
            | Reference::S2Dataset(_)
            | Reference::GitHub(_)
            | Reference::GitLab(_)
            | Reference::GitFolder(_) => None,
            _ => self.title.as_deref(),
        }
    }

    /// whether a generated name may come from the one file the listing holds. a paper is known by
    /// its title, and its file only by an id, as `1706.03762v7.pdf`; an arXiv month and a
    /// semantic scholar dataset by their reference, and their files by ids and times; a dandiset
    /// by its title, its files by subject, as `sub-RAT123.nwb`; a bucket's folder, a repository's
    /// and a web server's by their path, since folders often hold objects of one name, as
    /// `part_0000.gz`.
    pub(crate) fn names_its_file(&self) -> bool {
        match &self.resolved {
            Reference::Arxiv(_)
            | Reference::ArxivMonth(_)
            | Reference::S2Paper(_)
            | Reference::S2Dataset(_)
            | Reference::Dandi(_)
            | Reference::List(_)
            | Reference::GitFolder(_)
            | Reference::Folder(_) => false,
            Reference::Bucket(prefix) => !prefix.names_a_folder(),
            _ => true,
        }
    }

    /// whether the file at `path` stays out of a dataset unless a select pattern keeps it: git's
    /// own files, which tell git, git-annex or a repository's host how to store, ignore or build
    /// the files and hold no data. that is the `.gitattributes` of a hugging face, openneuro,
    /// github or gitlab repository, the `.gitignore` of a github or gitlab one, and at its top
    /// folder its `.gitmodules`, `.gitlab-ci.yml` and what `.github/` and `.gitlab/` hold.
    pub(crate) fn kept_only_if_selected(&self, path: &str) -> bool {
        match &self.resolved {
            Reference::HuggingFace(_) | Reference::OpenNeuro(_) => {
                path.rsplit('/').next() == Some(".gitattributes")
            }
            Reference::GitFolder(folder) => {
                let name = path.rsplit('/').next();
                matches!(name, Some(".gitattributes" | ".gitignore"))
                    || folder.is_top()
                        && (matches!(path, ".gitmodules" | ".gitlab-ci.yml")
                            || path.starts_with(".github/")
                            || path.starts_with(".gitlab/"))
            }
            _ => false,
        }
    }

    /// the paths select and exclude can match, including entries without downloadable bytes.
    pub(crate) fn paths(&self) -> impl Iterator<Item = &str> {
        self.files
            .iter()
            .map(|file| file.path.as_str())
            .chain(self.unavailable.iter().map(|entry| entry.path.as_str()))
    }

    /// what the listing says of `remote`'s size that binds no download: a web server's folder
    /// listing states a size as the file was when it listed it, in bytes or rounded.
    pub(crate) fn listed(&self, remote: &Remote) -> crate::net::Listed {
        let about = || self.rounded.as_ref()?.get(&remote.path).copied();
        match (&self.resolved, remote.size) {
            (Reference::Folder(_), Some(_)) => crate::net::Listed::Bytes,
            (Reference::Folder(_), None) => {
                about().map_or(crate::net::Listed::No, crate::net::Listed::About)
            }
            _ => crate::net::Listed::No,
        }
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
    /// the file's blob in the git repository it is listed from, which a relock takes from the
    /// cache while an earlier download's bytes there are that blob.
    pub(crate) blob: Option<Blob>,
}

/// the prefixes [`Reference::parse`] reads before a `:`, which a guess at a typo picks from.
pub(crate) const SCHEMES: [&str; 26] = [
    "http",
    "https",
    "doi",
    "zenodo",
    "invenio",
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
    "ia",
    "kaggle",
    "arxiv",
    "s2",
    "drive",
    "s3",
    "gs",
    "azure",
    "list",
];

impl Reference {
    /// reads a reference as a person writes it: an id such as `zenodo:3608990`, a DOI, or a link
    /// pasted from a browser.
    pub(crate) fn parse(text: &str) -> Result<Parsed, Error> {
        let text = text.trim();
        // a link parser drops tabs and line breaks, so what is written and what is fetched could
        // differ, and data.lock's record of the entry could match another one
        if text.contains(char::is_control) {
            let text = crate::shown::mask_reference(text);
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
            "zenodo" => whole(Reference::Invenio(invenio::Record::zenodo(rest)?)),
            "invenio" => whole(Reference::Invenio(invenio::Record::parse(rest)?)),
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
            "ia" => whole(Reference::InternetArchive(ia::Item::parse(rest)?)),
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
            "list" => {
                let (list, secret) = list::List::parse(rest)?;
                Ok(Parsed {
                    reference: Reference::List(list),
                    file: None,
                    folder: None,
                    secret,
                })
            }
            "sftp" => Err(sftp(text)),
            other => Err(match site_named(other) {
                Some(prefix) => unknown(text).help(format!(
                    "did you mean {prefix}:{}?",
                    crate::shown::mask_reference(rest)
                )),
                None => unknown(text),
            }),
        }
    }

    /// the origin a person names by writing this reference, when it is no fixed public service. it
    /// alone may be on a private network: another port or scheme of its host is not named.
    pub(crate) fn written_origin(&self) -> Option<url::Origin> {
        match self {
            Reference::Url(url) => Some(url.origin()),
            Reference::Dataverse(dataset) => dataset.origin(),
            Reference::Invenio(record) => record.origin(),
            Reference::GitLab(release) => release.origin(),
            Reference::GitFolder(folder) => folder.origin(),
            Reference::Bucket(prefix) => prefix.written_origin(),
            Reference::List(list) => list.written_origin(),
            Reference::Folder(folder) => Some(folder.link().origin()),
            _ => None,
        }
    }

    /// the host of the origin this reference names, with its port when that is not https's own,
    /// as a key for it is saved: a host a person or a project's data.toml chose.
    pub(crate) fn chosen_host(&self) -> Option<String> {
        let url::Origin::Tuple(_, host, port) = self.written_origin()? else {
            return None;
        };
        Some(match port {
            443 => host.to_string(),
            port => format!("{host}:{port}"),
        })
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
            Reference::GitFolder(folder) => folder.is_at_commit(),
            _ => false,
        }
    }

    /// whether `before`, a reference as data.toml wrote it, names the dataset `now` names at a
    /// version `now` leaves out, as `kaggle:uciml/iris@2` does `kaggle:uciml/iris`. a source with
    /// versions writes one after `@`, while an `@` in a link or a bucket's key names other data.
    pub(crate) fn version_left_out(before: &str, now: &str) -> bool {
        if before == now {
            return false;
        }
        let (Ok(before), Ok(now)) = (Reference::parse(before), Reference::parse(now)) else {
            return false;
        };
        let versions = matches!(
            before.reference,
            Reference::Dryad(_)
                | Reference::Mendeley(_)
                | Reference::GitHub(_)
                | Reference::GitLab(_)
                | Reference::Dandi(_)
                | Reference::OpenNeuro(_)
                | Reference::Figshare(_)
                | Reference::Dataverse(_)
                | Reference::HuggingFace(_)
                | Reference::Kaggle(_)
                | Reference::Arxiv(_)
                | Reference::S2Dataset(_)
        );
        let named = before.reference.to_string();
        let version = named
            .strip_prefix(&now.reference.to_string())
            .and_then(|rest| rest.strip_prefix('@'));
        versions
            && std::mem::discriminant(&before.reference) == std::mem::discriminant(&now.reference)
            && (&before.file, &before.folder) == (&now.file, &now.folder)
            && version.is_some_and(|version| !version.is_empty())
    }

    /// the text a dataset's name is made from when no file or title names it: the reference
    /// itself, for a list its folder or its own name, for a folder its path.
    pub(crate) fn naming(&self) -> String {
        match self {
            Reference::List(list) => list.naming(),
            Reference::GitFolder(folder) => folder.naming(),
            Reference::Folder(folder) => folder.naming(),
            other => other.to_string(),
        }
    }

    /// this reference as a dataset that keeps the web page its link answers with, as `page`
    /// says, reads it: a folder's link is then the plain link it is.
    pub(crate) fn taking_page(self, page: bool) -> Reference {
        match self {
            Reference::Folder(folder) if page => Reference::Url(folder.link().clone()),
            other => other,
        }
    }

    /// the reference as messages and logs show it, a secret in a link masked.
    pub(crate) fn shown(&self) -> String {
        match self {
            Reference::Url(url) => crate::shown::Shown(url).to_string(),
            Reference::Folder(folder) => crate::shown::Shown(folder.link()).to_string(),
            Reference::List(list::List::Link(url)) => format!("list:{}", crate::shown::Shown(url)),
            other => crate::shown::mask_query(&other.to_string()).into_owned(),
        }
    }

    /// [`Reference::shown`] with a full commit cut to its first 7 digits, as git shows one; data.lock
    /// keeps it whole.
    pub(crate) fn shown_short(&self) -> String {
        if let Reference::GitFolder(folder) = self {
            return folder.shown_short();
        }
        let shown = self.shown();
        match shown.rsplit_once('@') {
            Some((head, revision)) if git::commit_id(revision).is_some() => {
                format!("{head}@{}", &revision[..7])
            }
            _ => shown,
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
        self.resolve_within(api, select, &[]).await
    }

    /// [`Reference::resolve_in`], listing no folder of a web server an `exclude` pattern leaves
    /// out whole.
    pub(crate) async fn resolve_within(
        &self,
        api: &impl Api,
        select: &[Glob],
        exclude: &[Glob],
    ) -> Result<Listing, Error> {
        let span = tracing::debug_span!("resolve", reference = %self.shown());
        let listing = async {
            match self {
                Reference::Doi(doi) => {
                    let scoped = access::Scoped::new(api, &access::DOI, None, self.shown());
                    let target = doi.locate(&scoped).await?;
                    let listing = match &target {
                        // a page another invenioRDM install made for the DOI, which only its
                        // answer for the record there can confirm
                        Reference::Url(url) => match invenio::Record::behind(url) {
                            Some((record, named)) => {
                                let scoped = access::Scoped::new(
                                    api,
                                    &access::INVENIO,
                                    record.origin(),
                                    target.shown(),
                                );
                                record
                                    .recognized(&scoped, named.as_deref())
                                    .await
                                    .and_then(|listing| listing.ok_or_else(|| doi.not_listed(url)))
                            }
                            None => Err(doi.not_listed(url)),
                        },
                        other => other.resolve_direct(api, select, exclude).await,
                    };
                    let mut listing = listing.map_err(|error| error.via(doi.to_string()))?;
                    listing.doi.get_or_insert_with(|| doi.name().to_owned());
                    Ok(listing)
                }
                other => other.resolve_direct(api, select, exclude).await,
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
        check(self, listing, api.most_files())
    }

    /// the version its source names as the newest of the record this reference pins one version
    /// of: zenodo's and other invenioRDM installs' records by their `versions/latest` answer, and
    /// dryad, figshare and dataverse datasets by what each lists without the version. none for a
    /// reference that pins no version of such a record.
    pub(crate) async fn newest_version(&self, api: &impl Api) -> Option<Result<Reference, Error>> {
        match self {
            Reference::Invenio(record) => {
                let api =
                    access::Scoped::new(api, self.access(), self.written_origin(), self.shown());
                Some(record.latest(&api).await.map(Reference::Invenio))
            }
            Reference::Dryad(_) | Reference::Figshare(_) | Reference::Dataverse(_) => {
                let named = self.to_string();
                let (base, _) = named.rsplit_once('@')?;
                if !Reference::version_left_out(&named, base) {
                    return None;
                }
                let base = Reference::parse(base).ok()?.reference;
                Some(base.resolve(api).await.map(|listing| listing.resolved))
            }
            _ => None,
        }
    }

    /// the web page where a person sees the dataset, for a source that has one per dataset, as
    /// a folder's listing is.
    pub(crate) fn page(&self) -> Option<String> {
        match self {
            Reference::Invenio(record) => Some(record.page()),
            Reference::HuggingFace(repo) => Some(repo.page()),
            Reference::Kaggle(dataset) => Some(dataset.page()),
            Reference::InternetArchive(item) => Some(item.page()),
            Reference::Folder(folder) => Some(folder.link().to_string()),
            _ => None,
        }
    }

    /// asks this reference's own source, which may ask only the origins its descriptor names. a
    /// link to a record's page on an invenioRDM install is that record once one request for it
    /// there says so, and otherwise the plain link or the folder it is.
    async fn resolve_direct(
        &self,
        api: &impl Api,
        select: &[Glob],
        exclude: &[Glob],
    ) -> Result<Listing, Error> {
        let page = match self {
            Reference::Url(url) => Some(url),
            Reference::Folder(folder) => Some(folder.link()),
            _ => None,
        };
        if let Some(url) = page
            && let Some((record, named)) = invenio::Record::behind(url)
        {
            let scoped = access::Scoped::new(api, &access::INVENIO, record.origin(), self.shown());
            if let Ok(Some(listing)) = record.recognized(&scoped, named.as_deref()).await {
                return Ok(listing);
            }
        }
        let api = &access::Scoped::new(api, self.access(), self.written_origin(), self.shown());
        match self {
            Reference::Url(url) => Ok(single(url)),
            Reference::Doi(doi) => Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{doi} leads to another DOI, which fetchloom does not follow"),
            )
            .help("open the DOI in a browser and use the dataset page or a direct file link")),
            Reference::Invenio(record) => record.resolve(api).await,
            Reference::Dryad(dataset) => dataset.resolve(api).await,
            Reference::Mendeley(dataset) => dataset.resolve(api).await,
            Reference::GitHub(release) => release.resolve(api).await,
            Reference::GitLab(release) => release.resolve(api).await,
            Reference::GitFolder(folder) => folder.resolve(api).await,
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
            Reference::InternetArchive(item) => item.resolve(api).await,
            Reference::Kaggle(dataset) => dataset.resolve(api).await,
            Reference::KaggleCompetition(competition) => competition.resolve(api).await,
            Reference::Drive(item) => item.resolve(api).await,
            Reference::Dropbox(link) => Ok(link.listing()),
            Reference::Bucket(prefix) => prefix.resolve(api).await,
            Reference::Arxiv(paper) => paper.resolve(api).await,
            Reference::ArxivMonth(month) => month.resolve(api).await,
            Reference::S2Paper(paper) => paper.resolve(api).await,
            Reference::S2Dataset(dataset) => dataset.resolve(api).await,
            Reference::List(list) => list.resolve(api).await,
            Reference::Folder(folder) => folder.resolve(api, select, exclude).await,
            Reference::Local => Err(local::unlisted()),
        }
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Reference::Url(url) => write!(f, "{url}"),
            Reference::Doi(doi) => write!(f, "{doi}"),
            Reference::Invenio(record) => write!(f, "{record}"),
            Reference::Dryad(dataset) => write!(f, "{dataset}"),
            Reference::Mendeley(dataset) => write!(f, "{dataset}"),
            Reference::GitHub(release) => write!(f, "{release}"),
            Reference::GitLab(release) => write!(f, "{release}"),
            Reference::GitFolder(folder) => write!(f, "{folder}"),
            Reference::Ena(accession) => write!(f, "{accession}"),
            Reference::Geo(accession) => write!(f, "{accession}"),
            Reference::Dandi(dandiset) => write!(f, "{dandiset}"),
            Reference::OpenNeuro(dataset) => write!(f, "{dataset}"),
            Reference::Figshare(article) => write!(f, "{article}"),
            Reference::Dataverse(dataset) => write!(f, "{dataset}"),
            Reference::Osf(node) => write!(f, "{node}"),
            Reference::HuggingFace(repo) => write!(f, "{repo}"),
            Reference::InternetArchive(item) => write!(f, "{item}"),
            Reference::Kaggle(dataset) => write!(f, "{dataset}"),
            Reference::KaggleCompetition(competition) => write!(f, "{competition}"),
            Reference::Drive(item) => write!(f, "{item}"),
            Reference::Dropbox(link) => write!(f, "{link}"),
            Reference::Bucket(prefix) => write!(f, "{prefix}"),
            Reference::Arxiv(paper) => write!(f, "{paper}"),
            Reference::ArxivMonth(month) => write!(f, "{month}"),
            Reference::S2Paper(paper) => write!(f, "{paper}"),
            Reference::S2Dataset(dataset) => write!(f, "{dataset}"),
            Reference::List(list) => write!(f, "{list}"),
            Reference::Folder(folder) => write!(f, "{folder}"),
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

/// a paper's first author's last name and its `year`, as `Vaswani et al. 2017`, without any
/// control character the server put in the name.
pub(crate) fn byline(authors: &[String], year: Option<&str>) -> Option<String> {
    let first = authors.first()?;
    let surname: String = first
        .rsplit(' ')
        .next()
        .unwrap_or(first)
        .chars()
        .filter(|c| !crate::name::is_control(*c))
        .collect();
    let who = if authors.len() > 1 {
        format!("{surname} et al.")
    } else {
        surname
    };
    let year =
        year.filter(|year| year.len() == 4 && year.bytes().all(|byte| byte.is_ascii_digit()));
    Some(match year {
        Some(year) => format!("{who} {year}"),
        None => who,
    })
}

/// what data.lock pins of the dataset at `resolved`, pinned whole with the files at `paths`, where
/// its source may list it in one query: an openneuro snapshot of at most 20,000 files; none for
/// any other.
pub(crate) fn one_query_pin<'a>(
    resolved: &str,
    paths: impl ExactSizeIterator<Item = &'a str>,
) -> Option<net::Pin> {
    openneuro::lists_in_one_query(resolved, paths.len()).then(|| net::Pin::of(paths))
}

/// what the sources say about requests to their hosts, for the network to follow.
pub(crate) fn rules() -> net::Rules {
    net::Rules {
        paced_as: access::paced_as,
        spacing: access::spacing,
        most_at_once: access::most_at_once,
        relink,
        link_expired,
        refused_download,
        lfs_help,
        drive_search: |net, root| {
            Box::pin(async move {
                let net = access::Scoped::new(net, &access::DRIVE, None, root.to_string());
                drive::search(&net, root).await
            })
        },
        drive_name: |net, id| {
            Box::pin(async move {
                let net = access::Scoped::new(net, &access::DRIVE, None, format!("drive:{id}"));
                Ok(drive::file(&net, id).await?.name)
            })
        },
        account_at: access::account_at,
        keyless_limit,
        accept: github::accept,
        keyless: github::keyless,
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
        expired: osf::expiry_check(url).or_else(|| s2::expiry_check(url)),
        ..Fetch::new(url.clone())
    }
}

/// a file of a source's dataset whose download link the source gives only for a while, as
/// semantic scholar's, by the stable link data.lock keeps: the request for a fresh link to it.
fn relink(url: &Url) -> Option<Box<dyn net::Relink>> {
    s2::relink(url)
        .filter(|relink| access::S2.api.holds(net::Relink::asked(relink), None))
        .map(|relink| Box::new(relink) as Box<dyn net::Relink>)
}

/// whether a signed link a source gave for a while expires before `now`, or too soon after it.
fn link_expired(link: &Url, now: std::time::SystemTime) -> bool {
    s2::link_expired(link, now)
}

/// whether the source behind `url` may send the file wrapped in a zip, `train.csv` as
/// `train.csv.zip`, as kaggle does with large competition files, so its listed size binds nothing.
pub(crate) fn may_wrap(url: &Url) -> bool {
    kaggle::may_wrap(url)
}

/// whether a download of `remote`, to be pinned at `path`, may give it another path: its server
/// names it, or its source may wrap it in a zip.
pub(crate) fn may_rename(remote: &Remote, path: &str) -> bool {
    remote.named_by_server || (may_wrap(&remote.url) && crate::archive::by_name(path).is_none())
}

impl Reference {
    /// what this reference's source may ask and link to, and the key it takes.
    pub(crate) fn access(&self) -> &'static access::Access {
        match self {
            Reference::Url(_) => &access::LINK,
            Reference::Doi(_) => &access::DOI,
            Reference::Invenio(record) if record.is_elsewhere() => &access::INVENIO,
            Reference::Invenio(_) => &access::ZENODO,
            Reference::Dryad(_) => &access::DRYAD,
            Reference::Mendeley(_) => &access::MENDELEY,
            Reference::GitHub(_) => &access::GITHUB,
            Reference::GitLab(release) if release.is_elsewhere() => &access::GITLAB_SERVER,
            Reference::GitLab(_) => &access::GITLAB,
            Reference::GitFolder(folder) => folder.access(),
            Reference::Ena(_) => &access::ENA,
            Reference::Geo(_) => &access::GEO,
            Reference::Dandi(_) => &access::DANDI,
            Reference::OpenNeuro(_) => &access::OPENNEURO,
            Reference::Figshare(_) => &access::FIGSHARE,
            Reference::Dataverse(_) => &access::DATAVERSE,
            Reference::Osf(_) => &access::OSF,
            Reference::HuggingFace(_) => &access::HUGGING_FACE,
            Reference::InternetArchive(_) => &access::INTERNET_ARCHIVE,
            Reference::Kaggle(_) | Reference::KaggleCompetition(_) => &access::KAGGLE,
            Reference::Drive(_) => &access::DRIVE,
            Reference::Dropbox(_) => &access::DROPBOX,
            Reference::Bucket(_) => &access::BUCKET,
            Reference::Arxiv(_) | Reference::ArxivMonth(_) => &access::ARXIV,
            Reference::S2Paper(_) | Reference::S2Dataset(_) => &access::S2,
            Reference::List(_) => &access::LIST,
            Reference::Folder(_) => &access::FOLDER,
            Reference::Local => &access::LOCAL,
        }
    }
}

/// the name requests to `host` are paced and counted under: the host, or for hosts whose source
/// limits them together, as arXiv's terms do its hosts, the name they share.
pub(crate) fn paced_as(host: &str) -> String {
    access::paced_as(host).map_or_else(|| host.to_owned(), str::to_owned)
}

/// a source's own error for a download it refused, or answered 404 for, when it says more than
/// the status.
pub(crate) fn refused_download(url: &Url, status: u16) -> Option<Error> {
    match status {
        401 => dryad::refused_download(url),
        403 => kaggle::refused_download(url).or_else(|| dryad::refused_download(url)),
        404 => git::unserved(url),
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

/// a link pasted from a browser, matched to the source that serves it by its host.
fn link(text: &str) -> Result<Parsed, Error> {
    let url = plain_url(text)?;
    let secret = crate::shown::secret(&url);
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
    if let Some(bucket) = hf::bucket(&url) {
        return bucket.map(|url| parsed(plain_link(url), None));
    }
    let routed = doi::Doi::from_link(&url)
        .map(|doi| parsed(Reference::Doi(doi), None))
        .or_else(|| {
            invenio::Record::from_link(&url)
                .map(|(record, file)| parsed(Reference::Invenio(record), file))
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
        })
        .or_else(|| {
            gitlab::Release::from_server_link(&url)
                .map(|release| parsed(Reference::GitLab(release), None))
        })
        .or_else(|| {
            git::Folder::from_server_link(&url)
                .map(|folder| parsed(Reference::GitFolder(folder), None))
        });
    let Some(routed) = routed else {
        if let Some(source) = access::page_of(&url) {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!(
                    "fetchloom does not know this kind of {source} link: {}",
                    crate::shown::Shown(&url)
                ),
            )
            .help(other_page_help(source)));
        }
        // a folder's link names the files its server lists in it
        let reference =
            folder::Folder::from_link(&url).map_or_else(|| plain_link(url), Reference::Folder);
        return Ok(parsed(reference, None));
    };
    Ok(routed)
}

/// a link no source reads: the file it names, or the files beside it a checksum list there names.
fn plain_link(url: Url) -> Reference {
    let url = raw_link(&url).unwrap_or(url);
    list::List::from_link(&url).map_or(Reference::Url(url), Reference::List)
}

/// `text` read as a plain link: one with a host and no user name or password, which would end up
/// in data.toml, data.lock and logs.
fn plain_url(text: &str) -> Result<Url, Error> {
    let url = Url::parse(text).map_err(|err| {
        Error::new(
            Kind::ReferenceInvalid,
            format!("\"{}\" is not a valid link: {err}", crate::shown::mask_query(text)),
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
            format!("\"{}\" names no host", crate::shown::mask_query(text)),
        )
        .help(
            "copy the complete link from your browser, including https:// and the website name",
        ));
    }
    Ok(url)
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
        .or_else(|| git::Folder::from_link(url).map(|folder| whole(Reference::GitFolder(folder))))
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
        .or_else(|| ia::Item::from_link(url).map(|item| whole(Reference::InternetArchive(item))))
}

/// repository file pages have a stable raw endpoint, unlike their HTML view: github's, and on
/// any gitlab server, a page its `/-/blob/` names.
fn raw_link(url: &Url) -> Option<Url> {
    let parts: Vec<_> = url.path_segments()?.collect();
    if parts.last().is_none_or(|part| part.is_empty()) {
        return None;
    }
    let mut raw = url.clone();
    if url.host_str()? == "github.com" {
        if parts.len() < 5 || !matches!(parts[2], "blob" | "raw") {
            return None;
        }
        raw.set_scheme("https").ok()?;
        raw.set_host(Some("raw.githubusercontent.com")).ok()?;
        raw.set_path(&format!(
            "/{}/{}/{}",
            parts[0],
            parts[1],
            parts[3..].join("/")
        ));
        return Some(raw);
    }
    let marker = parts.windows(2).position(|pair| pair == ["-", "blob"])?;
    if marker < 2 || parts.len() < marker + 4 {
        return None;
    }
    let mut parts = parts;
    parts[marker + 1] = "raw";
    raw.set_path(&format!("/{}", parts.join("/")));
    Some(raw)
}

/// a rate limit that stopped a request to `url` that went with no key, in the words of the source
/// whose host limits such requests: the key that lifts it.
fn keyless_limit(url: &Url, error: Error) -> Error {
    match github::keyless_limit(url) {
        Some(limited) => limited.detail(error.message),
        None => gitlab::keyless_limit(url, error),
    }
}

/// what to do about the git-lfs pointer `url` answered with: a private github repository's file
/// cannot be downloaded, and a raw repository link's is served at [`lfs_link`].
fn lfs_help(url: &Url) -> Option<String> {
    github::lfs_help(url).or_else(|| {
        lfs_link(url).map(|link| {
            format!("git-lfs keeps that file apart from the repository: use {link} instead")
        })
    })
}

/// the link that serves the file git-lfs keeps for a raw repository link: github's media host,
/// or the api of the gitlab server the link is on, asked for the file. it leaves out the link's
/// query, which may hold a token.
fn lfs_link(url: &Url) -> Option<String> {
    if url.host_str()? == "raw.githubusercontent.com" {
        return Some(format!(
            "https://media.githubusercontent.com/media{}",
            url.path()
        ));
    }
    let (project, rest) = url.path().trim_start_matches('/').split_once("/-/raw/")?;
    let (reference, path) = rest.split_once('/')?;
    let encoded = |text: &str| {
        let decoded = percent_decode_str(text).decode_utf8_lossy();
        utf8_percent_encode(&decoded, NON_ALPHANUMERIC).to_string()
    };
    Some(format!(
        "{}/api/v4/projects/{}/repository/files/{}/raw?ref={}&lfs=true",
        url.origin().ascii_serialization(),
        encoded(project),
        encoded(path),
        encoded(reference)
    ))
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
            blob: None,
        }],
        rounded: None,
    }
}

/// a listing is hostile input: every path must stay inside the dataset and appear once, every
/// file's link must point where its source's files come from, and it holds at most `most` files.
/// one of only its readme and git files is told, since its data has usually moved elsewhere. its
/// title loses the spaces around it.
fn check(reference: &Reference, mut listing: Listing, most: usize) -> Result<Listing, Error> {
    listing.title = listing
        .title
        .take()
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty());
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
    if count > most {
        return Err(too_many(
            reference,
            most,
            "select the folders you need with --select",
        ));
    }
    let from = listing.resolved.access();
    let written = listing.resolved.written_origin();
    if let Some(file) = listing
        .files
        .iter()
        .find(|file| !from.files.holds(&file.url, written.as_ref()))
    {
        return Err(invalid(
            &reference,
            &format!(
                "it links \"{}\" at {}, where {}'s files are not kept",
                file.path.escape_debug(),
                file.url.origin().ascii_serialization(),
                from.name
            ),
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

/// the error for a listing `subject` sent that fetchloom cannot read, for `reason`.
pub(crate) fn invalid(subject: impl fmt::Display, reason: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!("{subject} sent an invalid listing: {reason}"),
    )
    .help(crate::error::report_answer())
}

/// the error for a file or folder `reference` lists at `path`, a name fetchloom refuses
/// `because` of what it holds, which only the dataset's owner can change.
fn refused_name(reference: &str, path: &str, because: &str) -> Error {
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
    match response.status {
        200..=299 => response.json(),
        _ => Err(unsuccessful(reference, response)),
    }
}

/// the error for an answer about `reference` that is no success, worded as [`json`] words it for
/// every source.
pub(crate) fn unsuccessful(reference: &dyn fmt::Display, response: &Response) -> Error {
    let reference = reference.to_string();
    let reference = crate::shown::mask_query(&reference);
    match response.status {
        404 | 410 => Error::new(
            Kind::ReferenceUnresolved,
            format!("{reference} does not exist, or is not public"),
        )
        .help(UNKNOWN_HELP),
        401 | 403 => refused(net::host(&response.url), &reference),
        status => Error::new(
            Kind::NetworkStatus,
            format!(
                "{} answered {status} for {reference}",
                net::host(&response.url)
            ),
        ),
    }
}

/// a refusal from the source at `host`, naming the account that could lift it.
pub(crate) fn refused(host: &str, reference: &str) -> Error {
    let error = Error::new(
        Kind::AuthRequired,
        format!("{host} refused access to {reference}"),
    );
    match access::account_at(host).or_else(|| crate::account::Account::named_login(host)) {
        Some(account) => error
            .help(format!(
                "if the dataset is private or restricted, sign in with `{}`, then run the same command again",
                account.command()
            ))
            .needs(account, None),
        None => error.help("if the dataset is private or restricted, ask its owner for a public link"),
    }
}
/// sites people call by another name than their prefix, as `huggingface` for `hf`.
const SITE_NAMES: [(&str, &str); 3] = [
    ("huggingface", "hf"),
    ("semanticscholar", "s2"),
    ("googledrive", "drive"),
];

/// the prefix a person meant by `scheme` when it is a site's name, or one typo from one, as
/// `hugging-face` and `hugingface` are; letters alone count, whatever their case.
fn site_named(scheme: &str) -> Option<&'static str> {
    let word: String = scheme
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    let name = SITE_NAMES
        .iter()
        .map(|(name, _)| *name)
        .find(|name| *name == word)
        .or_else(|| crate::name::sole_closest(&word, SITE_NAMES.map(|(name, _)| name)))?;
    SITE_NAMES
        .iter()
        .find(|(site, _)| *site == name)
        .map(|(_, prefix)| *prefix)
}

fn unknown(text: &str) -> Error {
    let text = crate::shown::mask_reference(text);
    Error::new(Kind::ReferenceInvalid, format!("fetchloom cannot read \"{text}\" as a dataset"))
        .help("use a name to search for, the link of the dataset's page, such as https://huggingface.co/datasets/scikit-learn/iris, a DOI, or an id such as kaggle:uciml/iris")
}

/// the error for an sftp link, an account on a server rather than published data: a tool that
/// logs in there copies the files, and the copy is added by its path.
fn sftp(text: &str) -> Error {
    let text = crate::shown::mask_typed(text);
    Error::new(
        Kind::ReferenceInvalid,
        format!("fetchloom does not speak sftp, so it cannot fetch {text}"),
    )
    .help(sftp_help(None))
}

/// whether `text` is an sftp link, which [`Reference::parse`] refuses.
pub(crate) fn is_sftp(text: &str) -> bool {
    text.trim()
        .split_once(':')
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("sftp"))
}

/// how to get what an sftp link names: a copy made with a tool that speaks sftp, added by its
/// path, in place of `entry`, the dataset whose data.toml entry holds the link, if one does.
pub(crate) fn sftp_help(entry: Option<&str>) -> String {
    let copy = "copy the file or folder with sftp or rclone";
    match entry {
        None => format!(
            "{copy}, then add the copy with `{}`",
            crate::suggested::command("add <path>")
        ),
        Some(name) => format!(
            "{copy}, then put the copy in place of this entry: `{}`, then `{}`",
            crate::suggested::command(&format!("remove {name}")),
            crate::suggested::command(&format!("add <path> --as {name}"))
        ),
    }
}

/// the error for a malformed id after a known prefix, such as `zenodo:abc`.
fn malformed(text: &str, expected: &str) -> Error {
    // the prefix is one fetchloom reads and holds no login, so it shows as typed
    let text = match text.split_once(':') {
        Some((prefix, rest)) => format!("{prefix}:{}", crate::shown::mask_reference(rest)),
        None => crate::shown::mask_reference(text).into_owned(),
    };
    Error::new(
        Kind::ReferenceInvalid,
        format!("\"{text}\" is not a valid reference"),
    )
    .help(format!("write it as {expected}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a site's own name, or one typo from it, before the colon gets the prefix fetchloom reads.
    #[test]
    fn a_sites_name_as_a_prefix_suggests_its_own() {
        for (typed, meant) in [
            (
                "huggingface:datasets/scikit-learn/iris",
                "hf:datasets/scikit-learn/iris",
            ),
            (
                "hugingface:datasets/scikit-learn/iris",
                "hf:datasets/scikit-learn/iris",
            ),
            (
                "Hugging-Face:datasets/mnist/mnist",
                "hf:datasets/mnist/mnist",
            ),
            ("hugginface:org/data?token=abc", "hf:org/data?token=***"),
            ("semantic-scholar:CorpusID:1", "s2:CorpusID:1"),
        ] {
            let error = Reference::parse(typed).unwrap_err();
            assert_eq!(error.kind, Kind::ReferenceInvalid, "{typed}");
            assert_eq!(
                error.help.as_deref(),
                Some(format!("did you mean {meant}?").as_str()),
                "{typed}"
            );
        }
        let error = Reference::parse("lab:iris").unwrap_err();
        assert!(
            error
                .help
                .is_some_and(|help| help.starts_with("use a name to search for")),
            "a word near no site keeps the general help"
        );
    }

    /// a listing holds at most the files its run's network allows, here a test's few.
    #[test]
    fn a_listing_holds_at_most_the_files_its_network_allows() {
        let url = "https://zenodo.org/api/records/7793716";
        let many = || recorded::Recorded::default().fixture(url, "zenodo/many.json");
        let reference = Reference::parse("zenodo:7793716").unwrap().reference;
        let listed = recorded::block_on(reference.resolve(&many().most(1123))).unwrap();
        assert_eq!(listed.files.len(), 1123);
        let error = recorded::block_on(reference.resolve(&many().most(1122))).unwrap_err();
        assert_eq!(error.kind, Kind::SourceInvalid);
        assert!(
            error.message.contains("lists more than 1,122 files"),
            "{}",
            error.message
        );
    }

    /// an author's name a server gave leaves its control characters out of the byline, so a
    /// surname cannot steer the terminal that shows a search.
    #[test]
    fn a_byline_leaves_control_characters_out() {
        let authors = [
            "Ada Love\u{202e}lace\u{1b}]0;x\u{7}".to_owned(),
            "Charles Babbage".to_owned(),
        ];
        assert_eq!(
            byline(&authors, Some("1843")).as_deref(),
            Some("Lovelace]0;x et al. 1843")
        );
    }

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
            (
                "physionet.org",
                Account::Login("physionet.org".to_owned()),
                "fetchloom auth login physionet",
            ),
            (
                "urs.earthdata.nasa.gov",
                Account::Login("urs.earthdata.nasa.gov".to_owned()),
                "fetchloom auth login earthdata",
            ),
        ] {
            let error = refused(host, "x");
            assert_eq!(
                error.help.as_deref(),
                Some(format!("if the dataset is private or restricted, sign in with `{command}`, then run the same command again").as_str()),
                "{host}"
            );
            assert_eq!(error.need.map(|need| need.account), Some(account), "{host}");
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
                "leave key out of the link: sign in once with `fetchloom auth login demo.dataverse.org --header x-dataverse-key`, and fetchloom sends your key to demo.dataverse.org itself"
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

    /// a raw repository link's file is served by the site's own link to it, never repeating the
    /// query; another link has none.
    #[test]
    fn a_git_lfs_pointer_is_answered_with_the_link_of_its_file() {
        for (link, served) in [
            (
                "https://raw.githubusercontent.com/lab/scans/main/data/model.bin?token=abc",
                Some("https://media.githubusercontent.com/media/lab/scans/main/data/model.bin"),
            ),
            (
                "https://gitlab.com/lab/vision/scans/-/raw/v1.0/data/model%20one.bin",
                Some(
                    "https://gitlab.com/api/v4/projects/lab%2Fvision%2Fscans/repository/files/data%2Fmodel%20one%2Ebin/raw?ref=v1%2E0&lfs=true",
                ),
            ),
            (
                "https://code.videolan.org:8443/videolan/dav1d/-/raw/1.5.3/tests/big%20file.bin",
                Some(
                    "https://code.videolan.org:8443/api/v4/projects/videolan%2Fdav1d/repository/files/tests%2Fbig%20file%2Ebin/raw?ref=1%2E5%2E3&lfs=true",
                ),
            ),
            ("https://example.org/model.bin", None),
        ] {
            assert_eq!(
                lfs_link(&Url::parse(link).unwrap()).as_deref(),
                served,
                "{link}"
            );
        }
    }

    #[test]
    fn a_full_commit_is_shown_short_and_anything_else_as_it_is() {
        let full = "0bda0ce801be0fa2f464ff845a9d5ceae99aad7d";
        for (text, shown) in [
            (
                format!("hf:datasets/scikit-learn/iris@{full}"),
                "hf:datasets/scikit-learn/iris@0bda0ce",
            ),
            (
                format!("https://github.com/lab/survey/tree/{full}/data"),
                "https://github.com/lab/survey/tree/0bda0ce/data",
            ),
            (
                format!("https://gitlab.com/lab/tools/-/tree/{full}"),
                "https://gitlab.com/lab/tools/-/tree/0bda0ce",
            ),
        ] {
            assert_eq!(parse(&text).reference.shown_short(), shown);
        }
        for kept in [
            "zenodo:3608990",
            "figshare:34744689@1",
            "hf:datasets/a/b@main",
            "kaggle:uciml/iris@2",
            "https://github.com/lab/survey/tree/main/0bda0ce801be0fa2f464ff845a9d5ceae99aad7d",
        ] {
            assert_eq!(parse(kept).reference.shown_short(), kept);
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
            (
                "https://code.videolan.org/videolan/dav1d/-/blob/master/README.md",
                "https://code.videolan.org/videolan/dav1d/-/raw/master/README.md",
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
        assert!(
            matches!(
                parse("https://github.com/org/repo/tree/main").reference,
                Reference::GitFolder(_)
            ),
            "a folder's page names the folder, no file"
        );
        for unchanged in [
            "https://github.com/org/repo/blob/main",
            "https://github.com/org/repo/blob/main/",
            "https://github.com.example.org/org/repo/blob/main/data.csv",
            "https://gitlab.com/group/repo/-/blob/main",
            "https://code.videolan.org/videolan/dav1d/-/blob/master",
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
            let error = check(&reference, listing, crate::net::MOST_FILES).unwrap_err();
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

    /// a release is named after its repository, whose title is often its tag alone, and a
    /// dandiset after its title, not its one file, which is named by subject.
    #[test]
    fn a_release_and_a_dandiset_get_readable_names() {
        let listed = |text: &str, title: &str| {
            let mut listing =
                single(&Url::parse("https://example.org/sub-RAT123/sub-RAT123.nwb").unwrap());
            listing.resolved = parse(text).reference;
            listing.title = Some(title.to_owned());
            listing
        };
        for release in [
            "github:BurntSushi/ripgrep@14.1.0",
            "gitlab:gitlab-org/gitlab-runner@v19.3.6",
        ] {
            assert_eq!(listed(release, "14.1.0").naming_title(), None, "{release}");
        }
        assert_eq!(
            crate::name::reference_name("github:BurntSushi/ripgrep@14.1.0").as_deref(),
            Some("ripgrep")
        );
        let dandiset = listed("dandi:000027", "Test dataset for testing dandi-cli.");
        assert!(!dandiset.names_its_file());
        assert_eq!(
            dandiset
                .naming_title()
                .and_then(crate::name::title_name)
                .as_deref(),
            Some("test-dataset-for-testing")
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
                signed: None,
                endpoint: None,
                remaining: None,
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
            let result = check(&reference, listing, crate::net::MOST_FILES);
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
        assert!(
            check(
                &reference,
                listing(&["a/b.csv", "c.csv"]),
                crate::net::MOST_FILES
            )
            .is_ok()
        );
        for bad in [
            &["../x"][..],
            &["/etc/passwd"],
            &["a//b"],
            &["a\\b"],
            &["C:/x"],
            &["x", "x"],
        ] {
            assert_eq!(
                check(&reference, listing(bad), crate::net::MOST_FILES)
                    .unwrap_err()
                    .kind,
                Kind::SourceInvalid,
                "{bad:?}"
            );
        }
    }

    /// a repository's `.gitattributes` is git's or git-annex's, at hugging face and openneuro
    /// alike, and a github or gitlab repository's files of git's and its host's are theirs too; a
    /// file of such a name elsewhere is data like any other.
    #[test]
    fn a_gitattributes_file_is_kept_only_if_selected() {
        let url = Url::parse("https://example.org/x").unwrap();
        for (reference, kept_out) in [
            ("hf:datasets/scikit-learn/iris", true),
            ("openneuro:ds001037", true),
            ("zenodo:3608990", false),
        ] {
            let listing = Listing {
                resolved: parse(reference).reference,
                ..single(&url)
            };
            for path in [".gitattributes", "sub-01/.gitattributes"] {
                assert_eq!(
                    listing.kept_only_if_selected(path),
                    kept_out,
                    "{reference} {path}"
                );
            }
            assert!(!listing.kept_only_if_selected("README.md"), "{reference}");
        }
        // a repository's own files of git's, at any depth or at its top alone
        for (reference, kept_out, kept_in) in [
            (
                "https://github.com/lab/survey",
                &[
                    ".gitignore",
                    "data/.gitignore",
                    ".gitattributes",
                    ".gitmodules",
                    ".gitlab-ci.yml",
                    ".github/workflows/scrape.yml",
                    ".gitlab/issue_templates/bug.md",
                ][..],
                &["README.md", ".githubx/notes.txt", "data/.gitmodules"][..],
            ),
            (
                "https://gitlab.com/lab/group/survey/-/tree/main/data",
                &[".gitignore", "raw/.gitattributes"][..],
                &[".gitmodules", ".gitlab-ci.yml", ".github/notes.yml"][..],
            ),
        ] {
            let listing = Listing {
                resolved: parse(reference).reference,
                ..single(&url)
            };
            for path in kept_out {
                assert!(listing.kept_only_if_selected(path), "{reference} {path}");
            }
            for path in kept_in {
                assert!(!listing.kept_only_if_selected(path), "{reference} {path}");
            }
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
        let checked = check(
            &repository,
            listing(&[".gitattributes", "README.md"]),
            crate::net::MOST_FILES,
        )
        .unwrap();
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
                check(&repository, listing(fine), crate::net::MOST_FILES)
                    .unwrap()
                    .warnings
                    .is_empty(),
                "{fine:?}"
            );
        }
        let one_file = Reference::Url(Url::parse("https://example.org/README.md").unwrap());
        assert!(
            check(&one_file, listing(&["README.md"]), crate::net::MOST_FILES)
                .unwrap()
                .warnings
                .is_empty(),
            "a link to one readme asked for it"
        );
    }

    /// a title is kept as the source wrote it but for the spaces around it, as kaggle's
    /// trailing one, which data.lock would keep.
    #[test]
    fn a_title_is_kept_without_the_spaces_around_it() {
        let url = Url::parse("https://example.org/sentences.zip").unwrap();
        let reference = Reference::Url(url.clone());
        for (title, kept) in [
            (
                "Sentiment Labelled Sentences Data Set ",
                Some("Sentiment Labelled Sentences Data Set"),
            ),
            ("\tIris ", Some("Iris")),
            ("  ", None),
        ] {
            let mut listing = single(&url);
            listing.title = Some(title.to_owned());
            let checked = check(&reference, listing, crate::net::MOST_FILES).unwrap();
            assert_eq!(checked.title.as_deref(), kept, "{title:?}");
        }
    }
}
