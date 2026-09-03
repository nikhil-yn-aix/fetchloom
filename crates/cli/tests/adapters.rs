//! Whether a run dispatches on what an adapter says it serves, rather than on
//! which adapter it is.

#![expect(
    clippy::unwrap_used,
    reason = "test assertions, where the run that failed is the message"
)]

use clap as _;
use clap_complete as _;
use ctrlc as _;
use fetchloom_archive as _;
use fetchloom_cache::Cache;
use fetchloom_cli::cache as cache_cli;
use fetchloom_cli::run::{self, Materialization};
use fetchloom_engine::credential::Credential;
use fetchloom_engine::degrade::Degradation;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::erased::{Adapters, AnySource};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::Sequence;
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, Listing, Revalidated, Served, Serves, Source, SourceIdentity, SourceMetadata,
    Validator,
};
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::work::WorkCounter;
use fetchloom_faults as _;
use fetchloom_platform::NativePlatform;
use fetchloom_sources as _;
use fetchloom_view as _;
use flate2 as _;
#[cfg(unix)]
use rustix as _;
use serde as _;
use serde_json as _;
use toml as _;
#[cfg(windows)]
use windows_sys as _;

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;

mod support;
use support::NoCredentialPolicy;

/// The scheme the third adapter answers for, which no adapter this build ships
/// answers for.
const SCHEME: &str = "invented://";

/// A third adapter, whole, serving fixed bytes over a scheme of its own.
struct InventedSource {
    bytes: Vec<u8>,
}

impl InventedSource {
    fn describe(&self, location: &str) -> SourceMetadata {
        SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new("invented".to_owned()),
            size: Some(self.bytes.len() as u64),
            content: None,
            interop: None,
            identity: SourceIdentity::ImmutableVersion("one".to_owned()),
            last_modified: None,
            supports_ranges: true,
            time_to_first_byte: Duration::from_millis(1),
            retry_after: None,
            cost: Cost::default(),
        }
    }
}

impl Source for InventedSource {
    type Body = std::io::Cursor<Vec<u8>>;

    fn serves(&self, reference: &str) -> Option<Serves> {
        reference.starts_with(SCHEME).then_some(Serves::Object)
    }

    fn take_degradations(&self) -> Vec<Degradation> {
        Vec::new()
    }

    fn probe(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        Ok(self.describe(location))
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        _credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        let span = range.unwrap_or(ByteRange {
            start: 0,
            end: self.bytes.len() as u64,
        });
        let start = usize::try_from(span.start).unwrap_or(0);
        let end = usize::try_from(span.end).unwrap_or(self.bytes.len());
        Ok(Served {
            metadata: self.describe(location),
            body: std::io::Cursor::new(self.bytes[start..end].to_vec()),
        })
    }

    fn revalidate(
        &self,
        location: &str,
        _validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        Ok(Revalidated::Changed(Box::new(
            self.fetch(location, None, credential)?,
        )))
    }

    fn list(&self, _location: &str, _credential: Option<&Credential>) -> Result<Listing, Error> {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            "name an object, because this adapter lists nothing".to_owned(),
        ))
    }
}

struct Quiet;

impl Observer for Quiet {
    fn emit(&self, _event: &fetchloom_engine::event::Event) {}
}

fn object() -> Vec<u8> {
    (0..64_usize * 1024)
        .map(|index| u8::try_from(index % 251).unwrap_or(0))
        .collect()
}

#[test]
fn a_third_adapter_that_states_it_serves_a_reference_is_the_one_a_run_uses() {
    let bytes = object();
    let scratch = TempDir::new().unwrap();
    let work = Arc::new(WorkCounter::new());
    let processor = Arc::new(
        Processor::new(ThreadBudget::resolve(
            std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
            None,
        ))
        .unwrap(),
    );
    let cache: Cache<NativePlatform> = cache_cli::require(
        &scratch.path().join("cache"),
        Arc::clone(&work),
        Arc::clone(&processor),
    )
    .unwrap();
    let platform = NativePlatform::new(Arc::clone(&work));
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let policy = NoCredentialPolicy::default();
    let tuning = run::Tuning {
        ceilings: fetchloom_engine::tuning::Ceilings {
            global: std::num::NonZeroU32::new(4).unwrap(),
            per_host: std::num::NonZeroU32::new(4).unwrap(),
        },
        adapts: true,
        bandwidth: None,
    };

    let adapters = Adapters::new(vec![
        AnySource::new(InventedSource {
            bytes: bytes.clone(),
        }),
        AnySource::new(fetchloom_sources::ObjectStoreSource::new(
            fetchloom_engine::limits::Limits::default(),
            Arc::clone(&work),
        )),
        AnySource::new(fetchloom_sources::HttpSource::new(
            fetchloom_engine::limits::Limits::default(),
            Arc::clone(&work),
        )),
    ]);

    let with = Materialization {
        processor: processor.as_ref(),
        platform: &platform,
        durability: DurabilityTier::Fast,
        cache: Some(&cache),
        work: &work,
        extract: true,
        digester: &digester,
        verify: fetchloom_engine::verification::VerificationPolicy::Fingerprint,
        tuning: &tuning,
        policy: &policy,
        adapters: &adapters,
    };

    let manifest = fetchloom_engine::manifest::Manifest {
        name: "invented".to_owned(),
        release: None,
        license: None,
        artifacts: vec![fetchloom_engine::manifest::Artifact {
            id: "object".to_owned(),
            sources: vec![format!("{SCHEME}host/object")],
            size: None,
            digest: Some(fetchloom_engine::manifest::DigestClaims {
                blake3: Some(hash_bytes(&bytes)),
                sha256: None,
            }),
            media_type: None,
            select: Vec::new(),
            layout: fetchloom_engine::selection::Layout::Keep,
            archive: None,
        }],
    };

    let destination = scratch.path().join("out");
    let run = run::materialize_manifest(
        &with,
        &manifest,
        scratch.path(),
        &destination,
        false,
        false,
        None,
        &Quiet,
        &Sequence::new(),
    );
    run.outcome.unwrap();

    let written = std::fs::read(destination.join("object")).unwrap();
    assert_eq!(
        hash_bytes(&written),
        hash_bytes(&bytes),
        "a run did not take the bytes from the adapter that stated it serves the reference"
    );
}
