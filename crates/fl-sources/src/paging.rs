//! The five ways an API spreads a listing over pages, and the walk that asks for each page
//! once and stops at the last.

use std::collections::HashSet;

use fl_transfer::client::Client;
use fl_transfer::fetch::{Page, Request};
use fl_transfer::url::{Origin, SafeUrl};
use serde_json::Value;

use crate::record;
use crate::source::SourceError;
use crate::template::{Query, plain};

/// The most pages one listing may have.
const PAGES: u64 = 100_000;

/// How a listing continues after a page.
#[derive(Debug)]
pub(crate) enum Paging {
    /// One page holds everything.
    None,
    /// A numbered page in the query parameter `param`, counted from `start`, until a page holds
    /// fewer than `size` items, or none when the size is not known.
    Page {
        param: String,
        start: u64,
        size: Option<usize>,
    },
    /// The token in `field` goes into the query parameter `param` of the first page's URL.
    Cursor { param: String, field: Query },
    /// The `Link` header's `rel="next"`.
    Link,
    /// The URL in `field`.
    Next { field: Query },
}

impl Paging {
    fn first(&self, url: &SafeUrl) -> SafeUrl {
        match self {
            Self::Page { param, start, .. } => url.with_query(param, &start.to_string()),
            _ => url.clone(),
        }
    }

    fn next(
        &self,
        first: &SafeUrl,
        number: u64,
        page: &Page,
        body: &Value,
        items: usize,
    ) -> Result<Option<SafeUrl>, String> {
        match self {
            Self::None => Ok(None),
            Self::Page { param, start, size } => {
                let last = items == 0 || size.is_some_and(|size| items < size);
                if last {
                    return Ok(None);
                }
                Ok(Some(first.with_query(param, &(start + number).to_string())))
            }
            Self::Cursor { param, field } => match token(field, body)? {
                Some(token) => Ok(Some(first.with_query(param, &token))),
                None => Ok(None),
            },
            Self::Link => page
                .headers("link")
                .find_map(next_link)
                .map(|link| join(&page.url, &link))
                .transpose(),
            Self::Next { field } => token(field, body)?
                .map(|next| join(&page.url, &next))
                .transpose(),
        }
    }
}

fn token(field: &Query, body: &Value) -> Result<Option<String>, String> {
    let found = field
        .optional(body)
        .map_err(|problem| format!("{problem} at {}", field.text))?;
    Ok(found.and_then(plain).filter(|text| !text.is_empty()))
}

fn join(base: &SafeUrl, link: &str) -> Result<SafeUrl, String> {
    base.join(link)
        .map_err(|_| "a next page that is not a URL".to_owned())
}

/// The target of the link with `rel="next"` in one `Link` header value.
fn next_link(value: &str) -> Option<String> {
    let mut rest = value;
    while let Some(open) = rest.find('<') {
        let close = open + rest[open..].find('>')?;
        let target = &rest[open + 1..close];
        let params_end = rest[close..].find('<').map_or(rest.len(), |at| close + at);
        let params = &rest[close + 1..params_end];
        let next = params.split(';').any(|param| {
            param.split_once('=').is_some_and(|(key, value)| {
                key.trim().eq_ignore_ascii_case("rel")
                    && value
                        .trim()
                        .trim_matches('"')
                        .split_ascii_whitespace()
                        .any(|rel| rel.eq_ignore_ascii_case("next"))
            })
        });
        if next {
            return Some(target.to_owned());
        }
        rest = &rest[params_end..];
    }
    None
}

/// Asks for every page of the listing at `url`, one request each, and hands each page's JSON
/// to `take`, which returns how many items it found there. Only pages on the `trusted`
/// origin, the one the user typed, may reach a private address.
pub(crate) async fn walk(
    client: &Client,
    url: &SafeUrl,
    trusted: Option<&Origin>,
    paging: &Paging,
    reference: &str,
    mut take: impl FnMut(&Value, &SafeUrl) -> Result<usize, SourceError>,
) -> Result<(), SourceError> {
    let protocol = |url: &SafeUrl, reason: String| SourceError::Body {
        url: url.clone(),
        reason,
    };
    let first = paging.first(url);
    let mut seen = HashSet::new();
    let mut previous: Option<Vec<u8>> = None;
    let mut next = Some(first.clone());
    let mut number = 0;
    while let Some(url) = next.take() {
        if !seen.insert(url.clone()) {
            return Err(protocol(&url, "named a page it already sent".to_owned()));
        }
        if number == PAGES {
            return Err(protocol(&url, format!("has more than {PAGES} pages")));
        }
        number += 1;
        let request = Request {
            url: &url,
            follow: true,
            typed: trusted.is_some_and(|trusted| url.origin().as_ref() == Some(trusted)),
        };
        let (mut page, body) = record::json(client, &request, reference).await?;
        if previous.as_ref() == Some(&page.body) {
            return Err(protocol(&page.url, "sent the same page twice".to_owned()));
        }
        let items = take(&body, &page.url)?;
        next = paging
            .next(&first, number, &page, &body, items)
            .map_err(|reason| protocol(&page.url, reason))?;
        previous = Some(std::mem::take(&mut page.body));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_link_is_found_among_others() {
        assert_eq!(
            next_link("<https://a/1>; rel=\"prev\", <https://a/3?x=1,2>; rel=\"next\"").as_deref(),
            Some("https://a/3?x=1,2")
        );
        assert_eq!(
            next_link("<https://a/1>; rel=\"prev last\"").as_deref(),
            None
        );
        assert_eq!(
            next_link("<https://a/2>; REL=next").as_deref(),
            Some("https://a/2")
        );
        assert_eq!(next_link("garbage"), None);
        assert_eq!(
            next_link("<https://a/2>rel=next").as_deref(),
            Some("https://a/2")
        );
    }
}
