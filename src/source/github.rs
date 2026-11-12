//! github releases: the files a release of a repository holds, at a tag or the newest release,
//! with the SHA-256 github states for each. a tag can move and a release's files can change, so
//! the lock's SHA-256 is what names the bytes. a key goes to api.github.com only, never to
//! github.com or the storage a download is sent on to, so a private repository's files are not
//! downloaded.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::{Access, Listing, Reference, Remote, Unavailable, malformed};
use crate::auth::Account;
use crate::digest::Sha256;
use crate::error::{Error, Kind};
use crate::net::{Api, Response};

const API: &str = "https://api.github.com";

/// a release of a github repository, at a tag or the newest one github calls the latest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    owner: String,
    repo: String,
    tag: Option<String>,
}

impl Release {
    /// reads the part after `github:`: `<owner>/<repo>`, with `@` and a tag to name a release.
    pub(super) fn parse(text: &str) -> Result<Release, Error> {
        let invalid = || malformed(&format!("github:{text}"), "github:jqlang/jq@jq-1.8.2");
        let text = text.trim();
        let (name, tag) = match text.split_once('@') {
            Some((name, tag)) => (name, Some(tag_name(tag).ok_or_else(invalid)?)),
            None => (text, None),
        };
        let (owner, repo) = name.split_once('/').ok_or_else(invalid)?;
        Release::named(owner, repo, tag).ok_or_else(invalid)
    }

    /// a pasted link to a release's page on github.com, or to the latest release.
    pub(super) fn from_link(url: &Url) -> Option<Release> {
        if !matches!(url.host_str()?, "github.com" | "www.github.com") {
            return None;
        }
        let parts: Vec<&str> = url.path_segments()?.collect();
        match parts.as_slice() {
            [owner, repo, "releases", "latest"] => Release::named(owner, repo, None),
            [owner, repo, "releases", "tag", tag @ ..] if !tag.is_empty() => {
                let joined = tag.join("/");
                let tag = percent_decode_str(&joined).decode_utf8().ok()?;
                Release::named(owner, repo, Some(tag_name(&tag)?))
            }
            _ => None,
        }
    }

    fn named(owner: &str, repo: &str, tag: Option<String>) -> Option<Release> {
        let owner_valid = (1..=39).contains(&owner.len())
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let repo_valid = (1..=100).contains(&repo.len())
            && !matches!(repo, "." | "..")
            && repo
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        (owner_valid && repo_valid).then(|| Release {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
            tag,
        })
    }

    /// lists the release's files: at the tag, or of the release github calls the latest, which
    /// pins its tag.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let signed_in = Account::GitHub
            .find(&super::env)
            .is_some_and(|found| Account::GitHub.sendable(&found));
        let url = match &self.tag {
            Some(tag) => self.url(&["releases", "tags", tag])?,
            None => self.url(&["releases", "latest"])?,
        };
        let response = self.get(api, &url, signed_in).await?;
        if response.status == 404 {
            return Err(self.missing(api, signed_in).await);
        }
        let answer: Answer = super::json(self, &response)?;
        if answer.tag_name.is_empty() {
            return Err(self.invalid("the release names no tag"));
        }
        if self.tag.as_ref().is_some_and(|tag| *tag != answer.tag_name) {
            return Err(self.invalid("the answer names another tag"));
        }
        if answer.assets.len() > super::most_files() {
            return Err(super::too_many(
                self,
                "download the files you need from the release's page one at a time",
            ));
        }
        let mut files = Vec::new();
        let mut unavailable = Vec::new();
        for asset in answer.assets {
            if asset.state != "uploaded" {
                unavailable.push(Unavailable {
                    path: asset.name,
                    reason: format!("github has not finished uploading it ({})", asset.state),
                });
                continue;
            }
            files.push(asset.remote(self)?);
        }
        Ok(Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable,
            warnings: Vec::new(),
            resolved: Reference::GitHub(Release {
                owner: self.owner.clone(),
                repo: self.repo.clone(),
                tag: Some(answer.tag_name),
            }),
            title: answer.name.filter(|name| !name.trim().is_empty()),
            license: None,
            doi: None,
            files,
        })
    }

    /// github's answer for `url`, or the error for its rate limit when the answer is that.
    async fn get(&self, api: &impl Api, url: &Url, signed_in: bool) -> Result<Response, Error> {
        let response = api.get(url).await.map_err(|error| {
            if error.kind == Kind::NetworkRateLimited {
                limited(signed_in).detail(error.message.clone())
            } else {
                error
            }
        })?;
        if says_limited(&response) {
            return Err(limited(signed_in));
        }
        Ok(response)
    }

    /// the error for a release github does not show, which it says alike of a release that is
    /// missing and of a repository that is private or missing. the repository's own answer tells
    /// them apart.
    async fn missing(&self, api: &impl Api, signed_in: bool) -> Error {
        let repository = match self.url(&[]) {
            Ok(url) => self.get(api, &url, signed_in).await,
            Err(error) => Err(error),
        };
        match repository {
            Ok(response) if (200..300).contains(&response.status) => {
                let message = match &self.tag {
                    Some(tag) => format!("{} has no release at the tag {tag}", self.page()),
                    None => format!("{} has no published release", self.page()),
                };
                Error::new(Kind::ReferenceUnresolved, message).help(format!(
                    "check the releases at {}/releases; a tag without a release has no files to list",
                    self.page()
                ))
            }
            Ok(_) => self.hidden(signed_in),
            Err(error) => error,
        }
    }

    /// github's answer that it shows no repository of this name, which may be private.
    fn hidden(&self, signed_in: bool) -> Error {
        let name = format!("{}/{}", self.owner, self.repo);
        let page = self.page();
        if signed_in {
            return Error::new(
                Kind::AuthRequired,
                format!("github has no repository {name}, or this key cannot see it"),
            )
            .help(format!("check the name at {page}"))
            .unseen();
        }
        Error::new(
            Kind::AuthRequired,
            format!("github has no repository {name}, or it is private"),
        )
        .help(format!(
            "check the name at {page}. a private repository's releases need a key that can read it: set GH_TOKEN or GITHUB_TOKEN, or sign in with `fetchloom auth login github`"
        ))
        .needs_or_missing(Account::GitHub, page)
    }

    /// the repository's URL in github's API, with `more` path segments after it.
    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("{API}/repos"))
            .map_err(|_| self.invalid("the API URL is invalid"))?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the API URL has no path"))?
            .push(&self.owner)
            .push(&self.repo)
            .extend(more);
        Ok(url)
    }

    fn page(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.repo)
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help("check the release on github.com and report the invalid listing at https://github.com/nikhil-yn-aix/fetchloom/issues")
    }
}

/// a tag as git allows one, never empty and without whitespace or control characters.
fn tag_name(text: &str) -> Option<String> {
    let valid = !text.is_empty()
        && text.len() <= 255
        && !text.chars().any(|c| c.is_whitespace() || c.is_control());
    valid.then(|| text.to_owned())
}

/// whether github's answer is its rate limit, a 403 or 429 whose message says so.
fn says_limited(response: &Response) -> bool {
    matches!(response.status, 403 | 429)
        && response.text().to_ascii_lowercase().contains("rate limit")
}

/// the error for github's rate limit used up: 60 requests an hour without a key, which a key
/// raises to 5,000.
fn limited(signed_in: bool) -> Error {
    if signed_in {
        return Error::new(
            Kind::NetworkRateLimited,
            "github's rate limit for this key is used up",
        )
        .help(
            "run the same command again later; github's hourly limit starts over within the hour",
        );
    }
    Error::new(
        Kind::NetworkRateLimited,
        "github allows 60 requests an hour without a key, and they are used up",
    )
    .help("set GH_TOKEN or GITHUB_TOKEN to a github token, or sign in with `fetchloom auth login github`, which raises the limit to 5,000 an hour; or run the same command again within the hour")
    .needs(Account::GitHub, None)
}

#[derive(Deserialize)]
struct Answer {
    tag_name: String,
    name: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    size: Option<u64>,
    state: String,
    digest: Option<String>,
    browser_download_url: String,
}

impl Asset {
    fn remote(self, release: &Release) -> Result<Remote, Error> {
        let quoted = self.name.escape_debug().to_string();
        let url = Url::parse(&self.browser_download_url)
            .ok()
            .filter(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("github.com")
                    && url.port().is_none()
                    && url.query().is_none()
                    && url.path().contains("/releases/download/")
            })
            .ok_or_else(|| {
                release.invalid(&format!(
                    "the download link of \"{quoted}\" is not on github.com"
                ))
            })?;
        let sha256 = match self.digest.as_deref() {
            None => None,
            Some(digest) => match digest.split_once(':') {
                Some(("sha256", hex)) => Some(Sha256::from_hex(hex).ok_or_else(|| {
                    release.invalid(&format!("the SHA-256 of \"{quoted}\" is not hex"))
                })?),
                // another algorithm checks nothing fetchloom keeps, and the bytes pin the file
                Some(_) => None,
                None => {
                    return Err(
                        release.invalid(&format!("the digest of \"{quoted}\" names no algorithm"))
                    );
                }
            },
        };
        Ok(Remote {
            path: self.name,
            size: self.size,
            sha256,
            md5: None,
            url,
            named_by_server: false,
            etag: None,
        })
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "github:{}/{}", self.owner, self.repo)?;
        if let Some(tag) = &self.tag {
            write!(f, "@{tag}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
