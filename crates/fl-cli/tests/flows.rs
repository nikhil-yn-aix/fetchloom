//! `fl` against a fake zenodo: what each command does to the project, the store and the server.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::Value;

use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use crate::support::{Failure, IRIS, IRIS_MD5, RECORD, World, json, make_writable};

type TestResult = Result<(), Failure>;

fn work(document: &Value, key: &str) -> Result<u64, Failure> {
    document["work"][key]
        .as_u64()
        .ok_or_else(|| format!("work.{key} is not a number in {document}").into())
}

#[tokio::test(flavor = "multi_thread")]
async fn add_downloads_the_file_once_locks_its_md5_and_links_it() -> TestResult {
    let world = World::start().await?;
    let ran = world.add()?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert!(ran.stdout.is_empty(), "{}", ran.stdout);
    assert_eq!(std::fs::read(world.iris())?, IRIS);
    let lock = std::fs::read_to_string(world.project.join("data.lock"))?;
    assert!(lock.contains(&format!("md5 = \"{IRIS_MD5}\"")), "{lock}");
    assert!(lock.contains("blake3 = \""), "{lock}");
    assert_eq!(world.requests().await?, 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_sync_makes_no_request_and_links_nothing() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let before = world.requests().await?;
    let ran = world.here(&["sync", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let document = json(&ran)?;
    assert_eq!(document["status"], "ok");
    for key in ["requests", "bytes_down", "files_linked", "objects_ingested"] {
        assert_eq!(work(&document, key)?, 0, "{key}");
    }
    assert_eq!(world.requests().await?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_project_with_the_same_lock_transfers_nothing_and_links_every_file() -> TestResult
{
    let world = World::start().await?;
    world.add()?;
    let other = world.temp.path().join("other");
    std::fs::create_dir_all(&other)?;
    for name in ["data.toml", "data.lock"] {
        std::fs::copy(world.project.join(name), other.join(name))?;
    }
    let before = world.requests().await?;
    let ran = world.fl(&other, &["sync", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let document = json(&ran)?;
    assert_eq!(work(&document, "requests")?, 0);
    assert_eq!(work(&document, "bytes_down")?, 0);
    assert_eq!(work(&document, "files_linked")?, 1);
    assert_eq!(work(&document, "objects_ingested")?, 0);
    assert_eq!(document["work"]["tier_hits"]["local"], 1);
    assert_eq!(
        std::fs::read(other.join("data").join("3608990").join("iris.csv"))?,
        IRIS
    );
    assert_eq!(world.requests().await?, before);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn get_prints_exactly_one_line_the_directory_holding_the_file() -> TestResult {
    let world = World::start().await?;
    let uri = format!("{}/files/iris.csv", world.server.uri());
    let ran = world.here(&["get", &uri])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert!(ran.stdout.ends_with('\n'), "{:?}", ran.stdout);
    assert_eq!(ran.stdout.lines().count(), 1, "{:?}", ran.stdout);
    let dir = Path::new(ran.stdout.trim_end());
    assert!(dir.is_absolute(), "{}", dir.display());
    assert_eq!(std::fs::read(dir.join("iris.csv"))?, IRIS);

    let ran = world.here(&["get", &uri, "-o", "out"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let dir = Path::new(ran.stdout.trim_end());
    assert_eq!(
        dir.canonicalize()?,
        world.project.join("out").canonicalize()?
    );
    assert_eq!(
        std::fs::read(world.project.join("out").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn path_prints_the_datasets_absolute_path_syncing_first() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let ran = world.here(&["path", "3608990"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert!(ran.stdout.ends_with('\n'), "{:?}", ran.stdout);
    assert_eq!(ran.stdout.lines().count(), 1, "{:?}", ran.stdout);
    assert_eq!(
        Path::new(ran.stdout.trim_end()).canonicalize()?,
        world.project.join("data").join("3608990").canonicalize()?
    );
    assert_eq!(std::fs::read(world.iris())?, IRIS);

    let ran = world.here(&["path", "nope"])?;
    assert_eq!(ran.code, Some(2));
    assert!(ran.stderr.contains("error[usage]"), "{}", ran.stderr);

    std::fs::remove_dir_all(world.project.join("data"))?;
    let ran = world.here(&["path", "3608990", "--no-sync"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stderr);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn status_reports_a_missing_file_and_a_modified_one_and_sync_refuses_to_overwrite()
-> TestResult {
    let world = World::start().await?;
    world.add()?;
    let file = world.iris();
    make_writable(&file)?;
    std::fs::remove_file(&file)?;
    let ran = world.here(&["status", "--json"])?;
    let document = json(&ran)?;
    let missing = document["result"]["missing"]
        .as_array()
        .ok_or("result.missing is not a list")?;
    assert_eq!(missing.len(), 1, "{document}");
    let entry = missing[0].as_str().ok_or("a missing entry is not text")?;
    assert!(entry.ends_with("iris.csv"), "{entry}");

    std::fs::write(&file, b"changed by hand\n")?;
    let ran = world.here(&["status", "--json"])?;
    let document = json(&ran)?;
    let modified = document["result"]["modified"]
        .as_array()
        .ok_or("result.modified is not a list")?;
    assert_eq!(modified.len(), 1, "{document}");

    let ran = world.here(&["sync"])?;
    assert_eq!(ran.code, Some(50), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("error[local.modified]"),
        "{}",
        ran.stderr
    );
    assert_eq!(std::fs::read(&file)?, b"changed by hand\n");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_record_is_reference_unresolved() -> TestResult {
    let world = World::start().await?;
    let ran = world.here(&["add", "zenodo:404"])?;
    assert_eq!(ran.code, Some(10), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("\nerror[reference.unresolved]"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("help:"), "{}", ran.stderr);
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(!manifest.contains("zenodo:404"), "{manifest}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_record_in_json_mode_leaves_stderr_empty() -> TestResult {
    let world = World::start().await?;
    let ran = world.here(&["add", "zenodo:404", "--json"])?;
    assert_eq!(ran.code, Some(10));
    assert!(ran.stderr.is_empty(), "{}", ran.stderr);
    let document = json(&ran)?;
    assert_eq!(document["errors"][0]["kind"], "reference.unresolved");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn no_project_is_project_missing() -> TestResult {
    let world = World::start().await?;
    let ran = world.here(&["status"])?;
    assert_eq!(ran.code, Some(50), "{}", ran.stderr);
    assert!(
        ran.stderr.contains("error[project.missing]"),
        "{}",
        ran.stderr
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn the_trace_file_holds_resolve_transfer_and_link_spans_on_more_than_one_thread() -> TestResult
{
    let world = World::start().await?;
    let trace = world.temp.path().join("trace.json");
    let log = world.temp.path().join("log.json");
    let trace_arg = trace.display().to_string();
    let log_arg = log.display().to_string();
    let ran = world.here(&[
        "add",
        "zenodo:3608990",
        "--trace-file",
        &trace_arg,
        "--log-file",
        &log_arg,
    ])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);

    let events: Value = serde_json::from_str(&std::fs::read_to_string(&trace)?)?;
    let begins: Vec<&Value> = events
        .as_array()
        .ok_or("the trace is not a JSON array")?
        .iter()
        .filter(|event| event["ph"] == "B")
        .collect();
    let names: BTreeSet<&str> = begins
        .iter()
        .filter_map(|event| event["name"].as_str())
        .collect();
    for wanted in ["resolve", "transfer", "link", "download"] {
        assert!(names.contains(wanted), "no {wanted} span in {names:?}");
    }
    let threads: BTreeSet<String> = begins
        .iter()
        .map(|event| event["tid"].to_string())
        .collect();
    assert!(threads.len() > 1, "spans ran on one thread: {threads:?}");

    for line in std::fs::read_to_string(&log)?.lines() {
        if !line.trim().is_empty() {
            serde_json::from_str::<Value>(line)?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_refuses_a_data_dir_outside_the_project_and_writes_nothing() -> TestResult {
    let world = World::start().await?;
    let outside = world.temp.path().join("outside");
    let absolute = outside.display().to_string().replace('\\', "/");
    for data_dir in ["../outside", "a/../../outside", absolute.as_str()] {
        std::fs::write(
            world.project.join("data.toml"),
            format!(
                "[project]\ndata_dir = \"{data_dir}\"\n\n[datasets.iris]\nref = \"zenodo:3608990\"\n"
            ),
        )?;
        let ran = world.here(&["sync"])?;
        assert_eq!(ran.code, Some(50), "{data_dir}: {}", ran.stderr);
        assert!(
            ran.stderr.contains("error[project.invalid_manifest]"),
            "{}",
            ran.stderr
        );
        assert!(ran.stderr.contains("\n  help: "), "{}", ran.stderr);
        assert!(!outside.exists(), "{data_dir}");
        assert!(!world.project.join("data.lock").exists(), "{data_dir}");
        assert_eq!(world.requests().await?, 0, "{data_dir}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lock_pointing_at_loopback_is_refused_and_a_typed_url_is_not() -> TestResult {
    let world = World::start().await?;
    let uri = format!("{}/files/iris.csv", world.server.uri());
    let ran = world.here(&["add", &uri, "--as", "iris"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert_eq!(world.requests().await?, 1);
    std::fs::remove_file(world.config.join("sources").join("zenodo.toml"))?;
    let other = world.temp.path().join("other");
    std::fs::create_dir_all(&other)?;
    for name in ["data.toml", "data.lock"] {
        std::fs::copy(world.project.join(name), other.join(name))?;
    }
    let lock = std::fs::read_to_string(other.join("data.lock"))?;
    assert!(lock.contains(&uri), "{lock}");
    let fresh = world.temp.path().join("fresh-store");
    let ran = world.fl_with_store(&other, &fresh, &["sync", "--json"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stdout);
    let document = json(&ran)?;
    assert_eq!(
        document["errors"][0]["kind"], "network.refused",
        "{document}"
    );
    assert_eq!(work(&document, "requests")?, 0);
    assert_eq!(work(&document, "bytes_down")?, 0);
    assert_eq!(work(&document, "objects_ingested")?, 0);
    assert!(!other.join("data").join("iris").exists());
    assert_eq!(world.requests().await?, 1);

    let ran = world.fl_with_store(&other, &fresh, &["get", &uri, "-o", "typed"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    assert_eq!(std::fs::read(other.join("typed").join("iris.csv"))?, IRIS);
    assert_eq!(world.requests().await?, 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_whose_file_link_points_at_loopback_is_refused() -> TestResult {
    let world = World::start().await?;
    let port = world.server.address().port();
    let record = RECORD
        .replace("https://zenodo.org", &format!("http://localhost:{port}"))
        .replace("\"id\": 3608990", "\"id\": 77");
    let record_bytes = record.len() as u64;
    Mock::given(method("GET"))
        .and(path("/api/records/77"))
        .respond_with(ResponseTemplate::new(200).set_body_string(record))
        .mount(&world.server)
        .await;
    let ran = world.here(&["add", "zenodo:77", "--json"])?;
    assert_eq!(ran.code, Some(20), "{}", ran.stdout);
    let document = json(&ran)?;
    assert_eq!(
        document["errors"][0]["kind"], "network.refused",
        "{document}"
    );
    assert_eq!(work(&document, "bytes_down")?, record_bytes);
    assert_eq!(work(&document, "objects_ingested")?, 0);
    let reached: Vec<String> = world
        .server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request| request.url.path().to_owned())
        .collect();
    assert_eq!(reached, ["/api/records/77"]);
    let manifest = std::fs::read_to_string(world.project.join("data.toml"))?;
    assert!(!manifest.contains("zenodo:77"), "{manifest}");
    Ok(())
}
