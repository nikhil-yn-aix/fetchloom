//! folders a web server lists, each one dataset by its link ending in `/`: every file in it and
//! in the folders inside it, at its path there, pinned by the SHA-256 its download gives. one
//! reader takes the listings apache, nginx, lighttpd, caddy, rclone and python's http.server
//! write, under any title a site gives apache's, NCBI's that GEO's folders have among them. a
//! listing is a page anyone could have written, so only links on the folder's own origin and
//! below it are followed, each once, and a name no file may have stops it.

use std::cmp::Reverse;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::future::Future;
use std::hash::{Hash as _, Hasher as _};
use std::pin::Pin;
use std::task::Poll;

use percent_encoding::percent_decode_str;
use url::Url;

use super::drive::html::unescape;
use super::{Access, Listing, Reference, Remote};
use crate::error::{Error, Kind};
use crate::glob::Glob;
use crate::net::{Api, Extra, Response};
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

/// what a request for a listing accepts: a web page before anything else, since caddy answers a
/// request that prefers json, as fetchloom's do, with its listing as json.
const LISTING: Extra = &[("accept", "text/html, */*;q=0.1")];

/// the elements a server writes the entries of a listing in: nginx's and apache's `<pre>`,
/// apache's and lighttpd's `<table>`, python's `<ul>`.
const LISTS: [&str; 3] = ["pre", "table", "ul"];

/// one entry of a listing: a file, or a folder, whose link ends in `/`, by its name with its
/// character references and percent escapes undone, its link, and the size in bytes its row
/// states, as nginx's `621` at a row's end or ncei's in a size column, or else about the bytes of
/// one it rounds to a unit, as apache's `1.3M`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) name: String,
    pub(super) link: Url,
    pub(super) folder: bool,
    pub(super) size: Option<u64>,
    pub(super) about: Option<u64>,
}

/// the size a row of a listing states: in bytes, as nginx's `621`, or rounded to a unit, as
/// apache's `1.3M`, about this many bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stated {
    Bytes(u64),
    About(u64),
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
/// it makes against the page, none for one that makes none, whether its text reads "Parent
/// Directory" and whether it reads as a way up at all, and the size its row states. a link whose
/// text names a folder, ending in `/`, while its `href` does not, as ncei's do, makes the
/// folder's link, with the `/`.
struct Link {
    href: String,
    url: Option<Url>,
    names_parent: bool,
    goes_up: bool,
    size: Option<Stated>,
}

impl Link {
    /// the link `anchor` makes against `page`.
    fn of(anchor: Anchor, page: &Url) -> Link {
        let slashed = anchor.names_folder && !anchor.href.is_empty() && !anchor.href.ends_with('/');
        let url = if slashed {
            page.join(&format!("{}/", anchor.href))
        } else {
            page.join(&anchor.href)
        };
        Link {
            url: url.ok(),
            href: anchor.href,
            names_parent: anchor.names_parent,
            goes_up: anchor.goes_up,
            size: anchor.size,
        }
    }
}

/// why a page is no listing to read.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unread {
    /// no server made it as a listing: nothing on it says it lists the folder, or it has no list
    /// of links, or one that does not end, as a page cut short.
    NoListing,
    /// an entry has a name no file may have, decoded.
    Name(String),
}

/// reads `html`, the page the link `page` answered with, as a listing of `folder`: the links in
/// its list of entries, their character references undone, each read against `page`.
pub(super) fn read(html: &str, page: &Url, folder: &Url) -> Result<Index, Unread> {
    let lower = html.to_ascii_lowercase();
    let parent = folder.join("..").ok();
    // each link of each list, read against the page, but those a query or a fragment marks
    let lists: Vec<Vec<Link>> = lists(&lower)
        .into_iter()
        .map(|(tag, start, end)| {
            links(&html[start..end], &lower[start..end], tag)
                .into_iter()
                .filter(|anchor| !anchor.href.contains(['?', '#']))
                .map(|anchor| Link::of(anchor, page))
                .collect()
        })
        .collect();
    // the link to the folder above, as `../` or the parent's path, or `..` under words that go
    // up, as ncei's "Parent Directory": a file's name over a link up is no way up
    let above = |link: &Link| {
        parent.is_some() && link.url == parent && (link.href.ends_with('/') || link.goes_up)
    };
    let entries = |links: &[Link]| {
        links
            .iter()
            .filter(|link| {
                !above(link)
                    && link
                        .url
                        .as_ref()
                        .and_then(|url| below(url, folder))
                        .is_some()
            })
            .count()
    };
    // a page may show lists above its listing, as a notice or a trail of the folders above, so
    // the listing is the list with the most entries, then one that links the folder above
    let linked = lists.iter().filter(|links| !links.is_empty()).count();
    let chosen = lists
        .iter()
        .enumerate()
        .max_by_key(|(at, links)| {
            let count = if linked > 1 {
                entries(links)
            } else {
                links.len()
            };
            (count, links.iter().any(above), Reverse(*at))
        })
        .map(|(at, _)| at)
        .ok_or(Unread::NoListing)?;
    let listing = &lists[chosen];
    let parent_row = listing.iter().any(|link| link.names_parent && above(link));
    if !parent_row && !titled(html, &lower, folder, listing.iter().any(above)) {
        return Err(Unread::NoListing);
    }
    let mut index = Index::default();
    let mut seen = HashSet::new();
    for each in listing {
        if above(each) {
            continue;
        }
        let Link {
            href,
            url: Some(link),
            size,
            ..
        } = each
        else {
            index
                .elsewhere
                .push(crate::name::escaped(&each.href).into_owned());
            continue;
        };
        let Some((name, is_folder)) = below(link, folder) else {
            index.elsewhere.push(Shown(link).to_string());
            continue;
        };
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
                size: match size {
                    Some(Stated::Bytes(bytes)) => Some(*bytes),
                    _ => None,
                },
                about: match size {
                    Some(Stated::About(bytes)) => Some(*bytes),
                    _ => None,
                },
            });
        }
    }
    Ok(index)
}

/// the name, as `link` writes it, of the entry `link` makes of `folder`, and whether it is a
/// folder, its link ending in `/`: one on the folder's own origin, one step below its path. none
/// for any other link.
fn below<'a>(link: &'a Url, folder: &Url) -> Option<(&'a str, bool)> {
    let same_origin = link.scheme() == folder.scheme()
        && link.host_str() == folder.host_str()
        && link.port_or_known_default() == folder.port_or_known_default();
    let rest = link
        .path()
        .strip_prefix(folder.path())
        .filter(|rest| same_origin && !rest.is_empty())?;
    let (name, is_folder) = match rest.strip_suffix('/') {
        Some(name) => (name, true),
        None => (rest, false),
    };
    (!name.contains('/')).then_some((name, is_folder))
}

/// whether the page says it lists `folder` in words: its title or first heading begins as a
/// server's listing does, as `Index of /data/`, or its title is the folder's path, decoded, as
/// rclone's `/data` is, or, where its list `links_above` the folder, the folder's name, as
/// caddy's `data` is.
fn titled(html: &str, lower: &str, folder: &Url, links_above: bool) -> bool {
    let begins = |text: &String| {
        let text = text.to_lowercase();
        TITLES.iter().any(|words| text.starts_with(words))
    };
    let title = inner(html, lower, "title");
    if title.as_ref().is_some_and(begins) || inner(html, lower, "h1").as_ref().is_some_and(begins) {
        return true;
    }
    let Some(title) = title else {
        return false;
    };
    let path = words(&percent_decode_str(folder.path()).decode_utf8_lossy());
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    (title.starts_with('/') && title.trim_end_matches('/') == path.trim_end_matches('/'))
        || (links_above && !name.is_empty() && title == name)
}

/// the text of the first element `tag` of the page, without the tags inside it, its character
/// references undone and its spaces as one; `lower` is the page in lowercase.
fn inner(html: &str, lower: &str, tag: &str) -> Option<String> {
    let open = opening(lower, 0, tag)?;
    let start = lower[open..].find('>').map(|end| open + end + 1)?;
    let end = lower[start..]
        .find(&format!("</{tag}"))
        .map(|end| start + end)?;
    Some(stripped(html, lower, start, end))
}

/// the text of `html` from `from` to `to`, without its tags, its character references undone and
/// its spaces as one; `lower` is `html` in lowercase.
fn stripped(html: &str, lower: &str, from: usize, to: usize) -> String {
    let mut text = String::new();
    let mut at = from;
    while let Some(offset) = lower[at..to].find('<') {
        text.push_str(&html[at..at + offset]);
        at = lower[at + offset..to]
            .find('>')
            .map_or(to, |close| at + offset + close + 1);
    }
    text.push_str(&html[at..to]);
    words(&unescape(&text))
}

/// `text` with its spaces as one, none at its ends.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// where each list of the lowercase page `lower` holds its entries, between its tags, in order,
/// up to its last whole list, so a list a page cut short leaves open is none. one pass reads
/// the page, so a page of many lists takes no longer than its length.
fn lists(lower: &str) -> Vec<(&'static str, usize, usize)> {
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
        found.push((*tag, start, end));
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

/// a link as a list writes it: its `href`, its character references undone, what its text says,
/// and the size its row states.
struct Anchor {
    href: String,
    names_folder: bool,
    names_parent: bool,
    goes_up: bool,
    size: Option<Stated>,
}

/// each link in `list`, a list written as the element `tag`, in order; `lower` is the list in
/// lowercase. a row's size is its own when a table heads a column "Size", or in a `<pre>` list
/// whose first line heads no description and none of whose rows ends in words, as apache's
/// descriptions, which may end in a number too, would end them. one pass reads the list.
fn links(list: &str, lower: &str, tag: &str) -> Vec<Anchor> {
    let mut described =
        tag == "pre" && lower[..lower.find('\n').unwrap_or(lower.len())].contains("description");
    let mut rows = (tag == "table").then(Rows::default);
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = next_byte(lower, b'<', at, lower.len()) {
        if !starts_tag(lower, open, "a") {
            if let Some(rows) = &mut rows {
                rows.tag(list, lower, open, &mut found);
            }
            at = open + 1;
            continue;
        }
        let (href, end) = href(list, lower, open + 2);
        if let Some(href) = href {
            // where the link's text ends, when no tag sits in it, as in most
            let close = next_byte(lower, b'<', end, lower.len())
                .filter(|&close| starts_tag(lower, close, "/a"));
            let Says {
                names_folder,
                names_parent,
                goes_up,
            } = text_says(list, lower, end, close);
            let size = match (tag == "pre").then(|| ending(lower, close)) {
                Some(Ending::Size(size)) => Some(size),
                Some(Ending::Words) => {
                    described = true;
                    None
                }
                _ => None,
            };
            found.push(Anchor {
                href: unescape(href),
                names_folder,
                names_parent,
                goes_up,
                size,
            });
        }
        at = end;
    }
    if let Some(rows) = &mut rows {
        rows.end(list, lower, lower.len(), &mut found);
    }
    if described {
        for anchor in &mut found {
            anchor.size = None;
        }
    }
    found
}

/// a table's rows as [`links`] reads them, in its pass: the column the table heads "Size", as
/// apache, lighttpd, rclone and caddy head theirs, once its heading is read, and the row being
/// read. a cell there that is a size, as `3659` or `9.3K`, is the size of the row's links; any
/// other, as `-`, states none, and so do rows above the heading.
#[derive(Default)]
struct Rows {
    column: Option<usize>,
    /// where the row's cells open, while no heading was read.
    cells: Vec<usize>,
    /// how many of the row's cells opened so far.
    opened: usize,
    /// where the row's cell in the size column opens, until it ends, and the size it holds.
    sized: Option<usize>,
    size: Option<Stated>,
    /// the row's first link among those found.
    first: usize,
}

impl Rows {
    /// takes in the tag that opens at `open` of the lowercase table `lower`: a row's or a cell's.
    fn tag(&mut self, list: &str, lower: &str, open: usize, found: &mut [Anchor]) {
        // most tags of a row close something, as `</td>`, so one byte rules them out
        if lower.as_bytes().get(open + 1) != Some(&b't') {
            return;
        }
        if starts_tag(lower, open, "tr") {
            self.end(list, lower, open, found);
        } else if starts_tag(lower, open, "td") || starts_tag(lower, open, "th") {
            self.close(lower, open);
            if self.column.is_none() {
                self.cells.push(open);
            } else if self.column == Some(self.opened) {
                self.sized = Some(open);
            }
            self.opened += 1;
        }
    }

    /// ends the row at `end`, giving its links its size, or reading the heading in it, and starts
    /// the next row there.
    fn end(&mut self, list: &str, lower: &str, end: usize, found: &mut [Anchor]) {
        self.close(lower, end);
        if let Some(size) = self.size.take() {
            for anchor in &mut found[self.first..] {
                anchor.size = Some(size);
            }
        }
        if self.column.is_none() {
            let cells = &self.cells;
            self.column = (0..cells.len()).find(|&index| {
                let (start, stop) = (cells[index], cells.get(index + 1).copied().unwrap_or(end));
                starts_tag(lower, start, "th")
                    && stripped(list, lower, start, stop)
                        .trim_end_matches(':')
                        .eq_ignore_ascii_case("size")
            });
        }
        self.cells.clear();
        self.opened = 0;
        self.first = found.len();
    }

    /// ends the size column's cell at `end`, if it is open, reading the size it holds.
    fn close(&mut self, lower: &str, end: usize) {
        if let Some(start) = self.sized.take() {
            self.size = cell(lower, start, end);
        }
    }
}

/// the words servers write on a link to the folder above, lowercase: apache's, nginx's, caddy's
/// and rclone's.
const UP: [&str; 5] = ["parent directory", "..", "../", "up", "go up"];

/// what the text of a link says: whether it ends in `/`, as a folder's name does, whether it
/// reads "Parent Directory", as apache's link to the folder above does, and whether it reads as
/// any server's link to the folder above.
struct Says {
    names_folder: bool,
    names_parent: bool,
    goes_up: bool,
}

impl Says {
    /// what `text`, its spaces as one and none at its ends, says.
    fn of(text: &str) -> Says {
        Says {
            names_folder: text.ends_with('/'),
            names_parent: text.eq_ignore_ascii_case(UP[0]),
            goes_up: UP.iter().any(|up| text.eq_ignore_ascii_case(up)),
        }
    }
}

/// what the text of the link whose tag ends at `at` in `list` says; `lower` is the list in
/// lowercase. a text that `close`s with no tag in it and is [`plain`], as most are, is read where
/// it is.
fn text_says(list: &str, lower: &str, at: usize, close: Option<usize>) -> Says {
    if let Some(close) = close {
        let text = list[at..close].trim_ascii();
        if plain(text) {
            return Says::of(text);
        }
    }
    Says::of(&words(&unescape(&text(list, lower, at))))
}

/// whether `text` reads the same with its character references undone and its spaces as one: it
/// holds no `&`, line break or tab and no two spaces in a row. one pass over its bytes tells, as
/// `str::contains("  ")` on a text this short calls `memcmp` for each pair of its bytes.
fn plain(text: &str) -> bool {
    let mut space = false;
    text.bytes().all(|byte| {
        let run = space && byte == b' ';
        space = byte == b' ';
        !run && !matches!(byte, b'&' | b'\n' | b'\r' | b'\t')
    })
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

/// what a row of a `<pre>` list holds after a link: its last word up to the next tag or line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ending {
    /// a size, in bytes as NCBI and nginx write `621`, or rounded as apache writes `1.3M`.
    Size(Stated),
    /// words, as apache's description of a file, so no row's last word is sure to be its size.
    Words,
    /// a folder's `-`, nothing, or a link whose text holds a tag.
    Other,
}

/// what the row of the lowercase list `lower` whose link's text `close`s with no tag in it ends
/// with; a link whose text holds a tag states none.
fn ending(lower: &str, close: Option<usize>) -> Ending {
    let Some(rest) = close.and_then(|close| lower[close..].strip_prefix("</a>")) else {
        return Ending::Other;
    };
    let words = &rest[..rest
        .bytes()
        .position(|byte| byte == b'<' || byte == b'\n')
        .unwrap_or(rest.len())];
    let Some(last) = words.split_ascii_whitespace().next_back() else {
        return Ending::Other;
    };
    if let Some(size) = sized(last) {
        Ending::Size(size)
    } else if last == "-" {
        Ending::Other
    } else {
        Ending::Words
    }
}

/// the size `text`, in lowercase, states: a whole number of bytes, as `3659`, or about the bytes
/// of one rounded to a unit, as `9.3k`.
fn sized(text: &str) -> Option<Stated> {
    whole(text)
        .map(Stated::Bytes)
        .or_else(|| about(text).map(Stated::About))
}

/// the bytes `text` states when it is a whole number, as `3659`.
fn whole(text: &str) -> Option<u64> {
    (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| text.parse().ok())
        .flatten()
}

/// about how many bytes `word`, in lowercase, states, a size rounded to a unit, as apache and
/// nginx write `9.3K` or `34M` and caddy `1.3 KiB`. a K is 1024 bytes, as those servers count it,
/// however written.
fn about(word: &str) -> Option<u64> {
    let number = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let power = match &word[number.len()..] {
        "b" => 0,
        "k" | "kb" | "kib" => 1,
        "m" | "mb" | "mib" => 2,
        "g" | "gb" | "gib" => 3,
        "t" | "tb" | "tib" => 4,
        "p" | "pb" | "pib" => 5,
        _ => return None,
    };
    let unit = 1024_u64.pow(power);
    let number = number.trim_end();
    let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
    if whole.is_empty()
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    // servers round to a tenth, so places past the third change no estimate
    let fraction = &fraction[..fraction.len().min(3)];
    let part = match fraction {
        "" => 0,
        digits => {
            digits.parse::<u64>().ok()? * unit / 10_u64.pow(u32::try_from(digits.len()).ok()?)
        }
    };
    whole
        .parse::<u64>()
        .ok()?
        .checked_mul(unit)?
        .checked_add(part)
}

/// the size a cell that opens at `open` and ends by `to` holds as its one text, as `3659` or
/// `9.3k`; `lower` is the table in lowercase.
fn cell(lower: &str, open: usize, to: usize) -> Option<Stated> {
    let mut at = next_byte(lower, b'>', open, to).map_or(to, |end| end + 1);
    let mut found = None;
    while at < to {
        let next = next_byte(lower, b'<', at, to).unwrap_or(to);
        let text = lower[at..next].trim_ascii();
        if !text.is_empty() {
            if found.is_some() {
                return None;
            }
            found = Some(sized(text)?);
        }
        at = next_byte(lower, b'>', next, to).map_or(to, |close| close + 1);
    }
    found
}

/// where the next `byte` of `lower` is, from `at` up to `to`. tags sit a few bytes apart in a
/// listing, closer than a search that skips ahead in long strides gains on.
fn next_byte(lower: &str, byte: u8, at: usize, to: usize) -> Option<usize> {
    lower.as_bytes()[at..to]
        .iter()
        .position(|&each| each == byte)
        .map(|offset| at + offset)
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
        let response = api.get_with(&self.url, LISTING).await?;
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
            rounded: HashMap::new(),
        };
        walk.add("", &self.url, index)?;
        let window = api.listing_window(&self.url);
        if window > 1 {
            self.list_together(api, &mut walk, window).await?;
        }
        while let Some((path, url)) = walk.folders.pop_front() {
            let response = api.get_with(&url, LISTING).await?;
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
            rounded: (!walk.rounded.is_empty()).then_some(walk.rounded),
        })
    }

    /// lists the folders `walk` queues, `window` at once, reading each answer in the order the
    /// walk queued its folder, so the walk finds, warns of and stops at what it would asking for
    /// one listing at a time.
    async fn list_together(
        &self,
        api: &impl Api,
        walk: &mut Walk<'_>,
        window: usize,
    ) -> Result<(), Error> {
        let ask = |path: String, url: Url| Asked {
            answer: None,
            asking: Box::pin(async move {
                let response = api.get_with(&url, LISTING).await;
                (path, url, response)
            }),
        };
        let mut asked = VecDeque::new();
        loop {
            while asked.len() < window
                && let Some((path, url)) = walk.folders.pop_front()
            {
                asked.push_back(ask(path, url));
            }
            if asked.is_empty() {
                return Ok(());
            }
            let (path, url, response) = std::future::poll_fn(|cx| {
                for each in &mut asked {
                    each.poll(cx);
                }
                match asked.front_mut().and_then(|first| first.answer.take()) {
                    Some(answer) => {
                        asked.pop_front();
                        Poll::Ready(answer)
                    }
                    None => Poll::Pending,
                }
            })
            .await;
            let response = response?;
            self.answered(&response, &url, &path)?;
            let index = self.listed(&response.text(), &response, &url, &path)?;
            walk.add(&path, &url, index)?;
        }
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
        let inside =
            response.url.origin() == url.origin() && response.url.path().starts_with(url.path());
        let index = match read(text, &response.url, url) {
            Ok(index) if inside => index,
            Ok(_) => return Err(sent_outside(url, &response.url)),
            // a redirect can hide a listing its site titled, as http to https does, so a page from
            // outside is told by where it came from. a forge's page is none wherever it came from
            Err(Unread::NoListing) if path.is_empty() => {
                return Err(if inside || forge(url).is_some() {
                    no_listing(&self.url, PAGE_FLAG)
                } else {
                    sent_outside(url, &response.url)
                });
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

/// the refusal of the folder `url`, whose server sent fetchloom on to `answered`, outside it, as
/// to the page of an S3 bucket, whose folders fetchloom lists by their s3 links.
fn sent_outside(url: &Url, answered: &Url) -> Error {
    let sent = format!("{} sends fetchloom on to {}", Shown(url), Shown(answered));
    if let Some(bucket) = bucket_of(answered) {
        return Error::new(
            Kind::SourceInvalid,
            format!("{sent}, a page of the S3 bucket {bucket}, not a folder listing"),
        )
        .help(format!(
            "fetchloom lists a bucket's folder by its s3 link: run `{}` with the folder's path in the bucket, which that page shows",
            crate::suggested::command(&format!("add s3://{bucket}/<folder>/"))
        ));
    }
    Error::new(Kind::SourceInvalid, format!("{sent}, outside the folder"))
        .help("fetchloom reads no listing outside the folder it was given. if that is the same folder, give its link instead")
}

/// the refusal of the web page `link`, a folder's link, answered with that is no listing
/// fetchloom reads, with `asked` saying how to keep the page itself. a git repository's page on a
/// forge fetchloom reads no folders of says what works there instead.
pub(crate) fn no_listing(link: &Url, asked: &str) -> Error {
    if let Some(host) = forge(link) {
        return Error::new(
            Kind::SourceInvalid,
            format!(
                "{} answered with a web page of a git repository on {host}, not a file or a folder listing",
                Shown(link)
            ),
        )
        .help(format!(
            "fetchloom reads the folders of github and gitlab repositories. for one on {host}, give fetchloom the repository's archive link, the .zip or .tar.gz of a branch its page offers to download, or clone the repository with git, then pin the folder from your clone with `{}`",
            crate::suggested::command("add <folder> --as <name>")
        ));
    }
    Error::new(
        Kind::SourceInvalid,
        format!(
            "{} answered with a web page, not a file or a folder listing fetchloom reads",
            Shown(link)
        ),
    )
    .help(format!(
        "fetchloom reads the folder listings apache, nginx, lighttpd, caddy, rclone and python's http.server make, and this page is none of them. open it in a browser, copy the link of each file you want, and give fetchloom those links. if the page itself is the data, {asked}"
    ))
}

/// the host of the forge whose git repository's page `link` is, as its path shows one: gitea's and
/// forgejo's, as codeberg's `/<owner>/<repo>/src/branch/<branch>/…`, on any host; gogs's,
/// gin's and bitbucket's `/<owner>/<repo>/src/<ref>/…` on their own hosts; and sourcehut's
/// `/~<owner>/<repo>/tree/…`. fetchloom reads the folders of none of them.
pub(crate) fn forge(link: &Url) -> Option<&str> {
    let host = link.host_str()?;
    let parts: Vec<&str> = link.path_segments()?.collect();
    let page = match parts.as_slice() {
        [_, _, "src", "branch" | "tag" | "commit", ..] => true,
        [_, _, "src", _, ..] => matches!(
            host,
            "bitbucket.org" | "gin.g-node.org" | "codeberg.org" | "gitea.com" | "notabug.org"
        ),
        [owner, _, "tree", ..] => host == "git.sr.ht" && owner.starts_with('~'),
        _ => false,
    };
    page.then_some(host)
}

/// the S3 bucket whose page `url` is, as `noaa-global-hourly-pds.s3.amazonaws.com/index.html`
/// and `s3.amazonaws.com/noaa-global-hourly-pds/index.html` are that bucket's, by a name S3
/// allows, which a shell runs nothing in.
fn bucket_of(url: &Url) -> Option<&str> {
    let (named, _) = crate::net::s3::s3_host(url.host_str()?)?;
    let bucket = named.or_else(|| {
        let mut path = url.path_segments()?;
        path.next().filter(|_| path.next().is_some())
    })?;
    crate::net::s3::is_bucket(bucket).then_some(bucket)
}

/// a listing [`Folder::list_together`] asked for: the request, until its answer comes.
struct Asked<F: Future> {
    asking: Pin<Box<F>>,
    answer: Option<F::Output>,
}

impl<F: Future> Asked<F> {
    /// lets the request make progress, keeping its answer once it comes.
    fn poll(&mut self, cx: &mut std::task::Context<'_>) {
        if self.answer.is_none()
            && let Poll::Ready(answer) = self.asking.as_mut().poll(cx)
        {
            self.answer = Some(answer);
        }
    }
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
    /// about how many bytes each file is whose size its listing rounds, by its path.
    rounded: HashMap<String, u64>,
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
                if let Some(about) = entry.about {
                    self.rounded.insert(at.clone(), about);
                }
                self.files.push(Remote {
                    path: at,
                    size: entry.size,
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
