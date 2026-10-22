//! Directory index pages as web servers write them: the entries a page lists directly under
//! its own location, and nothing outside it.

use std::collections::HashSet;

use fl_transfer::url::SafeUrl;
use url::Url;

use crate::http::decode;

/// One entry an index page lists directly under its own location.
pub(crate) struct Link {
    /// The entry's location: the page's location joined with the link.
    pub(crate) url: SafeUrl,
    /// Its name, percent escapes decoded, without the trailing slash of a directory.
    pub(crate) name: String,
    /// It is a directory: its link ends with a slash.
    pub(crate) dir: bool,
}

/// The entries `html`, served at `base`, lists directly under `base`, in page order, each once.
pub(crate) fn links(base: &SafeUrl, html: &str) -> Vec<Link> {
    let Some(base_url) = directory(base) else {
        return Vec::new();
    };
    let mut found: Vec<Link> = Vec::new();
    let mut names = HashSet::new();
    for href in hrefs(html) {
        if let Some(link) = entry(&base_url, &href)
            && names.insert(link.name.clone())
        {
            found.push(link);
        }
    }
    found
}

fn directory(base: &SafeUrl) -> Option<Url> {
    let mut url = Url::parse(&base.to_string()).ok()?;
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Some(url)
}

fn entry(base: &Url, href: &str) -> Option<Link> {
    let href = href.split('#').next().unwrap_or_default().trim();
    if href.is_empty() || href.contains('?') {
        return None;
    }
    let joined = base.join(href).ok()?;
    if joined.scheme() != base.scheme()
        || joined.host_str() != base.host_str()
        || joined.port_or_known_default() != base.port_or_known_default()
    {
        return None;
    }
    let rest = joined.path().strip_prefix(base.path())?;
    let (raw, dir) = match rest.strip_suffix('/') {
        Some(raw) => (raw, true),
        None => (rest, false),
    };
    if raw.contains('/') {
        return None;
    }
    let name = decode(raw)?;
    if name.is_empty() || name == "." || name == ".." || name.contains('/') {
        return None;
    }
    let url = SafeUrl::parse(joined.as_str()).ok()?;
    Some(Link { url, name, dir })
}

fn hrefs(html: &str) -> Vec<String> {
    let bytes = html.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(offset) = html[at..].find('<') {
        let start = at + offset;
        let rest = &html[start..];
        if rest.starts_with("<!--") {
            at = rest.find("-->").map_or(html.len(), |end| start + end + 3);
            continue;
        }
        let is_anchor = bytes
            .get(start + 1)
            .is_some_and(|byte| byte.eq_ignore_ascii_case(&b'a'))
            && bytes.get(start + 2).is_some_and(u8::is_ascii_whitespace);
        at = start + 1;
        if is_anchor {
            let (href, end) = anchor(html, start + 2);
            at = end;
            found.extend(href);
        }
    }
    found
}

fn anchor(html: &str, from: usize) -> (Option<String>, usize) {
    let bytes = html.as_bytes();
    let mut at = from;
    let mut href = None;
    loop {
        while at < bytes.len() && (bytes[at].is_ascii_whitespace() || bytes[at] == b'/') {
            at += 1;
        }
        if at >= bytes.len() {
            return (href, at);
        }
        if bytes[at] == b'>' {
            return (href, at + 1);
        }
        let name_start = at;
        while at < bytes.len()
            && !bytes[at].is_ascii_whitespace()
            && !matches!(bytes[at], b'=' | b'>' | b'/')
        {
            at += 1;
        }
        let name = &html[name_start..at];
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if bytes.get(at) != Some(&b'=') {
            continue;
        }
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let value_start;
        let value_end;
        if let Some(&quote @ (b'"' | b'\'')) = bytes.get(at) {
            value_start = at + 1;
            value_end = html[value_start..]
                .find(char::from(quote))
                .map_or(html.len(), |end| value_start + end);
            at = (value_end + 1).min(html.len());
        } else {
            value_start = at;
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'>' {
                at += 1;
            }
            value_end = at;
        }
        if href.is_none() && name.eq_ignore_ascii_case("href") {
            href = Some(decode_entities(&html[value_start..value_end]));
        }
    }
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        if let Some((decoded, used)) = entity(rest) {
            out.push(decoded);
            rest = &rest[used..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn entity(text: &str) -> Option<(char, usize)> {
    let end = text.find(';').filter(|end| *end <= 10)?;
    let body = &text[1..end];
    let decoded = match body {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let digits = body.strip_prefix('#')?;
            let code = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => digits.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(text: &str) -> SafeUrl {
        SafeUrl::parse(text).unwrap()
    }

    fn names(links: &[Link]) -> Vec<(&str, bool)> {
        links
            .iter()
            .map(|link| (link.name.as_str(), link.dir))
            .collect()
    }

    #[test]
    fn apache_lists_dirs_and_files_only() {
        let html = include_str!("../../../fixtures/responses/http/apache.html");
        let found = links(&base("https://archive.example.org/dist/ant/"), html);
        let listed = names(&found);
        assert_eq!(listed[0], ("antidote", true));
        assert_eq!(listed[7], ("source", true));
        assert_eq!(listed[8], ("KEYS", false));
        assert!(listed.contains(&("test.foo", false)));
        assert!(listed.iter().all(|(name, _)| !name.contains('?')));
        assert_eq!(
            found[0].url.to_string(),
            "https://archive.example.org/dist/ant/antidote/"
        );
    }

    #[test]
    fn apache_skips_external_and_parent_links() {
        let html = include_str!("../../../fixtures/responses/http/apache.html");
        let found = links(&base("https://archive.example.org/dist/ant/"), html);
        assert!(
            found
                .iter()
                .all(|link| link.url.host() == Some("archive.example.org"))
        );
        assert!(found.iter().all(|link| link.name != "Parent Directory"));
        assert!(found.iter().all(|link| link.name != "dist"));
    }

    #[test]
    fn nginx_drops_parent_link() {
        let html = include_str!("../../../fixtures/responses/http/nginx.html");
        let found = links(&base("http://nginx.example.org/download/"), html);
        assert_eq!(found[0].name, "nginx-0.1.0.tar.gz");
        assert!(found.iter().all(|link| link.name != ".." && !link.dir));
        assert!(found.len() > 10);
    }

    #[test]
    fn python_decodes_escapes() {
        let html = include_str!("../../../fixtures/responses/http/python.html");
        let found = links(&base("http://127.0.0.1:8000/"), html);
        assert_eq!(
            names(&found),
            [
                ("a.txt", false),
                ("b c&d#e.txt", false),
                ("lnk", false),
                ("sp ace", true),
                ("sub", true),
            ]
        );
    }

    #[test]
    fn python_keeps_escapes_in_urls() {
        let html = include_str!("../../../fixtures/responses/http/python.html");
        let found = links(&base("http://127.0.0.1:8000/"), html);
        assert_eq!(
            found[1].url.to_string(),
            "http://127.0.0.1:8000/b%20c%26d%23e.txt"
        );
    }

    #[test]
    fn hand_written_page_keeps_only_direct_children() {
        let html = concat!(
            "<A HREF=\"https://other.example.org/p/q.txt\">far</A>",
            "<a href=\"/\">root</a>",
            "<a href=\"a/b/c.txt\">deep</a>",
            "<a href=\"q.txt\">q</a>",
            "<a class=x href=\"q.txt\">again</a>",
            "<a href=\"x&amp;y.txt\">x</a>",
            "<a href='single.txt' id=1>s</a>",
            "<a id=2 href=plain.txt>u</a>",
            "<a href=\"sub/#top\">sub</a>",
            "<a href=\"../up.txt\">up</a>",
            "<a href=\"?C=N;O=D\">sort</a>",
            "<a href=\"bad%ff.txt\">bad</a>",
            "<a href=\"bad%zz.txt\">bad</a>",
        );
        let found = links(&base("https://h.example.org/p"), html);
        assert_eq!(
            names(&found),
            [
                ("q.txt", false),
                ("x&y.txt", false),
                ("single.txt", false),
                ("plain.txt", false),
                ("sub", true),
            ]
        );
    }

    #[test]
    fn other_port_is_rejected() {
        let found = links(
            &base("http://h.example.org:8000/p/"),
            "<a href=\"http://h.example.org:9000/p/x\">x</a><a href=\"http://h.example.org:8000/p/y\">y</a>",
        );
        assert_eq!(names(&found), [("y", false)]);
    }

    #[test]
    fn numeric_entities_decode() {
        let found = links(
            &base("http://h.example.org/"),
            "<a href=\"a&#39;b&#x2d;c.txt\">x</a>",
        );
        assert_eq!(names(&found), [("a'b-c.txt", false)]);
    }
}
