//! The words and the document `fl` prints, kept as snapshots with the machine left out.

use serde_json::Value;

use crate::support::{Failure, World, found, json};

type TestResult = Result<(), Failure>;

fn redact(world: &World, text: &str) -> String {
    world.scrub(text)
}

fn pretty(world: &World, document: &Value) -> Result<String, Failure> {
    let mut document = document.clone();
    if let Some(phases) = document["work"]["phase_ms"].as_object_mut() {
        for value in phases.values_mut() {
            *value = Value::from(0);
        }
    }
    if let Some(links) = document["work"].get_mut("links") {
        *links = Value::from("[BY PLATFORM]");
    }
    Ok(redact(world, &serde_json::to_string_pretty(&document)?))
}

macro_rules! snapshot {
    ($name:expr, $text:expr) => {
        insta::with_settings!({
            filters => vec![
                (r"127\.0\.0\.1:\d+", "[SERVER]"),
                (r"\d+(\.\d+)?(ms|s)\b", "[TIME]"),
                (r"by (reflink|hardlink|copy|symlink)\b", "by [MODE]"),
            ],
        }, {
            insta::assert_snapshot!($name, $text);
        });
    };
}

#[tokio::test(flavor = "multi_thread")]
async fn the_story_of_a_first_add() -> TestResult {
    let world = World::start().await?;
    let ran = world.add()?;
    snapshot!("add_stderr", redact(&world, &ran.stderr));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_story_of_a_second_verbose_sync() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let ran = world.here(&["sync", "-v"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    snapshot!("sync_verbose_stderr", redact(&world, &ran.stderr));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_document_of_a_second_sync() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    world.here(&["sync", "-v"])?;
    let ran = world.here(&["sync", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let text = pretty(&world, &json(&ran)?)?;
    snapshot!("sync_json_stdout", text);
    Ok(())
}

/// A sync that has work to do: the project's lock and an empty store, so every file is
/// downloaded and placed.
async fn fresh_sync(world: &World, args: &[&str]) -> Result<crate::support::Ran, Failure> {
    world
        .record(
            "7",
            "cc-by-4.0",
            &[("a.csv", b"a,1\n", None), ("b.csv", b"b,2\n", None)],
        )
        .await;
    let added = world.here(&["add", "zenodo:7", "--as", "ab"])?;
    if added.code != Some(0) {
        return Err(added.stderr.into());
    }
    std::fs::remove_dir_all(world.project.join("data"))?;
    std::fs::remove_dir_all(world.project.join(".fetchloom"))?;
    let fresh = world.temp.path().join("fresh");
    let mut every = vec!["sync"];
    every.extend_from_slice(args);
    world.fl_with_store(&world.project, &fresh, &every)
}

/// Debug log lines carry times and differ by platform, such as a reflink refused on one
/// file system and taken on another, so they are left out.
fn logs_dropped(text: &str) -> String {
    text.lines()
        .filter(|line| {
            !(line.len() > 20 && line.as_bytes()[4] == b'-' && line.as_bytes()[10] == b'T')
        })
        .collect::<Vec<_>>()
        .join(
            "
",
        )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_with_work_at_every_loudness() -> TestResult {
    for (name, args) in [
        ("sync_quiet", vec!["-q"]),
        ("sync_default", vec![]),
        ("sync_verbose", vec!["-v"]),
        ("sync_debug", vec!["-vv"]),
    ] {
        let world = World::start().await?;
        let ran = fresh_sync(&world, &args).await?;
        assert_eq!(ran.code, Some(0), "{}", ran.stderr);
        assert!(ran.stdout.is_empty(), "{name}: {}", ran.stdout);
        snapshot!(name, logs_dropped(&redact(&world, &ran.stderr)));
    }
    let world = World::start().await?;
    let ran = fresh_sync(&world, &["--json"]).await?;
    assert!(ran.stderr.is_empty(), "{}", ran.stderr);
    let document = pretty(&world, &json(&ran)?)?;
    snapshot!("sync_json", document);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn quiet_never_prints_more_than_the_default() -> TestResult {
    for command in [
        vec!["add", "zenodo:3608990"],
        vec!["sync"],
        vec!["status"],
        vec!["lock"],
        vec!["path", "3608990"],
        vec!["add", "zenodo:404"],
    ] {
        let world = World::start().await?;
        if command[0] != "add" {
            world.add()?;
        }
        let default = world.here(&command)?;
        let other = World::start().await?;
        if command[0] != "add" {
            other.add()?;
        }
        let mut quiet = command.clone();
        quiet.push("-q");
        let quiet = other.here(&quiet)?;
        assert!(
            quiet.stderr.len() <= default.stderr.len()
                && quiet.stdout.len() <= default.stdout.len(),
            "{command:?}\n-q: {}{}\ndefault: {}{}",
            quiet.stdout,
            quiet.stderr,
            default.stdout,
            default.stderr
        );
        assert!(quiet.stderr.is_empty(), "{command:?}: {}", quiet.stderr);
        let mut silent = command.clone();
        silent.push("-qq");
        let silent = other.here(&silent)?;
        assert!(
            silent.stdout.is_empty() && silent.stderr.is_empty(),
            "{command:?}"
        );
    }
    Ok(())
}

/// The first run of someone new to fetchloom: a fresh store and no project, then each thing
/// they are likely to try. Every screen is kept in default loudness.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_run_walk() -> TestResult {
    let world = World::start().await?;
    found(&world, "iris", &[(1, "Iris flowers"), (2, "Iris images")]).await;
    world
        .record("10", "cc-by-4.0", &[("eeg.bin", b"x", Some(2 << 30))])
        .await;
    let mut screens = Vec::new();
    let mut step = |title: &str, ran: &crate::support::Ran| {
        screens.push(format!(
            "$ {title}\n{}{}[exit {}]\n",
            ran.stdout,
            ran.stderr,
            ran.code.unwrap_or(-1)
        ));
    };
    step("fl", &world.here(&[])?);
    step("fl snyc", &world.here(&["snyc"])?);
    step("fl add", &world.here(&["add"])?);
    step("fl add iris", &world.here(&["add", "iris"])?);
    step("fl add zenodo:10", &world.here(&["add", "zenodo:10"])?);
    step(
        "fl add zenodo:3608990",
        &world.here(&["add", "zenodo:3608990"])?,
    );
    step("fl", &world.here(&[])?);
    let iris = world.iris();
    fl_store::data_dir::remove(&iris)?;
    std::fs::write(&iris, b"edited by hand\n")?;
    step("fl sync", &world.here(&["sync"])?);
    step("fl restore --yes", &world.here(&["restore", "--yes"])?);
    step("fl add zenodo:404", &world.here(&["add", "zenodo:404"])?);
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh");
    step(
        "fl sync --offline",
        &world.fl_with_store(&world.project, &fresh, &["sync", "--offline"])?,
    );
    step("fl sync", &world.here(&["sync"])?);
    snapshot!("first_run_walk", redact(&world, &screens.join("\n")));
    Ok(())
}
