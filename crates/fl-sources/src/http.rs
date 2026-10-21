//! The adapter for plain http and https links: one URL is one file, and nothing is fetched.

use fl_model::path::DataPath;
use fl_model::reference::Reference;
use fl_transfer::url::SafeUrl;
use futures_util::future::BoxFuture;
use url::Url;

use crate::listing::{Entry, Listing, Meta};
use crate::source::{Source, SourceError};

/// Serves `http` and `https` references. Resolving makes no request: the last segment of the
/// path names the file.
#[derive(Clone, Debug)]
pub struct Http {
    schemes: Vec<String>,
}

impl Http {
    /// The adapter for `http` and `https`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            schemes: vec!["http".to_owned(), "https".to_owned()],
        }
    }

    fn listing(reference: &Reference) -> Result<Listing, SourceError> {
        let invalid = |reason: &str| SourceError::Invalid {
            reference: reference.as_str().to_owned(),
            reason: reason.to_owned(),
        };
        let parsed = Url::parse(reference.as_str()).map_err(|error| invalid(&error.to_string()))?;
        let segment = parsed
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .ok_or_else(|| invalid("the path names no file"))?;
        let name = decode(segment).ok_or_else(|| invalid("the file name is not valid text"))?;
        let path = DataPath::new(&name).map_err(|error| invalid(&error.to_string()))?;
        let url =
            SafeUrl::parse(reference.as_str()).map_err(|error| invalid(&error.to_string()))?;
        Ok(Listing {
            resolved: reference.clone(),
            meta: Meta::default(),
            entries: vec![Entry {
                path,
                size: None,
                stated: Vec::new(),
                urls: vec![url],
            }],
        })
    }
}

impl Default for Http {
    fn default() -> Self {
        Self::new()
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
        Box::pin(std::future::ready(Self::listing(reference)))
    }
}

fn decode(segment: &str) -> Option<String> {
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
