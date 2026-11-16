//! gitlab: the links of a release on gitlab.com or another gitlab server, at a tag or the newest,
//! each downloaded from its own address, and the commits and trees a folder of a project is listed
//! from. gitlab states no size or hash, so the lock's SHA-256 names the bytes; source archives are
//! left out. a key goes to its own server only: gitlab.com's from `GITLAB_TOKEN` or `auth login
//! gitlab`, another server's from `auth login <host>`, which saves it for that host alone.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::git::{Entry, Held, Listed};
use super::{Access, Listing, Reference, Remote, Unavailable, invalid, malformed};
use crate::account::Account;
use crate::error::{Error, Kind};
use crate::net::Api;

const GITLAB_COM: &str = "gitlab.com";

/// a gitlab project, on gitlab.com or another server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Project {
    /// another server's host, with its port when that is not 443; none for gitlab.com.
    server: Option<String>,
    /// the project's path below its groups, as `gitlab-org/gitlab-runner`.
    path: String,
}

/// a release of a gitlab project, at a tag or the newest one, on gitlab.com or another server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    project: Project,
    tag: Option<String>,
}

impl Release {
    /// reads the part after `gitlab:`: a project's path on gitlab.com, or after `//` another
    /// server's host and a project's path there, with `@` and a tag to name a release. no path
    /// on gitlab.com starts with `/`, so the marked form is never read as a group's.
    pub(super) fn parse(text: &str) -> Result<Release, Error> {
        let invalid = || {
            malformed(
                &format!("gitlab:{text}"),
                "gitlab:gitlab-org/gitlab-runner@v19.3.6, or gitlab://<host>/<group>/<project>@<tag> for another gitlab server",
            )
        };
        let text = text.trim();
        let (server, path) = match text.strip_prefix("//") {
            Some(rest) => {
                let (host, path) = rest.split_once('/').ok_or_else(invalid)?;
                (Some(server(host).ok_or_else(invalid)?), path)
            }
            None => (None, text),
        };
        let (project, tag) = match path.split_once('@') {
            Some((project, tag)) => (project, Some(tag_name(tag).ok_or_else(invalid)?)),
            None => (path, None),
        };
        let parts: Vec<&str> = project.split('/').collect();
        Release::named(server, &parts, tag).ok_or_else(invalid)
    }

    /// a pasted link to a release's page on gitlab.com, or to the newest release's permalink.
    pub(super) fn from_link(url: &Url) -> Option<Release> {
        if url.host_str()? != GITLAB_COM {
            return None;
        }
        Release::from_page(None, url)
    }

    /// a pasted link to a release's page on another gitlab server, over https, known by gitlab's
    /// `/-/releases/` in its path alone. it is read after every other source's links.
    pub(super) fn from_server_link(url: &Url) -> Option<Release> {
        Release::from_page(Some(server_of(url)?), url)
    }

    /// the release a page of `url`'s project names: `/-/releases/<tag>` or the newest release's
    /// permalink.
    fn from_page(server: Option<String>, url: &Url) -> Option<Release> {
        let parts: Vec<&str> = url.path_segments()?.collect();
        let marker = parts.iter().position(|part| *part == "-")?;
        let (project, rest) = (&parts[..marker], &parts[marker + 1..]);
        match rest {
            ["releases", "permalink", "latest"] => Release::named(server, project, None),
            ["releases", tag @ ..]
                if !tag.is_empty() && tag.iter().all(|part| !part.is_empty()) =>
            {
                let joined = tag.join("/");
                let tag = percent_decode_str(&joined).decode_utf8().ok()?;
                Release::named(server, project, Some(tag_name(&tag)?))
            }
            _ => None,
        }
    }

    fn named(server: Option<String>, parts: &[&str], tag: Option<String>) -> Option<Release> {
        Some(Release {
            project: Project::named(server, parts)?,
            tag,
        })
    }

    /// the origin of another server, where every request for its releases goes; none for
    /// gitlab.com, which is no origin a person names.
    pub(super) fn origin(&self) -> Option<url::Origin> {
        self.project.origin()
    }

    /// whether the release is on another server than gitlab.com.
    pub(super) fn is_elsewhere(&self) -> bool {
        self.project.is_elsewhere()
    }

    /// the server's host, with its port when that is not 443.
    fn host(&self) -> &str {
        self.project.host()
    }

    /// lists the release's links: at the tag, or of the newest release, which pins its tag.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let url = match &self.tag {
            Some(tag) => self.url(&["releases", tag])?,
            None => self.url(&["releases", "permalink", "latest"])?,
        };
        let response = api.get(&url).await?;
        if response.status == 404 {
            return Err(if response.text().contains("Project Not Found") {
                self.project.hidden(self.project.signed_in(), "releases")
            } else {
                self.no_release()
            });
        }
        if matches!(response.status, 401 | 403) && self.is_elsewhere() {
            return Err(self.project.refused(self));
        }
        let answer: Answer = super::json(self, &response)?;
        // the tag is pinned in data.lock, which must read back as written
        if !answer.tag_name.is_empty() && tag_name(&answer.tag_name).is_none() {
            return Err(invalid(
                self,
                "the release's tag is no tag a reference can hold",
            ));
        }
        if answer.tag_name.is_empty() {
            return Err(invalid(self, "the release names no tag"));
        }
        if self.tag.as_ref().is_some_and(|tag| *tag != answer.tag_name) {
            return Err(invalid(self, "the answer names another tag"));
        }
        let assets = answer.assets;
        if assets.count > assets.links.len() + assets.sources.len() {
            return Err(invalid(
                self,
                &format!(
                    "it lists {} of the {} it counts",
                    assets.links.len() + assets.sources.len(),
                    crate::units::count(assets.count as u64, "asset", "assets")
                ),
            ));
        }
        if assets.links.len() > api.most_files() {
            return Err(super::too_many(
                self,
                api.most_files(),
                "download the files you need from the release's page one at a time",
            ));
        }
        let (files, unavailable) = self.links(assets.links)?;
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

    /// the files of the release's links at the paths it gives them, and the links fetchloom
    /// cannot download.
    fn links(&self, links: Vec<Link>) -> Result<(Vec<Remote>, Vec<Unavailable>), Error> {
        let mut files = Vec::new();
        let mut unavailable = Vec::new();
        for link in links {
            let Some(direct) = link.direct_asset_url else {
                unavailable.push(Unavailable {
                    path: link.name,
                    reason: "gitlab gives no path in the release for it".to_owned(),
                });
                continue;
            };
            // a link given no path in the release has its own address as its direct one
            let path = if direct == link.url {
                let named = Url::parse(&link.url)
                    .ok()
                    .as_ref()
                    .and_then(crate::name::from_url);
                let Some(name) = named else {
                    unavailable.push(Unavailable {
                        path: link.name,
                        reason: "its address names no file".to_owned(),
                    });
                    continue;
                };
                name
            } else {
                self.path(&direct)?
            };
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
        Ok((files, unavailable))
    }

    /// the path the release gives a link's file, after `/downloads/` in its download address on
    /// the release's own server.
    fn path(&self, direct: &str) -> Result<String, Error> {
        let invalid = || {
            invalid(
                self,
                &format!(
                    "a download link is not this release's on {}: {}",
                    self.host(),
                    crate::shown::mask_query(direct)
                ),
            )
        };
        let url = Url::parse(direct)
            .ok()
            .filter(|url| {
                url.scheme() == "https"
                    && Some(url.origin()) == self.project.api_origin()
                    && url.query().is_none()
            })
            .ok_or_else(invalid)?;
        let prefix = format!("/{}/-/releases/", self.project.path.to_ascii_lowercase());
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

    /// the project's URL in gitlab's API, with `more` after it.
    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        self.project
            .url(more)
            .map_err(|reason| invalid(self, reason))
    }

    fn page(&self) -> String {
        self.project.page()
    }
}

impl Project {
    /// a project's path from its parts: a group or more, then the project, each made of letters,
    /// digits, `_`, `-` and `.`, on `server`, which is no other server when it is gitlab.com.
    fn named(server: Option<String>, parts: &[&str]) -> Option<Project> {
        let valid = parts.len() >= 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 255
                    && !part.starts_with(['-', '.'])
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            });
        valid.then(|| Project {
            server: server.filter(|host| host != GITLAB_COM),
            path: parts.join("/"),
        })
    }

    /// the project whose page `url` is a page of, `parts` being its path's parts before gitlab's
    /// `-`: on gitlab.com, or on another server over https.
    pub(super) fn of_page(url: &Url, parts: &[&str]) -> Option<Project> {
        if url.host_str()? == GITLAB_COM {
            return Project::named(None, parts);
        }
        Project::named(Some(server_of(url)?), parts)
    }

    /// the origin of another server, where every request for the project goes; none for
    /// gitlab.com, which is no origin a person names.
    pub(super) fn origin(&self) -> Option<url::Origin> {
        self.server.as_ref().and_then(|_| self.api_origin())
    }

    /// whether the project is on another server than gitlab.com.
    pub(super) fn is_elsewhere(&self) -> bool {
        self.server.is_some()
    }

    /// the server's host, with its port when that is not 443.
    fn host(&self) -> &str {
        self.server.as_deref().unwrap_or(GITLAB_COM)
    }

    /// the project's own name, the last part of its path.
    pub(super) fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// whether a key for the project's server is on this machine and can go with a request.
    pub(super) fn signed_in(&self) -> bool {
        match &self.server {
            Some(host) => crate::auth::site::find(&super::env, host).is_some(),
            None => Account::GitLab
                .find(&super::env)
                .is_some_and(|found| Account::GitLab.sendable(&found)),
        }
    }

    /// gitlab's answer that it shows no project of this path, which may be private: a key that
    /// can read it lifts that, to read its `what`.
    fn hidden(&self, signed_in: bool, what: &str) -> Error {
        let page = self.page();
        let host = self.host();
        if signed_in {
            return Error::new(
                Kind::AuthRequired,
                format!(
                    "{host} has no project {}, or this key cannot see it",
                    self.path
                ),
            )
            .help(format!("check the path at {page}"))
            .unseen();
        }
        let error = Error::new(
            Kind::AuthRequired,
            format!("{host} has no project {}, or it is private", self.path),
        );
        match &self.server {
            Some(server) => error
                .help(format!(
                    "check the path at {page}. a private project's {what} need a key that can read it: save one with `{}`, which goes to {server} alone",
                    token_command(server)
                ))
                .needs_site(server.clone())
                .unseen(),
            None => error
                .help(format!(
                    "check the path at {page}. a private project's {what} need a key that can read it: set GITLAB_TOKEN, or sign in with `fetchloom auth login gitlab`"
                ))
                .needs_or_missing(Account::GitLab, page),
        }
    }

    /// another server's refusal of `subject`, which a key saved for it may lift.
    fn refused(&self, subject: &dyn fmt::Display) -> Error {
        let host = self.host();
        Error::new(
            Kind::AuthRequired,
            format!("{host} refused access to {subject}"),
        )
        .help(format!(
            "if the project is private, save a key that can read it with `{}`, then run the same command again",
            token_command(host)
        ))
        .needs_site(host.to_owned())
    }

    /// the origin of the server's api: gitlab.com's, or another server's.
    fn api_origin(&self) -> Option<url::Origin> {
        Url::parse(&format!("https://{}/", self.host()))
            .ok()
            .map(|url| url.origin())
    }

    /// the project's URL in gitlab's API, its path one encoded segment, with `more` after it,
    /// each one segment however many `/` it holds.
    fn url(&self, more: &[&str]) -> Result<Url, &'static str> {
        let mut url = Url::parse(&format!("https://{}/api/v4/projects", self.host()))
            .map_err(|_| "the API URL is invalid")?;
        url.path_segments_mut()
            .map_err(|()| "the API URL has no path")?
            .push(&self.path)
            .extend(more);
        Ok(url)
    }

    /// the project's page on its server.
    pub(super) fn page(&self) -> String {
        format!("https://{}/{}", self.host(), self.path)
    }

    /// the commit `reference`, a branch, a tag or a commit, names in the project, or none when it
    /// names nothing there.
    pub(super) async fn commit(
        &self,
        api: &impl Api,
        reference: &str,
    ) -> Result<Option<String>, Error> {
        let url = self
            .url(&["repository", "commits", reference])
            .map_err(|reason| invalid(self, reason))?;
        let response = api.get(&url).await?;
        match response.status {
            404 if response.text().contains("Project Not Found") => {
                Err(self.hidden(self.signed_in(), "files"))
            }
            404 => Ok(None),
            401 | 403 if self.is_elsewhere() => Err(self.refused(self)),
            _ => {
                let answer: Commit = super::json(self, &response)?;
                super::git::commit_id(&answer.id).map(Some).ok_or_else(|| {
                    invalid(self, &format!("its answer for {reference} names no commit"))
                })
            }
        }
    }

    /// what the tree at `commit` holds in `folder`, read page by page.
    pub(super) async fn tree(
        &self,
        api: &impl Api,
        commit: &str,
        folder: &str,
    ) -> Result<Listed, Error> {
        let mut first = self
            .url(&["repository", "tree"])
            .map_err(|reason| invalid(self, reason))?;
        {
            let mut query = first.query_pairs_mut();
            if !folder.is_empty() {
                query.append_pair("path", folder);
            }
            query
                .append_pair("ref", commit)
                .append_pair("recursive", "true")
                .append_pair("per_page", "100")
                .append_pair("pagination", "keyset");
        }
        let start = first.clone();
        let mut next = Some(first);
        let mut pages = super::Pages::most(api.most_files());
        let mut entries: Vec<Entry> = Vec::new();
        while let Some(url) = next {
            pages
                .next(url.clone(), entries.len())
                .map_err(|reason| invalid(self, &reason))?;
            let response = api.get(&url).await?;
            // gitlab says so of a folder its tree does not have, since version 17.7
            if response.status == 404 && url == start {
                return Ok(Listed::Missing);
            }
            if matches!(response.status, 401 | 403) && self.is_elsewhere() {
                return Err(self.refused(self));
            }
            let page: Vec<TreeEntry> = super::json(self, &response)?;
            for entry in page {
                let held = entry.held().ok_or_else(|| {
                    invalid(
                        self,
                        &format!(
                            "it gives \"{}\" a kind of entry git has not: {} {}",
                            entry.path.escape_debug(),
                            entry.kind.escape_debug(),
                            entry.mode.escape_debug()
                        ),
                    )
                })?;
                if !super::git::holds(folder, &entry.path) || entry.path == folder {
                    return Err(invalid(
                        self,
                        &format!(
                            "it lists \"{}\", which is not in the folder asked for",
                            entry.path.escape_debug()
                        ),
                    ));
                }
                entries.push(Entry {
                    path: entry.path,
                    held,
                });
            }
            if entries.len() > api.most_files() {
                return Err(super::too_many(
                    self,
                    api.most_files(),
                    "copy the link of a folder inside it that holds fewer files",
                ));
            }
            // a page goes on only to the next page of this listing, on the same server
            next = match response.next {
                Some(page) if page.origin() == start.origin() && page.path() == start.path() => {
                    Some(page)
                }
                Some(_) => {
                    return Err(invalid(
                        self,
                        "a page's next page is not one of this listing's",
                    ));
                }
                None => None,
            };
        }
        // an older gitlab answers an empty list for a folder its tree does not have, and git
        // keeps no empty folder
        if entries.is_empty() {
            return Ok(Listed::Missing);
        }
        Ok(Listed::Entries(super::git::relative(entries, folder)))
    }

    /// where a file of the project at `commit` downloads from: its raw page on the project's own
    /// server, which serves a file git-lfs keeps as its bytes, and takes the key saved for the
    /// server.
    pub(super) fn file(&self, commit: &str, path: &str) -> Url {
        let mut url =
            Url::parse(&self.page()).unwrap_or_else(|_| unreachable!("a project's page is a link"));
        if let Ok(mut segments) = url.path_segments_mut() {
            segments
                .extend(["-", "raw", commit])
                .extend(path.split('/'));
        }
        url
    }
}

/// a gitlab server's host as a reference writes it: a host name or address, lowercase, with its
/// port when that is not 443, as `auth login` saves a key for it.
fn server(text: &str) -> Option<String> {
    crate::auth::site::host(text)
}

/// the server another gitlab server's link names, over https: its host, with its port when that
/// is not 443; none for gitlab.com.
fn server_of(url: &Url) -> Option<String> {
    let host = url.host_str()?;
    if url.scheme() != "https" || host == GITLAB_COM {
        return None;
    }
    let named = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    server(&named)
}

/// the command that saves a token for the gitlab server at `host`, for it alone.
fn token_command(host: &str) -> String {
    format!("{} --token", Account::Login(host.to_owned()).command())
}

/// where a link's file is downloaded from: the link's own address, since its gitlab.com address
/// answers a program with a warning page for a link to another host. an address that is not
/// https, or that holds a secret, is not kept.
fn address(link: &str) -> Result<Url, &'static str> {
    let url = Url::parse(link).map_err(|_| "its address is not a link")?;
    if url.scheme() != "https" {
        return Err("its address is not https");
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || crate::shown::secret(&url).is_some()
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

/// a commit as gitlab's api gives it.
#[derive(Deserialize)]
struct Commit {
    id: String,
}

/// an entry of a tree as gitlab's api gives it, at its path in the project.
#[derive(Deserialize)]
struct TreeEntry {
    id: String,
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
}

impl TreeEntry {
    /// what git holds at the entry, none for a kind git has not.
    fn held(&self) -> Option<Held> {
        Some(match (self.kind.as_str(), self.mode.as_str()) {
            ("tree", _) => Held::Folder(self.id.clone()),
            ("commit", _) => Held::Submodule,
            ("blob", "120000") => Held::Link,
            ("blob", _) => Held::File(None),
            _ => return None,
        })
    }
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

impl fmt::Display for Project {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.page())
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.project.server {
            Some(server) => write!(f, "gitlab://{server}/{}", self.project.path)?,
            None => write!(f, "gitlab:{}", self.project.path)?,
        }
        if let Some(tag) = &self.tag {
            write!(f, "@{tag}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
