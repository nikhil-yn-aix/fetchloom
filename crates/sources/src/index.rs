//! Recognizing a directory index before parsing it.

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::source::{Listing, ListingEntry};

const OBJECT_STORE_NAMESPACE: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

const OBJECT_STORE_ROOT: &str = "<ListBucketResult";

const WEBDAV_NAMESPACE: &str = "DAV:";

const GENERATED_HEADING: &str = "<h1>Index of ";

pub fn parse(location: &str, status: u16, body: &str) -> Result<Listing, Error> {
    if body.contains(OBJECT_STORE_ROOT) && body.contains(OBJECT_STORE_NAMESPACE) {
        return Ok(entries(location, &between_all(body, "<Key>", "</Key>")));
    }
    if status == 207 && body.contains("multistatus") && body.contains(WEBDAV_NAMESPACE) {
        return Ok(entries(location, &elements(body, "href")));
    }
    if body.contains(GENERATED_HEADING) {
        return Ok(entries(location, &links(body)));
    }
    Err(Error::new(
        ErrorKind::ReferenceUnresolved,
        format!(
            "name the objects instead of the container, because {} answered with an index in no format Fetchloom recognizes: an object store list response, a multi-status response, or a generated index carrying its own heading",
            SafeUrl::new(location)
        ),
    )
    .with_source(location))
}

fn between_all(body: &str, open: &str, close: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find(open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(close) else {
            return found;
        };
        found.push(after[..end].to_owned());
        rest = &after[end..];
    }
    found
}

fn elements(body: &str, name: &str) -> Vec<String> {
    let open = format!("{name}>");
    let mut found = Vec::new();
    let mut rest = body;
    let mut consumed = 0usize;
    while let Some(start) = rest.find(&open) {
        let absolute = consumed + start;
        let opening = body[..absolute].ends_with('<')
            || body[..absolute]
                .rsplit_once('<')
                .is_some_and(|(_, prefix)| !prefix.starts_with('/') && !prefix.contains('>'));
        let after = &rest[start + open.len()..];
        consumed = absolute + open.len();
        rest = after;
        if !opening {
            continue;
        }
        let Some(end) = after.find('<') else {
            return found;
        };
        found.push(after[..end].to_owned());
        consumed += end;
        rest = &after[end..];
    }
    found
}

fn links(body: &str) -> Vec<String> {
    between_all(body, "<a href=\"", "\"")
}

fn entries(location: &str, raw: &[String]) -> Listing {
    let prefix = prefix_of(location);
    let mut kept = Vec::new();
    let mut skipped = 0u64;
    for name in raw {
        let Some(path) = relative(&prefix, name) else {
            skipped += 1;
            continue;
        };
        if path.is_empty() {
            continue;
        }
        if path.contains("..") {
            skipped += 1;
            continue;
        }
        kept.push(ListingEntry {
            location: SafeUrl::new(&format!("{location}{path}")),
            path,
            size: None,
            content: None,
            interop: None,
        });
    }
    Listing {
        entries: kept,
        skipped,
    }
}

fn prefix_of(location: &str) -> String {
    let after_scheme = location
        .split_once("://")
        .map_or(location, |(_, rest)| rest);
    match after_scheme.find('/') {
        Some(start) => after_scheme[start..].to_owned(),
        None => "/".to_owned(),
    }
}

fn relative(prefix: &str, name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.starts_with("../") || trimmed == "../" || trimmed == ".." {
        return None;
    }
    if trimmed.starts_with("//") || trimmed.contains("://") {
        return None;
    }
    let without_prefix = match trimmed
        .strip_prefix(prefix)
        .or_else(|| trimmed.strip_prefix(prefix.trim_start_matches('/')))
    {
        Some(rest) => rest,
        None if trimmed.starts_with('/') => return None,
        None => trimmed,
    };
    Some(without_prefix.trim_start_matches('/').to_owned())
}

#[cfg(test)]
#[expect(
    clippy::unwrap_used,
    reason = "test assertions, where the parse that failed is the message"
)]
mod tests {
    use super::parse;

    #[test]
    fn a_relative_escape_and_an_absolute_link_are_both_counted_as_skipped() {
        let body = concat!(
            "<html><body><h1>Index of /set/</h1><pre>",
            r#"<a href="../">../</a>"#,
            r#"<a href="one">one</a>"#,
            r#"<a href="http://elsewhere/object">elsewhere</a>"#,
            "</pre></body></html>"
        );
        let listing = parse("http://host/set/", 200, body).unwrap();
        let paths: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, vec!["one"]);
        assert_eq!(listing.skipped, 2);
    }

    #[test]
    fn a_link_resolving_to_the_container_itself_is_not_counted_as_skipped() {
        let body = concat!(
            "<html><body><h1>Index of /set/</h1><pre>",
            r#"<a href="/set/">/set/</a>"#,
            r#"<a href="one">one</a>"#,
            "</pre></body></html>"
        );
        let listing = parse("http://host/set/", 200, body).unwrap();
        let paths: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(paths, vec!["one"]);
        assert_eq!(listing.skipped, 0);
    }

    #[test]
    fn a_link_outside_the_prefix_is_ignored_and_counted() {
        let body = concat!(
            "<html><body><h1>Index of /set/</h1><pre>",
            r#"<a href="/other/thing">thing</a>"#,
            r#"<a href="//elsewhere/x">x</a>"#,
            r#"<a href="one">one</a>"#,
            "</pre></body></html>"
        );
        let listing = parse("http://host/set/", 200, body).unwrap();
        let paths: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec!["one"],
            "a link pointing outside the prefix became an entry naming a location nothing serves"
        );
        assert_eq!(
            listing.skipped, 2,
            "a link pointing outside the prefix was dropped without being counted"
        );
    }
}
