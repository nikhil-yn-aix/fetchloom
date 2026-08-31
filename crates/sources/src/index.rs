//! Recognizing a directory index before parsing it.

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::source::ListingEntry;

/// The namespace an object store's list response carries.
const OBJECT_STORE_NAMESPACE: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The root element an object store's list response carries.
const OBJECT_STORE_ROOT: &str = "<ListBucketResult";

/// The namespace a multi-status response carries.
const WEBDAV_NAMESPACE: &str = "DAV:";

/// The heading a generated index carries.
const GENERATED_HEADING: &str = "<h1>Index of ";

/// Reads a listing out of a body whose format is recognized.
///
/// # Errors
///
/// Fails with `reference.unresolved` when the body matches no recognized
/// signature.
pub fn parse(location: &str, status: u16, body: &str) -> Result<Vec<ListingEntry>, Error> {
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
            "name the objects instead of the container, because {location} answered with an index in no format Fetchloom recognizes: an object store list response, a multi-status response, or a generated index carrying its own heading"
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

fn entries(location: &str, raw: &[String]) -> Vec<ListingEntry> {
    let prefix = prefix_of(location);
    raw.iter()
        .filter_map(|name| relative(&prefix, name))
        .filter(|name| !name.is_empty() && !name.contains(".."))
        .map(|path| ListingEntry {
            location: SafeUrl::new(&format!("{location}{path}")),
            path,
            size: None,
        })
        .collect()
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
    let without_prefix = trimmed
        .strip_prefix(prefix)
        .or_else(|| trimmed.strip_prefix(prefix.trim_start_matches('/')))
        .unwrap_or(trimmed);
    let without_prefix = without_prefix.trim_start_matches('/');
    if without_prefix.starts_with("http") {
        return None;
    }
    Some(without_prefix.to_owned())
}
