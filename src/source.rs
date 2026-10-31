//! sources: where datasets live, how a person names one, and how each lists its files. a source
//! never downloads file bodies. it says what to download and how to check it.

mod dataverse;
mod doi;
mod drive;
mod dropbox;
mod figshare;
mod hf;
mod kaggle;
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

use crate::digest::{Md5, Sha256};
use crate::error::{Error, Kind};
use crate::name;
use crate::net::{self, Api, Credential, Fetch, Response};

/// the most files one dataset may list, so `data.lock` stays small and memory stays bounded.
pub(crate) const MOST_FILES: usize = 100_000;

/// reads the environment, so tests can give each run its own.
pub(crate) type Var<'a> = &'a dyn Fn(&str) -> Option<OsString>;

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
    Drive(drive::Item),
    Dropbox(dropbox::Link),
}

/// a reference, and the one file inside it when a pasted link pointed at a single file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub(crate) reference: Reference,
    pub(crate) file: Option<String>,
}

/// what a source says a dataset holds, at one exact version.
#[derive(Clone, Debug)]
pub(crate) struct Listing {
    /// the reference to resolve again, pinned to a version when the source provides one.
    pub(crate) resolved: Reference,
    pub(crate) title: Option<String>,
    pub(crate) license: Option<String>,
    pub(crate) doi: Option<String>,
    pub(crate) files: Vec<Remote>,
    pub(crate) unavailable: Vec<Unavailable>,
}

impl Listing {
    /// the paths select and exclude can match, including entries without downloadable bytes.
    pub(crate) fn paths(&self) -> impl Iterator<Item = &str> {
        self.files
            .iter()
            .map(|file| file.path.as_str())
            .chain(self.unavailable.iter().map(|entry| entry.path.as_str()))
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
        let whole = |reference| {
            Ok(Parsed {
                reference,
                file: None,
            })
        };
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
            "kaggle" => whole(Reference::Kaggle(kaggle::Dataset::parse(rest)?)),
            "drive" => whole(Reference::Drive(drive::Item::parse(rest)?)),
            _ => Err(unknown(text)),
        }
    }

    /// the host a person names by writing this reference, when it is not one of the fixed public
    /// services. such a host may be on a private network.
    pub(crate) fn written_host(&self) -> Option<String> {
        match self {
            Reference::Url(url) => url.host_str().map(str::to_owned),
            Reference::Dataverse(dataset) => Some(dataset.host_name().to_owned()),
            _ => None,
        }
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
        let Reference::Url(url) = self else {
            return Ok(());
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
        let span = tracing::debug_span!("resolve", reference = %self.shown());
        let listing = async {
            match self {
                Reference::Doi(doi) => {
                    let target = doi.locate(api).await?;
                    let mut listing = target.resolve_direct(api).await?;
                    listing.doi.get_or_insert_with(|| doi.name().to_owned());
                    Ok(listing)
                }
                other => other.resolve_direct(api).await,
            }
        }
        .instrument(span)
        .await?;
        check(self, listing)
    }

    async fn resolve_direct(&self, api: &impl Api) -> Result<Listing, Error> {
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
            Reference::HuggingFace(repo) => repo.resolve(api).await,
            Reference::Kaggle(dataset) => dataset.resolve(api).await,
            Reference::Drive(item) => item.resolve(api).await,
            Reference::Dropbox(link) => Ok(link.listing()),
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
            Reference::Drive(item) => write!(f, "{item}"),
            Reference::Dropbox(link) => write!(f, "{link}"),
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

/// every credential found where its source keeps it.
pub(crate) fn credentials(var: Var<'_>) -> Vec<Credential> {
    let mut found = Vec::new();
    found.extend(zenodo::credentials(var));
    found.extend(figshare::credentials(var));
    found.extend(osf::credentials(var));
    found.extend(hf::credentials(var));
    found.extend(kaggle::credentials(var));
    found
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
        .help("remove the user name and password from the link. for a private dataset, set its source's token, such as HF_TOKEN"));
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
    let routed = doi::Doi::from_link(&url)
        .map(|doi| (Reference::Doi(doi), None))
        .or_else(|| {
            zenodo::Record::from_link(&url).map(|(record, file)| (Reference::Zenodo(record), file))
        })
        .or_else(|| {
            figshare::Article::from_link(&url).map(|article| (Reference::Figshare(article), None))
        })
        .or_else(|| {
            dataverse::Dataset::from_link(&url).map(|dataset| (Reference::Dataverse(dataset), None))
        })
        .or_else(|| osf::Node::from_link(&url).map(|node| (Reference::Osf(node), None)))
        .or_else(|| {
            hf::Repo::from_link(&url).map(|(repo, file)| (Reference::HuggingFace(repo), file))
        })
        .or_else(|| {
            kaggle::Dataset::from_link(&url)
                .map(|(dataset, file)| (Reference::Kaggle(dataset), file))
        })
        .or_else(|| drive::Item::from_link(&url).map(|item| (Reference::Drive(item), None)))
        .or_else(|| dropbox::Link::from_link(&url).map(|link| (Reference::Dropbox(link), None)));
    let Some((reference, file)) = routed else {
        if let Some(competition) = kaggle::competition_link(&url) {
            return Err(competition);
        }
        if let Some(source) = known_host(&url) {
            return Err(Error::new(
                Kind::ReferenceInvalid,
                format!(
                    "fetchloom does not know this kind of {source} link: {}",
                    net::Shown(&url)
                ),
            )
            .help("paste the link of the dataset's own page, or its id, as in `fetchloom add zenodo:3608990`"));
        }
        return Ok(Parsed {
            reference: Reference::Url(raw_link(&url).unwrap_or(url)),
            file: None,
        });
    };
    Ok(Parsed { reference, file })
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
        "figshare.com" => "figshare",
        "osf.io" | "www.osf.io" => "osf",
        "huggingface.co" | "www.huggingface.co" | "hf.co" => "hugging face",
        "kaggle.com" | "www.kaggle.com" => "kaggle",
        "drive.google.com" | "docs.google.com" => "google drive",
        "dropbox.com" | "www.dropbox.com" => "dropbox",
        _ => return None,
    })
}

/// a plain link: one file, named by its last path segment until the server names it.
fn single(url: &Url) -> Listing {
    Listing {
        unavailable: Vec::new(),
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
/// twice.
fn check(reference: &Reference, listing: Listing) -> Result<Listing, Error> {
    let reference = reference.shown();
    let count = listing.files.len() + listing.unavailable.len();
    if count > MOST_FILES {
        return Err(Error::new(Kind::SourceInvalid, format!("{reference} lists {count} entries, more than the {MOST_FILES} fetchloom handles in one dataset"))
            .help("ask the dataset's authors to pack its files into archives, or pass the link of each file you need to `fetchloom add`"));
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
        404 | 410 => Err(Error::new(Kind::ReferenceUnresolved, format!("{reference} does not exist, or is not public"))
            .help("check the id in a browser, then fix it on the command line or as ref in data.toml. a dataset that was removed cannot be fetched")),
        401 | 403 => Err(Error::new(Kind::AuthRequired, format!("{} refused access to {reference}", net::host(&response.url)))
            .help(format!("if the dataset is private or restricted, {}, then run the same command again", token_for(net::host(&response.url))))),
        status => Err(Error::new(Kind::NetworkStatus, format!("{} answered {status} for {reference}", net::host(&response.url)))),
    }
}

/// how to give a token to the source at `host`, in the variable fetchloom reads for it.
fn token_for(host: &str) -> &'static str {
    let host = host.trim_start_matches("www.").trim_start_matches("api.");
    match host {
        "zenodo.org" => "set ZENODO_TOKEN",
        "figshare.com" => "set FIGSHARE_TOKEN",
        "osf.io" => "set OSF_TOKEN",
        "huggingface.co" | "hf.co" => "set HF_TOKEN",
        "kaggle.com" => "set KAGGLE_API_TOKEN",
        _ => "set its source's token, such as HF_TOKEN or ZENODO_TOKEN",
    }
}

fn unknown(text: &str) -> Error {
    let text = net::mask_query(text);
    Error::new(Kind::ReferenceInvalid, format!("fetchloom cannot read \"{text}\" as a dataset"))
        .help("use a link, a DOI, or an id such as hf:datasets/org/name or kaggle:owner/name, as in `fetchloom add zenodo:3608990`")
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
    fn a_refusal_names_the_token_its_source_reads() {
        assert_eq!(token_for("zenodo.org"), "set ZENODO_TOKEN");
        assert_eq!(token_for("api.figshare.com"), "set FIGSHARE_TOKEN");
        assert_eq!(token_for("api.osf.io"), "set OSF_TOKEN");
        assert_eq!(token_for("huggingface.co"), "set HF_TOKEN");
        assert_eq!(token_for("www.kaggle.com"), "set KAGGLE_API_TOKEN");
        assert!(token_for("example.org").contains("such as HF_TOKEN"));
    }

    fn parse(text: &str) -> Parsed {
        Reference::parse(text).unwrap()
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
    fn response_errors_mask_query_secrets() {
        let url = Url::parse("https://example.org/data?token=abc").unwrap();
        let reference = Reference::Url(url.clone());
        for status in [404, 403, 500, 200] {
            let response = Response {
                status,
                url: url.clone(),
                next: None,
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
            unavailable: Vec::new(),
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
}
