//! a dataset of a semantic scholar release: its name and license from the release, which anyone
//! may read, and its files from the links semantic scholar gives a key alone, each for a while.
//! data.lock keeps a stable link to each file, the API's listing with the file's name after `#`,
//! and every download asks for a fresh signed link.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use url::Url;

use super::{API, answer, busy, sent_key};
use crate::auth::{Account, S2_KEYS};
use crate::error::{Error, Kind};
use crate::net::{Api, Response};
use crate::source::{Access, Listing, Reference, Remote};

/// how long a signed link must still last for a download to start on it.
const LEFT: Duration = Duration::from_secs(60);

/// a dataset of semantic scholar's, as `papers`, at a release, as `2026-11-10`, or at the newest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Dataset {
    name: String,
    release: Option<String>,
}

impl Dataset {
    /// reads `papers` or `papers@2026-11-10`. a name of hexadecimal digits alone is a paper's id
    /// mistyped, not a dataset's name.
    pub(super) fn parse(text: &str) -> Option<Dataset> {
        let (name, release) = match text.split_once('@') {
            Some((name, release)) => (name, Some(release)),
            None => (text, None),
        };
        let name = name.to_ascii_lowercase();
        let named = name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'));
        let id = name.bytes().all(|b| b.is_ascii_hexdigit());
        if !named || id || release.is_some_and(|release| !is_release(release)) {
            return None;
        }
        Some(Dataset {
            name,
            release: release.map(str::to_owned),
        })
    }

    /// a pasted link to a dataset of a release in semantic scholar's API, with the file a stable
    /// link of fetchloom's own names after `#`.
    pub(super) fn from_link(url: &Url) -> Option<(Dataset, Option<String>)> {
        let parts: Vec<&str> = url.path_segments()?.collect();
        let ["datasets", "v1", "release", release, "dataset", name] = parts.as_slice() else {
            return None;
        };
        let release = (*release != "latest").then_some(*release);
        let dataset = Dataset::parse(&match release {
            Some(release) => format!("{name}@{release}"),
            None => (*name).to_owned(),
        })?;
        Some((dataset, url.fragment().map(str::to_owned)))
    }

    /// lists the dataset's files at the release named, or at the newest: its name, description
    /// and license from the release, its files from the links the API gives a key.
    pub(in crate::source) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let asked = Url::parse(&format!(
            "{API}/datasets/v1/release/{}",
            self.release.as_deref().unwrap_or("latest")
        ))
        .map_err(|err| Error::new(Kind::ReferenceInvalid, format!("{self}: {err}")))?;
        let response = api.get(&asked).await.map_err(busy)?;
        if response.status == 404 {
            return Err(self.no_release());
        }
        let release: Release = answer(&self.to_string(), &response)?;
        let Some(described) = release.datasets.iter().find(|item| item.name == self.name) else {
            return Err(self.unknown(&release));
        };
        let pinned = Dataset {
            name: self.name.clone(),
            release: Some(release.release_id.clone()),
        };
        let listed = pinned.listed()?;
        let response = api.get(&listed).await.map_err(busy)?;
        let links: Links = pinned.links(&response)?;
        let mut files = Vec::with_capacity(links.files.len());
        for link in &links.files {
            let file = signed(link).and_then(|link| name(&link)).ok_or_else(|| {
                Error::new(
                    Kind::SourceInvalid,
                    format!("semantic scholar gave a link for {pinned} that is no file on S3"),
                )
                .help(crate::error::report_answer())
            })?;
            let mut url = listed.clone();
            url.set_fragment(Some(&file));
            files.push(Remote {
                path: file,
                size: None,
                sha256: None,
                md5: None,
                url,
                named_by_server: false,
                etag: None,
            });
        }
        Ok(Listing {
            within: Vec::new(),
            access: Access::Login,
            resolved: Reference::S2Dataset(pinned),
            title: described
                .description
                .lines()
                .next()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned),
            license: license(&described.readme),
            doi: None,
            files,
            unavailable: Vec::new(),
            warnings: Vec::new(),
        })
    }

    /// where the API lists the signed links of the dataset's files at its release.
    fn listed(&self) -> Result<Url, Error> {
        let release = self.release.as_deref().unwrap_or("latest");
        Url::parse(&format!(
            "{API}/datasets/v1/release/{release}/dataset/{}",
            self.name
        ))
        .map_err(|err| Error::new(Kind::ReferenceInvalid, format!("{self}: {err}")))
    }

    /// reads the API's answer of the dataset's signed links. without a key it is a refusal that
    /// says a key gives them.
    fn links(&self, response: &Response) -> Result<Links, Error> {
        if response.status == 401 && sent_key().is_none() {
            return Err(Error::new(
                Kind::AuthRequired,
                format!("semantic scholar gives the files of {self} only for an API key"),
            )
            .help(format!(
                "get a free key at {S2_KEYS}, then sign in with `fetchloom auth login s2`, or set S2_API_KEY"
            ))
            .needs(Account::S2, None));
        }
        answer(&format!("the files of {self}"), response)
    }

    /// the error for a release semantic scholar does not have.
    fn no_release(&self) -> Error {
        Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "semantic scholar has no release {}",
                self.release.as_deref().unwrap_or("latest")
            ),
        )
        .help(format!(
            "its releases are listed at {API}/datasets/v1/release/, newest last"
        ))
    }

    /// the error for a dataset the release does not hold, naming those it does.
    fn unknown(&self, release: &Release) -> Error {
        let names: Vec<&str> = release
            .datasets
            .iter()
            .map(|item| item.name.as_str())
            .collect();
        Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "semantic scholar's release {} has no dataset {}",
                release.release_id, self.name
            ),
        )
        .help(format!("it holds {}", crate::units::listed(&names, "and")))
    }
}

impl fmt::Display for Dataset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s2:{}", self.name)?;
        if let Some(release) = &self.release {
            write!(f, "@{release}")?;
        }
        Ok(())
    }
}

/// whether `text` is a release's id, a date as `2026-11-10`.
fn is_release(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(at, b)| match at {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
}

/// a link semantic scholar gives to a file: https, on S3.
fn signed(text: &str) -> Option<Url> {
    let url = Url::parse(text).ok()?;
    let on_s3 = url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|host| host.ends_with(".amazonaws.com"));
    on_s3.then_some(url)
}

/// a signed link's file name, the last part of its path, which stays the same while the
/// signature changes.
fn name(url: &Url) -> Option<String> {
    let last = url.path_segments()?.next_back()?;
    let name = percent_encoding::percent_decode_str(last)
        .decode_utf8()
        .ok()?
        .into_owned();
    (!name.is_empty() && crate::name::escapes(&name).is_none()).then_some(name)
}

/// the license a dataset's README states, in the words the license check knows.
fn license(readme: &str) -> Option<String> {
    if readme.contains("ODC-BY") {
        Some("ODC-By-1.0".to_owned())
    } else if readme.contains("Apache 2.0") {
        Some("Apache-2.0".to_owned())
    } else {
        None
    }
}

/// what semantic scholar says of a release.
#[derive(Deserialize)]
struct Release {
    release_id: String,
    datasets: Vec<Described>,
}

#[derive(Deserialize)]
struct Described {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "README", default)]
    readme: String,
}

/// the signed links the API gives a key for a dataset's files.
#[derive(Deserialize)]
struct Links {
    files: Vec<String>,
}

/// a file of a semantic scholar dataset, by its stable link in data.lock: the request that gives
/// a fresh signed link to it, and its name among them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Relink {
    dataset: Dataset,
    asked: Url,
    file: String,
}

impl Relink {
    /// the file a stable link names: the API's listing of a dataset at a release, with the file's
    /// name after `#`.
    pub(crate) fn of(url: &Url) -> Option<Relink> {
        if url.host_str() != Some(super::S2_HOST) {
            return None;
        }
        let (dataset, Some(file)) = Dataset::from_link(url)? else {
            return None;
        };
        dataset.release.as_ref()?;
        let mut asked = url.clone();
        asked.set_fragment(None);
        Some(Relink {
            dataset,
            asked,
            file,
        })
    }

    /// the signed link to the file, from the API's listing of the dataset's links, which `api`
    /// answers again from this run's memory until it is told to forget it.
    pub(crate) async fn link(&self, api: &impl Api) -> Result<Url, Error> {
        let response = api.get(&self.asked).await.map_err(busy)?;
        self.found(&response)
    }

    /// the signed link to the file among those the API's `response` gives.
    fn found(&self, response: &Response) -> Result<Url, Error> {
        let links = self.dataset.links(response).map_err(busy)?;
        links
            .files
            .iter()
            .filter_map(|link| signed(link))
            .find(|link| name(link).as_deref() == Some(self.file.as_str()))
            .ok_or_else(|| {
                Error::new(
                    Kind::ReferenceUnresolved,
                    format!(
                        "semantic scholar no longer lists {} in {}",
                        self.file, self.dataset
                    ),
                )
                .help(format!(
                    "pin the files it lists now with `fetchloom lock --upgrade`, or move to a newer release, as {}",
                    Dataset {
                        name: self.dataset.name.clone(),
                        release: None
                    }
                ))
            })
    }

    /// the error for a signed link that semantic scholar gave again once it had expired.
    pub(crate) fn stale(&self) -> Error {
        Error::new(
            Kind::NetworkStatus,
            format!(
                "semantic scholar gave an expired link to {} of {} again",
                self.file, self.dataset
            ),
        )
        .help("check this machine's clock, then run the same command again later")
    }
}

/// whether a signed link's `Expires`, in seconds since 1970, is past at `now`, or too near to
/// start a download on.
pub(crate) fn expired(link: &Url, now: SystemTime) -> bool {
    let Some(expires) = link
        .query_pairs()
        .find(|(name, _)| name == "Expires")
        .and_then(|(_, value)| value.parse::<u64>().ok())
    else {
        return false;
    };
    Duration::from_secs(expires) <= now.duration_since(UNIX_EPOCH).unwrap_or_default() + LEFT
}

/// whether S3's refusal of a signed link, its XML error document, says the link expired: its time
/// ran out, `AccessDenied` with `Request has expired`, or the session that signed it ended,
/// `ExpiredToken`.
pub(crate) fn expired_answer(text: &str) -> bool {
    (text.contains("<Code>AccessDenied</Code>")
        && text.contains("<Message>Request has expired</Message>"))
        || text.contains("<Code>ExpiredToken</Code>")
}
