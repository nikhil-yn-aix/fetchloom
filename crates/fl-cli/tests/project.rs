//! The rules of the project loop, one test each: add, remove, lock, sync, status and restore
//! against a fake zenodo and local folders.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use crate::support::{Failure, IRIS, World, json};

type TestResult = Result<(), Failure>;

fn ok(ran: &crate::support::Ran) -> Result<(), Failure> {
    if ran.code == Some(0) {
        Ok(())
    } else {
        Err(format!("exit {:?}\n{}{}", ran.code, ran.stdout, ran.stderr).into())
    }
}

fn data(world: &World, rest: &str) -> std::path::PathBuf {
    world.project.join("data").join(rest)
}

/// Replaces a placed file with new bytes the way an editor saves, leaving the store alone.
fn edit(file: &Path, bytes: &[u8]) -> TestResult {
    fl_store::data_dir::remove(file)?;
    std::fs::write(file, bytes)?;
    Ok(())
}

async fn two_files(world: &World) -> TestResult {
    world
        .record(
            "7",
            "cc-by-4.0",
            &[("a.csv", b"a,1\n", None), ("b.csv", b"b,2\n", None)],
        )
        .await;
    ok(&world.here(&["add", "zenodo:7", "--as", "ab"])?)
}

#[tokio::test(flavor = "multi_thread")]
async fn init_writes_a_minimal_manifest_and_never_overwrites_it() -> TestResult {
    let world = World::start().await?;
    std::fs::create_dir(world.project.join(".git"))?;
    let ran = world.here(&["init", "--name", "eeg-study"])?;
    ok(&ran)?;
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert_eq!(manifest, "[project]\nname = \"eeg-study\"\n");
    let ignore = std::fs::read_to_string(world.project.join(".gitignore"))?;
    assert_eq!(ignore, "data/\n.fetchloom/\n");
    assert!(ran.stderr.contains(".gitignore"), "{}", ran.stderr);
    let again = world.here(&["init"])?;
    assert_eq!(again.code, Some(50));
    assert!(
        again.stderr.contains("error[project.exists]"),
        "{}",
        again.stderr
    );
    assert!(again.stderr.contains("data.toml"), "{}", again.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_again_with_other_filters_says_what_changed_and_the_same_is_a_no_op() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let ran = world.here(&["add", "zenodo:7", "--as", "ab", "--select", "a.csv"])?;
    ok(&ran)?;
    assert!(
        ran.stderr
            .contains("Updated ab: 2 files, 8 B before, 1 file, 4 B now"),
        "{}",
        ran.stderr
    );
    assert!(!data(&world, "ab/b.csv").exists());
    assert!(
        ran.stderr.contains("Removed data/ab/b.csv"),
        "{}",
        ran.stderr
    );
    let before = world.requests().await?;
    let ran = world.here(&["add", "zenodo:7", "--as", "ab", "--select", "a.csv"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("nothing to change"), "{}", ran.stderr);
    assert_eq!(world.requests().await?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_name_for_another_record_suggests_as() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world
        .record("8", "cc-by-4.0", &[("c.csv", b"c", None)])
        .await;
    let ran = world.here(&["add", "zenodo:8", "--as", "ab"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("--as NAME"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filter_that_matches_nothing_names_the_pattern_the_count_and_examples() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let ran = world.here(&["add", "zenodo:7", "--as", "none", "--select", "*.edf"])?;
    assert_eq!(ran.code, Some(12), "{}", ran.stderr);
    for part in ["*.edf", "2", "a.csv"] {
        assert!(ran.stderr.contains(part), "{part}: {}", ran.stderr);
    }
    assert!(ran.stderr.contains("help:"), "{}", ran.stderr);
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(!manifest.contains("none"), "{manifest}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restrictive_license_is_a_warning() -> TestResult {
    let world = World::start().await?;
    world
        .record("9", "cc-by-nc-4.0", &[("a.csv", b"a", None)])
        .await;
    let ran = world.here(&["add", "zenodo:9"])?;
    ok(&ran)?;
    assert!(
        ran.stderr.contains("warning: 9 is licensed CC-BY-NC-4.0"),
        "{}",
        ran.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_record_needs_a_yes_and_writes_nothing_without_one() -> TestResult {
    let world = World::start().await?;
    world
        .record("10", "cc-by-4.0", &[("big.bin", b"x", Some(2 << 30))])
        .await;
    let ran = world.here(&["add", "zenodo:10"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("2.0 GiB"), "{}", ran.stderr);
    assert!(ran.stderr.contains("--yes"), "{}", ran.stderr);
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(!manifest.contains("zenodo:10"), "{manifest}");
    assert!(!world.project.join("data.lock").exists());
    let downloads = world
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().starts_with("/files/"))
        .count();
    assert_eq!(downloads, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_sync_writes_the_manifest_and_lock_only() -> TestResult {
    let world = World::start().await?;
    let ran = world.here(&["add", "zenodo:3608990", "--no-sync"])?;
    ok(&ran)?;
    assert!(world.project.join("data.lock").exists());
    assert!(!world.iris().exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn link_copy_places_writable_copies() -> TestResult {
    let world = World::start().await?;
    ok(&world.here(&["add", "zenodo:3608990", "--link", "copy"])?)?;
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(manifest.contains("link = \"copy\""), "{manifest}");
    assert!(!std::fs::metadata(world.iris())?.permissions().readonly());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relative_folder_added_from_below_the_root_is_stored_from_the_root() -> TestResult {
    let world = World::start().await?;
    ok(&world.here(&["init"])?)?;
    let below = world.project.join("scripts");
    std::fs::create_dir_all(below.join("raw"))?;
    std::fs::write(below.join("raw").join("a.csv"), b"a")?;
    ok(&world.fl(&below, &["add", "./raw"])?)?;
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(manifest.contains("ref = \"./scripts/raw\""), "{manifest}");
    ok(&world.here(&["sync"])?)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_drops_the_dataset_everywhere_and_keeps_its_downloads() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world.add()?;
    let ran = world.here(&["remove", "3608990"])?;
    ok(&ran)?;
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(!manifest.contains("zenodo:3608990"), "{manifest}");
    assert!(manifest.contains("zenodo:7"), "{manifest}");
    let lock = std::fs::read_to_string(world.project.join("data.lock"))?;
    assert!(!lock.contains("zenodo:3608990"), "{lock}");
    assert!(!data(&world, "3608990").exists());
    assert!(data(&world, "ab/a.csv").exists());
    let before = world.requests().await?;
    world.add()?;
    assert_eq!(
        world.requests().await? - before,
        1,
        "only the record is asked for again"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn remove_refuses_a_dataset_a_step_reads_and_an_unknown_name_gets_a_suggestion() -> TestResult
{
    let world = World::start().await?;
    two_files(&world).await?;
    let manifest = world.project.join("data.toml");
    let text = std::fs::read_to_string(&manifest)?;
    std::fs::write(
        &manifest,
        format!("{text}\n[steps.clean]\nrun = \"x {{in.ab}}\"\ninputs = [\"ab\"]\n"),
    )?;
    let ran = world.here(&["remove", "ab"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("`clean`"), "{}", ran.stderr);
    let ran = world.here(&["remove", "ac"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("did you mean `ab`?"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_keeps_unchanged_entries_without_the_network_and_check_changes_nothing() -> TestResult
{
    let world = World::start().await?;
    two_files(&world).await?;
    let before = world.requests().await?;
    let ran = world.here(&["lock"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("nothing to do"), "{}", ran.stderr);
    assert_eq!(world.requests().await?, before);
    ok(&world.here(&["lock", "--check"])?)?;
    let manifest = world.project.join("data.toml");
    let text = std::fs::read_to_string(&manifest)?;
    std::fs::write(
        &manifest,
        text.replace(
            "ref = \"zenodo:7\"",
            "ref = \"zenodo:7\"\nexclude = [\"b.csv\"]",
        ),
    )?;
    let lock_before = std::fs::read(world.project.join("data.lock"))?;
    let ran = world.here(&["lock", "--check"])?;
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(ran.stderr.contains("error[lock.stale]"), "{}", ran.stderr);
    assert_eq!(std::fs::read(world.project.join("data.lock"))?, lock_before);
    assert_eq!(world.requests().await?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrade_prints_the_per_file_diff_and_leaves_pinned_datasets() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world.server.reset().await;
    world
        .record(
            "7",
            "cc-by-4.0",
            &[("a.csv", b"a,1,changed\n", None), ("c.csv", b"c\n", None)],
        )
        .await;
    let ran = world.here(&["lock", "--upgrade"])?;
    ok(&ran)?;
    for line in [
        "+ ab/c.csv (2 B)",
        "- ab/b.csv (4 B)",
        "~ ab/a.csv (4 B to 12 B)",
    ] {
        assert!(ran.stderr.contains(line), "{line}\n{}", ran.stderr);
    }
    let ran = world.here(&["lock", "--upgrade", "nope"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn one_dataset_failing_to_resolve_does_not_stop_the_others_and_writes_no_lock() -> TestResult
{
    let world = World::start().await?;
    world
        .record("7", "cc-by-4.0", &[("a.csv", b"a", None)])
        .await;
    ok(&world.here(&["init"])?)?;
    std::fs::write(
        world.project.join("data.toml"),
        "[datasets.gone]\nref = \"zenodo:404\"\n\n[datasets.good]\nref = \"zenodo:7\"\n\n[datasets.alsogone]\nref = \"zenodo:405\"\n",
    )?;
    let ran = world.here(&["lock"])?;
    assert_eq!(ran.code, Some(10), "{}", ran.stderr);
    assert!(ran.stderr.contains("Resolved zenodo:7"), "{}", ran.stderr);
    assert_eq!(ran.stderr.matches("error[").count(), 2, "{}", ran.stderr);
    assert!(!world.project.join("data.lock").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_locks_first_unless_locked_is_given() -> TestResult {
    let world = World::start().await?;
    ok(&world.here(&["add", "zenodo:3608990", "--no-sync"])?)?;
    std::fs::remove_file(world.project.join("data.lock"))?;
    let ran = world.here(&["sync", "--locked"])?;
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(ran.stderr.contains("--locked"), "{}", ran.stderr);
    assert!(!world.iris().exists());
    ok(&world.here(&["sync"])?)?;
    assert!(world.project.join("data.lock").exists());
    assert_eq!(std::fs::read(world.iris())?, IRIS);
    ok(&world.here(&["sync", "--locked"])?)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn files_added_by_hand_are_kept_and_reported_and_clean_removes_them() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    std::fs::write(data(&world, "ab/notes.txt"), b"mine")?;
    let ran = world.here(&["sync"])?;
    ok(&ran)?;
    assert!(
        ran.stderr
            .contains("Kept 1 file you added: data/ab/notes.txt"),
        "{}",
        ran.stderr
    );
    assert!(data(&world, "ab/notes.txt").exists());
    let ran = world.here(&["status"])?;
    assert!(
        ran.stderr.contains("Extra data/ab/notes.txt"),
        "{}",
        ran.stderr
    );
    let ran = world.here(&["sync", "--clean"])?;
    ok(&ran)?;
    assert!(
        ran.stderr.contains("Removed data/ab/notes.txt"),
        "{}",
        ran.stderr
    );
    assert!(!data(&world, "ab/notes.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_modified_file_stops_sync_before_anything_and_force_overwrites_it() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let a = data(&world, "ab/a.csv");
    edit(&a, b"edited")?;
    fl_store::data_dir::remove(&data(&world, "ab/b.csv"))?;
    let ran = world.here(&["sync"])?;
    assert_eq!(ran.code, Some(50), "{}", ran.stderr);
    assert!(ran.stderr.contains("data/ab/a.csv"), "{}", ran.stderr);
    assert!(ran.stderr.contains("fl restore"), "{}", ran.stderr);
    assert!(ran.stderr.contains("--force"), "{}", ran.stderr);
    assert_eq!(std::fs::read(&a)?, b"edited");
    assert!(!data(&world, "ab/b.csv").exists(), "nothing was touched");
    ok(&world.here(&["sync", "--force"])?)?;
    assert_eq!(std::fs::read(&a)?, b"a,1\n");
    assert!(data(&world, "ab/b.csv").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dataset_removed_from_the_manifest_leaves_data_at_the_next_sync() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    ok(&world.here(&["add", "zenodo:3608990"])?)?;
    std::fs::write(
        world.project.join("data.toml"),
        "[project]\nname = \"project\"\n\n[datasets.3608990]\nref = \"zenodo:3608990\"\n",
    )?;
    let ran = world.here(&["sync"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("Removed data/ab"), "{}", ran.stderr);
    assert!(!data(&world, "ab").exists());
    assert!(world.iris().exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn upstream_serving_other_bytes_is_drift_with_the_upgrade_line() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world.server.reset().await;
    world
        .record(
            "7",
            "cc-by-4.0",
            &[("a.csv", b"other\n", None), ("b.csv", b"b,2\n", None)],
        )
        .await;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh");
    let ran = world.fl_with_store(&world.project, &fresh, &["sync"])?;
    assert_eq!(ran.code, Some(30), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("error[integrity.drift]"),
        "{}",
        ran.stderr
    );
    assert!(
        ran.stderr.contains("fl lock --upgrade ab"),
        "{}",
        ran.stderr
    );
    assert!(
        data(&world, "ab/b.csv").exists(),
        "the file that arrived stays"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_fails_does_not_stop_the_rest_and_the_worst_code_wins() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world.server.reset().await;
    world.serve("/files/7/b.csv", b"b,2\n").await;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh");
    let ran = world.fl_with_store(&world.project, &fresh, &["sync", "--json"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stdout);
    let document = json(&ran)?;
    assert_eq!(document["status"], "partial");
    assert_eq!(document["result"]["linked"], 1);
    assert_eq!(document["errors"][0]["kind"], "network.status");
    assert!(data(&world, "ab/b.csv").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_location_rescues_a_file_whose_first_one_is_gone() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let lock_path = world.project.join("data.lock");
    let lock = std::fs::read_to_string(&lock_path)?;
    let first = format!("{}/files/7/a.csv", world.server.uri());
    let gone = format!("{}/gone/a.csv", world.server.uri());
    std::fs::write(
        &lock_path,
        lock.replace(
            &format!("at = [\"{first}\"]"),
            &format!("at = [\"{gone}\", \"{first}\"]"),
        ),
    )?;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh");
    let ran = world.fl_with_store(&world.project, &fresh, &["sync"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("so it came from"), "{}", ran.stderr);
    assert_eq!(std::fs::read(data(&world, "ab/a.csv"))?, b"a,1\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn offline_makes_no_request_and_lists_what_no_store_holds() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let before = world.requests().await?;
    let fresh = world.temp.path().join("fresh");
    let ran = world.fl_with_store(&world.project, &fresh, &["sync", "--offline"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("error[offline.missing]"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("data/ab/a.csv"), "{}", ran.stderr);
    assert_eq!(world.requests().await?, before);
    let ran = world.here(&["sync", "--offline"])?;
    ok(&ran)?;
    assert!(data(&world, "ab/a.csv").exists());
    assert_eq!(world.requests().await?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn status_check_fails_on_any_difference_and_names_the_next_command() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    ok(&world.here(&["status", "--check"])?)?;
    fl_store::data_dir::remove(&data(&world, "ab/b.csv"))?;
    let ran = world.here(&["status"])?;
    ok(&ran)?;
    assert!(
        ran.stderr.contains("Missing data/ab/b.csv"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("fl sync"), "{}", ran.stderr);
    let ran = world.here(&["status", "--check"])?;
    assert_eq!(ran.code, Some(1), "{}", ran.stderr);
    assert!(ran.stderr.contains("error[status.dirty]"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_puts_files_back_from_the_store_and_never_downloads() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let a = data(&world, "ab/a.csv");
    let b = data(&world, "ab/b.csv");
    edit(&a, b"edited")?;
    fl_store::data_dir::remove(&b)?;
    let before = world.requests().await?;
    let ran = world.here(&["restore"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("--yes"), "{}", ran.stderr);
    assert_eq!(std::fs::read(&a)?, b"edited");
    let ran = world.here(&["restore", "--yes"])?;
    ok(&ran)?;
    assert_eq!(std::fs::read(&a)?, b"a,1\n");
    assert_eq!(std::fs::read(&b)?, b"b,2\n");
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh");
    let ran = world.fl_with_store(&world.project, &fresh, &["restore", "data/ab/a.csv"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stderr);
    assert!(ran.stderr.contains("fl sync ab"), "{}", ran.stderr);
    assert_eq!(world.requests().await?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lock_held_by_a_process_that_died_does_not_block_the_next_one() -> TestResult {
    let world = World::start().await?;
    Mock::given(method("GET"))
        .and(path("/api/records/77"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
        .mount(&world.server)
        .await;
    ok(&world.here(&["init"])?)?;
    let mut holder = Command::new(env!("CARGO_BIN_EXE_fl"))
        .args(["add", "zenodo:77"])
        .current_dir(&world.project)
        .env("FETCHLOOM_STORE", &world.store)
        .env("FETCHLOOM_CONFIG", world.config.join("config.toml"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let lock = world.project.join(".fetchloom").join("lock");
    let started = Instant::now();
    while !held(&lock) {
        if started.elapsed() > Duration::from_secs(20) {
            holder.kill()?;
            return Err("the first fl never took the project lock".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    holder.kill()?;
    holder.wait()?;
    let started = Instant::now();
    ok(&world.here(&["sync"])?)?;
    assert!(started.elapsed() < Duration::from_secs(20));
    Ok(())
}

fn held(lock: &Path) -> bool {
    std::fs::OpenOptions::new()
        .write(true)
        .open(lock)
        .is_ok_and(|file| matches!(file.try_lock(), Err(std::fs::TryLockError::WouldBlock)))
}
