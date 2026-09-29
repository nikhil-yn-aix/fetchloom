//! The lock written from fixed values is the same bytes on every platform.

use std::error::Error;

use fl_model::digest::Digest;
use fl_model::lock::{Lock, LockedDataset, LockedFile, LockedStep, Role};
use fl_model::name::Name;
use fl_model::path::DataPath;
use fl_model::tree::{Tree, TreeEntry};

const GOLDEN: &str = include_str!("golden/data.lock");

const GOLDEN_BLAKE3: &str = "405180a2cab6999ff329db42f1019133043a5961f7cf999ab4ca90f5df98af62";

fn file(path: &str, size: u64, byte: u8, at: &[&str]) -> Result<LockedFile, Box<dyn Error>> {
    Ok(LockedFile {
        path: DataPath::new(path)?,
        size,
        blake3: [byte; 32],
        sha256: None,
        sha1: None,
        md5: None,
        at: at.iter().map(|location| (*location).to_owned()).collect(),
        from: None,
        role: None,
    })
}

fn tree(files: &[LockedFile]) -> Result<Digest, Box<dyn Error>> {
    let entries = files
        .iter()
        .filter(|file| file.role.is_none())
        .map(|file| TreeEntry {
            path: file.path.clone(),
            size: file.size,
            blake3: file.blake3,
        })
        .collect();
    Ok(Tree::new(entries)?.hash())
}

fn golden() -> Result<Lock, Box<dyn Error>> {
    let mut participants = file(
        "participants.tsv",
        1834,
        0x5a,
        &[
            "https://s3.amazonaws.com/openneuro.org/ds003061/participants.tsv",
            "https://doi.org/10.5281/zenodo.1?file=\"participants.tsv\"",
        ],
    )?;
    participants.md5 = Some([0x3c; 16]);
    participants.sha256 = Some([0x01; 32]);
    participants.sha1 = Some([0xfe; 20]);
    let archive = LockedFile {
        role: Some(Role::Archive),
        ..file(
            "raw.tar.gz",
            9_007_199_254_740_993,
            0x77,
            &["https://example.org/raw.tar.gz"],
        )?
    };
    let member = LockedFile {
        from: Some(Digest::Blake3([0x77; 32])),
        ..file("sub-01/eeg/r\u{e9}sum\u{e9} #1.edf", 0, 0x10, &[])?
    };
    let files = vec![member, archive, participants];
    let eeg = LockedDataset {
        name: Name::new("eeg")?,
        reference: "openneuro:ds003061@1.1.0".parse()?,
        spec: "blake3:1c9e04a7b2f1aa000000000000000000000000000000000000000000000000ff".parse()?,
        resolved: "openneuro:ds003061@1.1.0".parse()?,
        title: Some("EEG \"Motor\" Movement\tImagery \\ 運動".to_owned()),
        license: Some("CC0-1.0".to_owned()),
        doi: Some("10.18112/openneuro.ds003061.v1.1.0".to_owned()),
        retrieved: "2026-09-28T10:00:00Z".parse()?,
        tree: tree(&files)?,
        files: Some(files),
    };
    let genome = LockedDataset {
        name: Name::new("grch38")?,
        reference: "ensembl:homo_sapiens/GRCh38@110".parse()?,
        spec: Digest::Blake3([0x2b; 32]),
        resolved: "ensembl:homo_sapiens/GRCh38@110".parse()?,
        title: None,
        license: None,
        doi: None,
        retrieved: "2024-02-29T23:59:59Z".parse()?,
        tree: Digest::Blake3([0x9f; 32]),
        files: None,
    };
    let steps = vec![
        LockedStep {
            name: Name::new("features")?,
            key: Digest::Blake3([0x77; 32]),
            items: None,
            tree: Digest::Blake3([0x4e; 32]),
        },
        LockedStep {
            name: Name::new("filter")?,
            key: Digest::Blake3([0xab; 32]),
            items: Some(3),
            tree: Digest::Blake3([0x10; 32]),
        },
    ];
    Ok(Lock::new(vec![genome, eeg], steps)?)
}

#[test]
#[expect(
    clippy::print_stdout,
    reason = "the hash is shown so runs on each platform can be compared"
)]
fn golden_lock_is_byte_identical() -> Result<(), Box<dyn Error>> {
    let text = golden()?.to_toml();
    let hash = blake3::hash(text.as_bytes()).to_hex();
    println!("golden data.lock blake3 {hash}");
    assert_eq!(text, GOLDEN);
    assert_eq!(hash.as_str(), GOLDEN_BLAKE3);
    assert_eq!(Lock::from_toml(GOLDEN)?, golden()?);
    Ok(())
}
