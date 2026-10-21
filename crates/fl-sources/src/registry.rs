//! The sources fetchloom knows, by scheme, and the one call that resolves and filters.

use std::sync::Arc;

use fl_model::filter::Filters;
use fl_model::reference::{Reference, Target};
use fl_transfer::client::Client;
use tracing::Instrument;

use crate::declarative::Declarative;
use crate::http::Http;
use crate::listing::Listing;
use crate::source::{Source, SourceError};

const ZENODO: &str = include_str!("../../../sources/zenodo.toml");

/// The sources, found by the scheme of a reference.
#[derive(Default)]
pub struct Registry {
    sources: Vec<Arc<dyn Source>>,
}

impl Registry {
    /// The built in sources: the http adapter for `http` and `https`, and every embedded
    /// declarative adapter.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Adapter`] when an embedded adapter cannot be read.
    pub fn builtin(client: Arc<Client>) -> Result<Self, SourceError> {
        let mut registry = Self::default();
        registry.add(Arc::new(Http::new()));
        registry.add(Arc::new(Declarative::from_toml(ZENODO, client)?));
        Ok(registry)
    }

    /// Adds a source. For a scheme already served, the later source replaces the earlier.
    pub fn add(&mut self, source: Arc<dyn Source>) {
        self.sources.push(source);
    }

    /// Resolves `reference` through the source of its scheme, then keeps the entries that
    /// `filters` keep, in order.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::NoSource`] when no source serves the scheme, whatever the source
    /// itself reports, and [`SourceError::Selection`] when a pattern matches no file.
    pub async fn resolve(
        &self,
        reference: &Reference,
        filters: &Filters,
    ) -> Result<Listing, SourceError> {
        let scheme = scheme_of(reference);
        let span = tracing::debug_span!("resolve", scheme, reference = reference.as_str());
        async {
            let source = self
                .sources
                .iter()
                .rev()
                .find(|source| source.schemes().iter().any(|served| served == scheme))
                .ok_or_else(|| SourceError::NoSource {
                    scheme: scheme.to_owned(),
                })?;
            let mut listing = source.resolve(reference).await?;
            let kept = filters
                .apply(
                    listing
                        .entries
                        .iter()
                        .map(|entry| (&entry.path, entry.size)),
                )
                .refuse_unmatched()?;
            let mut flags = vec![false; listing.entries.len()];
            for index in kept {
                flags[index] = true;
            }
            let mut flags = flags.into_iter();
            listing.entries.retain(|_| flags.next().unwrap_or(false));
            Ok(listing)
        }
        .instrument(span)
        .await
    }
}

fn scheme_of(reference: &Reference) -> &str {
    match reference.target() {
        Target::Url { scheme, .. } => scheme.name(),
        Target::Scheme { scheme, .. } => scheme,
        Target::File { .. } => "file",
        Target::Path(_) => "a local path",
        Target::Doi { .. } => "doi",
        Target::Remote { .. } => "a named remote",
        Target::Name(_) => "a bare name",
    }
}
