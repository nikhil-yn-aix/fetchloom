//! The adapter for plain http and https links: a URL naming a file is that one file, and
//! nothing is fetched; a URL ending in `/` is a directory index, walked without leaving it.

use std::collections::VecDeque;
use std::sync::Arc;

use fl_model::path::DataPath;
use fl_model::reference::Reference;
use fl_transfer::client::Client;
use fl_transfer::fetch::Request;
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use url::Url;

use crate::index;
use crate::listing::{Entry, Listing, Meta};
use crate::source::{Source, SourceError};

/// The most directories one index walk visits.
const DIRECTORIES: usize = 10_000;

/// Serves `http` and `https` references. A URL whose path names a file is resolved without a
/// request; one whose path ends in `/` is read as an HTML directory index, one request per
/// directory, keeping only what lies under it.
pub struct Http {
    schemes: Vec<String>,
    client: Arc<Client>,
}

impl Http {
    /// The adapter for `http` and `https`, reading indexes through `client`.
    #[must_use]
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            schemes: vec!["http".to_owned(), "https".to_owned()],
            client,
        }
    }

    async fn listing(&self, reference: &Reference) -> Result<Listing, SourceError> {
        let invalid = |reason: &str| SourceError::Invalid {
            reference: reference.as_str().to_owned(),
            reason: reason.to_owned(),
        };
        let parsed = Url::parse(reference.as_str()).map_err(|error| invalid(&error.to_string()))?;
        let url =
            SafeUrl::parse(reference.as_str()).map_err(|error| invalid(&error.to_string()))?;
        let entries = if parsed.path().ends_with('/') {
            self.index(&url, reference.as_str()).await?
        } else {
            let segment = parsed
                .path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .ok_or_else(|| invalid("the path names no file"))?;
            let name = decode(segment).ok_or_else(|| invalid("the file name is not valid text"))?;
            let path = DataPath::new(&name).map_err(|error| invalid(&error.to_string()))?;
            vec![entry(path, url)]
        };
        Ok(Listing {
            resolved: reference.clone(),
            meta: Meta::default(),
            entries,
        })
    }

    async fn index(&self, root: &SafeUrl, reference: &str) -> Result<Vec<Entry>, SourceError> {
        let mut entries = Vec::new();
        let mut queue = VecDeque::from([(root.clone(), String::new())]);
        let mut visited = 0;
        while let Some((url, prefix)) = queue.pop_front() {
            visited += 1;
            if visited > DIRECTORIES {
                return Err(SourceError::Body {
                    url,
                    reason: format!("holds more than {DIRECTORIES} directories"),
                });
            }
            let page = self
                .client
                .get(&Request {
                    url: &url,
                    follow: true,
                    typed: true,
                })
                .await
                .map_err(|error| match error {
                    fl_transfer::error::TransferError::Status {
                        status: 404 | 410, ..
                    } => SourceError::Unresolved {
                        reference: reference.to_owned(),
                    },
                    other => SourceError::Transfer(other),
                })?;
            let html = String::from_utf8_lossy(&page.body);
            for link in index::links(&url, &html) {
                let path = format!("{prefix}{}", link.name);
                if link.dir {
                    queue.push_back((link.url, format!("{path}/")));
                } else {
                    let path = DataPath::new(&path).map_err(|_| SourceError::Body {
                        url: url.clone(),
                        reason: format!("lists `{path}`, which is not a valid path"),
                    })?;
                    entries.push(entry(path, link.url));
                }
            }
        }
        Ok(entries)
    }
}

fn entry(path: DataPath, url: SafeUrl) -> Entry {
    Entry {
        path,
        size: None,
        stated: Vec::new(),
        urls: vec![url],
    }
}

impl Source for Http {
    fn schemes(&self) -> &[String] {
        &self.schemes
    }

    fn resolve<'a>(
        &'a self,
        reference: &'a Reference,
    ) -> BoxFuture<'a, Result<Listing, SourceError>> {
        Box::pin(self.listing(reference))
    }
}

/// `segment` with its percent escapes decoded, when they are whole and the result is UTF-8.
pub(crate) fn decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%' {
            let pair = segment.get(index + 1..index + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            index += 3;
        } else {
            out.push(byte);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(decode("a%20b%C3%A9.csv").as_deref(), Some("a bé.csv"));
        assert_eq!(decode("plain.csv").as_deref(), Some("plain.csv"));
    }

    #[test]
    fn broken_escapes_and_invalid_utf8_are_refused() {
        assert_eq!(decode("a%2"), None);
        assert_eq!(decode("a%zz"), None);
        assert_eq!(decode("%FF"), None);
    }
}
