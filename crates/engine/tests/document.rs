//! Contract tests over the canonical model, its three surface syntaxes, and the
//! two canonical forms written back out.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use blake3 as _;
use rayon as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use toml as _;

use fetchloom_engine::document::{Syntax, canonical_json, parse, render};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::Manifest;

const YAML: &str = r#"name: silesia
release: "2024-06"
artifacts:
  - id: corpus
    sources:
      - https://host/silesia.tar.zst
      - https://mirror/silesia.tar.zst
    size: 68132864
    media_type: application/zstd
    archive:
      format: tar+zstd
    select: ["**/*.txt"]
    layout: keep
license:
  spdx: CC-BY-4.0
  requires_acceptance: false
"#;

const TOML: &str = r#"
name = "silesia"
release = "2024-06"

[[artifacts]]
id = "corpus"
sources = ["https://host/silesia.tar.zst", "https://mirror/silesia.tar.zst"]
size = 68132864
media_type = "application/zstd"
select = ["**/*.txt"]
layout = "keep"

[artifacts.archive]
format = "tar+zstd"

[license]
spdx = "CC-BY-4.0"
requires_acceptance = false
"#;

const JSON: &str = r#"{
  "license": {"requires_acceptance": false, "spdx": "CC-BY-4.0"},
  "artifacts": [
    {
      "layout": "keep",
      "archive": {"format": "tar+zstd"},
      "select": ["**/*.txt"],
      "media_type": "application/zstd",
      "size": 68132864,
      "sources": ["https://host/silesia.tar.zst", "https://mirror/silesia.tar.zst"],
      "id": "corpus"
    }
  ],
  "release": "2024-06",
  "name": "silesia"
}"#;

fn read(text: &str, syntax: Syntax) -> Manifest {
    Manifest::parse(text.as_bytes(), syntax, &Limits::default()).unwrap()
}

#[test]
fn three_syntaxes_parse_into_one_model() {
    let yaml = read(YAML, Syntax::Yaml);
    let toml = read(TOML, Syntax::Toml);
    let json = read(JSON, Syntax::Json);
    assert_eq!(yaml, toml, "yaml and toml did not parse into one model");
    assert_eq!(yaml, json, "yaml and json did not parse into one model");
}

#[test]
fn one_model_has_one_digest_whatever_it_was_written_in() {
    let yaml = read(YAML, Syntax::Yaml).digest().unwrap();
    let toml = read(TOML, Syntax::Toml).digest().unwrap();
    let json = read(JSON, Syntax::Json).digest().unwrap();
    assert_eq!(yaml, toml);
    assert_eq!(yaml, json);
}

#[test]
fn reformatting_a_manifest_does_not_change_its_digest() {
    let dense = "name: silesia\nartifacts:\n- id: corpus\n  sources: [https://host/a]\n";
    let spaced = "\n# a comment\nname:    silesia\n\nartifacts:\n  - sources:\n      - https://host/a\n    id: corpus\n";
    assert_eq!(
        read(dense, Syntax::Yaml).digest().unwrap(),
        read(spaced, Syntax::Yaml).digest().unwrap()
    );
}

#[test]
fn a_carriage_return_before_every_line_feed_changes_nothing() {
    let unix = read(YAML, Syntax::Yaml);
    let windows = read(&YAML.replace('\n', "\r\n"), Syntax::Yaml);
    assert_eq!(unix, windows);
    assert_eq!(unix.digest().unwrap(), windows.digest().unwrap());
}

#[test]
fn the_canonical_form_holds_no_line_ending_at_all() {
    let bytes = fetchloom_engine::document::canonical_json_of(&read(YAML, Syntax::Yaml)).unwrap();
    assert!(
        !bytes.contains(&b'\n') && !bytes.contains(&b'\r'),
        "the canonical form holds a line ending, so it cannot be identical on two platforms"
    );
}

#[test]
fn the_canonical_form_orders_every_mapping_by_the_bytes_of_its_keys() {
    let document = parse(
        br#"{"b": 1, "a": 2, "A": 3, "\u00e1": 4}"#,
        Syntax::Json,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(canonical_json(&document)).unwrap(),
        r#"{"A":3,"a":2,"b":1,"á":4}"#
    );
}

#[test]
fn the_canonical_form_writes_a_control_character_as_one_escape() {
    let document = parse(
        b"{\"k\": \"a\\u0007b\\tc\\\"d\"}",
        Syntax::Json,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(canonical_json(&document)).unwrap(),
        r#"{"k":"a\u0007b\tc\"d"}"#
    );
}

#[test]
fn the_rendered_form_ends_every_line_with_one_line_feed() {
    let document = parse(YAML.as_bytes(), Syntax::Yaml, &Limits::default()).unwrap();
    let rendered = render(&document);
    assert!(
        !rendered.contains('\r'),
        "a carriage return reached a written artifact"
    );
    assert!(rendered.ends_with('\n'));
}

#[test]
fn what_is_rendered_reads_back_as_what_was_rendered() {
    let document = parse(YAML.as_bytes(), Syntax::Yaml, &Limits::default()).unwrap();
    let rendered = render(&document);
    let again = parse(rendered.as_bytes(), Syntax::Yaml, &Limits::default()).unwrap();
    assert_eq!(document, again, "rendered as:\n{rendered}");
}

fn refused(text: &str) -> String {
    let error = parse(text.as_bytes(), Syntax::Yaml, &Limits::default())
        .expect_err("this document must be refused");
    assert_eq!(error.kind().label(), "manifest.invalid");
    error.next_action().to_owned()
}

#[test]
fn every_construct_that_makes_a_document_mean_two_things_is_refused_by_name() {
    assert!(refused("a: &anchor 1\n").contains("anchor"));
    assert!(refused("a: *alias\n").contains("alias"));
    assert!(refused("a:\n  <<: 1\n").contains("merge"));
    assert!(refused("a: !!str 1\n").contains("tag"));
    assert!(refused("a: |\n  text\n").contains("block scalar"));
    assert!(refused("---\na: 1\n").contains("one document"));
    assert!(refused("%YAML 1.2\na: 1\n").contains("directive"));
    assert!(refused("a: 1\n\tb: 2\n").contains("tab"));
    assert!(refused("a: 1\na: 2\n").contains("two keys named a"));
}

#[test]
fn a_word_that_looks_like_a_yes_is_a_word() {
    let document = parse(
        b"a: yes\nb: true\nc: \"true\"\nd: on\ne: 0755\nf: 12\n",
        Syntax::Yaml,
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(canonical_json(&document)).unwrap(),
        r#"{"a":"yes","b":true,"c":"true","d":"on","e":"0755","f":12}"#
    );
}

#[test]
fn a_reserved_key_is_refused_rather_than_ignored() {
    let error = Manifest::parse(
        b"name: a\nx-extra: 1\nartifacts:\n- id: b\n  sources: []\n",
        Syntax::Yaml,
        &Limits::default(),
    )
    .expect_err("a reserved key must be refused");
    assert_eq!(error.kind().label(), "manifest.invalid");
    assert!(error.next_action().contains("x-extra"));
}

#[test]
fn an_unknown_key_is_refused_rather_than_ignored() {
    let error = Manifest::parse(
        b"name: a\nwhatever: 1\nartifacts:\n- id: b\n  sources: []\n",
        Syntax::Yaml,
        &Limits::default(),
    )
    .expect_err("an unknown key must be refused");
    assert_eq!(error.kind().label(), "manifest.invalid");
    assert!(error.next_action().contains("whatever"));
}

#[test]
fn a_manifest_naming_no_artifact_is_refused() {
    let error = Manifest::parse(
        b"name: a\nartifacts: []\n",
        Syntax::Yaml,
        &Limits::default(),
    )
    .expect_err("a manifest with no artifact must be refused");
    assert_eq!(error.kind().label(), "manifest.invalid");
}

#[test]
fn a_document_past_a_bound_is_refused_rather_than_read() {
    let small = Limits {
        manifest_size: 8,
        ..Limits::default()
    };
    assert!(parse(YAML.as_bytes(), Syntax::Yaml, &small).is_err());

    let few = Limits {
        manifest_nodes: 3,
        ..Limits::default()
    };
    assert!(parse(YAML.as_bytes(), Syntax::Yaml, &few).is_err());

    let shallow = Limits {
        nesting_depth: 1,
        ..Limits::default()
    };
    assert!(parse(YAML.as_bytes(), Syntax::Yaml, &shallow).is_err());
}

/// A fingerprint tuple holds a volume identifier, a file identifier and two
/// instants, and two of those are a hundred and twenty-eight bits wide. The
/// canonical form carries a number as a run of digits that fits sixty-four, so
/// a receipt that wrote them as numbers could be written on a platform whose
/// values happen to be small and not on one whose values are not. It is written
/// on every platform or on none.
#[test]
fn a_receipt_renders_with_the_widest_fingerprint_any_platform_can_produce() {
    let mut fingerprints = std::collections::BTreeMap::new();
    fingerprints.insert(
        "data/one.bin".to_owned(),
        fetchloom_engine::receipt::RecordedFingerprint {
            volume: u64::MAX.to_string(),
            file: u128::MAX.to_string(),
            size: u64::MAX,
            modified_nanos: i128::MIN.to_string(),
            changed_nanos: i128::MAX.to_string(),
        },
    );
    let receipt = fetchloom_engine::receipt::Receipt {
        dataset: "silesia".to_owned(),
        manifest: fetchloom_engine::digest::ManifestDigest::from_bytes(
            *blake3::hash(b"a manifest").as_bytes(),
        ),
        artifacts: std::collections::BTreeMap::new(),
        tree: None,
        executable: Vec::new(),
        fingerprints,
        destination: std::path::PathBuf::from("/data/silesia"),
        accepted_terms: None,
        fetchloom: "0.1.0-dev".to_owned(),
        completed_at: fetchloom_engine::timestamp::Timestamp::from_epoch_seconds(1_700_000_000),
    };

    let rendered = receipt
        .render()
        .expect("a receipt has to render on every platform");
    let read = fetchloom_engine::receipt::Receipt::parse(
        rendered.as_bytes(),
        &fetchloom_engine::limits::Limits::default(),
    )
    .expect("a receipt has to read back");
    assert_eq!(read, receipt);
}
