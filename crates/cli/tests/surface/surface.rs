//! The command surface, driven the way a person drives it.

#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions, where the run that failed is the message"
)]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::support;
use fetchloom_faults::{
    Reply, Script, TYPEFLAG_DIRECTORY, TYPEFLAG_REGULAR, TarHeader, TarWriter, TestServer,
};
use flate2::Compression;
use flate2::write::GzEncoder;

use tempfile::TempDir;

const TAR_XZ: &[u8] = &[
    253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 4, 192, 103, 128, 80, 33, 1, 28, 0, 0, 0, 0,
    0, 0, 0, 0, 170, 92, 211, 43, 224, 39, 255, 0, 95, 93, 0, 52, 25, 73, 238, 141, 240, 186, 200,
    255, 155, 255, 242, 12, 105, 175, 17, 235, 99, 84, 137, 37, 157, 151, 75, 154, 96, 127, 12,
    192, 125, 103, 54, 167, 222, 237, 38, 42, 135, 16, 38, 51, 17, 204, 166, 22, 48, 182, 234, 179,
    228, 102, 164, 78, 124, 233, 102, 84, 149, 64, 68, 244, 30, 147, 47, 92, 241, 55, 168, 247, 29,
    192, 103, 122, 241, 156, 161, 58, 246, 63, 239, 248, 214, 0, 176, 184, 38, 172, 90, 211, 198,
    185, 207, 8, 206, 0, 0, 0, 161, 42, 131, 53, 95, 95, 180, 154, 0, 1, 131, 1, 128, 80, 0, 0,
    144, 3, 244, 231, 177, 196, 103, 251, 2, 0, 0, 0, 0, 4, 89, 90,
];

const TAR_BZ2: &[u8] = &[
    66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 68, 112, 61, 139, 0, 0, 111, 251, 128, 201, 144, 0, 4,
    64, 1, 71, 128, 0, 128, 98, 68, 158, 64, 8, 8, 32, 0, 84, 52, 128, 76, 70, 0, 77, 160, 146, 36,
    211, 70, 140, 128, 104, 31, 119, 49, 208, 130, 87, 33, 8, 206, 212, 192, 174, 88, 201, 2, 24,
    44, 227, 86, 2, 131, 8, 217, 200, 53, 94, 37, 50, 171, 20, 129, 60, 131, 182, 134, 192, 223,
    12, 248, 113, 244, 136, 128, 232, 187, 146, 41, 194, 132, 130, 35, 129, 236, 88,
];

const GREETING_XZ: &[u8] = &[
    253, 55, 122, 88, 90, 0, 0, 4, 230, 214, 180, 70, 4, 192, 10, 6, 33, 1, 28, 0, 0, 0, 0, 0, 0,
    0, 0, 0, 99, 160, 172, 177, 1, 0, 5, 104, 101, 108, 108, 111, 10, 0, 0, 0, 165, 96, 151, 241,
    148, 246, 253, 224, 0, 1, 38, 6, 58, 147, 59, 10, 31, 182, 243, 125, 1, 0, 0, 0, 0, 4, 89, 90,
];

const GREETING_BZ2: &[u8] = &[
    66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 193, 192, 128, 226, 0, 0, 1, 65, 0, 0, 16, 2, 68, 160,
    0, 48, 205, 0, 195, 70, 41, 151, 23, 114, 69, 56, 80, 144, 193, 192, 128, 226,
];

const GREETING: &[u8] = b"hello\n";

struct Run {
    output: Output,
}

impl Run {
    fn code(&self) -> i32 {
        self.output.status.code().unwrap_or(-1)
    }

    fn out(&self) -> String {
        String::from_utf8_lossy(&self.output.stdout).into_owned()
    }

    fn err(&self) -> String {
        String::from_utf8_lossy(&self.output.stderr).into_owned()
    }

    fn json(&self) -> serde_json::Value {
        let text = self.out();
        serde_json::from_str(text.trim()).unwrap_or_else(|error| {
            panic!(
                "the result was not JSON ({error}): {text}\nstderr: {}",
                self.err()
            )
        })
    }

    fn kind(&self) -> String {
        self.json()["kind"].as_str().unwrap_or("").to_owned()
    }
}

struct Workspace {
    temporary: TempDir,
}

impl Workspace {
    fn new() -> Self {
        Self {
            temporary: TempDir::new().unwrap(),
        }
    }

    fn path(&self) -> &Path {
        self.temporary.path()
    }

    fn cache(&self) -> PathBuf {
        self.path().join("cache")
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let target = self.path().join(name);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&target, bytes).unwrap();
        target
    }

    fn run(&self, arguments: &[&str]) -> Run {
        Run {
            output: support::fetchloom()
                .current_dir(self.path())
                .args(arguments)
                .env("FETCHLOOM_CACHE_DIR", self.cache())
                .output()
                .unwrap(),
        }
    }

    fn run_reading_configuration(&self, arguments: &[&str]) -> Run {
        Run {
            output: support::fetchloom_reading_configuration()
                .current_dir(self.path())
                .args(arguments)
                .output()
                .unwrap(),
        }
    }
}

fn entries_under(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing.flatten() {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                pending.push(path);
                found.push(format!("{relative}/"));
            } else {
                found.push(relative);
            }
        }
    }
    found.sort();
    found
}

fn gzip(content: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(content).unwrap();
    encoder.finish().unwrap()
}

fn zstd(content: &[u8]) -> Vec<u8> {
    assert!(content.len() < 0x0002_0000, "one raw block bounds this");
    let mut frame = vec![0x28, 0xB5, 0x2F, 0xFD, 0xA0];
    frame.extend_from_slice(&u32::try_from(content.len()).unwrap().to_le_bytes());
    let header = (u32::try_from(content.len()).unwrap() << 3) | 1;
    frame.extend_from_slice(&header.to_le_bytes()[..3]);
    frame.extend_from_slice(content);
    frame
}

fn greeting_tar() -> Vec<u8> {
    let mut header = TarHeader::ustar(b"hello.txt", TYPEFLAG_REGULAR);
    header.set_size(GREETING.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&header, GREETING);
    writer.finish()
}

fn package_tar() -> Vec<u8> {
    let mut writer = TarWriter::new();
    for name in [&b"pkg-1.0/"[..], b"pkg-1.0/src/"] {
        let header = TarHeader::ustar(name, TYPEFLAG_DIRECTORY);
        writer.push(&header, b"");
    }
    for (name, content) in [
        (&b"pkg-1.0/README"[..], &b"readme\n"[..]),
        (b"pkg-1.0/src/m.txt", b"module\n"),
    ] {
        let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
        header.set_size(content.len() as u64);
        writer.push(&header, content);
    }
    writer.finish()
}

fn package_zip() -> Vec<u8> {
    let members: [(&[u8], &[u8]); 2] = [
        (b"pkg-1.0/README", b"readme\n"),
        (b"pkg-1.0/src/m.txt", b"module\n"),
    ];
    let mut body = Vec::new();
    let mut directory = Vec::new();
    for (name, content) in members {
        let offset = u32::try_from(body.len()).unwrap();
        let mut crc = flate2::Crc::new();
        crc.update(content);
        let checksum = crc.sum();
        let length = u32::try_from(content.len()).unwrap();
        let name_length = u16::try_from(name.len()).unwrap();

        body.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04]);
        body.extend_from_slice(&20_u16.to_le_bytes());
        body.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        body.extend_from_slice(&checksum.to_le_bytes());
        body.extend_from_slice(&length.to_le_bytes());
        body.extend_from_slice(&length.to_le_bytes());
        body.extend_from_slice(&name_length.to_le_bytes());
        body.extend_from_slice(&0_u16.to_le_bytes());
        body.extend_from_slice(name);
        body.extend_from_slice(content);

        directory.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02]);
        directory.extend_from_slice(&20_u16.to_le_bytes());
        directory.extend_from_slice(&20_u16.to_le_bytes());
        directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&checksum.to_le_bytes());
        directory.extend_from_slice(&length.to_le_bytes());
        directory.extend_from_slice(&length.to_le_bytes());
        directory.extend_from_slice(&name_length.to_le_bytes());
        directory.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        directory.extend_from_slice(&offset.to_le_bytes());
        directory.extend_from_slice(name);
    }
    let count = u16::try_from(members.len()).unwrap();
    let directory_offset = u32::try_from(body.len()).unwrap();
    let directory_length = u32::try_from(directory.len()).unwrap();
    body.extend_from_slice(&directory);
    body.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06]);
    body.extend_from_slice(&[0, 0, 0, 0]);
    body.extend_from_slice(&count.to_le_bytes());
    body.extend_from_slice(&count.to_le_bytes());
    body.extend_from_slice(&directory_length.to_le_bytes());
    body.extend_from_slice(&directory_offset.to_le_bytes());
    body.extend_from_slice(&0_u16.to_le_bytes());
    body
}

fn tree(workspace: &Workspace) -> PathBuf {
    let root = workspace.path().join("tree");
    std::fs::create_dir_all(root.join("nested")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("a.txt"), b"alpha").unwrap();
    std::fs::write(root.join("nested").join("b.txt"), b"beta").unwrap();
    root
}

#[test]
fn get_resolves_transfers_verifies_materializes_and_records() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    let run = workspace.run(&["get", source.to_str().unwrap(), "--output", "out", "--json"]);
    assert_eq!(run.code(), 0, "{}", run.err());
    let body = run.json();
    assert_eq!(body["status"], "materialized");
    assert_eq!(body["entries"], 4);
    assert_eq!(
        entries_under(&workspace.path().join("out")),
        ["a.txt", "empty/", "nested/", "nested/b.txt"]
    );
}

#[test]
fn plan_resolves_and_reports_and_moves_no_bytes() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--lock",
        "one.lock",
    ]);
    let run = workspace.run(&[
        "plan",
        archive.to_str().unwrap(),
        "--output",
        "second",
        "--lock",
        "one.lock",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    let plan = run.out();
    assert!(plan.contains("artifacts:"), "{plan}");
    assert!(plan.contains("trust:"), "{plan}");
    assert!(!workspace.path().join("second").exists());
}

#[test]
fn apply_executes_a_plan() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--lock",
        "one.lock",
    ]);
    let plan = workspace
        .run(&[
            "plan",
            archive.to_str().unwrap(),
            "--output",
            "second",
            "--lock",
            "one.lock",
        ])
        .out();
    workspace.write("one.plan", plan.as_bytes());
    let run = workspace.run(&["apply", "one.plan", "--output", "second", "--json"]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(
        std::fs::read(workspace.path().join("second").join("hello.txt")).unwrap(),
        GREETING
    );
}

#[test]
fn verify_recomputes_a_materialized_tree() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    let run = workspace.run(&["verify", "out", "--json"]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(run.json()["status"], "verified");
}

#[test]
fn every_cache_subcommand_the_surface_names_acts() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    workspace.run(&["get", archive.to_str().unwrap(), "--output", "out"]);

    let listed = workspace.run(&["cache", "ls"]);
    assert_eq!(listed.code(), 0, "{}", listed.err());
    let digest = listed
        .out()
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    assert!(digest.starts_with("blake3:"), "{digest}");

    for arguments in [
        vec!["cache", "status"],
        vec!["cache", "verify"],
        vec!["cache", "pin", &digest],
        vec!["cache", "unpin", &digest],
        vec!["cache", "repair"],
        vec!["cache", "prune"],
        vec!["cache", "export", "bundle.tar"],
        vec!["cache", "import", "bundle.tar"],
    ] {
        let run = workspace.run(&arguments);
        assert_eq!(run.code(), 0, "{arguments:?} said {}", run.err());
    }

    let cleared = workspace.run(&["cache", "clear", "--yes"]);
    assert_eq!(cleared.code(), 0, "{}", cleared.err());
    assert_eq!(workspace.run(&["cache", "ls"]).out().trim(), "");
}

#[test]
fn completions_writes_a_script_for_every_shell_it_offers() {
    let workspace = Workspace::new();
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let run = workspace.run(&["completions", shell]);
        assert_eq!(run.code(), 0, "{shell} said {}", run.err());
        assert!(
            run.out().contains("fetchloom"),
            "the {shell} script named nothing"
        );
    }
}

#[test]
fn explain_reports_a_setting_and_where_it_came_from() {
    let workspace = Workspace::new();
    let whole = workspace.run(&["explain"]);
    assert_eq!(whole.code(), 0, "{}", whole.err());
    assert!(whole.out().contains("cache.dir"), "{}", whole.out());

    let one = workspace.run(&["--threads", "3", "explain", "threads"]);
    assert_eq!(one.code(), 0, "{}", one.err());
    assert!(
        one.out().contains("threads = 3 (command line)"),
        "{}",
        one.out()
    );
}

#[test]
fn a_word_that_is_no_command_is_a_usage_error() {
    let workspace = Workspace::new();
    for command in ["fetch", "download", "sync", "install"] {
        let run = workspace.run(&[command, "anything"]);
        assert_eq!(run.code(), 2, "{command} was accepted: {}", run.out());
    }
}

#[test]
fn a_file_url_resolves_the_same_as_the_path_it_names() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let by_path = workspace.run(&["get", archive.to_str().unwrap(), "--output", "a", "--json"]);
    let url = format!(
        "file:///{}",
        archive
            .to_string_lossy()
            .replace('\\', "/")
            .trim_start_matches('/')
    );
    let by_url = workspace.run(&["get", &url, "--output", "b", "--json"]);
    assert_eq!(by_path.code(), 0, "{}", by_path.err());
    assert_eq!(by_url.code(), 0, "{}", by_url.err());
    assert_eq!(by_path.json()["tree"], by_url.json()["tree"]);
}

#[test]
fn a_local_manifest_is_read_as_one_and_a_data_file_is_not() {
    let workspace = Workspace::new();
    workspace.write("payload.txt", b"payload\n");
    let manifest =
        "name: sample\nartifacts:\n  - id: payload.txt\n    sources: [\"payload.txt\"]\n";
    workspace.write("data.yaml", manifest.as_bytes());
    let run = workspace.run(&["get", "./data.yaml", "--output", "out", "--json"]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(run.json()["dataset"], "sample");
}

#[test]
fn a_reference_naming_one_file_records_a_lock_entry_for_it() {
    let workspace = Workspace::new();
    let blob = workspace.write("blob.txt", b"plain bytes\n");
    let run = workspace.run(&[
        "get",
        blob.to_str().unwrap(),
        "--output",
        "out",
        "--lock",
        "one.lock",
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    let lock = std::fs::read_to_string(workspace.path().join("one.lock")).unwrap_or_else(|error| {
        panic!("a reference resolving to one object wrote no lock: {error}")
    });
    assert!(lock.contains("blob.txt"), "{lock}");
    assert!(lock.contains("interop:"), "{lock}");
}

#[test]
fn a_reference_an_adapter_serves_and_cannot_reach_is_unresolved_rather_than_a_transport_failure() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "get",
        "s3://bucket/prefix/",
        "--output",
        "out",
        "--json",
        "--offline",
    ]);
    assert_eq!(run.code(), 40, "{}", run.out());
    assert_eq!(run.kind(), "policy.offline");
}

#[test]
fn a_reference_no_adapter_serves_says_that_and_never_that_the_build_serves_only_local_paths() {
    let workspace = Workspace::new();
    for reference in [
        "sftp://host/dataset.tar",
        "gopher://host/dataset.tar",
        "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        "unserved:record",
    ] {
        let run = workspace.run(&["get", reference, "--output", "out", "--json"]);
        assert_eq!(run.code(), 10, "{reference} said {}", run.out());
        assert_eq!(run.kind(), "reference.unresolved", "{reference}");
        let action = run.json()["next_action"].as_str().unwrap().to_owned();
        assert!(action.contains(reference), "{reference}: {action}");
        assert!(action.contains("serves"), "{reference}: {action}");
        for denied in ["file:", "only a local path"] {
            assert!(!action.contains(denied), "{reference}: {action}");
        }
    }
}

#[test]
fn a_reference_naming_nothing_says_so_and_names_the_reference() {
    let workspace = Workspace::new();
    let missing = workspace.run(&["get", "./not-here", "--json"]);
    assert_eq!(missing.code(), 10);
    assert_eq!(missing.kind(), "reference.unresolved");
    let action = missing.json()["next_action"].as_str().unwrap().to_owned();
    assert!(action.contains("./not-here"), "{action}");
    assert!(action.contains("exists"), "{action}");
}

#[test]
fn every_shipped_container_is_extracted_by_the_command() {
    let tar = greeting_tar();
    let cases: [(&str, Vec<u8>); 6] = [
        ("plain.tar", tar.clone()),
        ("wrapped.tar.gz", gzip(&tar)),
        ("wrapped.tar.zst", zstd(&tar)),
        ("wrapped.tar.xz", TAR_XZ.to_vec()),
        ("wrapped.tar.bz2", TAR_BZ2.to_vec()),
        ("pack.zip", package_zip()),
    ];
    for (name, bytes) in cases {
        let workspace = Workspace::new();
        let archive = workspace.write(name, &bytes);
        let run = workspace.run(&[
            "get",
            archive.to_str().unwrap(),
            "--output",
            "out",
            "--json",
        ]);
        assert_eq!(run.code(), 0, "{name} said {}{}", run.out(), run.err());
        let listing = entries_under(&workspace.path().join("out"));
        assert!(!listing.is_empty(), "{name} produced nothing");
    }
}

#[test]
fn every_shipped_single_object_compression_is_materialized_as_one_file() {
    let cases: [(&str, Vec<u8>); 4] = [
        ("one.gz", gzip(GREETING)),
        ("one.zst", zstd(GREETING)),
        ("one.xz", GREETING_XZ.to_vec()),
        ("one.bz2", GREETING_BZ2.to_vec()),
    ];
    for (name, bytes) in cases {
        let workspace = Workspace::new();
        let object = workspace.write(name, &bytes);
        let run = workspace.run(&["get", object.to_str().unwrap(), "--output", "out", "--json"]);
        assert_eq!(run.code(), 0, "{name} said {}", run.err());
        let listing = entries_under(&workspace.path().join("out"));
        assert_eq!(listing.len(), 1, "{name} produced {listing:?}");
        assert_eq!(
            std::fs::read(workspace.path().join("out").join(&listing[0])).unwrap(),
            GREETING,
            "{name}"
        );
    }
}

#[test]
fn select_and_exclude_choose_members_and_an_empty_selection_fails() {
    let workspace = Workspace::new();
    let archive = workspace.write("pack.tar", &package_tar());
    let selected = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "chosen",
        "--select",
        "pkg-1.0/src/**",
        "--json",
    ]);
    assert_eq!(selected.code(), 0, "{}", selected.err());
    assert_eq!(
        entries_under(&workspace.path().join("chosen")),
        ["pkg-1.0/", "pkg-1.0/src/", "pkg-1.0/src/m.txt"]
    );

    let excluded = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "trimmed",
        "--exclude",
        "pkg-1.0/src/**",
        "--json",
    ]);
    assert_eq!(excluded.code(), 0, "{}", excluded.err());
    assert!(
        !workspace
            .path()
            .join("trimmed")
            .join("pkg-1.0/src/m.txt")
            .exists()
    );

    let empty = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "none",
        "--select",
        "nothing/**",
        "--json",
    ]);
    assert_eq!(empty.code(), 10);
    assert_eq!(empty.kind(), "reference.unresolved");
}

#[test]
fn layout_flatten_drops_the_leading_components_of_a_real_archive() {
    let workspace = Workspace::new();
    let archive = workspace.write("pack.tar", &package_tar());
    let run = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "flat",
        "--layout",
        "flatten:1",
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(
        entries_under(&workspace.path().join("flat")),
        ["README", "src/", "src/m.txt"]
    );
}

#[test]
fn one_logical_tree_flattens_the_same_whether_or_not_its_encoding_declares_directories() {
    let workspace = Workspace::new();
    let tarred = workspace.write("pack.tar", &package_tar());
    let zipped = workspace.write("pack.zip", &package_zip());
    let from_tar = workspace.run(&[
        "get",
        tarred.to_str().unwrap(),
        "--output",
        "a",
        "--layout",
        "flatten:1",
        "--json",
    ]);
    let from_zip = workspace.run(&[
        "get",
        zipped.to_str().unwrap(),
        "--output",
        "b",
        "--layout",
        "flatten:1",
        "--json",
    ]);
    assert_eq!(from_tar.code(), 0, "{}", from_tar.err());
    assert_eq!(from_zip.code(), 0, "{}", from_zip.err());
    assert_eq!(from_tar.json()["tree"], from_zip.json()["tree"]);
}

#[test]
fn layout_flatten_past_a_file_member_is_unrepresentable() {
    let workspace = Workspace::new();
    let archive = workspace.write("pack.tar", &package_tar());
    let run = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "flat",
        "--layout",
        "flatten:9",
        "--json",
    ]);
    assert_eq!(run.code(), 60);
    assert_eq!(run.kind(), "destination.unrepresentable");
}

#[test]
fn no_extract_keeps_a_recognized_archive_as_one_file() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let run = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--no-extract",
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(
        entries_under(&workspace.path().join("out")),
        ["sample.tar.gz"]
    );
}

#[test]
fn no_cache_retains_nothing() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let run = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--no-cache",
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(workspace.run(&["cache", "ls"]).out().trim(), "");
}

#[test]
fn verify_always_rereads_the_destination_and_fingerprint_does_not() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    let fingerprint = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "out",
        "--verify",
        "fingerprint",
        "--json",
    ]);
    let always = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "out",
        "--verify",
        "always",
        "--json",
    ]);
    assert_eq!(fingerprint.code(), 0, "{}", fingerprint.err());
    assert_eq!(always.code(), 0, "{}", always.err());
    let read_lightly = fingerprint.json()["work"]["bytes_read"].as_u64().unwrap();
    let read_wholly = always.json()["work"]["bytes_read"].as_u64().unwrap();
    assert!(
        read_wholly > read_lightly,
        "always read {read_wholly} and fingerprint read {read_lightly}"
    );
}

#[test]
fn every_durability_tier_produces_the_same_tree() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    let mut digests = Vec::new();
    for tier in ["strict", "normal", "fast"] {
        let run = workspace.run(&[
            "get",
            source.to_str().unwrap(),
            "--output",
            tier,
            "--durability",
            tier,
            "--json",
        ]);
        assert_eq!(run.code(), 0, "{tier} said {}", run.err());
        digests.push(run.json()["tree"].as_str().unwrap().to_owned());
    }
    assert_eq!(digests[0], digests[1]);
    assert_eq!(digests[1], digests[2]);
}

#[test]
fn force_overwrites_a_modified_entry_that_a_plain_run_refuses() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    std::fs::write(workspace.path().join("out").join("a.txt"), b"changed").unwrap();

    let refused = workspace.run(&["get", source.to_str().unwrap(), "--output", "out", "--json"]);
    assert_eq!(refused.code(), 60);
    assert_eq!(refused.kind(), "destination.modified");

    let forced = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "out",
        "--force",
        "--json",
    ]);
    assert_eq!(forced.code(), 0, "{}", forced.err());
    assert_eq!(
        std::fs::read(workspace.path().join("out").join("a.txt")).unwrap(),
        b"alpha"
    );
}

#[test]
fn a_run_after_an_adopt_never_reports_a_tree_the_destination_does_not_hold() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    std::fs::write(workspace.path().join("out").join("a.txt"), b"changed").unwrap();

    let adopted = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "out",
        "--adopt",
        "--json",
    ]);
    assert_eq!(adopted.code(), 0, "{}", adopted.err());
    assert_eq!(adopted.json()["status"], "adopted");
    let held = adopted.json()["tree"].as_str().unwrap().to_owned();

    // contracts.md:422 and :427 — the adopted record states what the
    // destination holds, and that entry still differs from the tree this run
    // resolves, so the run stops rather than reporting a tree that is not
    // there. One outcome, not either of two.
    let next = workspace.run(&["get", source.to_str().unwrap(), "--output", "out", "--json"]);
    assert!(
        !held.is_empty(),
        "the adopt reported no tree, so nothing was compared"
    );
    assert_eq!(
        next.code(),
        60,
        "a run after an adopt did not stop on the entry the adopt recorded as differing: {}",
        next.err()
    );
    assert_eq!(next.kind(), "destination.modified");
}

#[test]
fn locked_refuses_a_reference_the_lock_does_not_pin() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    let run = workspace.run(&[
        "get",
        archive.to_str().unwrap(),
        "--output",
        "out",
        "--lock",
        "empty.lock",
        "--locked",
        "--json",
    ]);
    assert_eq!(run.code(), 40);
    assert_eq!(run.kind(), "policy.trust_refused");
}

#[test]
fn offline_refuses_a_network_reference_before_reaching_for_it() {
    let workspace = Workspace::new();
    let run = workspace.run(&[
        "get",
        "https://host.invalid/data.tar.gz",
        "--offline",
        "--json",
    ]);
    assert_eq!(run.code(), 40);
    assert_eq!(run.kind(), "policy.offline");
}

#[test]
fn events_are_written_where_the_flag_names_and_json_stays_the_result() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    let run = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "out",
        "--json",
        "--events",
        "stream.ndjson",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert_eq!(run.json()["status"], "materialized");
    let stream = std::fs::read_to_string(workspace.path().join("stream.ndjson")).unwrap();
    let names: Vec<String> = stream
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["event"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(names.first().map(String::as_str), Some("run.start"));
    assert_eq!(names.last().map(String::as_str), Some("run.end"));
}

#[test]
fn quiet_and_the_display_modes_change_no_result() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    let mut digests = Vec::new();
    for extra in [
        vec!["--quiet"],
        vec!["--display", "plain"],
        vec!["--display", "none"],
        vec!["--display", "live"],
        vec!["--no-animation"],
    ] {
        let mut arguments = vec!["get", source.to_str().unwrap(), "--output", "out", "--json"];
        arguments.extend_from_slice(&extra);
        let run = workspace.run(&arguments);
        assert_eq!(run.code(), 0, "{extra:?} said {}", run.err());
        digests.push(run.json()["tree"].as_str().unwrap().to_owned());
    }
    assert!(
        digests.windows(2).all(|pair| pair[0] == pair[1]),
        "{digests:?}"
    );
}

#[test]
fn threads_bounds_the_pool_and_changes_no_result() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    let detected = workspace.run(&["get", source.to_str().unwrap(), "--output", "out", "--json"]);
    let bounded = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "second",
        "--threads",
        "1",
        "--json",
    ]);
    assert_eq!(detected.code(), 0, "{}", detected.err());
    assert_eq!(bounded.code(), 0, "{}", bounded.err());
    assert_eq!(detected.json()["tree"], bounded.json()["tree"]);
}

#[test]
fn cache_dir_and_no_config_and_config_each_decide_where_settings_come_from() {
    let workspace = Workspace::new();
    let elsewhere = workspace.path().join("elsewhere");
    let source = tree(&workspace);
    let run = workspace.run(&[
        "get",
        source.to_str().unwrap(),
        "--output",
        "out",
        "--cache-dir",
        elsewhere.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert!(elsewhere.join("objects").is_dir());

    workspace.write("chosen.toml", b"[cache]\ndir = \"from-the-config\"\n");
    let named =
        workspace.run_reading_configuration(&["--config", "chosen.toml", "explain", "cache.dir"]);
    assert!(named.out().contains("from-the-config"), "{}", named.out());
    let refused = workspace.run_reading_configuration(&[
        "--config",
        "chosen.toml",
        "--no-config",
        "explain",
        "cache.dir",
    ]);
    assert!(
        !refused.out().contains("from-the-config"),
        "{}",
        refused.out()
    );
}

#[test]
fn every_exit_code_the_table_names_is_accounted_for() {
    let reference = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("docs")
            .join("reference.md"),
    )
    .unwrap();
    let table = reference
        .split("## Exit codes")
        .nth(1)
        .expect("reference.md states no exit codes")
        .split("## ")
        .next()
        .unwrap();
    let named: std::collections::BTreeSet<i32> = table
        .lines()
        .filter_map(|line| line.strip_prefix("| "))
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(code, _)| code.parse().ok())
        .collect();
    assert!(
        named.len() >= 11,
        "the exit code table parsed to {named:?}, so this proves nothing"
    );

    // Three of the codes are produced by runs that need a different fixture,
    // and each is named here so no code in the table is unaccounted for.
    let elsewhere: std::collections::BTreeSet<i32> = [30, 50, 130].into_iter().collect();
    let here: std::collections::BTreeSet<i32> =
        [0, 2, 10, 20, 40, 60, 70, 80].into_iter().collect();
    let covered: std::collections::BTreeSet<i32> = here.union(&elsewhere).copied().collect();
    assert_eq!(
        covered, named,
        "a code the table names is produced by no run, or a run produces one the table does not name"
    );

    let workspace = Workspace::new();

    let source = tree(&workspace);

    let success = workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    assert_eq!(success.code(), 0, "{}", success.err());

    assert_eq!(workspace.run(&["not-a-command"]).code(), 2);
    assert_eq!(workspace.run(&["get", "./missing"]).code(), 10);
    assert_eq!(
        workspace
            .run(&["get", "https://host.invalid/x.tar", "--offline"])
            .code(),
        40
    );
    let refusing = TestServer::start(Script::serving(Vec::new()).replying(vec![
        Reply::Status {
            code: 503,
            retry_after: None,
        };
        8
    ]))
    .unwrap();
    assert_eq!(
        workspace
            .run(&[
                "get",
                &format!("{}/object.bin", refusing.origin()),
                "--output",
                "remote"
            ])
            .code(),
        20
    );

    std::fs::write(workspace.path().join("out").join("stray.txt"), b"stray").unwrap();
    assert_eq!(
        workspace
            .run(&["get", source.to_str().unwrap(), "--output", "out"])
            .code(),
        60
    );

    let broken = workspace.write("broken.tar.gz", b"this is not a gzip stream at all");
    assert_eq!(
        workspace
            .run(&["get", broken.to_str().unwrap(), "--output", "bad"])
            .code(),
        70
    );

    std::fs::write(workspace.cache().join("format"), b"not-the-fingerprint").unwrap();
    assert_eq!(workspace.run(&["cache", "status"]).code(), 80);
}

#[test]
fn an_integrity_mismatch_against_a_receipt_exits_thirty() {
    let workspace = Workspace::new();
    let source = tree(&workspace);
    workspace.run(&["get", source.to_str().unwrap(), "--output", "out"]);
    std::fs::write(workspace.path().join("out").join("a.txt"), b"tampered").unwrap();
    let run = workspace.run(&["verify", "out", "--json"]);
    assert_eq!(run.code(), 30);
    assert_eq!(run.kind(), "integrity.mismatch");
}

#[test]
fn cache_clear_clears_a_cache_whose_format_does_not_match() {
    let workspace = Workspace::new();
    let archive = workspace.write("sample.tar.gz", &gzip(&greeting_tar()));
    workspace.run(&["get", archive.to_str().unwrap(), "--output", "out"]);
    std::fs::write(workspace.cache().join("format"), b"not-the-fingerprint").unwrap();

    let refused = workspace.run(&["cache", "status"]);
    assert_eq!(refused.code(), 80);
    assert!(
        refused.err().contains("cache clear"),
        "the message named no fix: {}",
        refused.err()
    );

    let cleared = workspace.run(&["cache", "clear", "--yes"]);
    assert_eq!(
        cleared.code(),
        0,
        "the command the message names was refused: {}",
        cleared.err()
    );
    assert_eq!(workspace.run(&["cache", "status"]).code(), 0);
}

#[test]
fn explain_answers_in_json_when_it_is_asked_to() {
    let workspace = Workspace::new();
    let run = workspace.run(&["--json", "explain"]);
    assert_eq!(run.code(), 0, "{}", run.err());

    let reported: serde_json::Value =
        serde_json::from_str(run.out().trim()).unwrap_or_else(|error| {
            panic!(
                "explain --json did not answer in JSON ({error}): {}",
                run.out()
            )
        });
    let settings = reported["settings"]
        .as_array()
        .unwrap_or_else(|| panic!("no settings array: {reported}"));
    assert!(
        settings.iter().any(|row| row["key"] == "cache.dir"),
        "{reported}"
    );
}

#[test]
fn explain_reports_a_measured_setting_as_measured_rather_than_as_unknown() {
    let workspace = Workspace::new();
    let run = workspace.run(&["explain", "threads"]);
    assert_eq!(run.code(), 0, "{}", run.err());
    assert!(
        !run.out().contains('?'),
        "a thread budget this machine detects was reported as a value nothing supplied: {}",
        run.out()
    );
    assert!(
        run.out().contains("(measured)"),
        "the measured budget did not say it was measured: {}",
        run.out()
    );
}

#[test]
fn a_flag_a_command_cannot_act_on_is_refused_rather_than_accepted() {
    let workspace = Workspace::new();
    workspace.write("thing.txt", b"hello\n");
    for flag in ["--force", "--adopt"] {
        let run = workspace.run(&["plan", "./thing.txt", flag]);
        assert_eq!(
            run.code(),
            2,
            "plan {flag} was accepted: {} {}",
            run.out(),
            run.err()
        );
        assert!(
            run.err().contains(flag),
            "the refusal did not name the flag: {}",
            run.err()
        );
    }
}

#[test]
fn a_second_reference_is_a_usage_error_rather_than_a_declared_capability() {
    let workspace = Workspace::new();
    workspace.write("one.txt", b"one\n");
    workspace.write("two.txt", b"two\n");
    let run = workspace.run(&["get", "./one.txt", "./two.txt"]);
    assert_eq!(run.code(), 2, "{} {}", run.out(), run.err());
}

fn bytes_under(root: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(listing) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in listing.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(found) = path.metadata() {
                total += found.len();
            }
        }
    }
    total
}

#[test]
fn the_byte_count_a_run_reports_is_what_its_destination_holds() {
    for (name, content) in [
        ("sample.tar.gz", gzip(&greeting_tar())),
        ("package.zip", package_zip()),
        ("greeting.txt.gz", gzip(GREETING)),
    ] {
        let workspace = Workspace::new();
        let archive = workspace.write(name, &content);
        let run = workspace.run(&[
            "get",
            archive.to_str().unwrap(),
            "--output",
            "out",
            "--json",
        ]);
        assert_eq!(run.code(), 0, "{}", run.err());
        let body = run.json();
        let destination = workspace.path().join("out");
        assert_eq!(
            body["bytes"].as_u64(),
            Some(bytes_under(&destination)),
            "{name} reported a byte count that is not the length of what it materialized"
        );
    }
}

#[test]
fn the_byte_count_a_manifest_run_reports_is_what_its_destination_holds() {
    let workspace = Workspace::new();
    workspace.write("one.tar.gz", &gzip(&greeting_tar()));
    workspace.write("two.tar.gz", &gzip(&package_tar()));
    let manifest = workspace.write(
        "data.yaml",
        b"name: sample\nartifacts:\n  - id: one\n    sources:\n      - ./one.tar.gz\n  - id: two\n    sources:\n      - ./two.tar.gz\n",
    );
    let run = workspace.run(&[
        "get",
        manifest.to_str().unwrap(),
        "--output",
        "out",
        "--json",
    ]);
    assert_eq!(run.code(), 0, "{}", run.err());
    let body = run.json();
    assert_eq!(
        body["bytes"].as_u64(),
        Some(bytes_under(&workspace.path().join("out"))),
        "a manifest run reported a byte count that is not the length of what it materialized"
    );
}
