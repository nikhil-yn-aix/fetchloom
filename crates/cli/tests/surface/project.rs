//! `get` with no argument: the datasets a project file names, where each lands,
//! and what two entries pointing at one place do.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::Path;
use std::process::Output;

use tempfile::TempDir;

use crate::support;

fn project(datasets: &str) -> TempDir {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path();
    for name in ["one", "two", "three"] {
        let source = root.join("sources").join(name);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.txt"), name.as_bytes()).unwrap();
    }
    std::fs::write(root.join("fetchloom.toml"), datasets).unwrap();
    temporary
}

fn source_of(root: &Path, name: &str) -> String {
    root.join("sources")
        .join(name)
        .join("a.txt")
        .to_string_lossy()
        .replace('\\', "/")
}

fn run_in(directory: &Path, cache: &Path, arguments: &[&str]) -> Output {
    support::fetchloom_reading_configuration()
        .current_dir(directory)
        .args(arguments)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .output()
        .unwrap()
}

#[test]
fn three_datasets_land_beside_the_project_file_from_however_deep_you_stand() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path();
    for name in ["one", "two", "three"] {
        let source = root.join("sources").join(name);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.txt"), name.as_bytes()).unwrap();
    }
    std::fs::write(
        root.join("fetchloom.toml"),
        format!(
            "[datasets]\nfirst = \"{}\"\nsecond = {{ ref = \"{}\" }}\nthird = {{ ref = \"{}\", output = \"data/third\" }}\n",
            source_of(root, "one"),
            source_of(root, "two"),
            source_of(root, "three"),
        ),
    )
    .unwrap();
    let deep = root.join("a").join("b").join("c").join("d");
    std::fs::create_dir_all(&deep).unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(&deep, cache.path(), &["get"]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(root.join("first").join("a.txt").is_file());
    assert!(root.join("second").join("a.txt").is_file());
    assert!(root.join("data").join("third").join("a.txt").is_file());
    assert!(
        !deep.join("first").exists(),
        "a run from a subdirectory wrote beside itself rather than beside the project file"
    );
    assert!(
        root.join("fetchloom.lock").is_file(),
        "the lock was not written beside the project file"
    );
}

#[test]
fn a_locked_project_run_is_the_reproducible_install() {
    let temporary = TempDir::new().unwrap();
    let root = temporary.path();
    let source = root.join("sources").join("one");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"first").unwrap();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!("[datasets]\nfirst = \"{}\"\n", source_of(root, "one")),
    )
    .unwrap();
    let cache = TempDir::new().unwrap();

    let first = run_in(root, cache.path(), &["get"]);
    assert_eq!(first.status.code(), Some(0));

    let again = run_in(root, cache.path(), &["get", "--locked"]);
    assert_eq!(
        again.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );

    std::fs::write(source.join("a.txt"), b"second and different").unwrap();
    let moved = run_in(root, cache.path(), &["get", "--locked", "--force"]);
    assert_ne!(
        moved.status.code(),
        Some(0),
        "a locked run accepted bytes the lock does not pin"
    );
}

#[test]
fn two_entries_writing_to_one_destination_fail_before_a_byte_moves() {
    let temporary = project("");
    let root = temporary.path();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!(
            "[datasets]\nalpha = {{ ref = \"{}\", output = \"out\" }}\nbeta = {{ ref = \"{}\", output = \"nested/../out\" }}\n",
            source_of(root, "one"),
            source_of(root, "two"),
        ),
    )
    .unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(root, cache.path(), &["get"]);

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("alpha") && stderr.contains("beta"),
        "{stderr}"
    );
    assert!(
        !root.join("out").exists(),
        "a refused project run wrote a destination anyway"
    );
}

#[test]
fn an_unknown_key_inside_an_entry_names_the_key() {
    let temporary = project("");
    let root = temporary.path();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!(
            "[datasets]\nalpha = {{ ref = \"{}\", selct = [\"a\"] }}\n",
            source_of(root, "one"),
        ),
    )
    .unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(root, cache.path(), &["get"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("selct"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn get_with_no_argument_and_no_project_file_says_what_to_do() {
    let temporary = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let output = run_in(temporary.path(), cache.path(), &["get"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("fetchloom.toml"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn a_project_file_naming_no_dataset_says_so() {
    let temporary = project("offline = false\n");
    let cache = TempDir::new().unwrap();
    let output = run_in(temporary.path(), cache.path(), &["get"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("datasets"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn one_destination_cannot_describe_several_datasets() {
    let temporary = project("");
    let root = temporary.path();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!("[datasets]\nalpha = \"{}\"\n", source_of(root, "one")),
    )
    .unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(root, cache.path(), &["get", "--output", "somewhere"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(!root.join("somewhere").exists());
}

#[test]
fn a_named_reference_writes_its_lock_beside_the_project_file_too() {
    let temporary = project("");
    let root = temporary.path();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!("[datasets]\nalpha = \"{}\"\n", source_of(root, "one")),
    )
    .unwrap();
    let deep = root.join("deep");
    std::fs::create_dir_all(&deep).unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(
        &deep,
        cache.path(),
        &["get", &source_of(root, "one"), "--output", "here"],
    );

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(deep.join("here").join("a.txt").is_file());
    assert!(
        root.join("fetchloom.lock").is_file(),
        "the lock landed somewhere other than beside the project file"
    );
    assert!(!deep.join("fetchloom.lock").exists());
}

#[test]
fn a_dataset_that_fails_does_not_stop_the_ones_after_it() {
    let temporary = project("");
    let root = temporary.path();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!(
            "[datasets]\nalpha = \"{}/definitely-missing\"\nbeta = \"{}\"\n",
            source_of(root, "one"),
            source_of(root, "two"),
        ),
    )
    .unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(root, cache.path(), &["get"]);

    assert_eq!(
        output.status.code(),
        Some(10),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        root.join("beta").join("a.txt").is_file(),
        "a dataset after the one that failed was never fetched"
    );
}

#[test]
fn a_layout_no_run_can_take_stops_the_run_before_anything_lands() {
    let temporary = project("");
    let root = temporary.path();
    std::fs::write(
        root.join("fetchloom.toml"),
        format!(
            "[datasets]\nalpha = \"{}\"\nbeta = {{ ref = \"{}\", layout = \"sideways\" }}\n",
            source_of(root, "one"),
            source_of(root, "two"),
        ),
    )
    .unwrap();
    let cache = TempDir::new().unwrap();

    let output = run_in(root, cache.path(), &["get"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("sideways"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.join("alpha").exists(),
        "the entry before the bad one was fetched before the file was checked"
    );
}
