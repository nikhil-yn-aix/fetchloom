//! What every metadata reader must hold, whatever format it reads.

#![expect(
    clippy::unwrap_used,
    clippy::panic,
    clippy::type_complexity,
    clippy::elidable_lifetime_names,
    reason = "test assertions, where the document that failed is the message"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::Manifest;
use fetchloom_engine::metadata::{Context, MetadataReader};
use fetchloom_engine::metadata::{bagit, croissant, frictionless, pooch, sidecar};

fn context<'a>(limits: &'a Limits) -> Context<'a> {
    Context {
        base: "https://lab.edu/set/",
        name: "inferred",
        limits,
    }
}

/// Every reader, with a document it reads and a document it must refuse.
fn readers() -> Vec<(Box<dyn MetadataReader>, &'static [u8], &'static [u8])> {
    vec![
        (
            Box::new(sidecar::ChecksumSidecar),
            b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  one.bin\n",
            b"d41d8cd98f00b204e9800998ecf8427e  one.bin\n",
        ),
        (
            Box::new(pooch::PoochRegistry),
            b"one.bin e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
            b"one.bin md5:d41d8cd98f00b204e9800998ecf8427e\n",
        ),
        (
            Box::new(bagit::BagIt),
            b"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855  data/one.bin\n",
            b"d41d8cd98f00b204e9800998ecf8427e  data/one.bin\n",
        ),
        (
            Box::new(croissant::Croissant),
            br#"{"@context":{"cr":"croissant"},"@type":"Dataset","name":"set","distribution":[{"@type":"sc:FileObject","@id":"one.bin","contentUrl":"https://lab.edu/one.bin","sha256":"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}]}"#,
            br#"{"@context":{"cr":"croissant"},"@type":"Dataset","name":"set","distribution":[{"@type":"sc:FileObject","@id":"one.bin","contentUrl":"https://lab.edu/one.bin","md5":"d41d8cd98f00b204e9800998ecf8427e"}]}"#,
        ),
        (
            Box::new(frictionless::FrictionlessPackage),
            br#"{"name":"set","profile":"data-package","resources":[{"name":"one","path":"one.csv","hash":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}]}"#,
            br#"{"name":"set","profile":"data-package","resources":[{"name":"one","path":"one.csv","hash":"d41d8cd98f00b204e9800998ecf8427e"}]}"#,
        ),
    ]
}

#[test]
fn no_reader_ever_emits_an_artifact_naming_no_source() {
    let limits = Limits::default();
    for (reader, good, _) in readers() {
        let manifest = reader.read(good, &context(&limits)).unwrap_or_else(|error| {
            panic!(
                "{} did not read its own well-formed document: {}",
                reader.format().label(),
                error.next_action()
            )
        });
        for artifact in &manifest.artifacts {
            assert!(
                !artifact.sources.is_empty(),
                "{} emitted the artifact {} with no source, which resolves to nothing",
                reader.format().label(),
                artifact.id
            );
            assert!(
                !artifact.id.is_empty(),
                "{} emitted an artifact with no identifier",
                reader.format().label()
            );
        }
        assert!(
            !manifest.artifacts.is_empty(),
            "{} read a document into no artifacts at all",
            reader.format().label()
        );
    }
}

#[test]
fn every_reader_refuses_a_digest_it_cannot_represent_by_name() {
    let limits = Limits::default();
    for (reader, _, refused) in readers() {
        let error = reader.read(refused, &context(&limits)).err().unwrap_or_else(|| {
            panic!(
                "{} accepted a document stating a digest it does not carry",
                reader.format().label()
            )
        });
        let said = error.next_action().to_lowercase();
        assert!(
            said.contains("md5"),
            "{} refused without naming the algorithm it found: {said}",
            reader.format().label()
        );
    }
}

#[test]
fn every_reader_reads_one_document_into_the_same_manifest_twice() {
    let limits = Limits::default();
    for (reader, good, _) in readers() {
        let one = reader.read(good, &context(&limits)).unwrap();
        let other = reader.read(good, &context(&limits)).unwrap();
        assert_eq!(
            canonical(&one),
            canonical(&other),
            "{} is not deterministic over one document",
            reader.format().label()
        );
    }
}

#[test]
fn every_reader_refuses_a_document_past_the_size_bound() {
    let limits = Limits {
        manifest_size: 8,
        ..Limits::default()
    };
    for (reader, good, _) in readers() {
        assert!(
            reader.read(good, &context(&limits)).is_err(),
            "{} read a document past the size a run bounds one to",
            reader.format().label()
        );
    }
}

#[test]
fn every_reader_refuses_bytes_that_are_not_its_format() {
    let limits = Limits::default();
    for (reader, _, _) in readers() {
        assert!(
            reader
                .read(b"\x00\x01\x02 not any metadata format", &context(&limits))
                .is_err(),
            "{} read arbitrary bytes as a manifest",
            reader.format().label()
        );
    }
}

fn canonical(manifest: &Manifest) -> String {
    format!("{manifest:?}")
}
