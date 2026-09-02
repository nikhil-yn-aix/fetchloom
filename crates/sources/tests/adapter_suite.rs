//! Runs the shared adapter contract suite against every source adapter, and
//! proves the suite itself can fail.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use rustls as _;
use rustls_graviola as _;
use ureq as _;

use std::io::{Cursor, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use fetchloom_engine::adapter::{Finding, Fixture, judge};
use fetchloom_engine::credential::Credential;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, ListingEntry, Revalidated, Served, Source, SourceIdentity, SourceMetadata,
    Validator,
};
use fetchloom_engine::work::WorkCounter;
use fetchloom_faults::{IndexFormat, Reply, Script, TestServer};
use fetchloom_sources::{FileSource, HttpSource};

fn object() -> Vec<u8> {
    (0..2048u32)
        .map(|value| u8::try_from(value % 256).unwrap())
        .collect()
}

fn assert_conforms(findings: &[Finding]) {
    assert!(
        findings.is_empty(),
        "expected the adapter to satisfy the suite, but it found: {findings:?}"
    );
}

#[test]
fn a_file_source_pointed_at_a_temporary_file_satisfies_the_suite() {
    let bytes = object();
    let mut path = std::env::temp_dir();
    path.push(format!("fetchloom-adapter-suite-{}", std::process::id()));
    let mut handle = std::fs::File::create(&path).unwrap();
    handle.write_all(&bytes).unwrap();
    drop(handle);

    let source = FileSource::new(Arc::new(WorkCounter::new()));
    let location = path.to_str().unwrap().to_owned();
    let fixture = Fixture {
        location: &location,
        bytes: &bytes,
        supports_ranges: true,
        exposes_identity: false,
        container: None,
        entries: &[],
    };

    let findings = judge(&source, &fixture);

    std::fs::remove_file(&path).unwrap();
    assert_conforms(&findings);
}

#[test]
fn an_http_source_with_ranges_and_a_strong_identity_satisfies_the_suite() {
    let bytes = object();
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .ranges(true)
            .tagged(vec!["\"one\"".to_owned()]),
    )
    .unwrap();
    let source = HttpSource::new(Limits::default(), Arc::new(WorkCounter::new()));
    let location = format!("{}/object", server.origin());
    let fixture = Fixture {
        location: &location,
        bytes: &bytes,
        supports_ranges: true,
        exposes_identity: true,
        container: None,
        entries: &[],
    };

    let findings = judge(&source, &fixture);

    assert_conforms(&findings);
}

#[test]
fn an_http_source_withholding_ranges_and_identity_satisfies_the_suite() {
    let bytes = object();
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .ranges(false)
            .replying(vec![Reply::Whole, Reply::RangeIgnored]),
    )
    .unwrap();
    let source = HttpSource::new(Limits::default(), Arc::new(WorkCounter::new()));
    let location = format!("{}/object", server.origin());
    let fixture = Fixture {
        location: &location,
        bytes: &bytes,
        supports_ranges: false,
        exposes_identity: false,
        container: None,
        entries: &[],
    };

    let findings = judge(&source, &fixture);

    assert_conforms(&findings);
}

#[test]
fn an_http_source_with_a_listable_container_satisfies_the_suite() {
    let bytes = object();
    let server = TestServer::start(
        Script::serving(bytes.clone())
            .ranges(true)
            .tagged(vec!["\"one\"".to_owned()])
            .replying(vec![
                Reply::Whole,
                Reply::Whole,
                Reply::Listing {
                    format: IndexFormat::ObjectStore,
                },
            ]),
    )
    .unwrap();
    let source = HttpSource::new(Limits::default(), Arc::new(WorkCounter::new()));
    let location = format!("{}/object", server.origin());
    let container = format!("{}/set/", server.origin());
    let fixture = Fixture {
        location: &location,
        bytes: &bytes,
        supports_ranges: true,
        exposes_identity: true,
        container: Some(&container),
        entries: &["one", "two"],
    };

    let findings = judge(&source, &fixture);

    assert_conforms(&findings);
}

struct LyingSource {
    bytes: Vec<u8>,
}

impl Source for LyingSource {
    type Body = Cursor<Vec<u8>>;

    fn probe(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        Ok(SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new("liar.invalid"),
            size: Some(u64::try_from(self.bytes.len()).unwrap()),
            content: None,
            interop: None,
            identity: SourceIdentity::None,
            last_modified: None,
            supports_ranges: true,
            time_to_first_byte: Duration::ZERO,
            retry_after: None,
            cost: Cost::default(),
        })
    }

    fn fetch(
        &self,
        location: &str,
        _range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        Ok(Served {
            metadata: self.probe(location, credential)?,
            body: Cursor::new(self.bytes.clone()),
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

    fn list(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error> {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("name an object, because {location} lists nothing"),
        ))
    }
}

#[test]
fn a_source_that_claims_range_support_and_serves_the_whole_object_is_caught() {
    let bytes = object();
    let source = LyingSource {
        bytes: bytes.clone(),
    };
    let fixture = Fixture {
        location: "liar://object",
        bytes: &bytes,
        supports_ranges: true,
        exposes_identity: false,
        container: None,
        entries: &[],
    };

    let findings = judge(&source, &fixture);

    assert!(
        findings
            .iter()
            .any(|found| found.check == "range_support_present"),
        "the suite did not catch a source that claims range support and serves the whole object: {findings:?}"
    );
}

fn read(body: impl Read) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut body = body;
    body.read_to_end(&mut bytes).unwrap();
    bytes
}

#[test]
fn the_suite_is_generic_over_the_source_it_judges() {
    fn generic_judge<S: Source>(source: &S, fixture: &Fixture<'_>) -> Vec<Finding> {
        judge(source, fixture)
    }

    let bytes = object();
    let server = TestServer::start(Script::serving(bytes.clone())).unwrap();
    let source = HttpSource::new(Limits::default(), Arc::new(WorkCounter::new()));
    let location = format!("{}/object", server.origin());
    let fixture = Fixture {
        location: &location,
        bytes: &bytes,
        supports_ranges: true,
        exposes_identity: false,
        container: None,
        entries: &[],
    };

    let findings = generic_judge(&source, &fixture);
    assert_conforms(&findings);

    let whole = source.fetch(&location, None, None).unwrap();
    assert_eq!(read(whole.body), bytes);
}

struct InventingSource {
    bytes: Vec<u8>,
}

impl Source for InventingSource {
    type Body = Cursor<Vec<u8>>;

    fn probe(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        Ok(SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new("inventor.invalid"),
            size: Some(u64::try_from(self.bytes.len()).unwrap()),
            content: None,
            interop: None,
            identity: SourceIdentity::StrongValidator("\"invented\"".to_owned()),
            last_modified: None,
            supports_ranges: false,
            time_to_first_byte: Duration::ZERO,
            retry_after: None,
            cost: Cost::default(),
        })
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        if range.is_some() {
            return Err(Error::new(
                ErrorKind::SourceUnsupportedRange,
                "ask for the whole object, because this source serves no span",
            ));
        }
        Ok(Served {
            metadata: self.probe(location, credential)?,
            body: Cursor::new(self.bytes.clone()),
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

    fn list(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error> {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("name an object, because {location} lists nothing"),
        ))
    }
}

#[test]
fn a_source_that_invents_an_identity_it_was_never_given_is_caught() {
    let bytes = object();
    let source = InventingSource {
        bytes: bytes.clone(),
    };
    let fixture = Fixture {
        location: "inventor://object",
        bytes: &bytes,
        supports_ranges: false,
        exposes_identity: false,
        container: None,
        entries: &[],
    };

    let findings = judge(&source, &fixture);

    assert!(
        findings
            .iter()
            .any(|found| found.check == "immutable_identity_absent"),
        "the suite did not catch a source inventing an identity: {findings:?}"
    );
}
