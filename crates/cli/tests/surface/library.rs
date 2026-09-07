//! `--library`, `where`, and what the library does with a name a filesystem
//! cannot take.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to run the binary is the assertion"
)]

use std::path::{Path, PathBuf};
use std::process::Output;

use tempfile::TempDir;

use crate::support;

struct Scene {
    temporary: TempDir,
    cache: TempDir,
    library: TempDir,
}

impl Scene {
    fn new() -> Self {
        let temporary = TempDir::new().unwrap();
        let source = temporary.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("a.txt"), b"hello").unwrap();
        std::fs::write(source.join("b.txt"), b"world").unwrap();
        Self {
            temporary,
            cache: TempDir::new().unwrap(),
            library: TempDir::new().unwrap(),
        }
    }

    fn manifest(&self, name: &str, release: Option<&str>) -> PathBuf {
        let path = self
            .temporary
            .path()
            .join(format!("{}.yaml", name.replace(['/', ':'], "-")));
        let release = release.map_or_else(String::new, |value| format!("release: \"{value}\"\n"));
        std::fs::write(
            &path,
            format!(
                "name: \"{name}\"\n{release}artifacts:\n  - id: \"data\"\n    sources:\n      - \"source/a.txt\"\n"
            ),
        )
        .unwrap();
        path
    }

    fn run(&self, arguments: &[&str]) -> Output {
        support::fetchloom()
            .current_dir(self.temporary.path())
            .args(arguments)
            .env("FETCHLOOM_CACHE_DIR", self.cache.path())
            .env("FETCHLOOM_LIBRARY_DIR", self.library.path())
            .output()
            .unwrap()
    }

    fn where_is(&self, reference: &str) -> PathBuf {
        let output = self.run(&["where", reference, "--json"]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let answered: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        PathBuf::from(answered["path"].as_str().unwrap())
    }
}

fn entries_under(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(names) = std::fs::read_dir(root) else {
        return found;
    };
    for name in names.flatten() {
        let Ok(versions) = std::fs::read_dir(name.path()) else {
            continue;
        };
        for version in versions.flatten() {
            found.push(version.path());
        }
    }
    found.sort();
    found
}

#[test]
fn where_names_the_path_a_run_would_use_and_fetches_nothing() {
    let scene = Scene::new();
    let manifest = scene.manifest("corpus", None);

    let named = scene.where_is(manifest.to_str().unwrap());

    assert!(
        named.starts_with(scene.library.path()),
        "{}",
        named.display()
    );
    assert!(
        !named.exists(),
        "where materialized something rather than naming a path"
    );

    let fetched = scene.run(&["get", manifest.to_str().unwrap(), "--library", "--json"]);
    assert_eq!(
        fetched.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&fetched.stdout).unwrap();
    assert_eq!(
        PathBuf::from(result["destination"].as_str().unwrap()),
        named,
        "get --library and where disagree about where a dataset lives"
    );
    assert!(named.join("data").is_file());
}

#[test]
fn two_releases_of_one_dataset_sit_beside_each_other() {
    let scene = Scene::new();
    scene.manifest("corpus", Some("2012"));
    std::fs::rename(
        scene.temporary.path().join("corpus.yaml"),
        scene.temporary.path().join("corpus-2012.yaml"),
    )
    .unwrap();
    let second = scene.manifest("corpus", Some("2013"));

    let one = scene.where_is(
        scene
            .temporary
            .path()
            .join("corpus-2012.yaml")
            .to_str()
            .unwrap(),
    );
    let two = scene.where_is(second.to_str().unwrap());

    assert_ne!(one, two);
    assert_eq!(one.parent(), two.parent());
}

#[test]
fn a_name_a_windows_volume_refuses_lands_somewhere_it_does_not() {
    let scene = Scene::new();
    for name in ["CON", "nul", "org/name", "zenodo:1234", "trailing "] {
        let manifest = scene.manifest(name, None);
        let named = scene.where_is(manifest.to_str().unwrap());
        let component = named
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            !component.contains('/') && !component.contains('\\') && !component.contains(':'),
            "{name} became {component}"
        );
        assert!(!component.ends_with(' ') && !component.ends_with('.'));
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        assert!(
            !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str()),
            "{name} became a device name"
        );

        let fetched = scene.run(&["get", manifest.to_str().unwrap(), "--library"]);
        assert_eq!(
            fetched.status.code(),
            Some(0),
            "{name}: {}",
            String::from_utf8_lossy(&fetched.stderr)
        );
        assert!(named.join("data").is_file(), "{name} did not land");
    }
}

#[test]
fn a_library_file_is_never_a_hardlink_to_the_object_the_cache_holds() {
    let scene = Scene::new();
    let manifest = scene.manifest("corpus", None);
    let fetched = scene.run(&["get", manifest.to_str().unwrap(), "--library"]);
    assert_eq!(
        fetched.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    let named = scene.where_is(manifest.to_str().unwrap());

    std::fs::write(named.join("data"), b"edited through the library").unwrap();

    let verified = scene.run(&["cache", "verify"]);
    assert_eq!(
        verified.status.code(),
        Some(0),
        "an edit through a library file reached the object the cache holds: {}",
        String::from_utf8_lossy(&verified.stderr)
    );
}

#[test]
fn a_library_and_an_output_together_are_not_a_run() {
    let scene = Scene::new();
    let manifest = scene.manifest("corpus", None);
    let output = scene.run(&[
        "get",
        manifest.to_str().unwrap(),
        "--library",
        "--output",
        "elsewhere",
    ]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn status_diff_revert_and_promote_work_against_a_library_tree() {
    let scene = Scene::new();
    let manifest = scene.manifest("corpus", None);
    assert_eq!(
        scene
            .run(&["get", manifest.to_str().unwrap(), "--library"])
            .status
            .code(),
        Some(0)
    );
    let named = scene.where_is(manifest.to_str().unwrap());
    let entry = named.join("data");
    std::fs::write(&entry, b"mine now").unwrap();

    let status = scene.run(&["status", named.to_str().unwrap()]);
    assert_eq!(status.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("modified"),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );

    let differed = scene.run(&["diff", named.to_str().unwrap()]);
    assert_eq!(differed.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&differed.stdout).contains("data"));

    let promoted = scene.run(&["promote", named.to_str().unwrap()]);
    assert_eq!(
        promoted.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&promoted.stderr)
    );

    let reverted = scene.run(&["revert", named.to_str().unwrap()]);
    assert_eq!(
        reverted.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&reverted.stderr)
    );
    assert_eq!(std::fs::read(&entry).unwrap(), b"hello");

    let verified = scene.run(&["verify", named.to_str().unwrap()]);
    assert_eq!(verified.status.code(), Some(0));
}

#[test]
fn the_library_lists_what_it_holds_and_removes_only_what_it_is_told_to() {
    let scene = Scene::new();
    let manifest = scene.manifest("corpus", None);
    assert_eq!(
        scene
            .run(&["get", manifest.to_str().unwrap(), "--library"])
            .status
            .code(),
        Some(0)
    );
    let named = scene.where_is(manifest.to_str().unwrap());

    let listed = scene.run(&["library", "ls", "--json"]);
    assert_eq!(listed.status.code(), Some(0));
    let holding: serde_json::Value = serde_json::from_slice(&listed.stdout).unwrap();
    assert_eq!(holding["entries"].as_array().unwrap().len(), 1);
    assert!(holding["bytes"].as_u64().unwrap() > 0);

    let outside = scene.temporary.path().join("source");
    let refused = scene.run(&["library", "rm", outside.to_str().unwrap()]);
    assert_ne!(refused.status.code(), Some(0));
    assert!(outside.join("a.txt").is_file());

    let climbing = scene
        .library
        .path()
        .join("..")
        .join(scene.temporary.path().file_name().unwrap_or_default())
        .join("source")
        .to_string_lossy()
        .into_owned();
    let out_through_the_library = scene.run(&["library", "rm", &climbing]);
    assert_ne!(
        out_through_the_library.status.code(),
        Some(0),
        "a path that climbs back out of the library was accepted"
    );
    assert!(
        outside.join("a.txt").is_file(),
        "a path that climbs out of the library removed something outside it"
    );

    let removed = scene.run(&["library", "rm", named.to_str().unwrap()]);
    assert_eq!(
        removed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!named.exists());
    assert!(entries_under(scene.library.path()).is_empty());

    let again = scene.run(&["get", manifest.to_str().unwrap(), "--library"]);
    assert_eq!(
        again.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert!(named.join("data").is_file());
}

#[test]
fn a_second_run_into_a_library_entry_says_nothing_changed() {
    let scene = Scene::new();
    let manifest = scene.manifest("corpus", None);
    assert_eq!(
        scene
            .run(&["get", manifest.to_str().unwrap(), "--library"])
            .status
            .code(),
        Some(0)
    );
    let again = scene.run(&["get", manifest.to_str().unwrap(), "--library", "--json"]);
    assert_eq!(
        again.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&again.stdout).unwrap();
    assert_eq!(result["status"].as_str(), Some("unchanged"));
}

#[test]
fn a_library_on_another_volume_than_the_cache_still_materializes() {
    let temporary = TempDir::new().unwrap();
    let source = temporary.path().join("source");
    std::fs::create_dir_all(&source).unwrap();
    std::fs::write(source.join("a.txt"), b"hello").unwrap();
    let manifest = temporary.path().join("corpus.yaml");
    std::fs::write(
        &manifest,
        "name: \"corpus\"\nartifacts:\n  - id: \"data\"\n    sources:\n      - \"source/a.txt\"\n",
    )
    .unwrap();

    let elsewhere = Path::new(env!("CARGO_TARGET_TMPDIR")).join("library-volume");
    let _ = std::fs::remove_dir_all(&elsewhere);
    std::fs::create_dir_all(&elsewhere).unwrap();
    let cache = TempDir::new().unwrap();

    let output = support::fetchloom()
        .current_dir(temporary.path())
        .args(["get", manifest.to_str().unwrap(), "--library", "--json"])
        .env("FETCHLOOM_CACHE_DIR", cache.path())
        .env("FETCHLOOM_LIBRARY_DIR", &elsewhere)
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let landed = PathBuf::from(result["destination"].as_str().unwrap());
    assert!(landed.starts_with(&elsewhere), "{}", landed.display());
    assert_eq!(std::fs::read(landed.join("data")).unwrap(), b"hello");
    let _ = std::fs::remove_dir_all(&elsewhere);
}

#[test]
fn a_volume_that_refuses_a_clone_says_the_bytes_were_copied() {
    use fetchloom_engine::seam::platform::Platform as _;

    let scene = Scene::new();
    let big = scene.temporary.path().join("source").join("big.bin");
    let mut noise = Vec::with_capacity(3 << 20);
    let mut state = 0x2545_F491_4F6C_DD1D_u64;
    while noise.len() < (3 << 20) {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        noise.extend_from_slice(&state.to_le_bytes());
    }
    noise.truncate(3 << 20);
    std::fs::write(&big, &noise).unwrap();
    let manifest = scene.temporary.path().join("big.yaml");
    std::fs::write(
        &manifest,
        "name: \"big\"\nartifacts:\n  - id: \"data\"\n    sources:\n      - \"source/big.bin\"\n",
    )
    .unwrap();

    let platform = fetchloom_platform::NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let clones = platform
        .volume_capabilities(scene.cache.path())
        .unwrap()
        .clone;

    let output = scene.run(&[
        "get",
        manifest.to_str().unwrap(),
        "--library",
        "--lock",
        scene.temporary.path().join("big.lock").to_str().unwrap(),
    ]);

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let landed = scene.where_is(manifest.to_str().unwrap()).join("data");
    assert_eq!(std::fs::read(&landed).unwrap().len(), 3 << 20);

    let said = String::from_utf8_lossy(&output.stderr).contains("copy-on-write clone");
    assert_eq!(
        said,
        !clones,
        "the volume {} clone, and the run {} say the bytes were copied instead",
        if clones { "does" } else { "does not" },
        if said { "did" } else { "did not" }
    );
}
