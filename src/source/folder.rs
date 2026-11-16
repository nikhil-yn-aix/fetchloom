//! folders a web server lists, each one dataset by its link ending in `/`: every file in it and
//! in the folders inside it, at its path there, pinned by the SHA-256 its download gives. one
//! reader takes the listings apache, nginx, lighttpd and python's http.server write, NCBI's that
//! GEO's folders have among them. a listing is a page anyone could have written, so only links on
//! the folder's own origin and below it are followed, each once, and a name no file may have
//! stops it.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::hash::{Hash as _, Hasher as _};

use percent_encoding::percent_decode_str;
use url::Url;

use super::drive::html::unescape;
use super::{Access, Listing, Reference, Remote};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::net::{Api, Response};
use crate::shown::Shown;

/// how a refusal of a web page says to keep the page itself, where no entry of data.toml asks
/// for it yet, as for a `get` or an add not written.
pub(crate) const PAGE_FLAG: &str = "add --web-page";

/// how a server's listing begins its title, lowercase, before the path of the folder it lists.
const TITLES: [&str; 3] = [
    "index of /",
    "directory listing for /",
    "directory listing of /",
];

/// the elements a server writes the entries of a listing in: nginx's and apache's `<pre>`,
/// apache's and lighttpd's `<table>`, python's `<ul>`.
const LISTS: [&str; 3] = ["pre", "table", "ul"];

/// one entry of a listing: a file, or a folder, whose link ends in `/`, by its name with its
/// character references and percent escapes undone, its link, and the size its row ends with
/// when that is in bytes, as `621`; one rounded, as `1.3M`, is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) link: Url,
    pub(super) folder: bool,
    pub(super) size: Option<u64>,
}

/// what a listing holds: the entries of its folder, each once, in the order it gives them, and
/// its other links that lead outside the folder, or deeper than its entries, as each leads. the
/// link to the folder above and those with a query or a fragment, as a column's sort link, are
/// in neither.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Index {
    pub(super) entries: Vec<Entry>,
    pub(super) elsewhere: Vec<String>,
}

/// a link of a list on a listing's page: its `href`, its character references undone, the link
/// it makes against the page, none for one that makes none, and the size its row states. a link
/// whose text names a folder, ending in `/`, while its `href` does not, as ncei's do, makes the
/// folder's link, with the `/`.
struct Link {
    href: String,
    url: Option<Url>,
    size: Option<u64>,
}

/// why a page is no listing to read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unread {
    /// no server made it as a listing: its title names no folder, or it has no list of links,
    /// or one that does not end, as a page cut short.
    NoListing,
    /// an entry has a name no file may have, decoded.
    Name(String),
}

/// reads `html`, the page the link `page` answered with, as a listing of `folder`: the links in
/// its list of entries, their character references undone, each read against `page`.
pub(super) fn read(html: &str, page: &Url, folder: &Url) -> Result<Index, Unread> {
    let lower = html.to_ascii_lowercase();
    if !titled(html, &lower) {
        return Err(Unread::NoListing);
    }
    let parent = folder.join("..").ok();
    // each link of each list, read against the page, but those a query or a fragment marks
    let lists: Vec<Vec<Link>> = lists(&lower)
        .into_iter()
        .map(|(start, end)| {
            links(&html[start..end], &lower[start..end])
                .into_iter()
                .filter(|anchor| !anchor.href.contains(['?', '#']))
                .map(|anchor| {
                    let slashed = anchor.names_folder
                        && !anchor.href.is_empty()
                        && !anchor.href.ends_with('/');
                    let url = if slashed {
                        page.join(&format!("{}/", anchor.href))
                    } else {
                        page.join(&anchor.href)
                    };
                    Link {
                        url: url.ok(),
                        href: anchor.href,
                        size: anchor.size,
                    }
                })
                .collect()
        })
        .collect();
    // the link to the folder above, as `../`, `..` or the parent's path
    let above = |link: &Link| parent.is_some() && link.url == parent;
    // a header a server shows above its listing may be a list too, so the listing is the list
    // that links the folder above, as apache's, nginx's and lighttpd's do, else the first with a
    // link, as python's has no such link
    let chosen = lists
        .iter()
        .position(|links| links.iter().any(above))
        .or_else(|| lists.iter().position(|links| !links.is_empty()))
        .or_else(|| (!lists.is_empty()).then_some(0))
        .ok_or(Unread::NoListing)?;
    let mut index = Index::default();
    let mut seen = HashSet::new();
    for each in &lists[chosen] {
        if above(each) {
            continue;
        }
        let Link {
            href,
            url: Some(link),
            size,
        } = each
        else {
            index
                .elsewhere
                .push(crate::name::escaped(&each.href).into_owned());
            continue;
        };
        let same_origin = link.scheme() == folder.scheme()
            && link.host_str() == folder.host_str()
            && link.port_or_known_default() == folder.port_or_known_default();
        let rest = same_origin
            .then(|| link.path().strip_prefix(folder.path()))
            .flatten()
            .filter(|rest| !rest.is_empty());
        let Some((name, is_folder)) = rest.map(|rest| match rest.strip_suffix('/') {
            Some(name) => (name, true),
            None => (rest, false),
        }) else {
            index.elsewhere.push(Shown(link).to_string());
            continue;
        };
        if name.contains('/') {
            index.elsewhere.push(Shown(link).to_string());
            continue;
        }
        let decoded = percent_decode_str(name)
            .decode_utf8()
            .map_err(|_| Unread::Name(crate::name::escaped(href).into_owned()))?;
        if !is_name(&decoded) {
            return Err(Unread::Name(decoded.into_owned()));
        }
        if seen.insert((name, is_folder)) {
            index.entries.push(Entry {
                name: decoded.into_owned(),
                link: link.clone(),
                folder: is_folder,
                size: *size,
            });
        }
    }
    Ok(index)
}

/// whether the page's title says a server listed a folder there, as `Index of /data/` does.
fn titled(html: &str, lower: &str) -> bool {
    let Some(open) = opening(lower, 0, "title") else {
        return false;
    };
    let Some(start) = lower[open..].find('>').map(|end| open + end + 1) else {
        return false;
    };
    let Some(end) = lower[start..].find("</title").map(|end| start + end) else {
        return false;
    };
    let title = unescape(&html[start..end]);
    let title = title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    TITLES.iter().any(|words| title.starts_with(words))
}

/// where each list of the lowercase page `lower` holds its entries, between its tags, in order,
/// up to its last whole list, so a list a page cut short leaves open is none. one pass reads
/// the page, so a page of many lists takes no longer than its length.
fn lists(lower: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = lower[at..].find('<') {
        let open = at + offset;
        let Some(tag) = LISTS.iter().find(|tag| starts_tag(lower, open, tag)) else {
            at = open + 1;
            continue;
        };
        let Some(start) = lower[open..].find('>').map(|end| open + end + 1) else {
            break;
        };
        let Some(end) = lower[start..]
            .find(&format!("</{tag}"))
            .map(|end| start + end)
        else {
            break;
        };
        found.push((start, end));
        at = end;
    }
    found
}

/// where the next tag `<tag` of the lowercase page `lower` opens at or after `at`.
fn opening(lower: &str, at: usize, tag: &str) -> Option<usize> {
    let mut from = at;
    while let Some(found) = lower.get(from..)?.find('<') {
        let open = from + found;
        if starts_tag(lower, open, tag) {
            return Some(open);
        }
        from = open + 1;
    }
    None
}

/// whether the `<` at `open` opens the tag `tag`, its name ending there, as `<pre>` and
/// `<pre class="x">` do and `<preview>` does not.
fn starts_tag(lower: &str, open: usize, tag: &str) -> bool {
    let after = open + 1 + tag.len();
    lower.get(open + 1..after).is_some_and(|name| name == tag)
        && lower
            .as_bytes()
            .get(after)
            .is_none_or(|next| *next == b'>' || *next == b'/' || next.is_ascii_whitespace())
}

/// a link as a list writes it: its `href`, its character references undone, whether its text
/// ends in `/`, as a folder's name does, and the size its row states.
struct Anchor {
    href: String,
    names_folder: bool,
    size: Option<u64>,
}

/// each link in `list`, in order; `lower` is the list in lowercase.
fn links(list: &str, lower: &str) -> Vec<Anchor> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = opening(lower, at, "a") {
        let (href, end) = href(list, lower, open + 2);
        if let Some(href) = href {
            found.push(Anchor {
                href: unescape(href),
                names_folder: unescape(&text(list, lower, end)).trim_end().ends_with('/'),
                size: size(&lower[end..]),
            });
        }
        at = end;
    }
    found
}

/// the text of the link whose tag ends at `at` in `list`, without the tags inside it, up to where
/// the link closes or the next one opens; `lower` is the list in lowercase.
fn text(list: &str, lower: &str, at: usize) -> String {
    let mut text = String::new();
    let mut from = at;
    while let Some(offset) = lower[from..].find('<') {
        let open = from + offset;
        text.push_str(&list[from..open]);
        if starts_tag(lower, open, "/a") || starts_tag(lower, open, "a") {
            return text;
        }
        let Some(close) = lower[open..].find('>') else {
            return text;
        };
        from = open + close + 1;
    }
    text.push_str(&list[from..]);
    text
}

/// the size in bytes a row states after a link whose text, then the rest of its row, `row`
/// holds: the row's last word up to the next tag or line, as NCBI and nginx write `621`. a link
/// whose text holds a tag, a rounded size, as `1.3M`, and a folder's `-` state none.
fn size(row: &str) -> Option<u64> {
    let close = row.find('<')?;
    let rest = row[close..].strip_prefix("</a>")?;
    let words = &rest[..rest.find(['<', '\n']).unwrap_or(rest.len())];
    words
        .split_ascii_whitespace()
        .next_back()
        .filter(|word| word.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|word| word.parse().ok())
}

/// the `href` the tag whose attributes start at `at` has, and where the tag ends. a value is in
/// quotes, as servers write it, or runs to a space or the tag's end.
fn href<'a>(text: &'a str, lower: &str, mut at: usize) -> (Option<&'a str>, usize) {
    let bytes = text.as_bytes();
    let space = |at: usize| bytes.get(at).is_some_and(u8::is_ascii_whitespace);
    let mut found = None;
    loop {
        while space(at) {
            at += 1;
        }
        match bytes.get(at) {
            None => return (found, at),
            Some(b'>') => return (found, at + 1),
            Some(_) => {}
        }
        let name = at;
        while bytes
            .get(at)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(byte, b'=' | b'>'))
        {
            at += 1;
        }
        let named = &lower[name..at];
        while space(at) {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            continue;
        }
        at += 1;
        while space(at) {
            at += 1;
        }
        let value = if let Some(&quote @ (b'"' | b'\'')) = bytes.get(at) {
            let start = at + 1;
            let end = text[start..]
                .find(char::from(quote))
                .map_or(text.len(), |end| start + end);
            at = (end + 1).min(text.len());
            &text[start..end]
        } else {
            let start = at;
            while bytes
                .get(at)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && *byte != b'>')
            {
                at += 1;
            }
            &text[start..at]
        };
        if named == "href" && found.is_none() {
            found = Some(value);
        }
    }
}

/// a name one entry of a folder may have: not empty, not a step up, with no separator and no
/// control character.
pub(super) fn is_name(name: &str) -> bool {
    !matches!(name, "" | "." | "..")
        && !name.contains(['/', '\\'])
        && !name.contains(crate::name::is_control)
}

/// a folder a web server lists, by its link, which ends in `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Folder {
    url: Url,
}

impl Folder {
    /// the folder a plain link names: one whose path ends in `/`, with no query or fragment. a
    /// share's link, as a nextcloud share's ending in `/`, is its page, never a folder listing.
    pub(super) fn from_link(url: &Url) -> Option<Folder> {
        let share =
            super::share::service(url).is_some() || super::share::cloud_download(url).is_some();
        (url.path().ends_with('/') && url.query().is_none() && url.fragment().is_none() && !share)
            .then(|| Folder { url: url.clone() })
    }

    /// the folder's link.
    pub(crate) fn link(&self) -> &Url {
        &self.url
    }

    /// the text a dataset's name is made from: the folder's host and path, its parts decoded,
    /// so `my%20data/` names `my-data`.
    pub(super) fn naming(&self) -> String {
        let path = percent_decode_str(self.url.path()).decode_utf8_lossy();
        format!("{}{path}", self.url.host_str().unwrap_or_default())
    }

    /// every file in the folder and in the folders inside it, by their listings, or only those in
    /// the folders the `select` patterns may reach, when each says where its files lie. a link
    /// ending in `/` that answers with a file, not a page, is that file's plain link.
    pub(super) async fn resolve(&self, api: &impl Api, select: &[Glob]) -> Result<Listing, Error> {
        let response = api.get(&self.url).await?;
        self.answered(&response, &self.url, "")?;
        let text = response.text();
        let page = text
            .trim_start_matches('\u{feff}')
            .trim_start()
            .starts_with('<');
        if !page {
            return Ok(super::single(&self.url));
        }
        let index = self.listed(&text, &response, &self.url, "")?;
        let scopes: Option<Vec<String>> = if select.is_empty() {
            None
        } else {
            select.iter().map(Glob::scope).collect()
        };
        let mut walk = Walk {
            folder: self,
            most: api.most_files(),
            select: if scopes.is_some() { select } else { &[] },
            files: Vec::new(),
            folders: VecDeque::new(),
            counted: 0,
            seen: HashMap::new(),
            warnings: Vec::new(),
        };
        walk.add("", &self.url, index)?;
        while let Some((path, url)) = walk.folders.pop_front() {
            let response = api.get(&url).await?;
            self.answered(&response, &url, &path)?;
            let index = self.listed(&response.text(), &response, &url, &path)?;
            walk.add(&path, &url, index)?;
        }
        if walk.files.is_empty() && scopes.is_none() {
            return Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{} lists no files", self.shown()),
            )
            .help(
                "open it in a browser to check that it holds files, in it or in a folder inside it",
            ));
        }
        let mut files = walk.files;
        files.sort_by(|a, b| a.path.cmp(&b.path));
        let mut within = scopes.unwrap_or_default();
        within.sort();
        within.dedup();
        Ok(Listing {
            resolved: Reference::Folder(self.clone()),
            title: None,
            license: None,
            doi: None,
            access: Access::Unknown,
            files,
            unavailable: Vec::new(),
            warnings: walk.warnings,
            within,
        })
    }

    /// refuses the answer to the listing of the folder at `path`, the top one for an empty path,
    /// asked for at `url`, unless it is one.
    fn answered(&self, response: &Response, url: &Url, path: &str) -> Result<(), Error> {
        let host = crate::net::host(&response.url);
        let asked = Shown(url).to_string();
        match response.status {
            200..=299 => Ok(()),
            404 | 410 if path.is_empty() => Err(Error::new(
                Kind::ReferenceUnresolved,
                format!("{host} has nothing at {asked} ({})", response.status),
            )
            .help(crate::net::MOVED_HELP)),
            404 | 410 => Err(Error::new(
                Kind::SourceInvalid,
                format!(
                    "{} lists the folder {}, and {host} has nothing there ({})",
                    self.shown(),
                    crate::name::escaped(path),
                    response.status
                ),
            )
            .help(
                "the folder may have changed while fetchloom read it: run the same command again",
            )),
            401 | 403 => Err(super::refused(host, &asked)),
            status => Err(Error::new(
                Kind::NetworkStatus,
                format!("{host} answered {status} for {asked}"),
            )),
        }
    }

    /// the listing `text` holds of the folder at `path`, asked for at `url`, which `response`
    /// answered: refused when it is none, names a file as no file may be named, or came from
    /// outside the folder after a redirect.
    fn listed(
        &self,
        text: &str,
        response: &Response,
        url: &Url,
        path: &str,
    ) -> Result<Index, Error> {
        let index = match read(text, &response.url, url) {
            Ok(index) => index,
            Err(Unread::NoListing) if path.is_empty() => {
                return Err(no_listing(&self.url, PAGE_FLAG));
            }
            Err(Unread::NoListing) => {
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "{} lists the folder {}, whose page is no folder listing fetchloom reads",
                        self.shown(),
                        crate::name::escaped(path)
                    ),
                )
                .help("select the folders you need with --select, which leaves that one out"));
            }
            Err(Unread::Name(name)) => {
                let at = crate::name::joined(path, &name);
                let reason = if name.contains('/') {
                    "its name holds a /"
                } else {
                    crate::name::escapes(&name).unwrap_or("no file may have that name")
                };
                return Err(super::refused_name(&self.shown(), &at, reason));
            }
        };
        let inside =
            response.url.origin() == url.origin() && response.url.path().starts_with(url.path());
        if !inside {
            return Err(Error::new(
                Kind::SourceInvalid,
                format!(
                    "{} sends fetchloom on to {}, outside the folder",
                    Shown(url),
                    Shown(&response.url)
                ),
            )
            .help("fetchloom reads no listing outside the folder it was given. if that is the same folder, give its link instead"));
        }
        Ok(index)
    }

    fn shown(&self) -> String {
        Shown(&self.url).to_string()
    }
}

impl fmt::Display for Folder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.url)
    }
}

/// the refusal of the web page `link`, a folder's link, answered with that is no listing
/// fetchloom reads, with `asked` saying how to keep the page itself.
pub(crate) fn no_listing(link: &Url, asked: &str) -> Error {
    Error::new(
        Kind::SourceInvalid,
        format!(
            "{} answered with a web page, not a file or a folder listing fetchloom reads",
            Shown(link)
        ),
    )
    .help(format!(
        "fetchloom reads the folder listings apache, nginx, lighttpd and python's http.server make, and this page is none of them. open it in a browser, copy the link of each file you want, and give fetchloom those links. if the page itself is the data, {asked}"
    ))
}

/// a folder's walk: the files listed so far, the folders still to list, by path and link, and
/// the patterns whose files are wanted, none when every file is.
struct Walk<'a> {
    folder: &'a Folder,
    most: usize,
    select: &'a [Glob],
    files: Vec<Remote>,
    folders: VecDeque<(String, Url)>,
    /// the folders listed or waiting to be, which count against the most files as files do.
    counted: usize,
    /// what each folder that holds a folder lists, by its path, to tell one that lists what a
    /// folder it lies in does.
    seen: HashMap<String, u64>,
    warnings: Vec<String>,
}

impl Walk<'_> {
    /// takes in `index`, the listing of the folder at `path`, at `url`, queueing its folders.
    fn add(&mut self, path: &str, url: &Url, index: Index) -> Result<(), Error> {
        self.loops(path, &index)?;
        if !index.elsewhere.is_empty() {
            self.warnings.push(format!(
                "the listing at {} links {} outside its folder, which fetchloom leaves out",
                Shown(url),
                crate::units::some_of(&index.elsewhere, 3)
            ));
        }
        for entry in index.entries {
            let at = crate::name::joined(path, &entry.name);
            if entry.folder {
                if self.opens(&at) {
                    self.counted += 1;
                    self.folders.push_back((at, entry.link));
                }
            } else if self.keeps(&at) {
                // apache may end a row with a file's description, so a row's last word is no
                // size to hold a download to
                self.files.push(Remote {
                    path: at,
                    size: None,
                    sha256: None,
                    md5: None,
                    url: entry.link,
                    named_by_server: false,
                    etag: None,
                    blob: None,
                });
            }
            if self.files.len() + self.counted > self.most {
                return Err(super::too_many(
                    self.folder.shown(),
                    self.most,
                    "select the folders you need with --select",
                ));
            }
        }
        Ok(())
    }

    /// refuses a folder whose listing names what a folder it lies in names, as a link on the
    /// server back to a folder above does, which would list without end.
    fn loops(&mut self, path: &str, index: &Index) -> Result<(), Error> {
        if !index.entries.iter().any(|entry| entry.folder) {
            return Ok(());
        }
        let mut names: Vec<(&str, bool)> = index
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.folder))
            .collect();
        names.sort_unstable();
        let mut hasher = DefaultHasher::new();
        names.hash(&mut hasher);
        let print = hasher.finish();
        let mut above = path;
        while !above.is_empty() {
            above = above.rsplit_once('/').map_or("", |(parent, _)| parent);
            if self.seen.get(above) == Some(&print) {
                let ancestor = if above.is_empty() {
                    "the folder itself".to_owned()
                } else {
                    crate::name::escaped(above).into_owned()
                };
                return Err(Error::new(
                    Kind::SourceInvalid,
                    format!(
                        "{} never ends: its folder {} lists what {ancestor} lists, as a link on the server back to a folder above does",
                        self.folder.shown(),
                        crate::name::escaped(path)
                    ),
                )
                .help("select the folders you need with --select, which leaves that one out"));
            }
        }
        self.seen.insert(path.to_owned(), print);
        Ok(())
    }

    /// whether the folder at `path` may hold a wanted file, at any depth.
    fn opens(&self, path: &str) -> bool {
        self.select.is_empty() || self.select.iter().any(|glob| glob.may_reach(path))
    }

    /// whether the file at `path` lies in the folders a pattern's scope names, so the listing
    /// holds every file of the folders it names.
    fn keeps(&self, path: &str) -> bool {
        self.select.is_empty() || self.select.iter().any(|glob| glob.scope_holds(path))
    }
}

#[cfg(test)]
mod tests;
