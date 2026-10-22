//! The sources fetchloom knows, by scheme, and the one call that resolves and filters.

use std::collections::HashMap;
use std::sync::Arc;

use fl_model::filter::Filters;
use fl_model::reference::{Reference, Target};
use fl_transfer::client::Client;
use fl_transfer::credentials::Variable;
use tracing::Instrument;
use url::Url;

use crate::croissant::Croissant;
use crate::declarative::Declarative;
use crate::doi;
use crate::hf::HuggingFace;
use crate::http::Http;
use crate::listing::Listing;
use crate::source::{Read, Source, SourceError};

/// The declarative adapters built into fetchloom.
const EMBEDDED: [&str; 1] = [include_str!("../../../sources/zenodo.toml")];

const DOI_RESOLVER: &str = "https://doi.org/";

/// The sources, found by the scheme of a reference.
pub struct Registry {
    sources: Vec<Arc<dyn Source>>,
    resolver: Url,
    client: Arc<Client>,
}

impl Registry {
    /// No sources yet, with DOIs looked up at doi.org.
    #[must_use]
    pub fn new(client: Arc<Client>) -> Self {
        Self {
            sources: Vec::new(),
            resolver: Url::parse(DOI_RESOLVER).unwrap_or_else(|_| unreachable!()),
            client,
        }
    }

    /// The built in sources: the http adapter for `http` and `https`, Hugging Face, Croissant,
    /// and every embedded declarative adapter.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Adapter`] when an embedded adapter cannot be read.
    pub fn builtin(client: &Arc<Client>) -> Result<Self, SourceError> {
        let mut registry = Self::new(Arc::clone(client));
        registry.add(Arc::new(Http::new(Arc::clone(client))));
        registry.add(Arc::new(HuggingFace::new(Arc::clone(client))));
        registry.add(Arc::new(Croissant::new(Arc::clone(client))));
        for text in EMBEDDED {
            registry.add(Arc::new(Declarative::from_toml(text, Arc::clone(client))?));
        }
        Ok(registry)
    }

    /// Looks DOIs up at `resolver` instead of doi.org.
    #[must_use]
    pub fn with_resolver(mut self, resolver: Url) -> Self {
        self.resolver = resolver;
        self
    }

    /// Adds a source. For a scheme already served, the later source replaces the earlier.
    pub fn add(&mut self, source: Arc<dyn Source>) {
        self.sources.push(source);
    }

    /// The credentials the sources declare, their tokens read through `read`.
    pub fn credentials(
        &self,
        read: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Vec<Variable> {
        let read: Read = Arc::new(read);
        self.sources
            .iter()
            .filter_map(|source| source.credential(Arc::clone(&read)))
            .collect()
    }

    /// Resolves `reference` through the source of its scheme, a DOI through the source its
    /// landing page belongs to, then keeps the entries that `filters` keep, in order.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::NoSource`] when no source serves the scheme,
    /// [`SourceError::NoLanding`] when no source serves a DOI's landing page, whatever the
    /// source itself reports, [`SourceError::Selection`] when a pattern matches no file, and
    /// [`SourceError::Collision`] when two kept paths differ only by case.
    pub async fn resolve(
        &self,
        reference: &Reference,
        filters: &Filters,
    ) -> Result<Listing, SourceError> {
        let scheme = scheme_of(reference);
        let span = tracing::debug_span!("resolve", scheme, reference = reference.as_str());
        async {
            let routed;
            let reference = match reference.target() {
                Target::Doi { prefix, suffix } => {
                    let doi = format!("{prefix}/{suffix}");
                    routed = doi::route(&self.client, &self.resolver, &doi, &self.sources).await?;
                    tracing::debug!("{} lands on {}", reference.as_str(), routed.as_str());
                    &routed
                }
                _ => reference,
            };
            let scheme = scheme_of(reference);
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
            refuse_collisions(&listing)?;
            Ok(listing)
        }
        .instrument(span)
        .await
    }
}

fn refuse_collisions(listing: &Listing) -> Result<(), SourceError> {
    let mut seen: HashMap<String, &str> = HashMap::with_capacity(listing.entries.len());
    for entry in &listing.entries {
        let path = entry.path.as_str();
        if let Some(first) = seen.insert(path.to_lowercase(), path) {
            return Err(SourceError::Collision {
                first: first.to_owned(),
                second: path.to_owned(),
            });
        }
    }
    Ok(())
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
