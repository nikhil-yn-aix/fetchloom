//! where each source's requests may go: one descriptor per source names the origins its resolver
//! asks, where its file links may point, the key it takes and how its hosts are paced. the
//! network's tables read these, and [`Scoped`] holds a resolver to them.
//!
//! a key goes with every request to its host, so an answer that carried a key must be read only by
//! reviewed code: every reader here is an adapter in this crate (docs/dev/architecture/sources.md).

use std::future::Future;
use std::time::Duration;

use url::Url;

use crate::account::{self, Account, KeyShape};
use crate::error::{Error, Kind};
use crate::net::{Api, Extra, Response, Through};

/// what one source may ask and link to, and the key it takes.
pub(crate) struct Access {
    /// the source as messages name it, as `hugging face`.
    pub(crate) name: &'static str,
    /// where a resolver's requests may go.
    pub(crate) api: Origins,
    /// where each listed file's link may point.
    pub(crate) files: Origins,
    /// the key it takes, which goes to its api alone.
    pub(crate) key: Option<&'static KeyShape>,
    /// hosts of the source's own site besides its api and the hosts its key goes to, such as
    /// its pages and download hosts, whose refusal names its account.
    pub(crate) sites: &'static [&'static str],
    /// whether a link is to a page of the source's site that names no dataset, which downloading
    /// would give only as HTML.
    pub(crate) pages: fn(&Url) -> bool,
    pub(crate) pace: &'static [Pace],
}

/// a set of origins.
pub(crate) enum Origins {
    /// these origins alone, as `https://zenodo.org`; `https://*.ebi.ac.uk` stands for every host
    /// below `ebi.ac.uk` on https's own port.
    Fixed(&'static [&'static str]),
    /// the reference's own origin, as a dataverse installation's.
    Written,
    /// anything the network's own checks let through, as a plain link's.
    Public,
}

/// how requests to a source's hosts are spaced.
pub(crate) struct Pace {
    /// the host paced, and with `below`, every host under it, as `ncbi.nlm.nih.gov` covers
    /// `eutils.ncbi.nlm.nih.gov`.
    pub(crate) host: &'static str,
    pub(crate) below: bool,
    /// whether those hosts share one pace under `host`, as arXiv's terms ask of its hosts.
    pub(crate) together: bool,
    pub(crate) spacing: fn() -> Duration,
    pub(crate) at_once: Option<usize>,
}

impl Origins {
    /// whether `url` is inside these origins, `written` being the reference's own origin.
    pub(crate) fn holds(&self, url: &Url, written: Option<&url::Origin>) -> bool {
        match self {
            Origins::Public => true,
            Origins::Written => written.is_some_and(|origin| *origin == url.origin()),
            Origins::Fixed(origins) => origins.iter().any(|origin| fixed_holds(origin, url)),
        }
    }

    /// the hosts of the fixed origins, in order, without those a `*` stands for.
    pub(crate) fn hosts(&self) -> impl Iterator<Item = &'static str> {
        let origins = match self {
            Origins::Fixed(origins) => *origins,
            Origins::Written | Origins::Public => &[],
        };
        origins
            .iter()
            .filter_map(|origin| origin.strip_prefix("https://"))
            .filter(|host| !host.starts_with('*'))
    }
}

/// whether `url` is at `origin`, an `https://host` that may stand for the hosts below one.
fn fixed_holds(origin: &str, url: &Url) -> bool {
    let Some(host) = origin.strip_prefix("https://") else {
        return false;
    };
    let Some(asked) = url.host_str() else {
        return false;
    };
    if url.scheme() != "https" || url.port_or_known_default() != Some(443) {
        return false;
    }
    match host.strip_prefix('*') {
        Some(below) => asked.ends_with(below),
        None => asked == host,
    }
}

impl Access {
    /// whether a refusal from `host`, without a `www.` before it, is this source's: where its
    /// key goes, or its own site.
    fn answers_at(&self, host: &str) -> bool {
        self.key
            .is_some_and(|key| key.host == Some(host) || key.every_hop.contains(&host))
            || self.sites.contains(&host)
    }
}

fn no_page(_: &Url) -> bool {
    false
}

/// whether `url`'s host is one of `hosts`.
fn on(url: &Url, hosts: &[&str]) -> bool {
    url.host_str().is_some_and(|host| hosts.contains(&host))
}

/// a link fetchloom downloads as it is.
pub(crate) static LINK: Access = Access {
    name: "a link",
    api: Origins::Public,
    files: Origins::Public,
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

pub(crate) static DOI: Access = Access {
    name: "doi",
    // kaggle registers each dataset version under a short link that only redirects to it
    api: Origins::Fixed(&["https://doi.org", "https://www.kaggle.com"]),
    files: Origins::Public,
    key: None,
    sites: &[],
    pages: |url| on(url, &["doi.org", "www.doi.org", "dx.doi.org"]),
    pace: &[],
};

pub(crate) static ZENODO: Access = Access {
    name: "zenodo",
    api: Origins::Fixed(&["https://zenodo.org"]),
    files: Origins::Fixed(&["https://zenodo.org"]),
    key: Some(&account::ZENODO_KEY),
    sites: &[],
    pages: |url| on(url, &["zenodo.org", "www.zenodo.org"]),
    pace: &[],
};

/// an invenioRDM install other than zenodo.org, at the origin its reference names. its key is the
/// token `auth login <host> --token` saves for that host alone, which goes with every request to
/// it.
pub(crate) static INVENIO: Access = Access {
    name: "invenioRDM",
    api: Origins::Written,
    files: Origins::Written,
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

pub(crate) static DRYAD: Access = Access {
    name: "dryad",
    api: Origins::Fixed(&["https://datadryad.org"]),
    files: Origins::Fixed(&["https://datadryad.org"]),
    key: Some(&account::DRYAD_KEY),
    sites: &[],
    pages: |url| on(url, &["datadryad.org", "www.datadryad.org"]),
    pace: &[],
};

pub(crate) static MENDELEY: Access = Access {
    name: "mendeley data",
    api: Origins::Fixed(&["https://data.mendeley.com"]),
    files: Origins::Fixed(&["https://data.mendeley.com"]),
    key: None,
    sites: &[],
    pages: |url| on(url, &["data.mendeley.com"]) && !url.path().starts_with("/public-files/"),
    pace: &[],
};

pub(crate) static GITHUB: Access = Access {
    name: "github",
    // only the API, never github.com or the storage a download is sent on to
    api: Origins::Fixed(&["https://api.github.com"]),
    // a public release's files on github.com, a public folder's on raw.githubusercontent.com or
    // on github.com, which sends a file git-lfs keeps on to media.githubusercontent.com, and a
    // private repository's at the API, which takes the key and sends a release asset on to its
    // storage without it
    files: Origins::Fixed(&[
        "https://github.com",
        "https://raw.githubusercontent.com",
        "https://media.githubusercontent.com",
        "https://api.github.com",
    ]),
    key: Some(&account::GITHUB_KEY),
    sites: &[],
    pages: no_page,
    pace: &[],
};

pub(crate) static GITLAB: Access = Access {
    name: "gitlab",
    api: Origins::Fixed(&["https://gitlab.com"]),
    // a release links its files on any host
    files: Origins::Public,
    key: Some(&account::GITLAB_KEY),
    sites: &[],
    pages: no_page,
    pace: &[],
};

/// a gitlab server other than gitlab.com, at the origin its reference names. its key is the one
/// `auth login <host>` saves for that host alone, which goes with every request to it.
pub(crate) static GITLAB_SERVER: Access = Access {
    name: "gitlab",
    api: Origins::Written,
    // a release links its files on any host
    files: Origins::Public,
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

pub(crate) static ENA: Access = Access {
    name: "ena",
    api: Origins::Fixed(&["https://www.ebi.ac.uk"]),
    files: Origins::Fixed(&[
        "https://ftp.sra.ebi.ac.uk",
        "https://ftp.ebi.ac.uk",
        "https://ebi.ac.uk",
        "https://*.ebi.ac.uk",
    ]),
    key: None,
    sites: &[],
    pages: |url| {
        on(url, &["www.ebi.ac.uk", "ebi.ac.uk"])
            && ["/ena/browser/view/", "/ena/data/view/"]
                .iter()
                .any(|page| url.path().starts_with(page))
    },
    pace: &[],
};

pub(crate) static GEO: Access = Access {
    name: "geo",
    // search asks NCBI's e-utilities, a listing its file server
    api: Origins::Fixed(&[
        "https://ftp.ncbi.nlm.nih.gov",
        "https://eutils.ncbi.nlm.nih.gov",
    ]),
    files: Origins::Fixed(&["https://ftp.ncbi.nlm.nih.gov"]),
    key: None,
    sites: &[],
    pages: |url| {
        on(url, &["www.ncbi.nlm.nih.gov", "ncbi.nlm.nih.gov"]) && url.path().starts_with("/geo/")
    },
    pace: &[Pace {
        host: "ncbi.nlm.nih.gov",
        below: true,
        together: false,
        spacing: || super::geo::SPACING,
        at_once: None,
    }],
};

pub(crate) static DANDI: Access = Access {
    name: "dandi",
    api: Origins::Fixed(&["https://api.dandiarchive.org"]),
    files: Origins::Fixed(&[
        "https://dandiarchive.s3.amazonaws.com",
        "https://api.dandiarchive.org",
    ]),
    key: None,
    sites: &[],
    pages: |url| on(url, &["dandiarchive.org", "www.dandiarchive.org"]),
    pace: &[],
};

pub(crate) static OPENNEURO: Access = Access {
    name: "openneuro",
    api: Origins::Fixed(&["https://openneuro.org"]),
    files: Origins::Fixed(&["https://s3.amazonaws.com", "https://openneuro.org"]),
    key: None,
    sites: &[],
    pages: |url| {
        on(url, &["openneuro.org", "www.openneuro.org"]) && !url.path().starts_with("/crn/")
    },
    pace: &[],
};

pub(crate) static FIGSHARE: Access = Access {
    name: "figshare",
    api: Origins::Fixed(&["https://api.figshare.com"]),
    // an article's files download from wherever its answer says
    files: Origins::Public,
    key: Some(&account::FIGSHARE_KEY),
    sites: &["figshare.com", "ndownloader.figshare.com"],
    pages: |url| {
        url.host_str().is_some_and(super::figshare::is_portal)
            && !url.path().starts_with("/ndownloader/")
    },
    pace: &[],
};

pub(crate) static DATAVERSE: Access = Access {
    name: "dataverse",
    api: Origins::Written,
    files: Origins::Written,
    key: Some(&account::DATAVERSE_KEY),
    sites: &[],
    pages: no_page,
    pace: &[],
};

pub(crate) static OSF: Access = Access {
    name: "osf",
    api: Origins::Fixed(&["https://api.osf.io"]),
    files: Origins::Fixed(&["https://osf.io", "https://files.osf.io"]),
    key: Some(&account::OSF_KEY),
    sites: &[],
    pages: |url| on(url, &["osf.io", "www.osf.io"]),
    pace: &[],
};

pub(crate) static HUGGING_FACE: Access = Access {
    name: "hugging face",
    api: Origins::Fixed(&["https://huggingface.co"]),
    files: Origins::Fixed(&["https://huggingface.co"]),
    key: Some(&account::HUGGING_FACE_KEY),
    sites: &["hf.co"],
    pages: |url| on(url, &["huggingface.co", "www.huggingface.co", "hf.co"]),
    pace: &[],
};

pub(crate) static KAGGLE: Access = Access {
    name: "kaggle",
    api: Origins::Fixed(&["https://api.kaggle.com"]),
    files: Origins::Fixed(&["https://api.kaggle.com"]),
    key: Some(&account::KAGGLE_KEY),
    sites: &["kaggle.com"],
    pages: |url| on(url, &["kaggle.com", "www.kaggle.com"]),
    pace: &[],
};

pub(crate) static DRIVE: Access = Access {
    name: "google drive",
    api: Origins::Fixed(&["https://drive.google.com", "https://www.googleapis.com"]),
    files: Origins::Fixed(&["https://drive.google.com"]),
    key: None,
    sites: &[],
    pages: |url| on(url, &["drive.google.com", "docs.google.com"]),
    pace: &[],
};

pub(crate) static DROPBOX: Access = Access {
    name: "dropbox",
    api: Origins::Fixed(&[]),
    files: Origins::Fixed(&["https://dropbox.com", "https://www.dropbox.com"]),
    key: None,
    sites: &[],
    pages: |url| on(url, &["dropbox.com", "www.dropbox.com"]),
    pace: &[],
};

pub(crate) static INTERNET_ARCHIVE: Access = Access {
    name: "internet archive",
    api: Origins::Fixed(&["https://archive.org"]),
    // archive.org sends each download on to a server that holds the item
    files: Origins::Fixed(&["https://archive.org"]),
    key: None,
    sites: &[],
    pages: |url| {
        on(url, &["archive.org", "www.archive.org"]) && url.path().starts_with("/details/")
    },
    pace: &[],
};

/// S3, google cloud storage and azure: a bucket's host varies with its store, region and the
/// endpoint a person names, and its keys go only to the buckets the settings list.
pub(crate) static BUCKET: Access = Access {
    name: "cloud storage",
    api: Origins::Public,
    files: Origins::Public,
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

pub(crate) static ARXIV: Access = Access {
    name: "arXiv",
    api: Origins::Fixed(&[
        "https://export.arxiv.org",
        "https://oaipmh.arxiv.org",
        "https://storage.googleapis.com",
    ]),
    // arXiv's bucket holds most papers; the rest come from arXiv itself
    files: Origins::Fixed(&["https://storage.googleapis.com", "https://export.arxiv.org"]),
    key: None,
    sites: &[],
    pages: |url| on(url, &["arxiv.org", "www.arxiv.org", "export.arxiv.org"]),
    pace: &[Pace {
        host: super::arxiv::PACED_AS,
        below: true,
        together: true,
        spacing: || super::arxiv::SPACING,
        at_once: Some(1),
    }],
};

pub(crate) static S2: Access = Access {
    name: "semantic scholar",
    // a paper arXiv holds is read through arXiv's own resolver
    api: Origins::Fixed(&[
        "https://api.semanticscholar.org",
        "https://export.arxiv.org",
        "https://oaipmh.arxiv.org",
        "https://storage.googleapis.com",
    ]),
    // a paper's pdf is wherever its answer links, a dataset's files on S3 with a signature
    files: Origins::Public,
    // the API alone: the links it gives to its files on S3 carry their own signature
    key: Some(&account::S2_KEY),
    sites: &[],
    pages: |url| {
        on(
            url,
            &[
                "www.semanticscholar.org",
                "semanticscholar.org",
                "api.semanticscholar.org",
            ],
        )
    },
    pace: &[Pace {
        host: "api.semanticscholar.org",
        below: false,
        together: false,
        spacing: super::s2::spacing_now,
        at_once: Some(1),
    }],
};

/// a list of files, at its link or in the project: its own link and the links it gives are plain
/// links, trusted no more than any other, and no key goes with them for the list's sake.
pub(crate) static LIST: Access = Access {
    name: "a list",
    api: Origins::Written,
    files: Origins::Public,
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

/// a folder a web server lists, at its link: its listings and its files on the folder's own
/// origin alone, and no key goes with them for the folder's sake.
pub(crate) static FOLDER: Access = Access {
    name: "a folder listing",
    api: Origins::Written,
    files: Origins::Written,
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

/// files imported from a machine, which no source lists.
pub(crate) static LOCAL: Access = Access {
    name: "imported files",
    api: Origins::Fixed(&[]),
    files: Origins::Fixed(&[]),
    key: None,
    sites: &[],
    pages: no_page,
    pace: &[],
};

/// every source, in the order a link's page is matched to one.
pub(super) static ALL: [&Access; 27] = [
    &ZENODO,
    &INVENIO,
    &DRYAD,
    &MENDELEY,
    &GEO,
    &DANDI,
    &OPENNEURO,
    &ENA,
    &FIGSHARE,
    &OSF,
    &HUGGING_FACE,
    &INTERNET_ARCHIVE,
    &KAGGLE,
    &DRIVE,
    &DROPBOX,
    &DOI,
    &ARXIV,
    &S2,
    &GITHUB,
    &GITLAB,
    &GITLAB_SERVER,
    &DATAVERSE,
    &BUCKET,
    &LINK,
    &LIST,
    &FOLDER,
    &LOCAL,
];

/// the source whose page `url` is, as its name: a link to it names no dataset.
pub(crate) fn page_of(url: &Url) -> Option<&'static str> {
    ALL.iter()
        .find(|access| (access.pages)(url))
        .map(|access| access.name)
}

/// the descriptor of the source `account` signs in to, for a source's own account.
pub(crate) fn of_account(account: &Account) -> Option<&'static Access> {
    Some(match account {
        Account::HuggingFace => &HUGGING_FACE,
        Account::Kaggle => &KAGGLE,
        Account::Zenodo => &ZENODO,
        Account::Figshare => &FIGSHARE,
        Account::Osf => &OSF,
        Account::Dryad => &DRYAD,
        Account::GitHub => &GITHUB,
        Account::GitLab => &GITLAB,
        Account::S2 => &S2,
        Account::Login(_) => return None,
    })
}

/// the descriptor of the source whose key `key` is.
pub(crate) fn of_key(key: &KeyShape) -> Option<&'static Access> {
    ALL.into_iter()
        .find(|access| access.key.is_some_and(|own| std::ptr::eq(own, key)))
}

/// the account whose source answered from `host`, for a refusal that names no account.
pub(crate) fn account_at(host: &str) -> Option<Account> {
    let host = host.to_ascii_lowercase();
    let bare = host.trim_start_matches("www.");
    Account::SOURCES
        .iter()
        .find(|account| of_account(account).is_some_and(|access| access.answers_at(bare)))
        .cloned()
}

/// the pace whose hosts take `host`.
fn pace_of(host: &str) -> Option<&'static Pace> {
    ALL.iter().flat_map(|access| access.pace).find(|pace| {
        host == pace.host
            || pace.below
                && host
                    .strip_suffix(pace.host)
                    .is_some_and(|before| before.ends_with('.'))
    })
}

/// the name requests to `host` are paced and counted under, when its source paces it together
/// with other hosts, as arXiv's terms ask of its hosts.
pub(crate) fn paced_as(host: &str) -> Option<&'static str> {
    pace_of(host)
        .filter(|pace| pace.together)
        .map(|pace| pace.host)
}

/// the pace of requests paced as `name`: a host, or the name hosts paced together share.
fn named(name: &str) -> Option<&'static Pace> {
    pace_of(name).filter(|pace| !pace.together || name == pace.host)
}

/// how far apart requests paced as `name` start, for a source that asks for that.
pub(crate) fn spacing(name: &str) -> Option<Duration> {
    named(name).map(|pace| (pace.spacing)())
}

/// how many requests paced as `name` may run at once, for a source that asks for fewer than the
/// network allows.
pub(crate) fn most_at_once(name: &str) -> Option<usize> {
    named(name).and_then(|pace| pace.at_once)
}

/// a source's network as its resolver sees it: a request to an origin its descriptor does not
/// name is refused before it goes out.
pub(crate) struct Scoped<'a, A> {
    api: &'a A,
    access: &'static Access,
    /// the reference's own origin, which a written origin stands for.
    written: Option<url::Origin>,
    /// the reference as messages name it.
    subject: String,
}

impl<'a, A: Api> Scoped<'a, A> {
    /// `api` held to `access`, for the reference `subject` names, whose own origin is `written`.
    pub(crate) fn new(
        api: &'a A,
        access: &'static Access,
        written: Option<url::Origin>,
        subject: String,
    ) -> Scoped<'a, A> {
        Scoped {
            api,
            access,
            written,
            subject,
        }
    }

    /// refuses `url` when the descriptor does not name its origin.
    fn allows(&self, url: &Url) -> Result<(), Error> {
        if self.access.api.holds(url, self.written.as_ref()) {
            return Ok(());
        }
        Err(Error::new(
            Kind::NetworkRefused,
            format!(
                "fetchloom will not ask {} for {}, since it is not one of {}'s own addresses",
                url.origin().ascii_serialization(),
                self.subject,
                self.access.name
            ),
        )
        .help(crate::error::report_answer()))
    }
}

impl<A: Api> Api for Scoped<'_, A> {
    fn get(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send {
        let allowed = self.allows(url);
        async move {
            allowed?;
            self.api.get(url).await
        }
    }

    fn get_with(
        &self,
        url: &Url,
        headers: Extra,
    ) -> impl Future<Output = Result<Response, Error>> + Send {
        let allowed = self.allows(url);
        async move {
            allowed?;
            self.api.get_with(url, headers).await
        }
    }

    fn get_once(&self, url: &Url) -> impl Future<Output = Result<Response, Error>> + Send {
        let allowed = self.allows(url);
        async move {
            allowed?;
            self.api.get_once(url).await
        }
    }

    fn get_through(
        &self,
        url: &Url,
        through: &mut impl Through,
    ) -> impl Future<Output = Result<Response, Error>> + Send {
        let allowed = self.allows(url);
        async move {
            allowed?;
            self.api.get_through(url, through).await
        }
    }

    fn most_files(&self) -> usize {
        self.api.most_files()
    }

    fn listing_window(&self, url: &Url) -> usize {
        self.api.listing_window(url)
    }

    fn drive_refused<'b>(
        &'b self,
        id: &'b str,
    ) -> impl Future<Output = Result<crate::net::Private, Error>> + Send + 'b {
        self.api.drive_refused(id)
    }
}

#[cfg(test)]
mod tests;
