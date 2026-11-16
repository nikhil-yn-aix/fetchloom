//! a folder of a github or gitlab repository at one commit, from the link a browser shows for it:
//! `https://github.com/<owner>/<repo>/tree/<ref>/<path>`, or `.../<project>/-/tree/<ref>/<path>`
//! on gitlab.com or another gitlab server, and a repository's own link on github.com or
//! gitlab.com, which is its top folder at the default branch. its ref, a branch, a tag or a
//! commit, is read as the commit it names when the folder is listed, and that commit is the
//! version data.lock pins, each file by the SHA-256 of its bytes. git's links, symbolic links and
//! submodules, are left out by name.

use std::fmt;
use std::sync::Arc;

use percent_encoding::percent_decode_str;
use url::Url;

use super::access::{self, Access};
use super::{Listing, Reference, Remote, github, gitlab};
use crate::digest::BlobId;
use crate::error::{Error, Kind};
use crate::net::Api;

/// the most parts of a link a branch or tag is looked for in, as `release/2024/q1` takes three:
/// each part tried costs a request, and git's names rarely take more.
const MOST_REF_PARTS: usize = 5;

/// the ref both hosts' apis read as the default branch, whatever it is called.
const DEFAULT_BRANCH: &str = "HEAD";

/// github.com's own pages whose link starts as a repository's would, as `/topics/<topic>`: no
/// account holds these names, so none owns a repository.
const GITHUB_PAGES: [&str; 31] = [
    "about",
    "account",
    "apps",
    "codespaces",
    "collections",
    "dashboard",
    "enterprise",
    "events",
    "explore",
    "features",
    "issues",
    "login",
    "marketplace",
    "new",
    "notifications",
    "organizations",
    "orgs",
    "pricing",
    "pulls",
    "readme",
    "resources",
    "search",
    "security",
    "settings",
    "site",
    "solutions",
    "sponsors",
    "stars",
    "topics",
    "trending",
    "users",
];

/// the first parts of gitlab's own pages, which no group's path takes, from gitlab's
/// `TOP_LEVEL_ROUTES`.
const GITLAB_PAGES: [&str; 22] = [
    "admin",
    "api",
    "assets",
    "dashboard",
    "explore",
    "groups",
    "help",
    "import",
    "jwt",
    "login",
    "o",
    "oauth",
    "profile",
    "projects",
    "public",
    "s",
    "search",
    "sitemap",
    "snippets",
    "unsubscribes",
    "uploads",
    "users",
];

/// the words of a gitlab project's pages, which no group's or project's path holds, from
/// gitlab's `PROJECT_WILDCARD_ROUTES`: a link holding one is a page of a project, never one.
const GITLAB_PAGE_WORDS: [&str; 18] = [
    "-",
    "badges",
    "blame",
    "blob",
    "builds",
    "commits",
    "create",
    "create_dir",
    "edit",
    "files",
    "find_file",
    "new",
    "preview",
    "raw",
    "refs",
    "tree",
    "update",
    "wikis",
];

/// a folder of a repository, at the branch, tag or commit its link names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Folder {
    host: Host,
    /// the link's parts after `tree`, decoded: a branch, tag or commit, which may take several
    /// parts as `release/v1` does, then the folder's path in the repository. none for the
    /// repository's own link, which names its top folder at the default branch.
    rest: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Host {
    GitHub(github::Repo),
    GitLab(gitlab::Project),
}

/// a file's blob in a repository, by the id the repository's tree states for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Blob {
    /// the repository's page, as `https://github.com/<owner>/<repo>`: an id vouches for bytes in
    /// its own repository alone.
    pub(crate) repository: Arc<str>,
    pub(crate) id: BlobId,
}

/// what git holds at a path of a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Held {
    /// a file, of the size the host states when it states one, and its blob's id when it is a
    /// SHA-1 id, as all but a repository of SHA-256 ids give.
    File(Option<u64>, Option<BlobId>),
    /// a folder, by its tree's id.
    Folder(String),
    /// a symbolic link: a name that points at another path.
    Link,
    /// a submodule: another repository, at one of its commits.
    Submodule,
}

/// an entry of a tree, at its path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) path: String,
    pub(super) held: Held,
}

/// what a tree holds at a folder's path.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Listed {
    /// the folder's entries, each at its path inside the folder.
    Entries(Vec<Entry>),
    /// no folder: git keeps no empty one.
    Missing,
    /// something else, as a file.
    NotAFolder,
}

impl Folder {
    /// a pasted link to a folder of a repository on github.com or gitlab.com, or to the
    /// repository itself, which is its top folder at the default branch. a repository's own link
    /// may end in `.git`, as the one git clones does.
    pub(super) fn from_link(url: &Url) -> Option<Folder> {
        let parts = parts(url)?;
        match url.host_str()? {
            "github.com" | "www.github.com" => match parts.as_slice() {
                [owner, name, "tree", rest @ ..] => {
                    Folder::new(Host::GitHub(github::Repo::named(owner, name)?), rest)
                }
                [owner, name] if !GITHUB_PAGES.contains(owner) => {
                    let repo = github::Repo::named(owner, cloned(name))?;
                    Some(Folder::top(Host::GitHub(repo)))
                }
                _ => None,
            },
            "gitlab.com" => {
                Folder::on_gitlab(url, &parts).or_else(|| Folder::gitlab_project(url, &parts))
            }
            _ => None,
        }
    }

    /// a pasted link to a folder of a project on another gitlab server, over https, known by
    /// gitlab's `/-/tree/` in its path alone. it is read after every other source's links.
    pub(super) fn from_server_link(url: &Url) -> Option<Folder> {
        if matches!(
            url.host_str()?,
            "github.com" | "www.github.com" | "gitlab.com"
        ) {
            return None;
        }
        Folder::on_gitlab(url, &parts(url)?)
    }

    /// a folder of the gitlab project whose page `url` is, by the `-/tree/` after its path.
    fn on_gitlab(url: &Url, parts: &[&str]) -> Option<Folder> {
        let marker = parts.iter().position(|part| *part == "-")?;
        let ["tree", rest @ ..] = &parts[marker + 1..] else {
            return None;
        };
        let project = gitlab::Project::of_page(url, &parts[..marker])?;
        Folder::new(Host::GitLab(project), rest)
    }

    /// a gitlab.com project's own link, `<group>/<project>` with any subgroups between: one that
    /// holds no word of gitlab's own pages. another server's project link is a link like any
    /// other, since nothing in it says the server runs gitlab.
    fn gitlab_project(url: &Url, parts: &[&str]) -> Option<Folder> {
        let (first, _) = parts.split_first()?;
        if GITLAB_PAGES.contains(first) || parts.iter().any(|part| GITLAB_PAGE_WORDS.contains(part))
        {
            return None;
        }
        let mut parts = parts.to_vec();
        if let Some(last) = parts.last_mut() {
            *last = cloned(last);
        }
        let project = gitlab::Project::of_page(url, &parts)?;
        Some(Folder::top(Host::GitLab(project)))
    }

    fn new(host: Host, rest: &[&str]) -> Option<Folder> {
        Some(Folder {
            host,
            rest: decoded(rest)?,
        })
    }

    /// the repository's top folder at its default branch.
    fn top(host: Host) -> Folder {
        Folder {
            host,
            rest: Vec::new(),
        }
    }

    /// what the folder's source may ask and link to, and the key it takes.
    pub(super) fn access(&self) -> &'static Access {
        match &self.host {
            Host::GitHub(_) => &access::GITHUB,
            Host::GitLab(project) if project.is_elsewhere() => &access::GITLAB_SERVER,
            Host::GitLab(_) => &access::GITLAB,
        }
    }

    /// the origin of another gitlab server, where every request for the folder goes; none for
    /// github.com and gitlab.com.
    pub(super) fn origin(&self) -> Option<url::Origin> {
        match &self.host {
            Host::GitHub(_) => None,
            Host::GitLab(project) => project.origin(),
        }
    }

    /// whether the link names a commit by its full id, which no push changes.
    pub(super) fn is_at_commit(&self) -> bool {
        self.rest
            .first()
            .is_some_and(|first| commit_id(first).is_some())
    }

    /// the text a dataset's name is made from: the folder's own name, or the repository's for
    /// the whole of it.
    pub(super) fn naming(&self) -> String {
        match self.rest.as_slice() {
            [_, .., last] => last.clone(),
            _ => match &self.host {
                Host::GitHub(repo) => repo.name().to_owned(),
                Host::GitLab(project) => project.name().to_owned(),
            },
        }
    }

    /// lists the folder's files at the commit its ref names now.
    pub(super) async fn resolve(&self, api: &impl Api) -> Result<Listing, Error> {
        let signed_in = match &self.host {
            Host::GitHub(_) => github::signed_in(),
            Host::GitLab(project) => project.signed_in(),
        };
        self.listing(api, signed_in).await
    }

    /// [`Folder::resolve`], where `signed_in` says whether a key goes with the requests to the
    /// host's api.
    async fn listing(&self, api: &impl Api, signed_in: bool) -> Result<Listing, Error> {
        if let Host::GitHub(repo) = &self.host
            && signed_in
        {
            repo.public(api).await?;
        }
        let (reference, commit, folder) = self.commit(api, signed_in).await?;
        let listed = match &self.host {
            Host::GitHub(repo) => repo.tree(api, &commit, &folder, signed_in).await?,
            Host::GitLab(project) => project.tree(api, &commit, &folder).await?,
        };
        let entries = match listed {
            Listed::Entries(entries) => entries,
            Listed::Missing => return Err(self.missing(&reference, &folder)),
            Listed::NotAFolder => return Err(self.not_a_folder(&reference, &folder)),
        };
        let repository: Arc<str> = self.page().into();
        let mut files = Vec::new();
        let mut left: Vec<(String, &str)> = Vec::new();
        for Entry { path, held } in entries {
            match held {
                Held::File(size, id) => {
                    let blob = id.map(|id| Blob {
                        repository: Arc::clone(&repository),
                        id,
                    });
                    files.push(self.remote(&commit, &folder, path, size, blob));
                }
                Held::Folder(_) => {}
                Held::Link => left.push((path, "symbolic link")),
                Held::Submodule => left.push((path, "submodule")),
            }
        }
        left.sort();
        if files.is_empty() {
            return Err(self.only_links(&left));
        }
        let rest = std::iter::once(commit)
            .chain(
                folder
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned),
            )
            .collect();
        Ok(Listing {
            within: Vec::new(),
            // a key may have gone to gitlab's server, and a private project's files with it;
            // github's are listed only for a repository anyone may read
            access: match &self.host {
                Host::GitLab(_) if signed_in => super::Access::Unknown,
                _ => super::Access::Public,
            },
            unavailable: Vec::new(),
            warnings: self.left_out(&left).into_iter().collect(),
            resolved: Reference::GitFolder(Folder {
                host: self.host.clone(),
                rest,
            }),
            title: None,
            license: None,
            doi: None,
            files,
        })
    }

    /// the commit the link's ref names, with the ref and the folder's path the rest of the link
    /// is then. a ref may hold `/`, so the parts are tried as a ref one more at a time, the
    /// fewest first: git allows no branch inside another's name, as `a` beside `a/b`.
    async fn commit(
        &self,
        api: &impl Api,
        signed_in: bool,
    ) -> Result<(String, String, String), Error> {
        if self.rest.is_empty() {
            return match self.named(api, DEFAULT_BRANCH, signed_in).await? {
                Some(commit) => Ok((DEFAULT_BRANCH.to_owned(), commit, String::new())),
                None => Err(Error::new(
                    Kind::ReferenceUnresolved,
                    format!("{self} is empty: its default branch holds no commit"),
                )
                .help("check the link in a browser; an empty repository has no file to pin")),
            };
        }
        let most = if self.is_at_commit() {
            1
        } else {
            self.rest.len().min(MOST_REF_PARTS)
        };
        let mut tried = Vec::new();
        for at in 1..=most {
            let reference = self.rest[..at].join("/");
            if let Some(commit) = self.named(api, &reference, signed_in).await? {
                return Ok((reference, commit, self.rest[at..].join("/")));
            }
            tried.push(reference);
        }
        Err(Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "{} has no branch, tag or commit named {}",
                self.page(),
                crate::units::listed(&tried, "or")
            ),
        )
        .help(format!(
            "check the name in the link against the branches and tags at {}",
            self.page()
        )))
    }

    /// the commit `reference` names in the repository, none when it names none there.
    async fn named(
        &self,
        api: &impl Api,
        reference: &str,
        signed_in: bool,
    ) -> Result<Option<String>, Error> {
        match &self.host {
            Host::GitHub(repo) => repo.commit(api, reference, signed_in).await,
            Host::GitLab(project) => project.commit(api, reference).await,
        }
    }

    /// a file of the folder at `path` inside it, of `size` when the host states one, and of `blob`.
    fn remote(
        &self,
        commit: &str,
        folder: &str,
        path: String,
        size: Option<u64>,
        blob: Option<Blob>,
    ) -> Remote {
        let whole = if folder.is_empty() {
            path.clone()
        } else {
            format!("{folder}/{path}")
        };
        let (url, size) = match &self.host {
            Host::GitHub(repo) => repo.file(commit, &whole, size),
            Host::GitLab(project) => (project.file(commit, &whole), None),
        };
        Remote {
            path,
            size,
            sha256: None,
            md5: None,
            url,
            named_by_server: false,
            etag: None,
            blob,
        }
    }

    /// the one line that names the entries git keeps as links rather than files, `left` with
    /// what each is, which the folder leaves out.
    fn left_out(&self, left: &[(String, &str)]) -> Option<String> {
        let (what, names) = described(left)?;
        let (kept, them) = if left.len() == 1 {
            ("a link rather than a file", "it is")
        } else {
            ("links rather than files", "they are")
        };
        Some(format!(
            "{self} holds {what}, which git keeps as {kept}, so {them} left out: {names}"
        ))
    }

    fn only_links(&self, left: &[(String, &str)]) -> Error {
        let Some((what, names)) = described(left) else {
            return Error::new(Kind::ReferenceUnresolved, format!("{self} holds no file"))
                .help("copy the link of a folder that holds the files themselves");
        };
        Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "{self} holds no file, only {what}, which git keeps as links rather than files: {names}"
            ),
        )
        .help("copy the link of a folder that holds the files themselves")
    }

    fn missing(&self, reference: &str, folder: &str) -> Error {
        Error::new(
            Kind::ReferenceUnresolved,
            format!("{} has no folder {folder} at {reference}", self.page()),
        )
        .help(format!(
            "check the folder's name at {}; git keeps no empty folder",
            self.at(reference, "tree", "")
        ))
    }

    fn not_a_folder(&self, reference: &str, folder: &str) -> Error {
        Error::new(
            Kind::ReferenceInvalid,
            format!(
                "{folder} in {} at {reference} is a file, not a folder",
                self.page()
            ),
        )
        .help(format!(
            "pin that one file by its own link: {}",
            self.at(reference, "blob", folder)
        ))
    }

    /// the folder's link as messages show it, a full commit cut to its first 7 digits, as git and
    /// the hosts' pages show one.
    pub(crate) fn shown_short(&self) -> String {
        let mut short = self.clone();
        if let Some(first) = short.rest.first_mut()
            && commit_id(first).is_some()
        {
            first.truncate(7);
        }
        short.to_string()
    }

    /// the repository's page.
    fn page(&self) -> String {
        match &self.host {
            Host::GitHub(repo) => repo.page(),
            Host::GitLab(project) => project.page(),
        }
    }

    /// the link of the repository's `view`, `tree` or `blob`, of `path` at `reference`.
    fn at(&self, reference: &str, view: &str, path: &str) -> String {
        let marker = match self.host {
            Host::GitHub(_) => "",
            Host::GitLab(_) => "/-",
        };
        let mut link = format!("{}{marker}/{view}/{reference}", self.page());
        if !path.is_empty() {
            link.push('/');
            link.push_str(path);
        }
        link
    }

    /// the folder's link, as data.toml and data.lock keep it: the repository's own link for its
    /// top folder at the default branch.
    fn link(&self) -> Url {
        let marker: &[&str] = match self.host {
            Host::GitHub(_) => &["tree"],
            Host::GitLab(_) => &["-", "tree"],
        };
        let mut url = Url::parse(&self.page())
            .unwrap_or_else(|_| unreachable!("a repository's page is a link"));
        if !self.rest.is_empty()
            && let Ok(mut segments) = url.path_segments_mut()
        {
            segments.extend(marker).extend(&self.rest);
        }
        url
    }
}

impl fmt::Display for Folder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.link())
    }
}

/// the kinds of `left` counted, as `1 symbolic link and 2 submodules`, and their names; none when
/// there are none.
fn described(left: &[(String, &str)]) -> Option<(String, String)> {
    if left.is_empty() {
        return None;
    }
    let counted: Vec<String> = [
        ("symbolic link", "symbolic links"),
        ("submodule", "submodules"),
    ]
    .into_iter()
    .filter_map(|(one, many)| {
        let count = left.iter().filter(|(_, what)| *what == one).count();
        (count > 0).then(|| crate::units::count(count as u64, one, many))
    })
    .collect();
    let names: Vec<&str> = left.iter().map(|(path, _)| path.as_str()).collect();
    Some((
        crate::units::listed(&counted, "and"),
        crate::units::some_of(&names, 10),
    ))
}

/// a commit's id as git writes it, lowercase: 40 hex digits, or 64 in a repository of SHA-256
/// objects.
pub(super) fn commit_id(text: &str) -> Option<String> {
    let valid = matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit());
    valid.then(|| text.to_ascii_lowercase())
}

/// whether `path`, in a repository, lies in `folder`: it is the folder, or it is below it. every
/// path lies in the top folder, written empty.
pub(super) fn holds(folder: &str, path: &str) -> bool {
    folder.is_empty()
        || path
            .strip_prefix(folder)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// whether a listing of `folder` needs the folder at `path`: one inside it, or one above it on
/// the way there.
pub(super) fn on_the_way(folder: &str, path: &str) -> bool {
    holds(folder, path) || holds(path, folder)
}

/// what `entries`, at their paths in the repository, hold in `folder`, as [`relative`] gives them,
/// or that the tree holds no folder there.
pub(super) fn inside(entries: Vec<Entry>, folder: &str) -> Listed {
    if !folder.is_empty() {
        match entries.iter().find(|entry| entry.path == folder) {
            None => return Listed::Missing,
            Some(Entry {
                held: Held::Folder(_),
                ..
            }) => {}
            Some(_) => return Listed::NotAFolder,
        }
    }
    Listed::Entries(relative(entries, folder))
}

/// the entries of `entries` below `folder`, each at its path inside it.
pub(super) fn relative(entries: Vec<Entry>, folder: &str) -> Vec<Entry> {
    if folder.is_empty() {
        return entries;
    }
    entries
        .into_iter()
        .filter_map(|entry| {
            let path = entry
                .path
                .strip_prefix(folder)?
                .strip_prefix('/')?
                .to_owned();
            Some(Entry {
                path,
                held: entry.held,
            })
        })
        .collect()
}

/// a repository's name in its link, without the `.git` the link git clones ends in.
fn cloned(name: &str) -> &str {
    name.strip_suffix(".git").unwrap_or(name)
}

/// the parts of `url`'s path, without the empty one a closing `/` leaves.
fn parts(url: &Url) -> Option<Vec<&str>> {
    let mut parts: Vec<&str> = url.path_segments()?.collect();
    if parts.last() == Some(&"") {
        parts.pop();
    }
    Some(parts)
}

/// the parts of a link's path after `tree`, decoded, an encoded `/` parting them as a plain one
/// does: none when there are none, or one is empty, `.` or `..`, or holds a `\` or a control
/// character, which no ref or path in a repository holds.
fn decoded(parts: &[&str]) -> Option<Vec<String>> {
    let mut decoded = Vec::new();
    for part in parts {
        let text = percent_decode_str(part).decode_utf8().ok()?;
        for piece in text.split('/') {
            let valid = !piece.is_empty()
                && !matches!(piece, "." | "..")
                && !piece.contains('\\')
                && !piece.chars().any(char::is_control);
            if !valid {
                return None;
            }
            decoded.push(piece.to_owned());
        }
    }
    (!decoded.is_empty()).then_some(decoded)
}

#[cfg(test)]
mod tests;
