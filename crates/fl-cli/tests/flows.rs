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
async fn adding_a_dataset_the_store_already_holds_counts_its_local_hits() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    let other = world.temp.path().join("fresh");
    std::fs::create_dir_all(&other)?;
    let ran = world.fl(&other, &["add", "zenodo:3608990", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let document = json(&ran)?;
    assert_eq!(work(&document, "objects_ingested")?, 0);
    assert_eq!(document["work"]["tier_hits"]["local"], 2);
    Ok(())
}

fn roots_under(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| {
            let path = entry.path();
            match (path.is_dir(), entry.file_name() == "roots") {
                (true, true) => std::fs::read_dir(&path).map_or(0, Iterator::count),
                (true, false) => roots_under(&path),
                _ => 0,
            }
        })
        .sum()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lock_is_registered_as_a_root_of_the_store() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    assert_eq!(roots_under(&world.store), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_whose_fingerprint_changed_is_counted_as_rehashed() -> TestResult {
    let world = World::start().await?;
    world.add()?;
    make_writable(&world.iris())?;
    std::fs::write(world.iris(), b"changed")?;
    let ran = world.here(&["status", "--json"])?;
    let document = json(&ran)?;
    assert_eq!(work(&document, "files_rehashed")?, 1, "{}", ran.stdout);
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
        ran.stderr
            .starts_with("\u{2717} error[reference.unresolved]"),
        "{}",
        ran.stderr
    );
    assert!(ran.stderr.contains("help:"), "{}", ran.stderr);
    assert!(!world.project.join("data.toml").exists());
    assert!(!world.project.join(".fetchloom").exists());
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
    std::fs::remove_dir_all(world.config.join("sources"))?;
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
    assert!(!world.project.join("data.toml").exists());
    Ok(())
}

fn symlink_dir(target: &Path, link: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    return std::os::windows::fs::symlink_dir(target, link);
}

#[tokio::test(flavor = "multi_thread")]
async fn sync_refuses_a_committed_symlink_on_the_way_to_the_data_dir_and_writes_nothing()
-> TestResult {
    for (data_dir, link) in [("data", "data"), ("a/b", "a")] {
        let world = World::start().await?;
        let outside = world.temp.path().join("outside");
        std::fs::create_dir_all(&outside)?;
        symlink_dir(&outside, &world.project.join(link))?;
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
        assert_eq!(std::fs::read_dir(&outside)?.count(), 0, "{data_dir}");
        assert!(!world.project.join("data.lock").exists(), "{data_dir}");
        assert_eq!(world.requests().await?, 0, "{data_dir}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sync_that_locks_first_counts_the_bytes_locking_fetched() -> TestResult {
    let world = World::start().await?;
    std::fs::write(
        world.project.join("data.toml"),
        "[datasets.iris]\nref = \"zenodo:3608990\"\n",
    )?;
    let ran = world.here(&["sync", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let document = json(&ran)?;
    assert_eq!(
        document["result"]["bytes_fetched"],
        IRIS.len(),
        "{document}"
    );
    assert_eq!(document["result"]["linked"], 1, "{document}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_directory_is_added_file_by_file_without_a_request() -> TestResult {
    let world = World::start().await?;
    let raw = world.project.join("raw");
    std::fs::create_dir_all(raw.join("sub"))?;
    std::fs::write(raw.join("a.csv"), b"a,b\n")?;
    std::fs::write(raw.join("sub").join("b.csv"), b"c,d\n")?;
    let ran = world.here(&["add", "./raw", "--json"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    let document = json(&ran)?;
    assert_eq!(work(&document, "requests")?, 0);
    assert_eq!(
        std::fs::read(
            world
                .project
                .join("data")
                .join("raw")
                .join("sub")
                .join("b.csv")
        )?,
        b"c,d\n"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_hugging_face_repository_synced_twice_transfers_nothing_the_second_time() -> TestResult {
    let world = World::start().await?;
    let sha = "c".repeat(40);
    let abc_sha256 = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    let tree = serde_json::json!([
        {"type": "directory", "oid": "0", "size": 0, "path": "data"},
        {"type": "file", "oid": "1", "size": 6, "path": "README.md"},
        {"type": "file", "oid": "2", "size": 3, "path": "data/x.bin", "lfs": {"oid": abc_sha256, "size": 3}},
    ]);
    let answers = [
        (
            "/api/datasets/lab/set/revision/main".to_owned(),
            serde_json::json!({"sha": sha, "gated": false})
                .to_string()
                .into_bytes(),
        ),
        (
            format!("/api/datasets/lab/set/tree/{sha}"),
            tree.to_string().into_bytes(),
        ),
        (
            format!("/datasets/lab/set/resolve/{sha}/README.md"),
            b"hello\n".to_vec(),
        ),
        (
            format!("/datasets/lab/set/resolve/{sha}/data/x.bin"),
            b"abc".to_vec(),
        ),
    ];
    for (route, body) in answers {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&world.server)
            .await;
    }
    let endpoint = world.server.uri();
    let env = [("HF_ENDPOINT", endpoint.as_str())];
    let run = |args: &[&str]| world.fl_with_env(&world.project, &world.store, args, &env);
    let added = run(&["add", "hf:datasets/lab/set", "--json"])?;
    assert_eq!(added.code, Some(0), "{}", added.stderr);
    let lock = std::fs::read_to_string(world.project.join("data.lock"))?;
    assert!(lock.contains(abc_sha256), "{lock}");
    for _ in 0..2 {
        let synced = run(&["sync", "--json"])?;
        assert_eq!(synced.code, Some(0), "{}", synced.stderr);
        let document = json(&synced)?;
        assert_eq!(work(&document, "bytes_down")?, 0);
        assert_eq!(work(&document, "requests")?, 0);
    }
    assert_eq!(
        std::fs::read(
            world
                .project
                .join("data")
                .join("set")
                .join("data")
                .join("x.bin")
        )?,
        b"abc"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_invalid_user_adapter_is_named_with_the_key_at_fault() -> TestResult {
    let world = World::start().await?;
    let broken = world.config.join("sources").join("broken.toml");
    std::fs::write(
        &broken,
        "[adapter]\nname = \"b\"\nscheme = \"b\"\npattern = \"{id}\"\nrecord = \"https://b.org/{nope}\"\nresolved = \"b:{id}\"\nfiles = \"$[*]\"\npath = \"$.p\"\nurl = \"$.u\"\n",
    )?;
    let ran = world.here(&["add", "zenodo:3608990", "--json"])?;
    assert_eq!(ran.code, Some(50), "{}", ran.stdout);
    let document = json(&ran)?;
    assert_eq!(document["errors"][0]["kind"], "local.invalid_config");
    let message = document["errors"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(message.contains("broken.toml"), "{message}");
    assert!(message.contains("{nope}"), "{message}");
    assert_eq!(world.requests().await?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn only_toml_files_in_the_sources_folder_are_adapters() -> TestResult {
    let world = World::start().await?;
    std::fs::write(
        world.config.join("sources").join("notes.md"),
        "not an adapter",
    )?;
    world.add()?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sources_folder_that_cannot_be_listed_is_an_error() -> TestResult {
    let world = World::start().await?;
    let sources = world.config.join("sources");
    std::fs::remove_dir_all(&sources)?;
    std::fs::write(&sources, "")?;
    let ran = world.here(&["add", "zenodo:3608990", "--json"])?;
    let document = json(&ran)?;
    assert_eq!(document["errors"][0]["kind"], "local.io", "{}", ran.stdout);
    assert_eq!(world.requests().await?, 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_file_changed_since_the_lock_is_drift() -> TestResult {
    let world = World::start().await?;
    let raw = world.project.join("raw");
    std::fs::create_dir_all(&raw)?;
    std::fs::write(raw.join("a.csv"), b"a,b")?;
    let ran = world.here(&["add", "./raw"])?;
    assert_eq!(ran.code, Some(0), "{}", ran.stderr);
    std::fs::write(raw.join("a.csv"), b"changed")?;
    std::fs::remove_dir_all(world.project.join("data"))?;
    let fresh = world.temp.path().join("fresh-store");
    let ran = world.fl_with_store(&world.project, &fresh, &["sync", "--json"])?;
    assert_eq!(ran.code, Some(30), "{}", ran.stdout);
    assert_eq!(json(&ran)?["errors"][0]["kind"], "integrity.drift");
    Ok(())
}
