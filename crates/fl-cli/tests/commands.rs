//! The small rules of each command, one test each: what is printed, counted and left alone.

use std::path::Path;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::project::{data, edit, ok, two_files};
use crate::support::{Failure, IRIS, World, found, json};

type TestResult = Result<(), Failure>;

fn counted(document: &Value, key: &str) -> Result<u64, Failure> {
    document["work"][key]
        .as_u64()
        .ok_or_else(|| format!("work.{key} is not a number in {document}").into())
}

fn set_modified(file: &Path, when: SystemTime) -> TestResult {
    std::fs::OpenOptions::new()
        .write(true)
        .open(file)?
        .set_modified(when)?;
    Ok(())
}

async fn solo(world: &World, id: &str, name: &str, bytes: &[u8]) -> TestResult {
    world
        .record(id, "cc-by-4.0", &[(&format!("{name}.csv"), bytes, None)])
        .await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_dataset_gets_the_add_hint_when_there_are_none_and_the_list_when_there_are()
-> TestResult {
    let world = World::start().await?;
    ok(&world.here(&["init"])?)?;
    let ran = world.here(&["status", "zzzzzzzz"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("add one with `fl add REF`"),
        "{}",
        ran.stderr
    );
    two_files(&world).await?;
    let ran = world.here(&["status", "zzzzzzzz"])?;
    assert_eq!(ran.code, Some(2), "{}", ran.stderr);
    assert!(ran.stderr.contains("datasets here: ab"), "{}", ran.stderr);
    assert!(!ran.stderr.contains("add one with"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_state_is_rewritten_only_when_a_file_had_to_be_rehashed() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let state = world.project.join(".fetchloom").join("state");
    let placed = std::fs::metadata(world.iris())?.modified()?;
    set_modified(&state, placed - Duration::from_secs(3600))?;
    let ran = world.here(&["sync", "--json"])?;
    ok(&ran)?;
    assert_eq!(counted(&json(&ran)?, "files_rehashed")?, 1);
    let ran = world.here(&["sync", "--json"])?;
    ok(&ran)?;
    assert_eq!(counted(&json(&ran)?, "files_rehashed")?, 0);
    set_modified(&state, SystemTime::now() + Duration::from_secs(3600))?;
    let stamp = std::fs::metadata(&state)?.modified()?;
    ok(&world.here(&["sync"])?)?;
    assert_eq!(std::fs::metadata(&state)?.modified()?, stamp);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_what_the_store_already_holds_downloads_nothing() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let other = world.temp.path().join("other");
    std::fs::create_dir_all(&other)?;
    let ran = world.fl(&other, &["add", "zenodo:3608990"])?;
    ok(&ran)?;
    assert!(!ran.stderr.contains("Downloaded"), "{}", ran.stderr);
    assert_eq!(std::fs::read(other.join("data/3608990/iris.csv"))?, IRIS);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_the_same_dataset_again_prints_no_added_line_and_no_hint() -> TestResult {
    let world = World::start().await?;
    world
        .record("7", "cc-by-4.0", &[("a.csv", b"a,1\n", None)])
        .await;
    let first = world.here(&["add", "zenodo:7", "--as", "ab"])?;
    ok(&first)?;
    assert!(
        first.stderr.contains("Added ab = zenodo:7 to data.toml"),
        "{}",
        first.stderr
    );
    assert!(first.stderr.contains("`fl path ab`"), "{}", first.stderr);
    let again = world.here(&["add", "zenodo:7", "--as", "ab"])?;
    ok(&again)?;
    assert!(!again.stderr.contains("to data.toml"), "{}", again.stderr);
    assert!(!again.stderr.contains("fl path"), "{}", again.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_without_a_sync_says_what_was_added_but_gives_no_path_hint() -> TestResult {
    let world = World::start().await?;
    solo(&world, "8", "c", b"c\n").await?;
    let ran = world.here(&["add", "zenodo:8", "--as", "cd", "--no-sync"])?;
    ok(&ran)?;
    assert!(
        ran.stderr.contains("Added cd = zenodo:8 to data.toml"),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains("fl path"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn adding_and_asking_for_a_path_sync_only_the_dataset_named() -> TestResult {
    let world = World::start().await?;
    solo(&world, "7", "a", b"a\n").await?;
    solo(&world, "8", "c", b"c\n").await?;
    solo(&world, "9", "d", b"d\n").await?;
    ok(&world.here(&["add", "zenodo:7", "--as", "ab", "--no-sync"])?)?;
    ok(&world.here(&["add", "zenodo:8", "--as", "cd"])?)?;
    assert!(data(&world, "cd/c.csv").exists());
    assert!(!data(&world, "ab").exists());
    ok(&world.here(&["add", "zenodo:9", "--as", "ef", "--no-sync"])?)?;
    ok(&world.here(&["path", "cd"])?)?;
    assert!(!data(&world, "ab").exists());
    assert!(!data(&world, "ef").exists());
    ok(&world.here(&["path", "ef"])?)?;
    assert!(data(&world, "ef/d.csv").exists());
    assert!(!data(&world, "ab").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_single_hit_is_taken_even_when_it_is_not_an_exact_match() -> TestResult {
    let world = World::start().await?;
    found(&world, "flowers", &[(3_608_990, "Iris data")]).await;
    let ran = world.here(&["add", "flowers"])?;
    ok(&ran)?;
    assert!(
        ran.stderr.contains("Found \"Iris data\" as zenodo:3608990"),
        "{}",
        ran.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn get_names_its_directory_again_when_nothing_changed_and_refuses_other_bytes() -> TestResult
{
    let world = World::start().await?;
    let uri = format!("{}/files/iris.csv", world.server.uri());
    let first = world.here(&["get", &uri, "-o", "out"])?;
    ok(&first)?;
    let again = world.here(&["get", &uri, "-o", "out"])?;
    ok(&again)?;
    assert_eq!(again.stdout, first.stdout);
    assert_eq!(again.stdout.lines().count(), 1, "{:?}", again.stdout);

    let file = world.project.join("out").join("iris.csv");
    edit(&file, b"mine")?;
    let refused = world.here(&["get", &uri, "-o", "out"])?;
    assert_eq!(refused.code, Some(50), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("already holds other bytes"),
        "{}",
        refused.stderr
    );
    assert_eq!(std::fs::read(&file)?, b"mine");

    let forced = world.here(&["get", &uri, "-o", "out", "--force"])?;
    ok(&forced)?;
    assert_eq!(forced.stdout, first.stdout);
    assert_eq!(std::fs::read(&file)?, IRIS);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn status_looks_only_at_the_datasets_named() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    solo(&world, "8", "c", b"c\n").await?;
    ok(&world.here(&["add", "zenodo:8", "--as", "cd"])?)?;
    fl_store::data_dir::remove(&data(&world, "cd/c.csv"))?;
    let all = world.here(&["status", "--json"])?;
    ok(&all)?;
    assert_eq!(
        json(&all)?["result"]["missing"].as_array().map(Vec::len),
        Some(1)
    );
    let ab = world.here(&["status", "ab", "--json"])?;
    ok(&ab)?;
    assert_eq!(
        json(&ab)?["result"]["missing"].as_array().map(Vec::len),
        Some(0)
    );
    let cd = world.here(&["status", "cd", "--json"])?;
    ok(&cd)?;
    assert_eq!(
        json(&cd)?["result"]["missing"].as_array().map(Vec::len),
        Some(1)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn status_says_in_words_what_it_found() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let clean = world.here(&["status"])?;
    ok(&clean)?;
    assert!(
        clean.stderr.contains("Everything matches data.lock."),
        "{}",
        clean.stderr
    );
    std::fs::write(data(&world, "ab/extra.txt"), b"mine")?;
    let extra = world.here(&["status"])?;
    ok(&extra)?;
    assert!(
        extra.stderr.contains("1 file you added is kept as it is."),
        "{}",
        extra.stderr
    );
    edit(&data(&world, "ab/a.csv"), b"edited")?;
    let changed = world.here(&["status"])?;
    ok(&changed)?;
    assert!(
        changed
            .stderr
            .contains("1 file differs from data.lock; `fl restore` puts the downloaded one back."),
        "{}",
        changed.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_takes_a_file_a_folder_or_the_whole_data_directory() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let a = data(&world, "ab/a.csv");
    let b = data(&world, "ab/b.csv");
    let spoil = || -> TestResult {
        edit(&a, b"edited")?;
        fl_store::data_dir::remove(&b)?;
        Ok(())
    };

    spoil()?;
    ok(&world.here(&["restore", "--yes", "data/ab/b.csv"])?)?;
    assert_eq!(std::fs::read(&b)?, b"b,2\n");
    assert_eq!(std::fs::read(&a)?, b"edited");

    spoil()?;
    ok(&world.here(&["restore", "--yes", "data/ab"])?)?;
    assert_eq!(std::fs::read(&a)?, b"a,1\n");
    assert_eq!(std::fs::read(&b)?, b"b,2\n");

    spoil()?;
    ok(&world.here(&["restore", "--yes", "data"])?)?;
    assert_eq!(std::fs::read(&a)?, b"a,1\n");
    assert_eq!(std::fs::read(&b)?, b"b,2\n");

    spoil()?;
    let ran = world.here(&["restore", "--yes", "data/ab/a"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("Nothing to restore"), "{}", ran.stderr);
    assert_eq!(std::fs::read(&a)?, b"edited");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_with_nothing_to_do_says_so() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let ran = world.here(&["restore"])?;
    ok(&ran)?;
    assert!(
        ran.stderr
            .contains("Nothing to restore, every file matches data.lock."),
        "{}",
        ran.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn restore_that_can_only_put_back_some_files_is_partial_and_exits_20() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let fresh = world.temp.path().join("fresh");
    let scratch = world.temp.path().join("scratch");
    std::fs::create_dir_all(&scratch)?;
    let uri = format!("{}/files/7/b.csv", world.server.uri());
    ok(&world.fl_with_store(&scratch, &fresh, &["get", &uri, "-o", "got"])?)?;
    std::fs::remove_dir_all(world.project.join("data"))?;

    let ran = world.fl_with_store(&world.project, &fresh, &["restore", "--json"])?;
    assert_eq!(ran.code, Some(20), "{}{}", ran.stdout, ran.stderr);
    let document = json(&ran)?;
    assert_eq!(document["status"], "partial");
    assert_eq!(document["result"]["restored"], 1);
    assert_eq!(
        document["result"]["unavailable"],
        serde_json::json!(["data/ab/a.csv"])
    );
    assert!(data(&world, "ab/b.csv").exists());
    assert!(!data(&world, "ab/a.csv").exists());

    std::fs::remove_dir_all(world.project.join("data"))?;
    let empty = world.temp.path().join("empty");
    let ran = world.fl_with_store(&world.project, &empty, &["restore", "--json"])?;
    assert_eq!(ran.code, Some(20), "{}{}", ran.stdout, ran.stderr);
    assert_ne!(json(&ran)?["status"], "partial");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrading_lists_only_the_files_whose_bytes_changed() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world.server.reset().await;
    world
        .record(
            "7",
            "cc-by-4.0",
            &[("a.csv", b"a,1\n", None), ("b.csv", b"b,22\n", None)],
        )
        .await;
    let ran = world.here(&["lock", "--upgrade", "--json"])?;
    ok(&ran)?;
    let change = &json(&ran)?["result"]["changes"][0];
    assert_eq!(change["dataset"], "ab");
    assert_eq!(change["added"], serde_json::json!([]));
    assert_eq!(change["removed"], serde_json::json!([]));
    assert_eq!(
        change["changed"],
        serde_json::json!([{ "path": "b.csv", "size_before": 4, "size": 5 }])
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn upgrading_to_the_same_files_says_unchanged_and_lists_no_file() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    world.server.reset().await;
    world
        .record(
            "7",
            "cc-by-4.0",
            &[("a.csv", b"a,1\n", None), ("b.csv", b"b,2\n", None)],
        )
        .await;
    let ran = world.here(&["lock", "--upgrade"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("Unchanged ab"), "{}", ran.stderr);
    assert!(!ran.stderr.contains("+ ab/"), "{}", ran.stderr);
    assert!(!ran.stderr.contains("~ ab/"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn lock_counts_the_files_it_holds() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let ran = world.here(&["lock", "--json"])?;
    ok(&ran)?;
    let result = &json(&ran)?["result"];
    assert_eq!(result["files"], 2);
    assert_eq!(result["datasets"], 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_a_dataset_removes_its_files_without_saying_anything_was_locked() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    solo(&world, "8", "c", b"c\n").await?;
    ok(&world.here(&["add", "zenodo:8", "--as", "cd"])?)?;
    std::fs::write(
        world.project.join("data.toml"),
        "[project]\nname = \"project\"\n\n[datasets.ab]\nref = \"zenodo:7\"\n",
    )?;
    let ran = world.here(&["sync", "--json"])?;
    ok(&ran)?;
    assert_eq!(json(&ran)?["result"]["removed"], 1);
    let again = world.here(&["sync"])?;
    ok(&again)?;
    std::fs::write(
        world.project.join("data.toml"),
        "[project]\nname = \"project\"\n\n[datasets.ab]\nref = \"zenodo:7\"\n\n[datasets.cd]\nref = \"zenodo:8\"\n",
    )?;
    ok(&world.here(&["sync"])?)?;
    std::fs::write(
        world.project.join("data.toml"),
        "[project]\nname = \"project\"\n\n[datasets.ab]\nref = \"zenodo:7\"\n",
    )?;
    let ran = world.here(&["sync"])?;
    ok(&ran)?;
    assert!(
        ran.stderr
            .contains("Removed 1 file that data.lock no longer lists."),
        "{}",
        ran.stderr
    );
    assert!(!ran.stderr.contains("Locked"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_the_last_dataset_keeps_the_data_directory() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let ran = world.here(&["remove", "ab", "--json"])?;
    ok(&ran)?;
    assert_eq!(json(&ran)?["result"]["files_removed"], 2);
    assert!(world.project.join("data").is_dir());
    assert!(!data(&world, "ab").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_where_no_file_could_be_fetched_says_nothing_was_placed() -> TestResult {
    let world = World::start().await?;
    solo(&world, "9", "a", b"a\n").await?;
    ok(&world.here(&["add", "zenodo:9", "--as", "solo"])?)?;
    let lock_path = world.project.join("data.lock");
    let lock = std::fs::read_to_string(&lock_path)?;
    let uri = world.server.uri();
    std::fs::write(
        &lock_path,
        lock.replace(
            &format!("{uri}/files/9/a.csv"),
            &format!("{uri}/gone/a.csv"),
        ),
    )?;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh");
    let ran = world.fl_with_store(&world.project, &fresh, &["sync"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stderr);
    assert!(
        ran.stderr
            .contains("Nothing was placed: 1 problem stopped it."),
        "{}",
        ran.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn forcing_a_sync_places_nothing_that_is_already_right() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let ran = world.here(&["sync", "--force", "--json"])?;
    ok(&ran)?;
    let document = json(&ran)?;
    assert_eq!(document["result"]["linked"], 0);
    assert_eq!(counted(&document, "files_linked")?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_counts_the_files_it_removes() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    let narrowed = world.here(&[
        "add", "zenodo:7", "--as", "ab", "--select", "a.csv", "--json",
    ])?;
    ok(&narrowed)?;
    assert_eq!(json(&narrowed)?["result"]["synced"]["removed"], 1);

    std::fs::write(data(&world, "ab/mine.txt"), b"mine")?;
    let kept = world.here(&["sync", "--json"])?;
    ok(&kept)?;
    assert_eq!(json(&kept)?["result"]["removed"], 0);
    let cleaned = world.here(&["sync", "--clean", "--json"])?;
    ok(&cleaned)?;
    assert_eq!(json(&cleaned)?["result"]["removed"], 1);
    assert!(!data(&world, "ab/mine.txt").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn add_reports_the_bytes_a_sync_fetched_after_locking() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    std::fs::remove_dir_all(world.project.join("data"))?;
    std::fs::remove_dir_all(&world.store)?;
    let ran = world.here(&["add", "zenodo:7", "--as", "ab", "--json"])?;
    ok(&ran)?;
    assert_eq!(json(&ran)?["result"]["synced"]["bytes_fetched"], 8);
    assert!(data(&world, "ab/a.csv").exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_copied_dataset_reports_the_extra_disk_it_took() -> TestResult {
    let world = World::start().await?;
    solo(&world, "7", "a", b"a,1\n").await?;
    let ran = world.here(&["add", "zenodo:7", "--as", "ab", "--link", "copy"])?;
    ok(&ran)?;
    assert!(ran.stderr.contains("4 B extra disk"), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_add_keeps_the_state_and_lock_a_folder_already_had() -> TestResult {
    let world = World::start().await?;
    let state = world.project.join(".fetchloom");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("keep"), b"x")?;
    let ran = world.here(&["add", "zenodo:404", "--as", "gone"])?;
    assert_eq!(ran.code, Some(10), "{}", ran.stderr);
    assert!(state.join("keep").exists());

    std::fs::remove_dir_all(&state)?;
    let lock = world.project.join("data.lock");
    std::fs::write(&lock, b"keep")?;
    let ran = world.here(&["add", "zenodo:404", "--as", "gone"])?;
    assert!(ran.code != Some(0), "{}", ran.stderr);
    assert!(lock.exists());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_placed_file_changed_by_hand_is_kept_when_the_lock_stops_listing_it() -> TestResult {
    let world = World::start().await?;
    two_files(&world).await?;
    edit(&data(&world, "ab/b.csv"), b"mine")?;
    let ran = world.here(&["add", "zenodo:7", "--as", "ab", "--select", "a.csv"])?;
    ok(&ran)?;
    assert_eq!(std::fs::read(data(&world, "ab/b.csv"))?, b"mine");
    assert!(
        !ran.stderr.contains("Removed data/ab/b.csv"),
        "{}",
        ran.stderr
    );
    Ok(())
}
