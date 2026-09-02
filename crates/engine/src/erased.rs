//! One adapter held without naming its type, so that a run dispatches on what
//! an adapter says it serves rather than on which adapter it is.

use std::io::Read;

use crate::credential::Credential;
use crate::degrade::Degradation;
use crate::error::Error;
use crate::seam::source::{
    ByteRange, Listing, Revalidated, Served, Serves, Source, SourceMetadata, Validator,
};

/// The bytes any adapter streams, once its own body type is forgotten.
pub type AnyBody = Box<dyn Read + Send>;

trait ErasedSource: Send + Sync {
    fn serves(&self, reference: &str) -> Option<Serves>;

    fn take_degradations(&self) -> Vec<Degradation>;

    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error>;

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<AnyBody>, Error>;

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<AnyBody>, Error>;

    fn list(&self, location: &str, credential: Option<&Credential>) -> Result<Listing, Error>;
}

impl<S> ErasedSource for S
where
    S: Source + Send + Sync,
    S::Body: Send + 'static,
{
    fn serves(&self, reference: &str) -> Option<Serves> {
        Source::serves(self, reference)
    }

    fn take_degradations(&self) -> Vec<Degradation> {
        Source::take_degradations(self)
    }

    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        Source::probe(self, location, credential)
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<AnyBody>, Error> {
        let served = Source::fetch(self, location, range, credential)?;
        Ok(Served {
            metadata: served.metadata,
            body: Box::new(served.body),
        })
    }

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<AnyBody>, Error> {
        match Source::revalidate(self, location, validator, credential)? {
            Revalidated::Unchanged => Ok(Revalidated::Unchanged),
            Revalidated::Changed(served) => Ok(Revalidated::Changed(Box::new(Served {
                metadata: served.metadata,
                body: Box::new(served.body),
            }))),
        }
    }

    fn list(&self, location: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        Source::list(self, location, credential)
    }
}

/// One adapter, held behind the seam rather than by its own type.
pub struct AnySource {
    held: Box<dyn ErasedSource>,
}

impl AnySource {
    /// Holds one adapter behind the seam.
    pub fn new<S>(adapter: S) -> Self
    where
        S: Source + Send + Sync + 'static,
        S::Body: Send + 'static,
    {
        Self {
            held: Box::new(adapter),
        }
    }
}

impl Source for AnySource {
    type Body = AnyBody;

    fn serves(&self, reference: &str) -> Option<Serves> {
        self.held.serves(reference)
    }

    fn take_degradations(&self) -> Vec<Degradation> {
        self.held.take_degradations()
    }

    fn probe(
        &self,
        location: &str,
        credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        self.held.probe(location, credential)
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        self.held.fetch(location, range, credential)
    }

    fn revalidate(
        &self,
        location: &str,
        validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        self.held.revalidate(location, validator, credential)
    }

    fn list(&self, location: &str, credential: Option<&Credential>) -> Result<Listing, Error> {
        self.held.list(location, credential)
    }
}

/// Every adapter a run may dispatch to, asked in the order they were given.
pub struct Adapters {
    held: Vec<AnySource>,
}

impl Adapters {
    /// Starts a registry holding the adapters given, in the order they are
    /// asked.
    #[must_use]
    pub fn new(held: Vec<AnySource>) -> Self {
        Self { held }
    }

    /// Returns the first adapter that serves a reference, and what it serves
    /// there.
    #[must_use]
    pub fn serving(&self, reference: &str) -> Option<(&AnySource, Serves)> {
        self.held
            .iter()
            .find_map(|adapter| Source::serves(adapter, reference).map(|what| (adapter, what)))
    }

    /// Returns every fallback any adapter in the registry performed since the
    /// last call.
    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.held
            .iter()
            .flat_map(Source::take_degradations)
            .collect()
    }
}
