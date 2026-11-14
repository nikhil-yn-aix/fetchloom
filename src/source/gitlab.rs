//! gitlab releases: a gitlab.com release's links, at a tag or the newest, each downloaded from its
//! own address. gitlab states no size or hash, so the lock's SHA-256 names the bytes; source
//! archives are left out, and a key goes to gitlab.com only.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::{Access, Listing, Reference, Remote, Unavailable, malformed};
use crate::account::Account;
use crate::error::{Error, Kind};
use crate::net::Api;

const SITE: &str = "https://gitlab.com";

/// a release of a gitlab.com project, at a tag or the newest one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    /// the project's path below its groups, as `gitlab-org/gitlab-runner`.
    project: String,
    tag: Option<String>,
}

impl Release {
    /// reads the part after `gitlab:`: a project's path, with `@` and a tag to name a release.
    pub(super) fn parse(text: &str) -> Result<Release, Error> {
        let invalid = || {
            malformed(
                &format!("gitlab:{text}"),
                "gitlab:gitlab-org/gitlab-runner@v19.3.6",
            )
        };
        let text = text.trim();
        let (project, tag) = match text.split_once('@') {
            Some((project, tag)) => (project, Some(tag_name(tag).ok_or_else(invalid)?)),
            None => (text, None),
        };
        let parts: Vec<&str> = project.split('/').collect();
        Release::named(&parts, tag).ok_or_else(invalid)
    }

    /// a pasted link to a release's page on gitlab.com, or to the newest release's permalink.
    pub(super) fn from_link(url: &Url) -> Option<Release> {
        if url.host_str()? != "gitlab.com" {
            return None;
        }
        let parts: Vec<&str> = url.path_segments()?.collect();
        let marker = parts.iter().position(|part| *part == "-")?;
        let (project, rest) = (&parts[..marker], &parts[marker + 1..]);
        match rest {
            ["releases", "permalink", "latest"] => Release::named(project, None),
            ["releases", tag @ ..]
                if !tag.is_empty() && tag.iter().all(|part| !part.is_empty()) =>
            {
                let joined = tag.join("/");
                let tag = percent_decode_str(&joined).decode_utf8().ok()?;
                Release::named(project, Some(tag_name(&tag)?))
            }
            _ => None,
        }
    }

    /// a project's path from its parts: a group or more, then the project, each made of letters,
    /// digits, `_`, `-` and `.`.
    fn named(parts: &[&str], tag: Option<String>) -> Option<Release> {
        let valid = parts.len() >= 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 255
                    && !part.starts_with(['-', '.'])
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            });
        valid.then(|| Release {
            project: parts.join("/"),
            tag,
        })
    }

    /// lists the release's links: at the tag, or of the newest release, which pins its tag.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let url = match &self.tag {
            Some(tag) => self.url(&["releases", tag])?,
            None => self.url(&["releases", "permalink", "latest"])?,
        };
        let response = api.get(&url).await?;
        if response.status == 404 {
            let signed_in = Account::GitLab
                .find(&super::env)
                .is_some_and(|found| Account::GitLab.sendable(&found));
            return Err(if response.text().contains("Project Not Found") {
                self.hidden(signed_in)
            } else {
                self.no_release()
            });
        }
        let answer: Answer = super::json(self, &response)?;
        if answer.tag_name.is_empty() {
            return Err(self.invalid("the release names no tag"));
        }
        if self.tag.as_ref().is_some_and(|tag| *tag != answer.tag_name) {
            return Err(self.invalid("the answer names another tag"));
        }
        let assets = answer.assets;
        if assets.count > assets.links.len() + assets.sources.len() {
            return Err(self.invalid(&format!(
                "it lists {} of the {} it counts",
                assets.links.len() + assets.sources.len(),
                crate::units::count(assets.count as u64, "asset", "assets")
            )));
        }
        if assets.links.len() > super::most_files() {
            return Err(super::too_many(
                self,
                "download the files you need from the release's page one at a time",
            ));
        }
        let mut files = Vec::new();
        let mut unavailable = Vec::new();
        for link in assets.links {
            let Some(direct) = link.direct_asset_url else {
                unavailable.push(Unavailable {
                    path: link.name,
                    reason: "gitlab gives no path in the release for it".to_owned(),
                });
                continue;
            };
            let path = self.path(&direct)?;
            match address(&link.url) {
                Ok(url) => files.push(Remote {
                    path,
                    size: None,
                    sha256: None,
                    md5: None,
                    url,
                    named_by_server: false,
                    etag: None,
                }),
                Err(reason) => unavailable.push(Unavailable {
                    path,
                    reason: reason.to_owned(),
                }),
            }
        }
        Ok(Listing {
            within: Vec::new(),
            access: Access::Unknown,
            unavailable,
            warnings: Vec::new(),
            resolved: Reference::GitLab(Release {
                project: self.project.clone(),
                tag: Some(answer.tag_name),
            }),
            title: answer.name.filter(|name| !name.trim().is_empty()),
            license: None,
            doi: None,
            files,
        })
    }

    /// the path the release gives a link's file, after `/downloads/` in its download address on
    /// gitlab.com.
    fn path(&self, direct: &str) -> Result<String, Error> {
        let invalid = || {
            self.invalid(&format!(
                "a download link is not this release's on gitlab.com: {}",
                crate::net::mask_query(direct)
            ))
        };
        let url = Url::parse(direct)
            .ok()
            .filter(|url| {
                url.scheme() == "https"
                    && url.host_str() == Some("gitlab.com")
                    && url.port().is_none()
                    && url.query().is_none()
            })
            .ok_or_else(invalid)?;
        let prefix = format!("/{}/-/releases/", self.project.to_ascii_lowercase());
        let path = url
            .path()
            .to_ascii_lowercase()
            .starts_with(&prefix)
            .then(|| &url.path()[prefix.len()..])
            .and_then(|rest| rest.split_once("/downloads/"))
            .map(|(_, file)| file)
            .filter(|file| !file.is_empty())
            .ok_or_else(invalid)?;
        Ok(percent_decode_str(path)
            .decode_utf8()
            .map_err(|_| invalid())?
            .into_owned())
    }

    fn no_release(&self) -> Error {
        let message = match &self.tag {
            Some(tag) => format!("{} has no release at the tag {tag}", self.page()),
            None => format!("{} has no release", self.page()),
        };
        Error::new(Kind::ReferenceUnresolved, message).help(format!(
            "check the releases at {}/-/releases; a tag without a release has no files to list",
            self.page()
        ))
    }

    /// gitlab's answer that it shows no project of this path, which may be private.
    fn hidden(&self, signed_in: bool) -> Error {
        let page = self.page();
        if signed_in {
            return Error::new(
                Kind::AuthRequired,
                format!(
                    "gitlab.com has no project {}, or this key cannot see it",
                    self.project
                ),
            )
            .help(format!("check the path at {page}"))
            .unseen();
        }
        Error::new(
            Kind::AuthRequired,
            format!("gitlab.com has no project {}, or it is private", self.project),
        )
        .help(format!(
            "check the path at {page}. a private project's releases need a key that can read it: set GITLAB_TOKEN, or sign in with `fetchloom auth login gitlab`"
        ))
        .needs_or_missing(Account::GitLab, page)
    }

    /// the project's URL in gitlab's API, its path one encoded segment, with `more` after it.
    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        let mut url = Url::parse(&format!("{SITE}/api/v4/projects"))
            .map_err(|_| self.invalid("the API URL is invalid"))?;
        url.path_segments_mut()
            .map_err(|()| self.invalid("the API URL has no path"))?
            .push(&self.project)
            .extend(more);
        Ok(url)
    }

    fn page(&self) -> String {
        format!("{SITE}/{}", self.project)
    }

    fn invalid(&self, reason: &str) -> Error {
        Error::new(
            Kind::SourceInvalid,
            format!("{self} sent an invalid listing: {reason}"),
        )
        .help(crate::error::report_answer())
    }
}

/// where a link's file is downloaded from: the link's own address, since its gitlab.com address
/// answers a program with a warning page for a link to another host. an address that is not
/// https, or that holds a secret, is not kept.
fn address(link: &str) -> Result<Url, &'static str> {
    let url = Url::parse(link).map_err(|_| "its address is not a link")?;
    if url.scheme() != "https" {
        return Err("its address is not https");
    }
    if !url.username().is_empty() || url.password().is_some() || crate::net::secret(&url).is_some()
    {
        return Err("its address holds a secret, which fetchloom does not keep");
    }
    Ok(url)
}

/// a tag as git allows one, never empty and without whitespace or control characters.
fn tag_name(text: &str) -> Option<String> {
    let valid = !text.is_empty()
        && text.len() <= 255
        && !text.chars().any(|c| c.is_whitespace() || c.is_control());
    valid.then(|| text.to_owned())
}

#[derive(Deserialize)]
struct Answer {
    tag_name: String,
    name: Option<String>,
    assets: Assets,
}

#[derive(Deserialize)]
struct Assets {
    count: usize,
    #[serde(default)]
    sources: Vec<serde_json::Value>,
    #[serde(default)]
    links: Vec<Link>,
}

#[derive(Deserialize)]
struct Link {
    name: String,
    url: String,
    direct_asset_url: Option<String>,
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "gitlab:{}", self.project)?;
        if let Some(tag) = &self.tag {
            write!(f, "@{tag}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
