//! github: a release's files, at a tag or the newest, with the SHA-256 github states, and the
//! commits and trees a folder of a repository is listed from. a tag or a branch can move, so the
//! lock's SHA-256 names the bytes. a key goes to api.github.com only, so a private repository's
//! files are pinned at its api, which serves them to a key that can read it: each file by its
//! blob, which no push changes, and each release asset by its id, whose download github sends on
//! to its storage without the key. a public repository's files come from github.com and its
//! storage, which take no key.

use std::fmt;

use percent_encoding::percent_decode_str;
use serde::Deserialize;
use url::Url;

use super::git::{Entry, Held, Listed};
use super::{Access, Listing, Reference, Remote, Unavailable, invalid, malformed};
use crate::account::Account;
use crate::digest::{BlobId, Sha256};
use crate::error::{Error, Kind};
use crate::net::{Api, Extra, Response};

const API: &str = "https://api.github.com";

/// the answer github gives for a commit with this, its id alone.
const SHA: Extra = &[("accept", "application/vnd.github.sha")];

/// a github repository, by its owner and its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Repo {
    owner: String,
    name: String,
}

/// a release of a github repository, at a tag or the newest one github calls the latest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Release {
    repo: Repo,
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
        Some(Release {
            repo: Repo::named(owner, repo)?,
            tag,
        })
    }

    /// lists the release's files: at the tag, or of the release github calls the latest, which
    /// pins its tag.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        self.listing(api, signed_in()).await
    }

    /// [`Release::resolve`], where `signed_in` says whether a key goes with the requests to
    /// github's api, which then asks whether the repository is private: its assets are pinned at
    /// the api, which serves them to the key.
    async fn listing(&self, api: &impl Api, signed_in: bool) -> Result<Listing, Error> {
        let url = match &self.tag {
            Some(tag) => self.url(&["releases", "tags", tag])?,
            None => self.url(&["releases", "latest"])?,
        };
        let response = get(api, &url, &[], signed_in).await?;
        if response.status == 404 {
            return Err(self.missing(api, signed_in).await);
        }
        let answer: Answer = super::json(self, &response)?;
        if answer.tag_name.is_empty() {
            return Err(invalid(self, "the release names no tag"));
        }
        // the tag is pinned in data.lock, which must read back as written
        if tag_name(&answer.tag_name).is_none() {
            return Err(invalid(
                self,
                "the release's tag is no tag a reference can hold",
            ));
        }
        if self.tag.as_ref().is_some_and(|tag| *tag != answer.tag_name) {
            return Err(invalid(self, "the answer names another tag"));
        }
        if answer.assets.len() > api.most_files() {
            return Err(super::too_many(
                self,
                api.most_files(),
                "download the files you need from the release's page one at a time",
            ));
        }
        let private = signed_in && self.repo.private(api).await?;
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
            files.push(asset.remote(self, private)?);
        }
        Ok(Listing {
            within: Vec::new(),
            access: match (signed_in, private) {
                (false, _) => Access::Unknown,
                (true, false) => Access::Public,
                (true, true) => Access::Login,
            },
            unavailable,
            warnings: Vec::new(),
            resolved: Reference::GitHub(Release {
                repo: self.repo.clone(),
                tag: Some(answer.tag_name),
            }),
            title: answer.name.filter(|name| !name.trim().is_empty()),
            license: None,
            doi: None,
            files,
        })
    }

    /// the error for a release github does not show, as it says alike of a missing release and a
    /// private or missing repository, which the repository's own answer tells apart.
    async fn missing(&self, api: &impl Api, signed_in: bool) -> Error {
        let repository = match self.url(&[]) {
            Ok(url) => get(api, &url, &[], signed_in).await,
            Err(error) => Err(error),
        };
        match repository {
            Ok(response) if (200..300).contains(&response.status) => {
                let (message, instead) = match &self.tag {
                    Some(tag) => (
                        format!("{} has no release at the tag {tag}", self.page()),
                        format!(
                            "to pin the repository's own files at that tag instead, run `{}`",
                            self.repo.adding(&["tree", tag])
                        ),
                    ),
                    None => (
                        format!("{} has no published release", self.page()),
                        format!(
                            "to pin the repository's own files at its default branch instead, run `{}`",
                            self.repo.adding(&[])
                        ),
                    ),
                };
                Error::new(Kind::ReferenceUnresolved, message).help(format!(
                    "check the releases at {}/releases; a tag without a release has no files to list. {instead}",
                    self.page()
                ))
            }
            Ok(_) => self.hidden(signed_in),
            Err(error) => error,
        }
    }

    /// github's answer that it shows no repository of this name, which may be private.
    fn hidden(&self, signed_in: bool) -> Error {
        let name = self.repo.to_string();
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

    fn url(&self, more: &[&str]) -> Result<Url, Error> {
        self.repo.url(more).map_err(|reason| invalid(self, reason))
    }

    fn page(&self) -> String {
        self.repo.page()
    }
}

impl Repo {
    /// a repository's owner and name as github allows them: an owner of letters, digits and
    /// `-`, a name of those, `_` and `.`.
    pub(super) fn named(owner: &str, name: &str) -> Option<Repo> {
        let owner_valid = (1..=39).contains(&owner.len())
            && owner
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        let name_valid = (1..=100).contains(&name.len())
            && !matches!(name, "." | "..")
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        (owner_valid && name_valid).then(|| Repo {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    /// the repository's own name, without its owner's.
    pub(super) fn name(&self) -> &str {
        &self.name
    }

    /// the repository's page on github.com.
    pub(super) fn page(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }

    /// the command that adds the repository's page with `more` path segments after it, as
    /// `tree` and a tag: its own files at that tag, or at the default branch without any.
    fn adding(&self, more: &[&str]) -> String {
        let mut link = Url::parse(&self.page())
            .unwrap_or_else(|_| unreachable!("a repository's page is a link"));
        if let Ok(mut segments) = link.path_segments_mut() {
            for part in more {
                segments.extend(part.split('/'));
            }
        }
        crate::suggested::command(&format!(
            "add {}",
            crate::suggested::shell_word(link.as_str())
        ))
    }

    /// the repository's URL in github's API, with `more` path segments after it, each one segment
    /// however many `/` it holds.
    fn url(&self, more: &[&str]) -> Result<Url, &'static str> {
        let mut url = Url::parse(&format!("{API}/repos")).map_err(|_| "the API URL is invalid")?;
        url.path_segments_mut()
            .map_err(|()| "the API URL has no path")?
            .push(&self.owner)
            .push(&self.name)
            .extend(more);
        Ok(url)
    }

    /// github's answer that it shows no repository of this name, which may be private: a key
    /// that can read it would show it.
    fn hidden(&self, signed_in: bool) -> Error {
        let page = self.page();
        if signed_in {
            return Error::new(
                Kind::AuthRequired,
                format!("github has no repository {self}, or this key cannot see it"),
            )
            .help(format!("check the name at {page}"))
            .unseen();
        }
        Error::new(
            Kind::AuthRequired,
            format!("github has no repository {self}, or it is private"),
        )
        .help(format!(
            "check the name at {page}. a private repository's files need a key that can read it: set GH_TOKEN or GITHUB_TOKEN, or sign in with `fetchloom auth login github`"
        ))
        .needs_or_missing(Account::GitHub, page)
    }

    /// whether github keeps the repository private, as it answers a key that can read it.
    pub(super) async fn private(&self, api: &impl Api) -> Result<bool, Error> {
        let url = self.url(&[]).map_err(|reason| invalid(self, reason))?;
        let response = get(api, &url, &[], true).await?;
        if response.status == 404 {
            return Err(self.hidden(true));
        }
        let answer: Visibility = super::json(self, &response)?;
        Ok(answer.private)
    }

    /// the commit `reference`, a branch, a tag or a commit, names in the repository, or none when
    /// it names nothing there.
    pub(super) async fn commit(
        &self,
        api: &impl Api,
        reference: &str,
        signed_in: bool,
    ) -> Result<Option<String>, Error> {
        let url = self
            .url(&["commits", reference])
            .map_err(|reason| invalid(self, reason))?;
        let response = get(api, &url, SHA, signed_in).await?;
        match response.status {
            // github's words for a branch, tag or commit it does not know, and for a repository
            // with no commit at all
            409 | 422 => Ok(None),
            404 => Err(self.hidden(signed_in)),
            200..=299 => super::git::commit_id(response.text().trim())
                .map(Some)
                .ok_or_else(|| {
                    invalid(self, &format!("its answer for {reference} names no commit"))
                }),
            _ => Err(super::unsuccessful(self, &response)),
        }
    }

    /// what the tree at `commit` holds in `folder`: the whole tree in one answer, or, where github
    /// cuts that short, the folders on the way to `folder` one by one, then each of its subtrees
    /// whole where github allows; and the requests github's rate limit left after the last
    /// answer, when it said.
    pub(super) async fn tree(
        &self,
        api: &impl Api,
        commit: &str,
        folder: &str,
        signed_in: bool,
    ) -> Result<(Listed, Option<u64>), Error> {
        let mut pages = super::Pages::most(api.most_files());
        let whole = self
            .listed(api, (commit, ""), true, signed_in, &mut pages, 0)
            .await?;
        let mut remaining = whole.remaining;
        if !whole.truncated {
            return Ok((super::git::inside(whole.entries, folder), remaining));
        }
        // the folders still to list, by their tree's id and path, and whether to ask for one
        // whole first, which the top folder no longer needs
        let mut pending = vec![(commit.to_owned(), String::new(), false)];
        let mut entries: Vec<Entry> = Vec::new();
        while let Some((tree, path, whole)) = pending.pop() {
            if whole && super::git::holds(folder, &path) {
                let listed = self
                    .listed(
                        api,
                        (&tree, &path),
                        true,
                        signed_in,
                        &mut pages,
                        entries.len(),
                    )
                    .await?;
                remaining = listed.remaining.or(remaining);
                if !listed.truncated {
                    entries.extend(listed.entries);
                    continue;
                }
            }
            let listed = self
                .listed(
                    api,
                    (&tree, &path),
                    false,
                    signed_in,
                    &mut pages,
                    entries.len(),
                )
                .await?;
            remaining = listed.remaining.or(remaining);
            if listed.truncated {
                let named = if path.is_empty() {
                    "the top folder".to_owned()
                } else {
                    format!("the folder {path}")
                };
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!("github lists only part of {named} of {self}, which holds more entries than its api gives at once"),
                )
                .help("copy the link of a folder inside it that holds fewer files"));
            }
            for entry in listed.entries {
                if let Held::Folder(id) = &entry.held
                    && super::git::on_the_way(folder, &entry.path)
                {
                    pending.push((id.clone(), entry.path.clone(), true));
                }
                if super::git::holds(folder, &entry.path) {
                    entries.push(entry);
                }
            }
            if entries.len() > api.most_files() {
                return Err(super::too_many(
                    self,
                    api.most_files(),
                    "copy the link of a folder inside it that holds fewer files",
                ));
            }
        }
        Ok((super::git::inside(entries, folder), remaining))
    }

    /// one answer of the trees api: the tree `tree` names, at `path` in the repository, with every
    /// subtree when `recursive`, its entries at their paths in the repository.
    async fn listed(
        &self,
        api: &impl Api,
        (tree, path): (&str, &str),
        recursive: bool,
        signed_in: bool,
        pages: &mut super::Pages<String>,
        held: usize,
    ) -> Result<Tree, Error> {
        pages
            .next(format!("{recursive} {path}"), held)
            .map_err(|reason| invalid(self, &reason))?;
        let mut url = self
            .url(&["git", "trees", tree])
            .map_err(|reason| invalid(self, reason))?;
        if recursive {
            url.query_pairs_mut().append_pair("recursive", "1");
        }
        let response = get(api, &url, &[], signed_in).await?;
        let answer: Answered = super::json(self, &response)?;
        let entries = answer
            .tree
            .into_iter()
            .map(|entry| {
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
                let full = if path.is_empty() {
                    entry.path
                } else {
                    format!("{path}/{}", entry.path)
                };
                Ok(Entry { path: full, held })
            })
            .collect::<Result<_, Error>>()?;
        Ok(Tree {
            entries,
            truncated: answer.truncated,
            remaining: response.remaining,
        })
    }

    /// where a file of a private repository downloads from: github's api, which serves the bytes
    /// of the blob `id` to a key that can read the repository. no push changes a blob.
    pub(super) fn blob(&self, id: &BlobId) -> Url {
        self.url(&["git", "blobs", &id.hex()])
            .unwrap_or_else(|_| unreachable!("a repository's api link is a link"))
    }

    /// where a file of the repository at `commit` downloads from, and its size when that is sure:
    /// a file of at least 1 KiB, too large for a git-lfs pointer, from raw.githubusercontent.com,
    /// which serves what git holds; a smaller one from github.com, which sends a file git-lfs
    /// keeps on to media.githubusercontent.com and any other to raw.githubusercontent.com.
    pub(super) fn file(&self, commit: &str, path: &str, size: Option<u64>) -> (Url, Option<u64>) {
        let large = size.filter(|size| *size >= crate::net::LFS_LARGEST);
        let mut url = match large {
            Some(_) => format!(
                "https://raw.githubusercontent.com/{}/{}",
                self.owner, self.name
            ),
            None => format!("https://github.com/{}/{}/raw", self.owner, self.name),
        }
        .parse::<Url>()
        .unwrap_or_else(|_| unreachable!("a repository's link is a link"));
        if let Ok(mut segments) = url.path_segments_mut() {
            segments.push(commit).extend(path.split('/'));
        }
        (url, large)
    }
}

/// what github's api serves at a link fetchloom pins a private repository's file at: a blob's
/// bytes, or a release asset's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Served {
    Blob,
    Asset,
}

/// what github's api serves at `url`, and the repository its path names as `<owner>/<name>`: a
/// renamed repository's api sends the request on by its number, which names none.
fn served(url: &Url) -> Option<(Served, Option<String>)> {
    if url.scheme() != "https"
        || url.host_str() != Some("api.github.com")
        || url.port().is_some()
        || url.query().is_some()
    {
        return None;
    }
    let parts: Vec<&str> = url.path_segments()?.collect();
    let (repository, rest) = match parts.as_slice() {
        ["repos", owner, name, rest @ ..] => (Some(format!("{owner}/{name}")), rest),
        ["repositories", number, rest @ ..]
            if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) =>
        {
            (None, rest)
        }
        _ => return None,
    };
    let served = match rest {
        ["git", "blobs", id] if BlobId::from_hex(id).is_some() => Served::Blob,
        ["releases", "assets", id] if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) => {
            Served::Asset
        }
        _ => return None,
    };
    Some((served, repository))
}

/// the media type github's api serves a file's bytes in at `url`: a blob's raw bytes, or a
/// release asset's, which it answers with json about them otherwise.
pub(super) fn accept(url: &Url) -> Option<&'static str> {
    Some(match served(url)?.0 {
        Served::Blob => "application/vnd.github.raw+json",
        Served::Asset => "application/octet-stream",
    })
}

/// why a private repository's file that git-lfs keeps cannot be downloaded.
pub(crate) const PRIVATE_LFS: &str = "github serves the files git-lfs keeps for a private repository only through github.com, where fetchloom sends no github key";

/// what to do about the git-lfs pointer `url`, a private repository's blob at github's api,
/// answered with, which its own bytes are: nothing downloads the file it stands for.
pub(super) fn lfs_help(url: &Url) -> Option<String> {
    (served(url)?.0 == Served::Blob)
        .then(|| format!("{PRIVATE_LFS}: leave the file out to pin the others"))
}

/// the refusal of `url`, a private repository's file at github's api, which github answered 404
/// for a request without a github key, as it answers for a repository it has not. a repository
/// made public since its pin serves the file to such a request.
pub(super) fn keyless(url: &Url) -> Option<Error> {
    let repository = served(url)?.1?;
    Some(
        Error::new(
            Kind::AuthRequired,
            format!(
                "github serves the files of the private repository {repository} only to a key that can read it"
            ),
        )
        .help("set GH_TOKEN or GITHUB_TOKEN to a github token that can read it, or sign in with `fetchloom auth login github`, then run the same command again")
        .needs(Account::GitHub, None),
    )
}

/// the error for github's limit on requests without a key, used up by downloads of files pinned
/// at its api, as a repository's pinned while it was private are: a key lifts it.
pub(super) fn keyless_limit(url: &Url) -> Option<Error> {
    served(url).map(|_| limited(false))
}

/// whether a github key is on this machine and can go with a request.
pub(super) fn signed_in() -> bool {
    Account::GitHub
        .find(&super::env)
        .is_some_and(|found| Account::GitHub.sendable(&found))
}

/// github's answer for `url`, asked with `headers`, or the error for its rate limit when the
/// answer is that.
async fn get(
    api: &impl Api,
    url: &Url,
    headers: Extra,
    signed_in: bool,
) -> Result<Response, Error> {
    let asked = if headers.is_empty() {
        api.get(url).await
    } else {
        api.get_with(url, headers).await
    };
    let response = asked.map_err(|error| {
        if error.kind == Kind::NetworkRateLimited {
            limited(signed_in).detail(error.message)
        } else {
            error
        }
    })?;
    if says_limited(&response) {
        return Err(limited(signed_in));
    }
    Ok(response)
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

/// who may see a repository, as github answers for it.
#[derive(Deserialize)]
struct Visibility {
    private: bool,
}

/// one answer of the trees api.
#[derive(Deserialize)]
struct Answered {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

/// an entry of a tree as github's api gives it, at its path below the tree listed.
#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
    size: Option<u64>,
}

impl TreeEntry {
    /// what git holds at the entry, none for a kind git has not.
    fn held(&self) -> Option<Held> {
        Some(match (self.kind.as_str(), self.mode.as_str()) {
            ("tree", _) => Held::Folder(self.sha.clone()),
            ("commit", _) => Held::Submodule,
            ("blob", "120000") => Held::Link,
            ("blob", _) => Held::File(self.size, BlobId::from_hex(&self.sha)),
            _ => return None,
        })
    }
}

/// a tree's entries, at their paths in the repository, whether github left some out, and the
/// requests its rate limit left.
struct Tree {
    entries: Vec<Entry>,
    truncated: bool,
    remaining: Option<u64>,
}

#[derive(Deserialize)]
struct Answer {
    tag_name: String,
    name: Option<String>,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    id: Option<u64>,
    name: String,
    size: Option<u64>,
    state: String,
    digest: Option<String>,
    browser_download_url: String,
}

impl Asset {
    /// the file of the asset: at its download link on github.com, or for a `private` repository
    /// at the api, by its id, which serves it to a key that can read the repository.
    fn remote(self, release: &Release, private: bool) -> Result<Remote, Error> {
        let quoted = self.name.escape_debug().to_string();
        let url = if private {
            let id = self.id.ok_or_else(|| {
                invalid(
                    release,
                    &format!("it gives \"{quoted}\" no id to download it by"),
                )
            })?;
            release.url(&["releases", "assets", &id.to_string()])?
        } else {
            Url::parse(&self.browser_download_url)
                .ok()
                .filter(|url| {
                    url.scheme() == "https"
                        && url.host_str() == Some("github.com")
                        && url.port().is_none()
                        && url.query().is_none()
                        && url.path().contains("/releases/download/")
                })
                .ok_or_else(|| {
                    invalid(
                        release,
                        &format!("the download link of \"{quoted}\" is not on github.com"),
                    )
                })?
        };
        let sha256 = match self.digest.as_deref() {
            None => None,
            Some(digest) => match digest.split_once(':') {
                Some(("sha256", hex)) => Some(Sha256::from_hex(hex).ok_or_else(|| {
                    invalid(release, &format!("the SHA-256 of \"{quoted}\" is not hex"))
                })?),
                // another algorithm checks nothing fetchloom keeps, and the bytes pin the file
                Some(_) => None,
                None => {
                    return Err(invalid(
                        release,
                        &format!("the digest of \"{quoted}\" names no algorithm"),
                    ));
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
            blob: None,
        })
    }
}

impl fmt::Display for Repo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

impl fmt::Display for Release {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "github:{}", self.repo)?;
        if let Some(tag) = &self.tag {
            write!(f, "@{tag}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
