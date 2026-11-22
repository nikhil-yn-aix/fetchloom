//! placing files in the data folder: what fetchloom may delete or replace, and what it keeps.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::support::{
    Part, Result, Sandbox, Server, delete, delete_folder, edit, file, json, reply, skipped, text,
    ways_left_out, without_copy_warning, zip_of,
};

const V1: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const V2: &[u8] = b"sepal_length,species\n9.9,new\n";

/// whether this disk takes `other` for `original`, as macos and windows do. the probe goes by the
/// name that made it: exfat on macos lists it decomposed, yet removes it by that name alone.
fn names_alias(sandbox: &Sandbox, original: &str, other: &str) -> Result<bool> {
    let probe = sandbox.folder(".names")?;
    fs::write(probe.join(original), b"names")?;
    let aliases = probe.join(other).try_exists()?;
    fs::remove_file(probe.join(original))?;
    fs::remove_dir(probe)?;
    Ok(aliases)
}

/// asserts that the file at `path` is read-only where a file made read-only stays so, as on every
/// disk but exfat on macos.
fn assert_read_only(sandbox: &Sandbox, path: &Path, context: &str) -> Result {
    let folder = sandbox.folder(".modes")?;
    let probe = folder.join("probe");
    fs::write(&probe, b"modes")?;
    let mut permissions = fs::metadata(&probe)?.permissions();
    permissions.set_readonly(true);
    let kept = fs::set_permissions(&probe, permissions).is_ok()
        && fs::metadata(&probe)?.permissions().readonly();
    delete(&probe)?;
    fs::remove_dir(folder)?;
    if !kept {
        return skipped("permissions", "this disk keeps no file read-only");
    }
    assert!(fs::metadata(path)?.permissions().readonly(), "{context}");
    Ok(())
}

/// whether a folder no one may write to refuses a new file here, as on every disk but exfat on
/// macos.
#[cfg(unix)]
fn folders_refuse_files(sandbox: &Sandbox) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let folder = sandbox.folder(".refusing")?;
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555))?;
    let made = fs::write(folder.join("probe"), b"refused").is_ok();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o755))?;
    fs::remove_dir_all(folder)?;
    Ok(!made)
}

/// whether a file no one may read refuses its own owner here, as on every disk but exfat on macos,
/// which takes a chmod and keeps the file readable.
#[cfg(unix)]
pub(crate) fn files_refuse_reads(sandbox: &Sandbox) -> Result<bool> {
    use std::os::unix::fs::PermissionsExt;
    let folder = sandbox.folder(".unreadable")?;
    let probe = folder.join("probe");
    fs::write(&probe, b"refused")?;
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o000))?;
    let read = fs::read(&probe).is_ok();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o644))?;
    fs::remove_dir_all(folder)?;
    Ok(!read)
}

fn copy_iris(sandbox: &Sandbox, server: &Server) -> Result {
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

fn clean_alias(sandbox: &Sandbox, server: &Server) -> Result {
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("everything matches data.lock"), "{stderr}");
    for args in [
        &["--json", "sync", "--offline"][..],
        &["--json", "sync", "--offline", "--force"][..],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let placement = &document["result"]["placement"];
        assert_eq!(placement["unchanged"], 1, "{document}");
        for count in ["placed", "replaced", "removed"] {
            assert_eq!(placement[count], 0, "{document}");
        }
        assert_eq!(placement["kept"], serde_json::json!([]), "{document}");
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn a_file_renamed_in_another_case_still_matches_its_pin() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return skipped(
            "aliases",
            "this disk keeps `Iris.csv` apart from `iris.csv`",
        );
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::rename(folder.join("iris.csv"), folder.join("Iris.csv"))?;

    clean_alias(&sandbox, &server)?;
    assert_eq!(fs::read(folder.join("Iris.csv"))?, V1);
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert!(state["files"]["data/iris/iris.csv"].is_object());
    Ok(())
}

#[test]
fn force_restores_pinned_bytes_in_a_file_renamed_in_another_case() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return skipped(
            "aliases",
            "this disk keeps `Iris.csv` apart from `iris.csv`",
        );
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    let renamed = folder.join("Iris.csv");
    fs::rename(folder.join("iris.csv"), &renamed)?;
    fs::write(&renamed, V2)?;

    let output = sandbox.run(&["sync", "--offline", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&renamed)?, V1);
    clean_alias(&sandbox, &server)
}

/// the file is respelled first, then the lock.
#[test]
fn a_name_in_another_unicode_form_still_matches_its_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let original = "caf\u{e9}.csv";
    let other = "cafe\u{301}.csv";
    if !names_alias(&sandbox, original, other)? {
        return skipped(
            "aliases",
            "this disk keeps two unicode forms of a name apart",
        );
    }
    let server = Server::start("/files/caf%C3%A9.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::rename(folder.join(original), folder.join(other))?;

    clean_alias(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    fs::write(&lock, fs::read_to_string(&lock)?.replace(original, other))?;
    clean_alias(&sandbox, &server)
}

#[test]
fn a_data_folder_in_another_case_matches_and_data_dir_takes_its_spelling() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "data", "Data")? {
        return skipped("aliases", "this disk keeps `Data` apart from `data`");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    fs::rename(sandbox.work().join("data"), sandbox.work().join("Data"))?;
    clean_alias(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    clean_alias(&sandbox, &server)?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert!(state["files"]["Data/iris/iris.csv"].is_object());
    assert!(state["files"]["data/iris/iris.csv"].is_null());
    Ok(())
}

/// the state records the file under the lock's spelling.
#[test]
fn a_lock_path_in_another_case_matches_the_placed_file() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return skipped(
            "aliases",
            "this disk keeps `Iris.csv` apart from `iris.csv`",
        );
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let lock = sandbox.work().join("data.lock");
    fs::write(
        &lock,
        fs::read_to_string(&lock)?.replace("path = \"iris.csv\"", "path = \"Iris.csv\""),
    )?;

    clean_alias(&sandbox, &server)?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(state_path)?)?;
    assert!(state["files"]["data/iris/Iris.csv"].is_object());
    assert!(state["files"]["data/iris/iris.csv"].is_null());
    Ok(())
}

/// both the file and the data folder are spelled in another case than data.toml and the lock say.
#[test]
fn remove_deletes_a_placed_file_spelled_in_another_case() -> Result {
    let sandbox = Sandbox::new()?;
    if !names_alias(&sandbox, "iris.csv", "Iris.csv")? {
        return skipped(
            "aliases",
            "this disk keeps `Iris.csv` apart from `iris.csv`",
        );
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let folder = sandbox.work().join("data").join("iris");
    fs::rename(folder.join("iris.csv"), folder.join("Iris.csv"))?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let output = sandbox.run(&["remove", "iris"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(!folder.join("Iris.csv").try_exists()?);
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// exfat on macos lists a name made composed as decomposed, yet removes it only by the spelling
/// that made it, so this takes such files and the folders they leave empty there too.
#[test]
fn remove_deletes_files_and_folders_with_accented_names() -> Result {
    let sandbox = Sandbox::new()?;
    let bytes = zip_of(&[
        ("caf\u{e9}.csv", b"first"),
        ("dossier \u{e9}/na\u{ef}ve.csv", b"other"),
    ])?;
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url, "--as", "accents"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data").join("accents");
    assert_eq!(
        fs::read(folder.join("dossier \u{e9}").join("na\u{ef}ve.csv"))?,
        b"other"
    );

    let removed = sandbox.run(&["remove", "accents"])?;

    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(!folder.try_exists()?, "{stderr}");
    Ok(())
}

/// status calls the pin missing and the respelled file extra, the person's, kept, and force puts
/// the pin back beside the respelled file, which stays.
#[test]
fn a_respelled_file_is_yours_where_the_disk_tells_names_apart() -> Result {
    for (original, other, url) in [
        ("iris.csv", "Iris.csv", "/files/iris.csv"),
        ("caf\u{e9}.csv", "cafe\u{301}.csv", "/files/caf%C3%A9.csv"),
    ] {
        let sandbox = Sandbox::new()?;
        if names_alias(&sandbox, original, other)? {
            skipped(
                "distinct-names",
                &format!("this disk takes `{other}` for `{original}`"),
            )?;
            continue;
        }
        let server = Server::start(url, vec![file(V1, "\"v1\"")])?;
        copy_iris(&sandbox, &server)?;
        let folder = sandbox.work().join("data").join("iris");
        fs::rename(folder.join(original), folder.join(other))?;

        let status = sandbox.run(&["--json", "status"])?;
        assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
        let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(
            document["result"]["missing"],
            serde_json::json!([format!("data/iris/{original}")])
        );
        assert_eq!(
            document["result"]["extra"],
            serde_json::json!([format!("data/iris/{other}")])
        );
        let output = sandbox.run(&["sync", "--offline", "--force"])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        assert_eq!(fs::read(folder.join(original))?, V1);
        assert_eq!(fs::read(folder.join(other))?, V1);
        let status: serde_json::Value =
            serde_json::from_slice(&sandbox.run(&["--json", "status"])?.stdout)?;
        assert_eq!(status["result"]["missing"], serde_json::json!([]));
        assert_eq!(
            status["result"]["extra"],
            serde_json::json!([format!("data/iris/{other}")])
        );
        assert_eq!(server.requests().len(), 1);
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn distinct_inodes_do_not_alias() -> Result {
    let sandbox = Sandbox::new()?;
    if names_alias(&sandbox, "data", "Data")? {
        return skipped("distinct-names", "this disk takes `Data` for `data`");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let original = sandbox.work().join("data").join("iris").join("iris.csv");
    let folder = sandbox.work().join("Data").join("iris");
    fs::create_dir_all(&folder)?;
    let other = folder.join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    fs::write(&other, edited)?;
    let modified = fs::metadata(&original)?.modified()?;
    set_time(&other, modified)?;
    assert_eq!(fs::metadata(&other)?.modified()?, modified);
    set_time(
        &sandbox.work().join(".fetchloom").join("state"),
        modified + Duration::from_secs(10),
    )?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let status = sandbox.run(&["--json", "status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
    let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        document["result"]["untracked"],
        serde_json::json!(["Data/iris/iris.csv"])
    );
    assert_eq!(fs::read(original)?, V1);
    Ok(())
}

#[test]
fn where_case_matters_the_exact_name_is_adoptable_and_its_case_twin_yours() -> Result {
    let sandbox = Sandbox::new()?;
    if names_alias(&sandbox, "data", "Data")? {
        return skipped("distinct-names", "this disk takes `Data` for `data`");
    }
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let original = sandbox.work().join("data").join("iris").join("iris.csv");
    let folder = sandbox.work().join("Data").join("iris");
    fs::create_dir_all(&folder)?;
    fs::hard_link(&original, folder.join("iris.csv"))?;
    fs::write(folder.join("Iris.csv"), V2)?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"Data\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let status = sandbox.run(&["--json", "status"])?;
    assert_eq!(status.status.code(), Some(0), "{}", text(&status.stderr)?);
    let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    // no record joins the link, which holds the pinned bytes, so sync adopts it
    assert_eq!(
        document["result"]["adoptable"],
        serde_json::json!(["Data/iris/iris.csv"])
    );
    assert_eq!(
        document["result"]["extra"],
        serde_json::json!(["Data/iris/Iris.csv"])
    );
    Ok(())
}

/// sets when `path` was last written, leaving its read-only flag as it was.
fn set_time(path: &Path, time: SystemTime) -> Result {
    let before = fs::metadata(path)?.permissions();
    let mut writable = before.clone();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "only a writable file takes a new time"
    )]
    writable.set_readonly(false);
    fs::set_permissions(path, writable)?;
    fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(time)?;
    fs::set_permissions(path, before)?;
    Ok(())
}

/// gives iris in `project`, and the state recording it, one whole second as their time, as a
/// clock as coarse as FAT's does when both are written within that second.
fn same_second(project: &Path) -> Result<SystemTime> {
    let placed = project.join("data").join("iris").join("iris.csv");
    let state_path = project.join(".fetchloom").join("state");
    let seconds = fs::metadata(&placed)?
        .modified()?
        .duration_since(UNIX_EPOCH)?
        .as_secs();
    let second = UNIX_EPOCH + Duration::from_secs(seconds);
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    state["files"]["data/iris/iris.csv"]["look"]["modified"] =
        serde_json::json!(u64::try_from(Duration::from_secs(seconds).as_nanos())?);
    fs::write(&state_path, serde_json::to_vec(&state)?)?;
    set_time(&placed, second)?;
    set_time(&state_path, second)?;
    Ok(second)
}

/// a coarse clock gives a file edited right after it was placed the size and time recorded for
/// it. the state written in that same tick tells such a file apart, so it is hashed.
#[test]
fn an_edit_a_coarse_clock_hides_is_found_by_hashing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let status = text(&sandbox.run(&["status"])?.stdout)?;
    let links = sandbox.links()?;
    if links.hardlinks_read_only() {
        // a hardlink is the cache's own copy, so the edit changed what every project reads, and
        // the sync path runs downloads the file again to put it back
        assert!(status.contains("Damaged data/iris/iris.csv"), "{status}");
        let path = text(&sandbox.run(&["path", "iris"])?.stderr)?;
        assert!(
            path.contains("data/iris/iris.csv was put back as pinned"),
            "path trusted the file without syncing: {path}"
        );
        assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
        assert_eq!(server.requests().len(), 2);
        return Ok(());
    }
    assert!(status.contains("Edited data/iris/iris.csv"), "{status}");
    let path = text(&sandbox.run(&["path", "iris"])?.stderr)?;
    assert!(
        path.contains("data/iris/iris.csv has changed"),
        "path trusted the file without syncing: {path}"
    );
    // `path` told of the edit once, so the sync after only counts it
    let synced = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(synced.contains("1 file left unchanged"), "{synced}");
    assert_eq!(fs::read(&placed)?, edited, "the edit stays without --force");
    let forced = sandbox.run(&["sync", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
    assert_eq!(
        server.requests().len(),
        1,
        "a project's own copy shares nothing"
    );
    Ok(())
}

/// a copy shares nothing with the cache, so no mark recalls an edit hidden by a coarse clock.
/// a sync that keeps the edit and writes the state must still leave it for --force to undo.
#[test]
fn a_kept_edit_a_coarse_clock_hides_still_yields_to_force() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let first = sandbox.run(&["sync"])?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let kept = text(&sandbox.run(&["sync"])?.stderr)?;
    assert!(
        kept.contains("data/iris/iris.csv has changed, so it stays"),
        "{kept}"
    );
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(status.contains("Edited data/iris/iris.csv"), "{status}");
    let forced = sandbox.run(&["--color", "always", "sync", "--force"])?;
    let told = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{told}");
    // it throws away the person's own bytes, so it asks for attention, never a finished step's green
    assert!(
        told.starts_with(
            "\u{1b}[1m\u{1b}[36m    Placing\u{1b}[0m 1 file\n\u{1b}[1m\u{1b}[33m   Replaced\u{1b}[0m \u{1b}[1mdata/iris/iris.csv"
        ),
        "{told:?}"
    );
    assert!(told.contains(" · discarding your edit"), "{told:?}");
    assert_eq!(fs::read(&placed)?, V1, "the locked bytes are back");
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    Ok(())
}

/// a copy whose time moved while its bytes stayed as placed, as `touch` or a restore from a backup
/// leaves it, is no edit, so a new pin replaces it.
#[test]
fn a_touched_copy_of_an_older_pin_is_replaced() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V2, "\"v2\"")],
    )?;
    copy_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    // the start of 2001, a time every disk keeps
    set_time(&placed, UNIX_EPOCH + Duration::from_hours(271_752))?;
    let upgraded = sandbox.run(&["lock", "--upgrade", "iris"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );

    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        status["result"]["outdated"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{status}"
    );
    let synced = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    let placement = &document["result"]["placement"];
    assert_eq!(placement["kept"], serde_json::json!([]), "{document}");
    assert_eq!(placement["replaced"], 1, "{document}");
    assert_eq!(fs::read(&placed)?, V2, "{document}");
    Ok(())
}

/// settling writes the state again, and the sync after it writes nothing.
#[test]
fn a_sync_settles_files_as_new_as_the_state_and_then_hashes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    // the file settles only once the state is written in a later tick than the file's time,
    // which a clock as coarse as FAT32's takes up to two seconds to reach
    sandbox.wait_for_a_later_time(second)?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let traces = sandbox.folder("traces")?;
    let sync = |name: &str| -> Result<String> {
        let trace = traces.join(name);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains("nothing to do"), "{stderr}");
        Ok(fs::read_to_string(trace)?)
    };

    let settling = sync("settling.json")?;
    assert!(settling.contains("\"name\":\"hash_racy\""), "{settling}");
    let settled = fs::metadata(&state_path)?.modified()?;
    assert_ne!(settled, second, "the state is written again");
    let quiet = sync("quiet.json")?;
    assert!(!quiet.contains("\"name\":\"hash_racy\""), "{quiet}");
    assert_eq!(fs::metadata(&state_path)?.modified()?, settled);
    Ok(())
}

/// a sync placing from the cache, with room for every file it needs, looks at what the cache lacks
/// once, after the fill, as its timeline shows: the look that only weighed the room is skipped.
#[test]
fn a_sync_from_the_cache_looks_at_what_the_cache_lacks_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    delete_folder(&sandbox.work().join("data"))?;
    let trace = sandbox.folder("traces")?.join("sync.json");
    let synced = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let events: Vec<serde_json::Value> = serde_json::from_str(&fs::read_to_string(&trace)?)?;
    let looks = events
        .iter()
        .filter(|event| event["name"] == "find_objects" && event["ph"] != "E")
        .count();
    assert_eq!(looks, 1);
    assert_eq!(fs::read(sandbox.work().join("data/iris/iris.csv"))?, V1);
    Ok(())
}

#[test]
fn removing_a_dataset_keeps_an_edit_a_coarse_clock_hides() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let second = same_second(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    set_time(&placed, second)?;

    let removed = sandbox.run(&["remove", "iris"])?;

    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv is no longer needed, but it has changed"),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, edited);
    Ok(())
}

#[test]
fn files_edited_while_a_download_runs_are_kept() -> Result {
    let sandbox = Sandbox::new()?;
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &V2.len().to_string()),
            ("ETag", "\"v2\""),
        ],
        b"",
    );
    let (release, held) = std::sync::mpsc::channel();
    let (requested, arrived) = std::sync::mpsc::channel();
    slow.push(Part::Action(Box::new(move || {
        let _ = requested.send(());
    })));
    slow.push(Part::Wait(held));
    slow.push(Part::Bytes(V2.to_vec()));
    let iris = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V2, "\"v2\""), slow],
    )?;
    sandbox.allow(&iris.url);
    let cows = Server::start("/files/cows.csv", vec![file(b"cow\nbess\n", "\"c1\"")])?;
    sandbox.allow(&cows.url);
    for server in [&iris, &cows] {
        let added = sandbox.run(&["add", &server.url])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let upgraded = sandbox.run(&["lock", "--upgrade", "iris"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\n", iris.url),
    )?;
    // a cache without the new bytes makes the sync download them after it has looked at data/
    let sync = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_CACHE", sandbox.folder("empty-cache")?)
        .stderr(Stdio::piped())
        .spawn()?;
    arrived.recv()?;
    let outdated = sandbox.work().join("data").join("iris").join("iris.csv");
    let unneeded = sandbox.work().join("data").join("cows").join("cows.csv");
    edit(&outdated, b"my notes on iris\n")?;
    edit(&unneeded, b"my notes on cows\n")?;
    drop(release);
    let output = sync.wait_with_output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&outdated)?, b"my notes on iris\n", "{stderr}");
    assert_eq!(fs::read(&unneeded)?, b"my notes on cows\n", "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv has changed, so it stays"),
        "{stderr}"
    );
    assert!(
        stderr.contains("data/cows/cows.csv is no longer needed, but it has changed"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_sync_with_nothing_to_do_writes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let written = |name: &str| fs::metadata(sandbox.work().join(name))?.modified();
    // a coarse clock, such as the tick linux stamps files with, can give the file placed last the
    // state's own time, and the first sync settles it by writing the state again in a later tick
    sandbox.wait_for_a_later_time(written(".fetchloom/state")?)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    let before = (written("data.lock")?, written(".fetchloom/state")?);
    // a rewrite now would carry a later time
    sandbox.wait_for_a_later_time(before.0.max(before.1))?;
    let trace = sandbox.folder("traces")?.join("sync.json");
    let output = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        (written("data.lock")?, written(".fetchloom/state")?),
        before
    );
    let events = fs::read_to_string(&trace)?;
    assert!(!events.contains("\"name\":\"hash_racy\""), "{events}");
    assert!(!events.contains("\"name\":\"aliases\""), "{events}");
    Ok(())
}

/// settled means the last sync left every file in place and nothing changed since.
#[test]
fn a_settled_project_syncs_without_planning_and_still_finds_edits() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    // the state the first sync writes must be newer than the file, even on a coarse clock
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    let traces = sandbox.folder("traces")?;
    let sync = |name: &str| -> Result<(String, String)> {
        let trace = traces.join(name);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok((stderr, fs::read_to_string(trace)?))
    };
    let plans = |events: &str| events.contains("\"name\":\"plan\"");

    let (_, first) = sync("first.json")?;
    // add records data.toml and data.lock as it leaves them, so this sync plans only when a
    // coarse clock left the placed file as new as the state
    assert!(
        !plans(&first) || first.contains("\"name\":\"hash_racy\""),
        "{first}"
    );
    let (said, second) = sync("second.json")?;
    assert!(said.contains("nothing to do"), "{said}");
    assert!(!plans(&second), "{second}");
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"sepal_length,species\n5.1,setosa\nedited\n")?;
    let (said, third) = sync("third.json")?;
    // a hardlink is the cache's own copy, which the edit changed, so it is put back
    let links = sandbox.links()?;
    let told = if links.hardlinks_read_only() {
        "data/iris/iris.csv was put back as pinned"
    } else {
        "data/iris/iris.csv has changed"
    };
    assert!(said.contains(told), "{said}");
    assert!(plans(&third), "{third}");
    Ok(())
}

/// the next sync still finds the file left out, though nothing the first one recorded changed, and
/// counts it without telling of it again, as the first told of it while it looked as it does now.
/// status names no sync that places files, as a sync keeps that file and places nothing.
#[test]
fn a_sync_that_left_a_file_out_is_never_taken_as_settled() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let foreign = clone.join("data").join("iris");
    fs::create_dir_all(&foreign)?;
    fs::write(foreign.join("iris.csv"), V2)?;
    let kept = "data/iris/iris.csv holds other bytes than data.lock pins, and fetchloom did not place it, so it stays";

    let synced = sandbox.command_in(&clone, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(kept), "{stderr}");
    let again = sandbox.command_in(&clone, &["sync"]).output()?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains(kept), "told once: {stderr}");
    assert!(stderr.contains("1 file left unchanged"), "{stderr}");
    assert_eq!(fs::read(foreign.join("iris.csv"))?, V2);
    let status = sandbox.command_in(&clone, &["status"]).output()?;
    assert_eq!(
        text(&status.stdout)?,
        "  Untracked data/iris/iris.csv · holds other bytes than data.lock pins\n"
    );
    let stderr = text(&status.stderr)?;
    assert!(
        !stderr.contains("places missing or updated files"),
        "{stderr}"
    );
    Ok(())
}

/// a sync of another dataset keeps what an earlier sync told of a file fetchloom did not place,
/// so the next whole sync says nothing more of it either.
#[test]
fn a_named_sync_keeps_what_was_told_of_a_file_fetchloom_did_not_place() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        ("/files/iris.csv".to_owned(), file(V1, "\"v1\"")),
        ("/files/wine.csv".to_owned(), file(b"w\n1\n", "\"w1\"")),
    ])?;
    sandbox.allow(&routes.url);
    for name in ["iris", "wine"] {
        let added = sandbox.run(&["add", &format!("{}/files/{name}.csv", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    fs::create_dir_all(clone.join("data/iris"))?;
    fs::write(clone.join("data/iris/iris.csv"), V2)?;
    let kept = "data/iris/iris.csv holds other bytes than data.lock pins, and fetchloom did not place it, so it stays\n  help: `fetchloom sync --force` replaces it\n";
    for (args, told) in [
        (&["sync"][..], true),
        (&["sync", "wine"], false),
        (&["sync"], false),
    ] {
        let synced = sandbox.command_in(&clone, args).output()?;
        let stderr = text(&synced.stderr)?;
        assert_eq!(synced.status.code(), Some(0), "{args:?}: {stderr}");
        assert_eq!(stderr.contains(kept), told, "{args:?}: {stderr}");
    }
    Ok(())
}

/// a `path` that has to sync syncs its dataset alone, which never settles a project, so the next
/// sync plans once more, finds nothing to do and settles it.
#[test]
fn add_settles_the_project_and_a_syncing_path_does_not() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let state = sandbox.work().join(".fetchloom").join("state");
    let settled = || -> Result<bool> {
        let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        Ok(recorded["synced"]["complete"] == serde_json::json!(true))
    };
    let sync = |trace: &str| -> Result<(String, String)> {
        let trace = sandbox.folder("traces")?.join(trace);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok((stderr, fs::read_to_string(&trace)?))
    };
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(settled()?, "{}", fs::read_to_string(&state)?);
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    let (stderr, events) = sync("after-add.json")?;
    // a coarse clock can leave the placed file as new as the state, which a sync hashes and
    // records again the long way
    if !events.contains("\"name\":\"hash_racy\"") {
        assert!(stderr.contains("nothing to do"), "{stderr}");
        assert!(!events.contains("\"name\":\"plan\""), "{events}");
    }

    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    let path = sandbox.run(&["path", "iris"])?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(placed.is_file());
    assert!(!settled()?, "{}", fs::read_to_string(&state)?);
    let (stderr, events) = sync("after-path.json")?;

    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert!(events.contains("\"name\":\"plan\""), "{events}");
    assert!(settled()?, "{}", fs::read_to_string(&state)?);
    Ok(())
}

#[test]
fn a_trace_shows_each_phase_of_a_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    // a missing file sends the sync through every phase
    delete(&sandbox.work().join("data").join("iris").join("iris.csv"))?;
    let trace = sandbox.folder("traces")?.join("sync.json");
    let output = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TRACE", &trace)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let events = fs::read_to_string(&trace)?;
    for phase in [
        "command",
        "read_manifest",
        "read_lock",
        "read_state",
        "scan",
        "audit",
        "relock",
        "plan",
        "decide",
        "find_objects",
        "place",
    ] {
        assert!(
            events.contains(&format!("\"name\":\"{phase}\"")),
            "{phase}: {events}"
        );
    }
    Ok(())
}

/// the projects of `shared`, with iris then edited through the first project's link.
fn shared_and_edited(sandbox: &Sandbox, server: &Server) -> Result<Option<std::path::PathBuf>> {
    let Some(other) = shared(sandbox, server)? else {
        return Ok(None);
    };
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(
        &sandbox.work().join("data").join("iris").join("iris.csv"),
        &edited,
    )?;
    Ok(Some(other))
}

/// a project in `work` and a second one in `other`, both holding iris as a link to one cached
/// file. nothing, on a filesystem without hardlinks.
fn shared(sandbox: &Sandbox, server: &Server) -> Result<Option<std::path::PathBuf>> {
    if !sandbox.links()?.hardlink {
        skipped("hardlinks", "this disk makes no hardlinks")?;
        return Ok(None);
    }
    sandbox.allow(&server.url);
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }
    let synced = sandbox
        .command_in(&other, &["sync"])
        .env("OneDrive", &other)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    for project in [sandbox.work(), other.clone()] {
        link_iris(sandbox, &project)?;
    }
    // hfs+ keeps whole seconds and fat two, so a state written in the link's tick leaves it racy
    // and hashing would take the edit below as each project's own; a later state vouches for it
    let modified =
        fs::metadata(sandbox.work().join("data").join("iris").join("iris.csv"))?.modified()?;
    if modified.duration_since(UNIX_EPOCH)?.subsec_nanos() == 0 {
        sandbox.wait_for_a_later_time(modified)?;
        for project in [sandbox.work(), other.clone()] {
            let state = project.join(".fetchloom").join("state");
            fs::write(&state, fs::read(&state)?)?;
        }
    }
    Ok(Some(other))
}

/// the editing project's own link is put back too, since it is the very copy the cache found
/// changed, and each project keeps the edit in a changed copy of its own.
#[test]
fn an_edit_through_a_shared_link_is_put_back_in_every_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared(&sandbox, &server)? else {
        return Ok(());
    };
    let mine = sandbox.work().join("data").join("iris").join("iris.csv");
    let theirs = other.join("data").join("iris").join("iris.csv");
    let mut grown = V1.to_vec();
    grown.extend_from_slice(b"local tweak\n");
    edit(&mine, &grown)?;

    let status = sandbox.command_in(&other, &["--json", "status"]).output()?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert_eq!(
        report["result"]["edited"],
        serde_json::json!([]),
        "{report}"
    );
    let synced = sandbox.command_in(&other, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&theirs)?, V1, "{stderr}");
    assert!(!stderr.contains("left unchanged"), "{stderr}");
    assert_eq!(
        server.requests().len(),
        2,
        "the changed file is fetched again"
    );

    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&mine)?, V1, "{stderr}");
    assert_eq!(server.requests().len(), 2, "{stderr}");
    for project in [sandbox.work(), other.clone()] {
        assert_eq!(
            fs::read(project.join("data").join("iris").join("iris.csv.changed"))?,
            grown
        );
        only_the_kept_copy_differs(&sandbox, &project)?;
    }
    // each project keeps a changed copy of its own, so a later edit of one changes no other
    let kept = sandbox
        .work()
        .join("data")
        .join("iris")
        .join("iris.csv.changed");
    let theirs = other.join("data").join("iris").join("iris.csv.changed");
    assert!(!same_file::is_same_file(&kept, &theirs)?);
    edit(&kept, b"mine alone\n")?;
    assert_eq!(fs::read(&theirs)?, grown);
    Ok(())
}

/// checks that status in `project` lists only the changed copy sync kept, as extra.
fn only_the_kept_copy_differs(sandbox: &Sandbox, project: &Path) -> Result {
    let status = sandbox
        .command_in(project, &["--json", "status"])
        .output()?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    let result = &report["result"];
    assert_eq!(
        result["extra"],
        serde_json::json!(["data/iris/iris.csv.changed"]),
        "{report}"
    );
    for found in [
        "missing",
        "edited",
        "untracked",
        "outdated",
        "blocked",
        "damaged",
    ] {
        assert_eq!(result[found], serde_json::json!([]), "{found}: {report}");
    }
    Ok(())
}

/// gives the fixture real hardlinks even when the filesystem chose reflinks or copies.
fn link_iris(sandbox: &Sandbox, project: &Path) -> Result {
    let state_path = project.join(".fetchloom").join("state");
    let mut state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    let path = "data/iris/iris.csv";
    let record = &mut state["files"][path];
    let digest = record["sha256"]
        .as_str()
        .ok_or("a placed file has a digest")?;
    let object = sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]);
    let destination = project.join(path);
    delete(&destination)?;
    fs::hard_link(object, &destination)?;
    let metadata = fs::symlink_metadata(destination)?;
    let modified = u64::try_from(
        metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
    )?;
    record["method"] = serde_json::json!("hardlink");
    record["look"] = serde_json::json!({"size": metadata.len(), "modified": modified});
    record["writable"] = serde_json::json!(false);
    fs::write(state_path, serde_json::to_vec(&state)?)?;
    Ok(())
}

/// on windows the other folder's listing keeps the old size and time, yet status sees the edit.
#[test]
fn an_edit_through_one_link_shows_in_the_other_projects_status() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let status = sandbox.command_in(&other, &["--json", "status"]).output()?;
    let document: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert!(
        document["result"]
            .to_string()
            .contains("data/iris/iris.csv"),
        "{document}"
    );
    Ok(())
}

/// the edit leaves the cached object at the wrong size, so the new project downloads the file.
#[test]
fn a_new_project_gets_the_locked_bytes_after_a_link_elsewhere_changed_size() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let mut grown = V1.to_vec();
    grown.extend_from_slice(b"6.3,edited\n");
    edit(
        &sandbox.work().join("data").join("iris").join("iris.csv"),
        &grown,
    )?;
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        text(&fs::read(other.join("data").join("iris").join("iris.csv"))?)?,
        text(V1)?
    );
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

/// an edit that keeps the size, saved as vim's `:w!` saves a read-only file and sealed again,
/// leaves the object looking whole but for its time, so the new project's sync checks it.
#[test]
fn a_new_project_gets_the_locked_bytes_after_a_link_elsewhere_changed_them_in_place() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let before = fs::metadata(&placed)?;
    if before.created().is_err() {
        return skipped("birth-time", "this disk keeps no birth time");
    }
    let mut changed = V1.to_vec();
    changed[0] = b'X';
    edit(&placed, &changed)?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(before.modified()? + std::time::Duration::from_secs(3600))?;
    let mut permissions = fs::metadata(&placed)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&placed, permissions)?;
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(
        text(&fs::read(other.join("data").join("iris").join("iris.csv"))?)?,
        text(V1)?
    );
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

#[test]
fn sync_force_puts_back_a_link_another_project_edited() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let forced = sandbox.command_in(&other, &["sync", "--force"]).output()?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1
    );
    assert_eq!(
        server.requests().len(),
        2,
        "the changed object is fetched again"
    );
    Ok(())
}

/// both forms say damaged, since the pinned bytes are gone and sync puts them back.
#[test]
fn status_files_a_changed_shared_copy_as_damaged_in_both_forms() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let status = sandbox.command_in(&other, &["status"]).output()?;
    let stdout = text(&status.stdout)?;
    assert!(
        stdout
            .contains("Damaged data/iris/iris.csv · linked to the cache's copy, which was changed"),
        "{stdout}"
    );
    let report = json(&sandbox.command_in(&other, &["--json", "status"]).output()?)?;
    let result = &report["result"];
    assert_eq!(
        result["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert!(result.get("edited_elsewhere").is_none(), "{report}");
    Ok(())
}

/// a folder's listing keeps the old size and time of a hardlink edited through another name, so
/// fetchloom opens each hardlink, and a plain sync puts the pinned file back as on linux.
#[cfg(windows)]
#[test]
fn another_projects_edit_through_a_link_is_seen_at_once_on_windows() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let synced = sandbox.command_in(&other, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv was put back as pinned"),
        "{stderr}"
    );
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1
    );
    Ok(())
}

/// gives iris in `project` back the modification time its state recorded, as a tool that keeps
/// times would after an edit of the same size.
fn keep_time(project: &Path) -> Result {
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(project.join(".fetchloom").join("state"))?)?;
    let modified = state["files"]["data/iris/iris.csv"]["look"]["modified"]
        .as_u64()
        .ok_or("a placed file has a time")?;
    fs::File::options()
        .write(true)
        .open(project.join("data").join("iris").join("iris.csv"))?
        .set_modified(std::time::UNIX_EPOCH + Duration::from_nanos(modified))?;
    Ok(())
}

/// a writable link leaves the copy every project shares open to an edit, so status calls it
/// outdated, and a plain sync checks its bytes and seals it with no download.
#[test]
fn a_link_made_writable_is_sealed_again() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut permissions = fs::metadata(&placed)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test opens a placed link to edits as a person can"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&placed, permissions)?;

    let status = sandbox.run(&["--json", "status"])?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["outdated"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        status.contains("Outdated data/iris/iris.csv · a link to the cache's copy made writable"),
        "{status}"
    );
    let synced = sandbox.run(&["sync"])?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(fs::metadata(&placed)?.permissions().readonly());
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    Ok(())
}

/// with no mark and the same size and time, a plain sync would miss the edit.
#[test]
fn status_points_to_sync_verify_for_changes_only_hashing_finds() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;

    let status = sandbox
        .command_in(&other, &["status", "--verify"])
        .output()?;

    let stdout = text(&status.stdout)?;
    assert!(
        stdout.contains("Damaged data/iris/iris.csv · linked to the cache's copy"),
        "{stdout}"
    );
    let stderr = text(&status.stderr)?;
    assert!(stderr.contains("`fetchloom sync --verify`"), "{stderr}");
    assert!(!stderr.contains("`fetchloom sync` places"), "{stderr}");
    Ok(())
}

/// an edit through a link that kept its size, time and mode changed the cache's copy with it, so
/// `status --verify` counts the download `sync --verify` makes to put it back.
#[test]
fn status_verify_counts_the_download_a_changed_link_needs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let edited = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut permissions = fs::metadata(&edited)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&edited, permissions)?;

    let status = sandbox
        .command_in(&other, &["--json", "status", "--verify"])
        .output()?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert_eq!(report["result"]["to_download"]["files"], 1, "{report}");
    let synced = sandbox.command_in(&other, &["sync", "--verify"]).output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(server.requests().len(), 2, "the changed copy is downloaded");
    Ok(())
}

/// an edit through one project's link changes the other's file too: status and the sync that puts
/// it back name the project that shares the copy, and once the cache holds the pinned bytes again,
/// the edited project's file is told as linked no more.
#[test]
fn a_change_through_a_shared_link_names_the_other_project() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let shared = |project: &Path| {
        format!(
            "hint: {} places the same files from this cache, so a link there may hold the changed bytes too: `fetchloom sync` there puts them back",
            project.display()
        )
    };
    let status = sandbox
        .command_in(&other, &["status", "--verify"])
        .output()?;
    let (stdout, stderr) = (text(&status.stdout)?, text(&status.stderr)?);
    assert!(
        stdout
            .contains("Damaged data/iris/iris.csv · linked to the cache's copy, which was changed"),
        "{stdout}"
    );
    assert!(stderr.contains(&shared(&sandbox.work())), "{stderr}");
    let synced = sandbox.command_in(&other, &["sync", "--verify"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains(&shared(&sandbox.work())), "{stderr}");
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        status.contains(
            "Damaged data/iris/iris.csv · was linked to the cache's copy, which was changed"
        ),
        "{status}"
    );
    Ok(())
}

/// an edit keeping the size and time shows in no listing, so the cache's mark is all the other
/// project has. status, sync and path agree on it, and the copy sync keeps is all that differs.
#[test]
fn a_marked_link_whose_size_and_time_stayed_is_put_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let before = sandbox.command_in(&other, &["--json", "status"]).output()?;
    let before: serde_json::Value = serde_json::from_slice(&before.stdout)?;
    assert_eq!(
        before["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"])
    );

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    assert!(
        stderr.contains("data/iris/iris.csv was put back as pinned"),
        "{stderr}"
    );
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv.changed"))?,
        edited
    );
    only_the_kept_copy_differs(&sandbox, &other)?;
    // a ready dataset answers without taking the project lock
    let lock = other.join(".fetchloom").join("lock");
    fs::remove_file(&lock)?;
    let path = sandbox.command_in(&other, &["path", "iris"]).output()?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(!lock.exists(), "path synced again");
    Ok(())
}

/// a coarse clock can give the state its link's own time, making the edit racy so hashing finds
/// it; the link is still the cache's copy, so a plain sync puts the pinned file back.
#[test]
fn a_racy_edit_through_a_shared_link_is_put_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    if shared_and_edited(&sandbox, &server)?.is_none() {
        return Ok(());
    }
    keep_time(&sandbox.work())?;
    let state = sandbox.work().join(".fetchloom").join("state");
    let recorded: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
    let modified = recorded["files"]["data/iris/iris.csv"]["look"]["modified"]
        .as_u64()
        .ok_or("a placed file has a time")?;
    set_time(&state, UNIX_EPOCH + Duration::from_nanos(modified))?;
    let verified = sandbox.run(&["cache", "verify"])?;
    assert_eq!(
        verified.status.code(),
        Some(30),
        "{}",
        text(&verified.stderr)?
    );
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");

    let synced = sandbox.run(&["sync"])?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv was put back as pinned"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn a_read_only_file_becomes_writable_when_the_dataset_asks_for_copies() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    // a folder a sync client owns gets no hardlinks, so iris is a read-only reflink or copy
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert_read_only(&sandbox, &placed, "")?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let synced = sandbox
        .command(&["sync"])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "{}",
        text(&synced.stderr)?
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    Ok(())
}

#[test]
fn a_recovered_hardlink_becomes_writable_when_copies_are_requested() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert!(fs::metadata(&placed)?.permissions().readonly());
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let before = fs::metadata(&placed)?.modified()?;
    set_time(&placed, before - Duration::from_secs(10))?;
    assert_ne!(fs::metadata(&placed)?.modified()?, before);

    let synced = sandbox.run(&["sync"])?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "recovery kept a read-only hardlink despite copy = true: {stderr}"
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(server.requests().len(), 1, "the cache still holds iris");
    let status = sandbox.run(&["status"])?;
    let stderr = text(&status.stderr)?;
    assert_eq!(status.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("everything matches data.lock"), "{stderr}");
    Ok(())
}

/// status calls the read-only files outdated, and path syncs them before it answers.
#[test]
fn read_only_files_of_a_dataset_that_asks_for_copies_need_a_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert_read_only(&sandbox, &placed, "")?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;

    let status = sandbox.run(&["status"])?;
    let stdout = text(&status.stdout)?;
    assert!(stdout.contains("Outdated data/iris/iris.csv"), "{stdout}");
    let document: serde_json::Value =
        serde_json::from_slice(&sandbox.run(&["--json", "status"])?.stdout)?;
    assert_eq!(
        document["result"]["outdated"],
        serde_json::json!(["data/iris/iris.csv"])
    );
    let path = sandbox.run(&["path", "iris"])?;
    assert_eq!(path.status.code(), Some(0), "{}", text(&path.stderr)?);
    assert!(
        !fs::metadata(&placed)?.permissions().readonly(),
        "path answered before making iris writable"
    );
    assert_eq!(fs::read(&placed)?, V1);
    let after = sandbox.run(&["status"])?;
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// once data.toml no longer asks for copies, the copies no one changed are placed as any
/// dataset's files again, read-only and linked to the cache where the disk links, and an edit stays.
#[test]
fn copies_no_dataset_asks_for_go_back_to_read_only_files() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        ("/amazon.csv".to_owned(), file(V1, "\"a\"")),
        ("/yelp.csv".to_owned(), file(V2, "\"y\"")),
    ])?;
    sandbox.allow(&routes.url);
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[datasets.amazon]\nref = \"{0}/amazon.csv\"\ncopy = true\n\n[datasets.yelp]\nref = \"{0}/yelp.csv\"\ncopy = true\n",
            routes.url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let asked = routes.requests().len();
    let data = sandbox.work().join("data");
    let (untouched, edited) = (data.join("amazon/amazon.csv"), data.join("yelp/yelp.csv"));
    fs::write(&edited, b"sepal_length,species\n9.9,fixed\n")?;
    fs::write(
        &manifest,
        fs::read_to_string(&manifest)?.replace("copy = true\n", ""),
    )?;

    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        document["result"]["outdated"],
        serde_json::json!(["data/amazon/amazon.csv"]),
        "{document}"
    );
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_read_only(&sandbox, &untouched, &stderr)?;
    assert_eq!(fs::read(&untouched)?, V1);
    if sandbox.links()?.hardlinks_read_only() {
        let hex = crate::support::sha256_hex(V1);
        let object = sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]);
        assert!(same_file::is_same_file(&untouched, object)?, "{stderr}");
    }
    assert_eq!(fs::read(&edited)?, b"sepal_length,species\n9.9,fixed\n");
    assert!(
        stderr.contains("data/yelp/yelp.csv has changed, so it stays"),
        "{stderr}"
    );
    assert_eq!(
        routes.requests().len(),
        asked,
        "the cache still holds amazon"
    );
    Ok(())
}

#[test]
fn sync_reports_zero_byte_writable_copies_in_a_synced_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/empty.csv", vec![file(b"", "\"empty\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.empty]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let output = sandbox
        .command(&["sync", "-v"])
        .env("OneDrive", sandbox.work())
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    if sandbox.links()?.reflink {
        assert!(stderr.contains("1 file cloned from the cache"), "{stderr}");
    } else {
        assert!(stderr.contains("1 file copied, 0 B"), "{stderr}");
        assert!(stderr.contains("1 copy"), "{stderr}");
        // copy = true asks for copies, so the sync client is not what made them
        assert!(!stderr.contains("data is inside onedrive"), "{stderr}");
    }
    assert_eq!(
        fs::metadata(sandbox.work().join("data/empty/empty.csv"))?.len(),
        0
    );
    Ok(())
}

/// a run cut off before writing the state leaves a link the next run adopts; it is the cached
/// object, so an edit through it is audited like any placed link's.
#[test]
fn an_adopted_link_is_recorded_as_a_hardlink() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let state_path = sandbox.work().join(".fetchloom").join("state");
    fs::remove_file(&state_path)?;

    let synced = sandbox.run(&["sync"])?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_path)?)?;
    assert_eq!(
        state["files"]["data/iris/iris.csv"]["method"],
        serde_json::json!("hardlink")
    );
    Ok(())
}

/// a project where iris was saved by hand at its place before `add`, which then downloaded it
/// to learn its hash, since the link publishes no checksum. the stderr of that add.
fn adopted_iris(sandbox: &Sandbox, server: &Server) -> Result<String> {
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    fs::create_dir_all(placed.parent().ok_or("a placed file has a folder")?)?;
    fs::write(&placed, V1)?;
    let added = sandbox.run(&["add", &server.url])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    Ok(stderr)
}

/// where the disk links, the file is linked as placing links a new one, so one copy stays on disk:
/// a clone where read-only files are cloned, else a hardlink; elsewhere the file stays as it is,
/// and the run says which.
#[test]
fn a_file_already_in_place_is_linked_to_the_download_it_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let stderr = adopted_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let linked = same_file::is_same_file(&placed, iris_object(&sandbox, &sandbox.work())?)?;
    let links = sandbox.links()?;
    if links.reflinks_read_only() {
        assert!(!linked, "a clone is a file of its own: {stderr}");
        assert_eq!(
            placed_method(&sandbox.work(), "data/iris/iris.csv")?,
            "reflink"
        );
        assert!(
            stderr.contains(
                "Adopted data/iris · 1 file already held the pinned bytes, so it is now cloned from the cache"
            ),
            "{stderr}"
        );
    } else if links.hardlink {
        assert!(linked, "{stderr}");
        assert!(fs::metadata(&placed)?.permissions().readonly());
        assert!(
            stderr.contains(
                "Adopted data/iris · 1 file already held the pinned bytes, so it is now linked to the cache"
            ),
            "{stderr}"
        );
    } else {
        assert!(!linked, "{stderr}");
        assert!(
            stderr.contains(
                "Adopted data/iris · 1 file already held the pinned bytes, so it stays as it is"
            ),
            "{stderr}"
        );
    }
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// how the state of `project` records the file at `path` placed: `hardlink`, `reflink` or `copy`.
fn placed_method(project: &Path, path: &str) -> Result<String> {
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(project.join(".fetchloom").join("state"))?)?;
    Ok(state["files"][path]["method"]
        .as_str()
        .ok_or("a placed file has a method")?
        .to_owned())
}

/// what the files below `folders` take on the disk, each file once however many names it has, as
/// `du` counts them.
#[cfg(unix)]
fn on_disk(folders: &[&Path]) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    let mut seen = std::collections::HashSet::new();
    let mut pending: Vec<PathBuf> = folders.iter().map(|folder| folder.to_path_buf()).collect();
    let mut bytes = 0;
    while let Some(path) = pending.pop() {
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.is_dir() {
            for entry in fs::read_dir(&path)? {
                pending.push(entry?.path());
            }
        } else if seen.insert((metadata.dev(), metadata.ino())) {
            bytes += metadata.blocks() * 512;
        }
    }
    Ok(bytes)
}

/// a project whose data folder was put back from a copy that kept no links, as `rsync -a` without
/// -H leaves it: each file that was linked to the cache is a file of its own with the same size
/// and time. returns the placed iris file and its cached object.
fn copied_back(sandbox: &Sandbox, server: &Server) -> Result<(PathBuf, PathBuf)> {
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let data = sandbox.work().join("data");
    let backup = sandbox.folder("backup")?;
    crate::support::copy_apart(&data, &backup)?;
    delete_folder(&data)?;
    crate::support::copy_apart(&backup, &data)?;
    let placed = data.join("iris").join("iris.csv");
    let object = iris_object(sandbox, &sandbox.work())?;
    assert!(!same_file::is_same_file(&placed, &object)?);
    Ok((placed, object))
}

/// sync links each such copy again where it placed links, after reading it to be sure of its
/// bytes, so the disk holds them once, and a second sync has nothing to do. a clone cannot be
/// told from a copy by how it looks, so a disk that clones keeps them, as does one that links
/// nothing.
#[test]
fn a_data_folder_copied_without_links_is_linked_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (placed, object) = copied_back(&sandbox, &server)?;
    #[cfg(unix)]
    let before = on_disk(&[&sandbox.work().join("data"), &sandbox.cache()])?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    let linked = same_file::is_same_file(&placed, &object)?;
    if sandbox.links()?.hardlinks_read_only() {
        assert!(linked, "{stderr}");
        assert!(
            stderr.contains(&format!(
                "     Linked data/iris · 1 copy of a cached file is now linked to the cache, freeing {} B\n",
                V1.len()
            )),
            "{stderr}"
        );
        assert_eq!(
            placed_method(&sandbox.work(), "data/iris/iris.csv")?,
            "hardlink"
        );
        // linking is work done, so the run says it synced rather than that nothing was to do
        assert!(
            stderr.contains("     Synced 1 dataset into data\n"),
            "{stderr}"
        );
        // what the copy took on the disk is free again
        #[cfg(unix)]
        assert!(
            on_disk(&[&sandbox.work().join("data"), &sandbox.cache()])? < before,
            "{stderr}"
        );
        let report = json(&sandbox.run(&["--json", "cache"])?)?;
        assert_eq!(report["result"]["copies"], 0, "{report}");
    } else {
        assert!(!linked, "{stderr}");
        assert!(!stderr.contains("Linked "), "{stderr}");
    }
    let again = sandbox.run(&["sync"])?;
    let told = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{told}");
    assert!(told.contains("nothing to do"), "{told}");
    assert_eq!(same_file::is_same_file(&placed, &object)?, linked, "{told}");
    Ok(())
}

/// a project copied with its data folder to a machine whose cache never held its files adopts
/// them as they are. once a push took them into the cache, two copies of each are on the disk,
/// so the next sync links the project's to the cache's, as it places a new file.
#[test]
fn files_taken_into_an_empty_cache_are_linked_by_the_next_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let moved = sandbox.folder("moved")?;
    crate::support::copy_apart(&sandbox.work(), &moved)?;
    let cache = sandbox.folder("new-cache")?;
    let mirror = sandbox.folder("mirror")?;
    let run = |args: &[&str]| {
        sandbox
            .command_in(&moved, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let adopted = run(&["sync"])?;
    let stderr = text(&adopted.stderr)?;
    assert_eq!(adopted.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("so it stays as it is"), "{stderr}");
    // a settled sync looks for the copy's object once, notes that the cache lacks it, and the
    // next looks at no object until the cache takes one
    let traces = sandbox.folder("traces")?;
    let looked = |name: &str| -> Result<bool> {
        let trace = traces.join(name);
        let settled = sandbox
            .command_in(&moved, &["sync"])
            .env("FETCHLOOM_CACHE", &cache)
            .env("FETCHLOOM_TRACE", &trace)
            .output()?;
        let stderr = text(&settled.stderr)?;
        assert!(stderr.contains("nothing to do"), "{stderr}");
        Ok(fs::read_to_string(&trace)?.contains("\"name\":\"cached_copies\""))
    };
    let links = sandbox.links()?;
    if links.hardlink || links.reflink {
        assert!(looked("first.json")?);
        assert!(!looked("second.json")?);
    }
    let mirror_arg = mirror.to_string_lossy();
    let pushed = run(&["push", "--to", &mirror_arg, "--yes"])?;
    let stderr = text(&pushed.stderr)?;
    assert_eq!(pushed.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("from data into the cache"), "{stderr}");
    let placed = moved.join("data").join("iris").join("iris.csv");
    let digest = {
        let state: serde_json::Value =
            serde_json::from_slice(&fs::read(moved.join(".fetchloom").join("state"))?)?;
        state["files"]["data/iris/iris.csv"]["sha256"]
            .as_str()
            .ok_or("a placed file has a digest")?
            .to_owned()
    };
    let object = cache.join("objects").join(&digest[..2]).join(&digest[2..]);
    assert!(object.exists());
    let synced = run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    let links = sandbox.links()?;
    let how = if links.reflinks_read_only() {
        Some(("cloned from", "reflink"))
    } else if links.hardlink {
        Some(("linked to", "hardlink"))
    } else {
        None
    };
    match how {
        Some((said, method)) => {
            assert!(
                stderr.contains(&format!(
                    "     Linked data/iris · 1 copy of a cached file is now {said} the cache, freeing {} B\n",
                    V1.len()
                )),
                "{stderr}"
            );
            assert_eq!(placed_method(&moved, "data/iris/iris.csv")?, method);
            assert_eq!(
                same_file::is_same_file(&placed, &object)?,
                method == "hardlink"
            );
        }
        None => assert!(!stderr.contains("Linked "), "{stderr}"),
    }
    let again = run(&["sync"])?;
    let told = text(&again.stderr)?;
    assert!(told.contains("nothing to do"), "{told}");
    Ok(())
}

/// sets the file at `path` to `bytes` of the same length, keeping its time and read-only flag, as
/// a tool that puts a file's time back after writing it does.
fn edit_keeping_look(path: &Path, bytes: &[u8]) -> Result {
    let metadata = fs::metadata(path)?;
    assert_eq!(metadata.len(), bytes.len() as u64);
    let mut writable = metadata.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the file is made read-only again next"
    )]
    writable.set_readonly(false);
    fs::set_permissions(path, writable)?;
    fs::write(path, bytes)?;
    let file = fs::File::options().write(true).open(path)?;
    file.set_modified(metadata.modified()?)?;
    drop(file);
    fs::set_permissions(path, metadata.permissions())?;
    assert_eq!(fs::metadata(path)?.modified()?, metadata.modified()?);
    Ok(())
}

/// sync never links a file of a dataset that asks for copies, nor one whose bytes are not the
/// pinned ones though it looks as placed. it reads such a file once and remembers it, so the next
/// sync reads nothing.
#[test]
fn copies_asked_for_and_hidden_edits_are_never_linked() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        ("/iris.csv".to_owned(), file(V1, "\"v1\"")),
        ("/penguins.csv".to_owned(), file(b"bill,39\n", "\"p1\"")),
    ])?;
    sandbox.allow(&routes.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{0}/iris.csv\"\ncopy = true\n\n[datasets.penguins]\nref = \"{0}/penguins.csv\"\n",
            routes.url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    // sync reads a file as new as the state, which finds an edit by its bytes, so the edit below
    // is hidden only once a later sync has vouched for the files
    let state_file = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state_file)?.modified()?)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    let data = sandbox.work().join("data");
    let backup = sandbox.folder("backup")?;
    crate::support::copy_apart(&data, &backup)?;
    delete_folder(&data)?;
    crate::support::copy_apart(&backup, &data)?;
    let iris = data.join("iris").join("iris.csv");
    let penguins = data.join("penguins").join("penguins.csv");
    edit_keeping_look(&penguins, b"bill,99\n")?;
    // a writable copy, or a writable clone where the disk clones
    let copied = placed_method(&sandbox.work(), "data/iris/iris.csv")?;
    let traces = sandbox.folder("traces")?;
    let trace = |name: &str| -> Result<(String, String)> {
        let path = traces.join(name);
        let output = sandbox
            .command(&["sync"])
            .env("FETCHLOOM_TRACE", &path)
            .output()?;
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(0), "{stderr}");
        Ok((stderr, fs::read_to_string(&path)?))
    };
    let (stderr, events) = trace("first.json")?;
    assert_eq!(fs::read(&iris)?, V1, "{stderr}");
    assert!(!fs::metadata(&iris)?.permissions().readonly(), "{stderr}");
    assert_eq!(fs::read(&penguins)?, b"bill,99\n", "{stderr}");
    assert!(!stderr.contains("Linked "), "{stderr}");
    let state: serde_json::Value = serde_json::from_slice(&fs::read(&state_file)?)?;
    assert_eq!(
        state["files"]["data/iris/iris.csv"]["method"],
        copied.as_str()
    );
    assert_ne!(copied, "hardlink");
    if sandbox.links()?.hardlinks_read_only() {
        // the copy was read, its other bytes found and remembered
        assert!(events.contains("\"name\":\"link_copies\""), "{events}");
        assert_eq!(
            state["files"]["data/penguins/penguins.csv"]["unlinkable"], true,
            "{state}"
        );
    }
    let (stderr, events) = trace("second.json")?;
    assert!(stderr.contains("nothing to do"), "{stderr}");
    assert!(!events.contains("\"name\":\"link_copies\""), "{events}");
    assert_eq!(fs::read(&penguins)?, b"bill,99\n", "{stderr}");
    Ok(())
}

/// a file of a dataset that asks for copies stays a copy when sync takes it as placed though it
/// was made read-only meanwhile, as a tool that guards its inputs makes them, and so it stays
/// at the next sync.
#[test]
fn a_copy_asked_for_made_read_only_while_sync_takes_it_stays_a_copy() -> Result {
    use crate::support::{Running, wait_until};
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    // the same bytes written again at a later time, so sync reads the file and takes it as placed
    sandbox.wait_for_a_later_time(fs::metadata(&placed)?.modified()?)?;
    fs::write(&placed, V1)?;
    let release = sandbox.folder("hold")?.join("release");
    let held = sandbox.folder("hold")?.join("release.held");
    let mut command = sandbox.command(&["sync"]);
    command.env("FETCHLOOM_TEST_HOLD_ADOPT", &release);
    let running = Running::start(&mut command)?;
    wait_until("sync to take the copy as placed", || Ok(held.exists()))?;
    let mut permissions = fs::metadata(&placed)?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&placed, permissions)?;
    fs::write(&release, b"")?;
    let output = running.finish()?;

    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Linked "), "{stderr}");
    let object = iris_object(&sandbox, &sandbox.work())?;
    assert!(!same_file::is_same_file(&placed, &object)?, "{stderr}");
    assert_eq!(
        placed_method(&sandbox.work(), "data/iris/iris.csv")?,
        "copy"
    );
    let again = sandbox.run(&["sync"])?;
    let told = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{told}");
    assert!(!told.contains("Linked "), "{told}");
    assert_eq!(
        placed_method(&sandbox.work(), "data/iris/iris.csv")?,
        "copy",
        "{told}"
    );
    assert_eq!(fs::read(&placed)?, V1, "{told}");
    Ok(())
}

/// a drive that makes no links says so when sync copies files there: fat and exfat refuse both
/// links, macos on exfat with its own ENOTSUP, which counts as a refusal too.
#[test]
fn a_drive_that_links_nothing_says_files_were_copied() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);

    let added = sandbox.run(&["add", &server.url])?;

    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    let links = sandbox.links()?;
    assert_eq!(
        stderr.contains("cannot link files"),
        !links.reflink && !links.hardlink,
        "{stderr}"
    );
    Ok(())
}

/// a file sync takes from the data folder into a cache that lacks its bytes, for another path
/// that needs them, is read again before sync links it, as any copy is: an edit made since the
/// take, though it kept the file's size and time, stays.
#[test]
fn a_file_taken_into_the_cache_is_read_again_before_it_is_linked() -> Result {
    use crate::support::{Running, wait_until};
    let sandbox = Sandbox::new()?;
    // only a placed hardlink can lose its link and be linked again; a clone or a copy stays
    if !sandbox.links()?.hardlinks_read_only() {
        return skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    let routes = crate::support::Routes::start(vec![
        ("/iris.csv".to_owned(), file(V1, "\"v1\"")),
        ("/twin.csv".to_owned(), file(V1, "\"v1\"")),
    ])?;
    sandbox.allow(&routes.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{0}/iris.csv\"\n\n[datasets.twin]\nref = \"{0}/twin.csv\"\n",
            routes.url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    // a later state vouches for the files, so no read of a file as new as it proves one
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    let settled = sandbox.run(&["sync"])?;
    assert_eq!(settled.status.code(), Some(0), "{}", text(&settled.stderr)?);
    let data = sandbox.work().join("data");
    let backup = sandbox.folder("backup")?;
    crate::support::copy_apart(&data, &backup)?;
    delete_folder(&data)?;
    crate::support::copy_apart(&backup, &data)?;
    let iris = data.join("iris").join("iris.csv");
    let twin = data.join("twin").join("twin.csv");
    delete(&twin)?;
    let empty = sandbox.folder("empty cache")?;
    let release = sandbox.folder("hold")?.join("release");
    let held = sandbox.folder("hold")?.join("release.held");
    let mut command = sandbox.command(&["sync", "--offline"]);
    command
        .env("FETCHLOOM_CACHE", &empty)
        .env("FETCHLOOM_TEST_HOLD_LINK", &release);
    let running = Running::start(&mut command)?;
    wait_until("sync to link the copies", || Ok(held.exists()))?;
    edit_keeping_look(&iris, b"sepal_length,species\n5.1,SETOSA\n")?;
    fs::write(&release, b"")?;
    let output = running.finish()?;

    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("from data into the cache"), "{stderr}");
    assert_eq!(fs::read(&twin)?, V1, "{stderr}");
    assert_eq!(
        fs::read(&iris)?,
        b"sepal_length,species\n5.1,SETOSA\n",
        "{stderr}"
    );
    assert!(!stderr.contains("Linked data/iris"), "{stderr}");
    Ok(())
}

/// a program reading a copy while sync links it keeps reading the bytes it opened: the link takes
/// the copy's name in one step, and the copy lasts while it is open.
#[test]
fn a_copy_open_in_a_reader_keeps_its_bytes() -> Result {
    use std::io::Read;
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlinks_read_only() {
        return skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (placed, object) = copied_back(&sandbox, &server)?;
    let mut reader = fs::File::open(&placed)?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let mut read = Vec::new();
    reader.read_to_end(&mut read)?;
    assert_eq!(read, V1, "{stderr}");
    assert!(same_file::is_same_file(&placed, &object)?, "{stderr}");
    Ok(())
}

/// windows refuses to rename over a file another program holds open without sharing its
/// deletion, so sync leaves it as it is, with no error, and links it at the next sync once it
/// is closed.
#[cfg(windows)]
#[test]
fn a_copy_another_program_holds_is_linked_once_it_is_closed() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlinks_read_only() {
        return skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (placed, object) = copied_back(&sandbox, &server)?;
    let held = hold(&placed)?;
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Linked "), "{stderr}");
    assert!(!stderr.contains("iris.csv"), "{stderr}");
    assert!(!same_file::is_same_file(&placed, &object)?, "{stderr}");
    assert!(fs::metadata(&placed)?.permissions().readonly(), "{stderr}");
    drop(held);
    let synced = sandbox.run(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Linked data/iris · 1 copy"), "{stderr}");
    assert!(same_file::is_same_file(&placed, &object)?, "{stderr}");
    assert_eq!(fs::read(&placed)?, V1);
    Ok(())
}

/// three datasets share `raw`, which already holds each one's file, so status says in one line
/// that a plain sync records them, as no difference, rather than offering `--force`.
#[test]
fn files_already_in_a_shared_folder_are_adopted_and_told_once() -> Result {
    let sandbox = Sandbox::new()?;
    let tables: [(&str, &[u8]); 3] = [("a", b"a,1\n"), ("b", b"b,2\n"), ("c", b"c,3\n")];
    let routes = crate::support::Routes::start(
        tables
            .iter()
            .map(|(name, bytes)| (format!("/{name}.csv"), file(bytes, "\"1\"")))
            .collect(),
    )?;
    let manifest = sandbox.work().join("data.toml");
    let raw = sandbox.work().join("data").join("raw");
    fs::create_dir_all(&raw)?;
    for (name, _) in tables {
        let added = sandbox.run(&["add", &format!("{}/{name}.csv", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    for (name, bytes) in tables {
        let table = format!("[datasets.{name}]\n");
        let written = fs::read_to_string(&manifest)?;
        fs::write(
            &manifest,
            written.replace(&table, &format!("{table}path = \"raw\"\n")),
        )?;
        fs::write(raw.join(format!("{name}.csv")), bytes)?;
    }
    let status = sandbox.run(&["status"])?;
    let told = text(&status.stdout)?;
    assert!(!told.contains("data/raw"), "{told}");
    let story = text(&status.stderr)?;
    assert!(
        story.contains(
            "   In place 3 files already hold the pinned bytes; `fetchloom sync` records them as placed\n"
        ),
        "{story}"
    );
    assert!(!format!("{told}{story}").contains("--force"), "{story}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["untracked"], serde_json::json!([]));
    assert_eq!(
        status["result"]["adoptable"],
        serde_json::json!(["data/raw/a.csv", "data/raw/b.csv", "data/raw/c.csv"])
    );
    let synced = sandbox.run(&["sync"])?;
    let told = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{told}");
    assert_eq!(told.matches("Adopted data/raw · ").count(), 1, "{told}");
    assert!(
        told.contains("Adopted data/raw · 3 files already held the pinned bytes, so they "),
        "{told}"
    );
    Ok(())
}

/// a pull took the dataset out of data.toml and data.lock, leaving the file fetchloom placed and
/// the person's notes in its folder, which no dataset owns now: status names the file sync removes
/// and never the notes, and sync then does as status said.
#[test]
fn status_tells_the_files_sync_removes_from_your_own() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = crate::support::Routes::start(vec![
        ("/tips.csv".to_owned(), file(b"tip,1\n", "\"1\"")),
        ("/penguins.csv".to_owned(), file(b"bill,39\n", "\"1\"")),
    ])?;
    let work = sandbox.work();
    let add = |name: &str| -> Result {
        let added = sandbox.run(&["add", &format!("{}/{name}.csv", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
        Ok(())
    };
    add("tips")?;
    let pulled: Vec<(PathBuf, Vec<u8>)> = ["data.toml", "data.lock"]
        .iter()
        .map(|name| Ok((work.join(name), fs::read(work.join(name))?)))
        .collect::<Result<_>>()?;
    add("penguins")?;
    let folder = work.join("data").join("penguins");
    fs::write(folder.join("notes.txt"), b"mine\n")?;
    for (path, bytes) in &pulled {
        fs::write(path, bytes)?;
    }
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stdout)?,
        "      Extra data/penguins/penguins.csv · fetchloom placed it and no dataset needs it now, so `fetchloom sync` removes it\n"
    );
    let stderr = text(&status.stderr)?;
    let hint =
        "  hint: `fetchloom sync` removes the files fetchloom placed that no dataset needs now\n";
    assert!(stderr.contains(hint), "{hint:?}");
    assert!(!stderr.contains("notes.txt"), "{stderr}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        (&status["result"]["extra"], &status["result"]["unneeded"]),
        (
            &serde_json::json!([]),
            &serde_json::json!(["data/penguins/penguins.csv"])
        )
    );
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert!(!folder.join("penguins.csv").exists());
    assert_eq!(fs::read(folder.join("notes.txt"))?, b"mine\n");
    Ok(())
}

/// the file goes as a placed one does, with a warning, since a person put it there.
#[test]
fn remove_says_it_deletes_a_file_that_was_there_before_it_was_adopted() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    adopted_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");

    let dry = json(&sandbox.run(&["--json", "remove", "iris", "--dry-run"])?)?;
    assert_eq!(
        crate::support::warned(&dry),
        [
            "would remove data/iris/iris.csv, a file that was already there when fetchloom adopted it"
        ],
        "{dry}"
    );
    assert_eq!(crate::support::kinds(&dry), ["place.adopted_removed"]);
    assert_eq!(fs::read(&placed)?, V1);
    let removed = sandbox.run(&["remove", "iris"])?;

    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "warning: removed data/iris/iris.csv, a file that was already there when fetchloom adopted it"
        ),
        "{stderr}"
    );
    assert!(!placed.exists(), "{stderr}");
    Ok(())
}

/// the cached object behind iris in `project`, by the digest its state records.
fn iris_object(sandbox: &Sandbox, project: &Path) -> Result<std::path::PathBuf> {
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(project.join(".fetchloom").join("state"))?)?;
    let digest = state["files"]["data/iris/iris.csv"]["sha256"]
        .as_str()
        .ok_or("a placed file has a digest")?;
    Ok(sandbox
        .cache()
        .join("objects")
        .join(&digest[..2])
        .join(&digest[2..]))
}

/// marks `object` as changed, as `cache verify` does before a new copy replaces its bytes.
fn mark(sandbox: &Sandbox, object: &Path) -> Result {
    let fan = object
        .parent()
        .and_then(Path::file_name)
        .ok_or("an object sits in a fan folder")?;
    let rest = object.file_name().ok_or("an object has a name")?;
    let folder = sandbox.cache().join("changed");
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join(format!(
            "{}{}",
            fan.to_string_lossy(),
            rest.to_string_lossy()
        )),
        b"",
    )?;
    Ok(())
}

/// keeps `path` from being removed while the result lives, as a program holding it open does:
/// windows refuses to delete a file opened without sharing deletion.
#[cfg(windows)]
fn hold(path: &Path) -> Result<Option<fs::File>> {
    use std::os::windows::fs::OpenOptionsExt;
    const FILE_SHARE_READ: u32 = 1;
    Ok(Some(
        fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)?,
    ))
}

/// a folder nobody may write to while it lives.
#[cfg(unix)]
struct Unwritable(std::path::PathBuf);

#[cfg(unix)]
impl Drop for Unwritable {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

/// keeps `path` from being removed while the result lives: unix refuses to remove a file from a
/// folder nobody may write to. none for root, who may remove it anyway.
#[cfg(unix)]
fn hold(path: &Path) -> Result<Option<Unwritable>> {
    use std::os::unix::fs::PermissionsExt;
    let folder = path
        .parent()
        .ok_or("a file sits in a folder")?
        .to_path_buf();
    fs::set_permissions(&folder, fs::Permissions::from_mode(0o555))?;
    let held = Unwritable(folder.clone());
    let probe = folder.join("probe");
    if fs::write(&probe, b"").is_ok() {
        let _ = fs::remove_file(probe);
        skipped("unprivileged", "root ignores file permissions")?;
        return Ok(None);
    }
    Ok(Some(held))
}

/// the mark is as `cache verify` leaves one awaiting repair, on bytes that changed under the same
/// size and time, so sync hashes the object before placing from it and downloads instead.
#[test]
fn a_marked_object_still_in_the_cache_is_never_placed_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let object = iris_object(&sandbox, &other)?;
    mark(&sandbox, &object)?;

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "the changed object is fetched again"
    );
    assert_eq!(
        fs::read(&object)?,
        V1,
        "the cache holds the locked bytes again"
    );
    Ok(())
}

/// a project that copies its files downloads the locked bytes again.
#[test]
fn a_copy_is_never_made_from_a_marked_object() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    mark(&sandbox, &iris_object(&sandbox, &other)?)?;
    let copier = sandbox.folder("copier")?;
    fs::write(
        copier.join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    fs::copy(sandbox.work().join("data.lock"), copier.join("data.lock"))?;

    let synced = sandbox.command_in(&copier, &["sync"]).output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(copier.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    assert_eq!(
        server.requests().len(),
        2,
        "the changed object is fetched again"
    );
    Ok(())
}

/// sync names the object another program holds open, and puts nothing back.
#[test]
fn a_marked_object_that_cannot_be_replaced_stops_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let object = iris_object(&sandbox, &other)?;
    mark(&sandbox, &object)?;
    let Some(held) = hold(&object)? else {
        return Ok(());
    };

    let synced = sandbox.command_in(&other, &["sync"]).output()?;

    drop(held);
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    let name = object
        .file_name()
        .ok_or("an object has a name")?
        .to_string_lossy()
        .into_owned();
    assert!(stderr.contains(&name), "{stderr}");
    assert!(!stderr.contains("put back"), "{stderr}");
    Ok(())
}

#[cfg(windows)]
#[test]
fn an_unreadable_marked_link_stops_sync_without_replacing_it() -> Result {
    use std::os::windows::fs::OpenOptionsExt;

    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let state_path = sandbox.work().join(".fetchloom").join("state");
    let state = fs::read(&state_path)?;
    sandbox.wait_for_a_later_time(fs::metadata(&state_path)?.modified()?)?;
    mark(&sandbox, &iris_object(&sandbox, &sandbox.work())?)?;
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&placed)?;

    let synced = sandbox.run(&["sync"])?;
    let status = sandbox.run(&["status"])?;

    drop(held);
    assert_eq!(fs::read(&placed)?, V1, "the unreadable file must stay");
    assert_eq!(fs::read(&state_path)?, state, "the state must stay");
    for output in [synced, status] {
        let stderr = text(&output.stderr)?;
        assert_eq!(output.status.code(), Some(50), "{stderr}");
        assert!(
            stderr.contains("could not read data/iris/iris.csv to check it"),
            "missing 'could not read data/iris/iris.csv to check it': {stderr}"
        );
    }
    Ok(())
}

/// a link placed before gc freed its object, which a download then brought back, is not the
/// marked copy, so sync hashes it once and keeps it, offline.
#[test]
fn a_link_that_still_holds_the_locked_bytes_is_kept_after_a_mark() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    let added = sandbox
        .command(&["add", &server.url])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let other = sandbox.folder("other")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }
    let synced = sandbox
        .command_in(&other, &["sync"])
        .env("OneDrive", &other)
        .output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    link_iris(&sandbox, &other)?;
    // gc frees an object no registered project needs, and the link keeps its bytes
    let object = iris_object(&sandbox, &other)?;
    delete(&object)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    delete(&placed)?;
    let again = sandbox
        .command(&["sync"])
        .env("OneDrive", sandbox.work())
        .output()?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    edit(&placed, &edited)?;
    keep_time(&sandbox.work())?;
    mark(&sandbox, &object)?;

    let synced = sandbox
        .command_in(&other, &["--offline", "sync"])
        .output()?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(other.join("data").join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    let status = sandbox.command_in(&other, &["status"]).output()?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// get hashes a marked object before taking it, as sync does, and downloads the locked bytes.
#[test]
fn get_never_takes_a_marked_object() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(other) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    mark(&sandbox, &iris_object(&sandbox, &other)?)?;
    let folder = sandbox.folder("got")?;

    let got = sandbox.run(&["get", &server.url, "--dir", &folder.display().to_string()])?;

    let stderr = text(&got.stderr)?;
    assert_eq!(got.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(folder.join("iris.csv"))?, V1, "{stderr}");
    Ok(())
}

#[test]
fn a_new_project_reseals_an_object_made_writable_through_a_link() -> Result {
    let sandbox = Sandbox::new()?;
    if !sandbox.links()?.hardlink {
        return skipped("hardlinks", "this disk makes no hardlinks");
    }
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    link_iris(&sandbox, &sandbox.work())?;
    let first = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut permissions = fs::metadata(&first)?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test models a user clearing a hardlink's flag"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(&first, permissions)?;
    let other = sandbox.folder("second-project")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), other.join(name))?;
    }

    let synced = sandbox
        .command_in(&other, &["sync", "--offline"])
        .output()?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let placed = other.join("data").join("iris").join("iris.csv");
    assert!(
        fs::metadata(&placed)?.permissions().readonly(),
        "the second project's input must be read-only"
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert!(
        fs::metadata(iris_object(&sandbox, &other)?)?
            .permissions()
            .readonly()
    );
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

#[test]
fn a_later_get_conflict_preserves_earlier_files() -> Result {
    let sandbox = Sandbox::new()?;
    let original = Server::start("/a.csv", vec![crate::support::file(b"old-a", "\"old\"")])?;
    let fetched = sandbox.run(&["get", &original.url, "--dir", "out"])?;
    assert!(fetched.status.success(), "{}", text(&fetched.stderr)?);
    fs::write(sandbox.work().join("out/b.csv"), b"foreign")?;
    let bytes = zip_of(&[("a.csv", b"new-a"), ("b.csv", b"new-b")])?;
    let archive = Server::start("/files.zip", vec![crate::support::file(&bytes, "\"new\"")])?;
    let output = sandbox.run(&["get", &archive.url, "--dir", "out", "--json"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stderr)?);
    let document = crate::support::json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "local.not_empty");
    assert_eq!(fs::read(sandbox.work().join("out/b.csv"))?, b"foreign");
    assert_eq!(
        fs::read(sandbox.work().join("out/a.csv")).ok().as_deref(),
        Some(b"old-a".as_slice()),
        "{document}"
    );
    Ok(())
}

/// status calls the link blocked rather than missing, and a plain sync warns and leaves it.
#[cfg(unix)]
#[test]
fn force_replaces_a_link_at_a_pinned_path_without_following_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let precious = sandbox.folder("important")?.join("thesis.txt");
    fs::write(&precious, b"precious")?;
    delete(&placed)?;
    if std::os::unix::fs::symlink(&precious, &placed).is_err() {
        return skipped("symlinks", "no symbolic link can be made here");
    }

    let status = sandbox.run(&["--json", "status"])?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["blocked"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    assert_eq!(
        report["result"]["missing"],
        serde_json::json!([]),
        "{report}"
    );
    let kept = sandbox.run(&["sync"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv is a link where data.lock places a file"),
        "{stderr}"
    );
    assert!(fs::symlink_metadata(&placed)?.file_type().is_symlink());
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert!(
        fs::symlink_metadata(&placed)?.file_type().is_file(),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, V1);
    assert_eq!(fs::read(&precious)?, b"precious");
    let status = sandbox.run(&["status"])?;
    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// status names the file as blocking rather than each of the dataset's files as missing, and a
/// plain sync warns and leaves it.
#[test]
fn force_replaces_a_file_where_a_dataset_folder_belongs() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data").join("iris");
    crate::support::delete_folder(&folder)?;
    fs::write(&folder, b"notes\n")?;

    let status = sandbox.run(&["--json", "status"])?;
    let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
    assert_eq!(
        report["result"]["blocked"],
        serde_json::json!(["data/iris"]),
        "{report}"
    );
    assert_eq!(
        report["result"]["missing"],
        serde_json::json!([]),
        "{report}"
    );
    assert_eq!(report["result"]["extra"], serde_json::json!([]), "{report}");
    let kept = sandbox.run(&["sync"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris is a file where data.lock needs a folder"),
        "{stderr}"
    );
    assert_eq!(fs::read(&folder)?, b"notes\n");
    let forced = sandbox.run(&["sync", "--force"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(folder.join("iris.csv"))?, V1, "{stderr}");
    // the file moved out of the way goes once the files in its stead are in place
    let after = sandbox.run(&["status"])?;
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// a project whose dataset d places a folder `data/d/sub` where a person's file stood, after a
/// `sync --force` moved the file aside and failed once a folder took its name: the forced run's
/// json and the hidden name, in the project, the file is kept under.
#[cfg(unix)]
fn kept_aside(sandbox: &Sandbox) -> Result<(serde_json::Value, String)> {
    use std::os::unix::fs::PermissionsExt;

    let bytes = zip_of(&[
        ("top.csv", b"top\n"),
        ("sub/a.csv", b"first\n"),
        ("sub/b.csv", b"second\n"),
    ])?;
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.d]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let folder = sandbox.work().join("data").join("d");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("sub"), b"my notes\n")?;
    let blocked = sandbox.run(&["sync"])?;
    assert_eq!(blocked.status.code(), Some(0), "{}", text(&blocked.stderr)?);
    // a copy of b.csv reads bytes the cache now refuses, after a.csv made the folder
    let object = crate::support::object(&sandbox.cache(), b"second\n");
    fs::set_permissions(&object, fs::Permissions::from_mode(0o000))?;
    let forced = sandbox.run(&["--json", "sync", "--force", "--offline"]);
    fs::set_permissions(&object, fs::Permissions::from_mode(0o444))?;
    let forced = forced?;
    let document = json(&forced)?;
    assert_eq!(forced.status.code(), Some(50), "{document}");
    let hidden: Vec<String> = fs::read_dir(&folder)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|name| {
            name.starts_with(".fetchloom-")
                && Path::new(name)
                    .extension()
                    .is_some_and(|extension| extension == "old")
        })
        .collect();
    let [hidden] = hidden.as_slice() else {
        return Err(format!("not one file moved aside: {hidden:?}").into());
    };
    assert_eq!(fs::read(folder.join(hidden))?, b"my notes\n");
    Ok((document, format!("data/d/{hidden}")))
}

/// a person's file `--force` moved out of the way cannot go back once a folder took its name, so
/// the run, and each sync and status after it, say where it is until the person moves it.
#[cfg(unix)]
#[test]
fn a_file_force_could_not_put_back_is_named_until_it_is_moved() -> Result {
    use crate::support::warning;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !files_refuse_reads(&sandbox)? {
        return skipped("permissions", "this disk keeps no file from being read");
    }
    let (document, hidden) = kept_aside(&sandbox)?;
    let told = |document: &serde_json::Value| {
        let found = warning(document, "place.set_aside");
        assert_eq!(found["details"], serde_json::json!([hidden]), "{document}");
        assert!(
            found["message"]
                .as_str()
                .is_some_and(|message| message.contains("data/d/sub")),
            "{document}"
        );
    };
    told(&document);

    let again = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&again)?;
    assert_eq!(again.status.code(), Some(0), "{document}");
    told(&document);
    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        status.contains(&format!("{hidden} · data/d/sub, ")),
        "{status}"
    );
    // the sync before settled the project, which a file kept aside keeps from settling
    let settled = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&settled)?;
    assert_eq!(settled.status.code(), Some(0), "{document}");
    told(&document);
    fs::rename(
        sandbox.work().join(&hidden),
        sandbox.work().join("my notes"),
    )?;
    let moved = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&moved)?;
    assert_eq!(moved.status.code(), Some(0), "{document}");
    assert_eq!(
        warning(&document, "place.set_aside"),
        &serde_json::Value::Null,
        "{document}"
    );
    Ok(())
}

/// `remove` of the dataset a person's file was kept aside in says where the file is, as each sync
/// and status do, rather than only naming the hidden name in a folder that stays.
#[cfg(unix)]
#[test]
fn remove_names_a_file_force_kept_aside() -> Result {
    use crate::support::warning;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !files_refuse_reads(&sandbox)? {
        return skipped("permissions", "this disk keeps no file from being read");
    }
    let (_, hidden) = kept_aside(&sandbox)?;

    let removed = sandbox.run(&["--json", "remove", "d"])?;

    let document = json(&removed)?;
    assert_eq!(removed.status.code(), Some(0), "{document}");
    let found = warning(&document, "place.set_aside");
    assert_eq!(found["details"], serde_json::json!([hidden]), "{document}");
    assert!(
        found["message"]
            .as_str()
            .is_some_and(|message| message.contains("data/d/sub")),
        "{document}"
    );
    Ok(())
}

/// `command` run until the hold `variable` names stops it, then killed, as a second ctrl-c, a
/// kill or a crash ends a run at once.
fn cut_off_at(sandbox: &Sandbox, args: &[&str], variable: &str, what: &str) -> Result {
    use crate::support::{Running, wait_until};
    let release = sandbox.folder("hold")?.join("release");
    let held = sandbox.folder("hold")?.join("release.held");
    let mut command = sandbox.command(args);
    command.env(variable, &release);
    let mut running = Running::start(&mut command)?;
    wait_until(what, || Ok(held.exists()))?;
    running.kill()?;
    running.wait()?;
    fs::remove_file(held)?;
    Ok(())
}

/// a file `--force` could not put back stays named by later runs though the run that kept it could
/// not write the state at its end, as on a full disk: its name was written before it moved.
#[cfg(unix)]
#[test]
fn a_file_force_kept_aside_stays_named_when_the_last_state_write_fails() -> Result {
    use std::os::unix::fs::PermissionsExt;

    use crate::support::{Running, wait_until, warning};
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !files_refuse_reads(&sandbox)? {
        return skipped("permissions", "this disk keeps no file from being read");
    }
    if !folders_refuse_files(&sandbox)? {
        return skipped("permissions", "this disk keeps no folder from new files");
    }
    let bytes = zip_of(&[
        ("top.csv", b"top\n"),
        ("sub/a.csv", b"first\n"),
        ("sub/b.csv", b"second\n"),
    ])?;
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.d]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let folder = sandbox.work().join("data").join("d");
    fs::create_dir_all(&folder)?;
    fs::write(folder.join("sub"), b"my notes\n")?;
    let blocked = sandbox.run(&["sync"])?;
    assert_eq!(blocked.status.code(), Some(0), "{}", text(&blocked.stderr)?);
    // b.csv cannot be placed, so sub cannot go back once a.csv made the folder
    let object = crate::support::object(&sandbox.cache(), b"second\n");
    fs::set_permissions(&object, fs::Permissions::from_mode(0o000))?;
    let own = sandbox.work().join(".fetchloom");
    let release = sandbox.folder("hold")?.join("release");
    let held = sandbox.folder("hold")?.join("release.held");
    let mut forced = sandbox.command(&["sync", "--force", "--offline"]);
    forced.env("FETCHLOOM_TEST_HOLD_ASIDE", &release);
    let running = Running::start(&mut forced)?;
    wait_until("sub to be moved aside", || Ok(held.exists()))?;
    fs::set_permissions(&own, fs::Permissions::from_mode(0o555))?;
    fs::write(&release, b"")?;
    let output = running.finish();
    fs::set_permissions(&own, fs::Permissions::from_mode(0o755))?;
    fs::set_permissions(&object, fs::Permissions::from_mode(0o444))?;
    let output = output?;
    assert_ne!(output.status.code(), Some(0), "{}", text(&output.stderr)?);

    let again = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&again)?;
    let found = warning(&document, "place.set_aside");
    assert!(
        found["message"]
            .as_str()
            .is_some_and(|message| message.contains("data/d/sub")),
        "{document}"
    );
    Ok(())
}

/// a read-only file of a person's own that `sync --force` replaces goes once the pinned file is
/// in place, though windows refuses to remove a read-only file, rather than staying hidden.
#[cfg(windows)]
#[test]
fn force_discards_a_read_only_file_it_replaced() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"my own notes\n")?;
    let mut sealed = fs::metadata(&placed)?.permissions();
    sealed.set_readonly(true);
    fs::set_permissions(&placed, sealed)?;

    let forced = sandbox.run(&["sync", "--force", "--offline"])?;

    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    let folder = placed.parent().ok_or("a file has a folder")?;
    let hidden: Vec<String> = fs::read_dir(folder)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|name| name.starts_with(".fetchloom-"))
        .collect();
    assert!(hidden.is_empty(), "{hidden:?} {stderr}");
    assert!(
        !stderr.contains("place.set_aside") && !stderr.contains("kept as"),
        "{stderr}"
    );
    Ok(())
}

/// windows gives each copied file a short 8.3 name, which can be the very name of another file of
/// the dataset, as `abcdefghij.csv` gets `abcdef~1.csv`: both are placed, the short-named first.
#[cfg(windows)]
#[test]
fn a_name_another_files_short_name_would_take_is_placed() -> Result {
    let sandbox = Sandbox::new()?;
    let probe = sandbox.folder(".short")?;
    fs::write(probe.join("abcdefghij.csv"), b"probe")?;
    let shortens = probe.join("abcdef~1.csv").exists();
    fs::remove_file(probe.join("abcdefghij.csv"))?;
    fs::remove_dir(&probe)?;
    if !shortens {
        return skipped("short-names", "this disk makes no short 8.3 names");
    }
    let bytes = zip_of(&[
        ("top/abcdefghij.csv", b"long\n"),
        ("top/abcdef~1.csv", b"short\n"),
    ])?;
    let server = Server::start("/short.zip", vec![file(&bytes, "\"s\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.short]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;

    let synced = sandbox.run(&["sync"])?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let folder = sandbox.work().join("data").join("short");
    assert_eq!(
        fs::read(folder.join("abcdefghij.csv"))?,
        b"long\n",
        "{stderr}"
    );
    assert_eq!(
        fs::read(folder.join("abcdef~1.csv"))?,
        b"short\n",
        "{stderr}"
    );
    Ok(())
}

/// a `sync --force` cut off at once after it moved a person's edit aside leaves the edit's name on
/// record: status names it, and the next sync puts it back and blames no one.
#[test]
fn a_force_cut_off_at_once_loses_no_name_it_moved_aside() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    edit(&placed, b"my own notes\n")?;
    cut_off_at(
        &sandbox,
        &["sync", "--force", "--offline"],
        "FETCHLOOM_TEST_HOLD_ASIDE",
        "the edit to be moved aside",
    )?;
    assert!(!placed.exists(), "the edit is under a hidden name");

    let status = text(&sandbox.run(&["status"])?.stdout)?;
    assert!(
        status.contains(
            ".old · data/iris/iris.csv, which a `sync --force` cut off before its end moved out of the way: `fetchloom sync` moves it back"
        ),
        "{status}"
    );
    let synced = sandbox.run(&["sync", "--offline"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, b"my own notes\n", "{stderr}");
    assert!(
        stderr.contains("Moved back data/iris/iris.csv · a `sync --force` cut off"),
        "{stderr}"
    );
    assert!(!stderr.contains("deleted or moved"), "{stderr}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["extra"], serde_json::json!([]), "{status}");
    assert_eq!(
        status["result"]["edited"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{status}"
    );
    Ok(())
}

/// a sync cut off at once after it removed a file it placed for an older pin, before the new one
/// is in place, leaves no later run saying someone deleted or moved it.
#[test]
fn a_replacement_cut_off_after_the_old_file_went_blames_no_one() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V2, "\"v2\"")],
    )?;
    copy_iris(&sandbox, &server)?;
    let upgraded = sandbox.run(&["lock", "--upgrade", "iris"])?;
    let stderr = text(&upgraded.stderr)?;
    assert_eq!(upgraded.status.code(), Some(0), "{stderr}");
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    cut_off_at(
        &sandbox,
        &["sync", "--offline"],
        "FETCHLOOM_TEST_HOLD_REPLACE",
        "the old file to be removed",
    )?;
    assert!(
        !placed.exists(),
        "the old file went before the new one came"
    );

    let synced = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    assert_eq!(
        document["result"]["placement"]["put_back"],
        serde_json::json!([]),
        "{document}"
    );
    assert_eq!(fs::read(&placed)?, V2, "{document}");
    Ok(())
}

/// an edit that kept its file's size and time, found by hashing it, stays an edit when a
/// `sync --force` that moved it aside is cut off at once: the state written before the move does
/// not vouch for the size and time the edit kept.
#[test]
fn a_force_cut_off_at_once_keeps_an_edit_that_kept_its_look() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let time = fs::metadata(&placed)?.modified()?;
    let edited = b"sepal_length,species\n5.1,SETOSA\n";
    assert_eq!(edited.len(), V1.len());
    fs::write(&placed, edited)?;
    fs::File::options()
        .write(true)
        .open(&placed)?
        .set_modified(time)?;
    // a state as new as the file, as on a clock as coarse as fat's, cannot vouch for its look
    fs::File::options()
        .write(true)
        .open(sandbox.work().join(".fetchloom").join("state"))?
        .set_modified(time)?;
    cut_off_at(
        &sandbox,
        &["sync", "--force", "--offline"],
        "FETCHLOOM_TEST_HOLD_ASIDE",
        "the edit to be moved aside",
    )?;
    assert!(!placed.exists(), "the edit is under a hidden name");

    let synced = sandbox.run(&["sync", "--offline"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, edited, "{stderr}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        status["result"]["edited"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{status}"
    );
    Ok(())
}

/// on a disk that ignores case, a person's file named as a folder data.lock places in another case
/// is in that folder's way: sync keeps it and names it, and `--force` clears it, as for any file
/// where a folder belongs. a disk that keeps the names apart has room for both.
#[test]
fn a_file_named_as_a_needed_folder_in_another_case_is_in_its_way() -> Result {
    let sandbox = Sandbox::new()?;
    let aliases = names_alias(&sandbox, "iris.csv", "Iris.csv")?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\n", server.url),
    )?;
    let data = sandbox.work().join("data");
    fs::create_dir_all(&data)?;
    fs::write(data.join("Iris"), b"my notes\n")?;

    let kept = sandbox.run(&["sync"])?;
    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    if !aliases {
        assert_eq!(
            fs::read(data.join("iris").join("iris.csv"))?,
            V1,
            "{stderr}"
        );
        assert_eq!(fs::read(data.join("Iris"))?, b"my notes\n", "{stderr}");
        return Ok(());
    }
    assert!(
        stderr.contains("data/Iris is a file where data.lock needs a folder"),
        "{stderr}"
    );
    assert_eq!(fs::read(data.join("Iris"))?, b"my notes\n", "{stderr}");
    let forced = sandbox.run(&["sync", "--force", "--offline"])?;
    let stderr = text(&forced.stderr)?;
    assert_eq!(forced.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read(data.join("iris").join("iris.csv"))?,
        V1,
        "{stderr}"
    );
    Ok(())
}

/// a folder whose path data.lock now gives a file goes with its files, though finder left a
/// `.DS_Store` in it, which goes with it, so the file takes its place in the same run.
#[test]
fn a_folder_holding_only_system_files_frees_the_path_it_held() -> Result {
    let sandbox = Sandbox::new()?;
    let before = zip_of(&[("top.csv", b"top\n"), ("x/a.csv", b"first\n")])?;
    let after = zip_of(&[("top.csv", b"top\n"), ("x", b"a file now\n")])?;
    let server = Server::start(
        "/files.zip",
        vec![file(&before, "\"v1\""), file(&after, "\"v2\"")],
    )?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.d]\nref = \"{}\"\n", server.url),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let x = sandbox.work().join("data").join("d").join("x");
    fs::write(x.join(".DS_Store"), b"finder")?;
    let upgraded = sandbox.run(&["lock", "--upgrade", "d"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );

    let output = sandbox.run(&["sync", "--offline"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        !without_copy_warning(&stderr).contains("warning"),
        "{stderr}"
    );
    assert_eq!(fs::read(&x)?, b"a file now\n", "{stderr}");
    Ok(())
}

/// a folder where data.lock places a file that holds nothing but what finder or explorer made is
/// an empty folder to a person: status calls it one, and `sync --force` replaces it, those files
/// with it, as it replaces an empty folder. one holding a file of the person's stays.
#[test]
fn a_folder_of_system_files_alone_in_a_files_way_is_empty() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("petals.csv"), b"length\n1.4\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox
        .work()
        .join("data")
        .join("flowers")
        .join("petals.csv");
    delete(&placed)?;
    fs::create_dir(&placed)?;
    fs::write(placed.join(".DS_Store"), b"finder")?;
    fs::write(placed.join("Thumbs.db"), b"explorer")?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(
        text(&output.stdout)?,
        "    Blocked data/flowers/petals.csv · an empty folder is where a file belongs\n"
    );
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("help: `fetchloom sync --force` replaces it"),
        "{stderr}"
    );
    let output = sandbox.run(&["sync", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&placed)?, b"length\n1.4\n");
    // a file of the person's beside them keeps the folder, even with --force
    delete(&placed)?;
    fs::create_dir(&placed)?;
    fs::write(placed.join(".DS_Store"), b"finder")?;
    fs::write(placed.join("notes.txt"), b"mine")?;
    let output = sandbox.run(&["sync", "--force"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(placed.join("notes.txt"))?, b"mine");
    Ok(())
}

#[test]
fn status_names_a_file_in_a_folders_way_once() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("petals.csv"), b"length\n1.4\n")?;
    fs::write(flowers.join("sepals.csv"), b"length\n5.1\n")?;
    let added = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let folder = sandbox.work().join("data").join("flowers");
    delete_folder(&folder)?;
    fs::write(&folder, b"notes\n")?;
    let report = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        report["result"]["blocked"],
        serde_json::json!(["data/flowers"]),
        "{report}"
    );
    Ok(())
}

/// a project that placed iris.csv, its data.lock, and that lock as it is and with iris.csv made a
/// folder holding the same file.
fn reshaped(sandbox: &Sandbox, server: &Server) -> Result<(std::path::PathBuf, String, String)> {
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let lock = sandbox.work().join("data.lock");
    let as_file = fs::read_to_string(&lock)?;
    let as_folder = as_file.replace("path = \"iris.csv\"", "path = \"iris.csv/iris.csv\"");
    assert_ne!(as_folder, as_file);
    Ok((lock, as_file, as_folder))
}

/// the sync clears what was placed in the way first, rather than leave the pin blocked.
#[test]
fn a_file_that_becomes_a_folder_and_back_is_placed_in_one_sync() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (lock, as_file, as_folder) = reshaped(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    for (pins, file) in [(&as_folder, placed.join("iris.csv")), (&as_file, placed)] {
        fs::write(&lock, pins)?;
        let output = sandbox.run(&["--json", "sync"])?;
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(output.status.code(), Some(0), "{document}");
        let placement = &document["result"]["placement"];
        assert_eq!(placement["kept"], serde_json::json!([]), "{document}");
        assert_eq!(
            (&placement["placed"], &placement["removed"]),
            (&serde_json::json!(1), &serde_json::json!(1)),
            "{document}"
        );
        assert_eq!(fs::read(&file)?, V1, "{document}");
        let status = sandbox.run(&["status"])?;
        assert_eq!(
            text(&status.stderr)?,
            "         Ok everything matches data.lock by size and time\n"
        );
    }
    assert_eq!(server.requests().len(), 1);
    Ok(())
}

/// sync removes an unneeded placed file, or a folder of only such files, before it places
/// anything, so status calls the file behind it missing rather than blocked.
#[test]
fn status_lists_what_sync_removes_as_unneeded_rather_than_blocking() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let (lock, as_file, as_folder) = reshaped(&sandbox, &server)?;
    for (pins, missing, unneeded) in [
        (
            &as_folder,
            "data/iris/iris.csv/iris.csv",
            "data/iris/iris.csv",
        ),
        (
            &as_file,
            "data/iris/iris.csv",
            "data/iris/iris.csv/iris.csv",
        ),
    ] {
        fs::write(&lock, pins)?;
        let status = sandbox.run(&["--json", "status"])?;
        let report: serde_json::Value = serde_json::from_slice(&status.stdout)?;
        assert_eq!(
            report["result"]["blocked"],
            serde_json::json!([]),
            "{report}"
        );
        assert_eq!(
            report["result"]["missing"],
            serde_json::json!([missing]),
            "{report}"
        );
        assert_eq!(
            report["result"]["unneeded"],
            serde_json::json!([unneeded]),
            "{report}"
        );
        assert_eq!(report["result"]["extra"], serde_json::json!([]), "{report}");
        // --force places it either way, so the next round starts from it in place
        let forced = sandbox.run(&["sync", "--force"])?;
        assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    }
    Ok(())
}

/// a failed add leaves nothing of the dataset in the data folder or the state.
#[cfg(unix)]
#[test]
fn a_rolled_back_add_takes_the_files_it_placed_with_it() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !folders_refuse_files(&sandbox)? {
        return skipped("permissions", "this disk keeps no folder from new files");
    }
    let bytes = zip_of(&[("a.csv", b"first"), ("sub/b.csv", b"other")])?;
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    // a folder the dataset needs that refuses new files, so its second file cannot be placed
    let refusing = sandbox.work().join("data").join("rolled").join("sub");
    fs::create_dir_all(&refusing)?;
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o555))?;

    let added = sandbox.run(&["add", &server.url, "--as", "rolled"]);
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o755))?;
    let added = added?;

    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("restored data.toml and data.lock"),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists(), "{stderr}");
    assert!(
        !sandbox.work().join("data/rolled/a.csv").exists(),
        "{stderr}"
    );
    let state = sandbox.work().join(".fetchloom").join("state");
    if state.exists() {
        let state: serde_json::Value = serde_json::from_slice(&fs::read(&state)?)?;
        assert_eq!(state["files"], serde_json::json!({}), "{state}");
    }
    Ok(())
}

/// an add rolled back before any of its files landed still takes back the .gitignore it wrote in
/// the dataset's folder first, so the folder is as the add found it.
#[cfg(unix)]
#[test]
fn a_rolled_back_add_that_placed_nothing_takes_its_marker_back() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !folders_refuse_files(&sandbox)? {
        return skipped("permissions", "this disk keeps no folder from new files");
    }
    // two top folders, so neither is cut as the one all the archive's files share
    let bytes = zip_of(&[("sub/b.csv", b"other"), ("two/c.csv", b"more")])?;
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    let dataset = sandbox.work().join("data").join("rolled");
    let refusing = [dataset.join("sub"), dataset.join("two")];
    for folder in &refusing {
        fs::create_dir_all(folder)?;
        fs::set_permissions(folder, fs::Permissions::from_mode(0o555))?;
    }

    let added = sandbox.run(&["add", &server.url, "--as", "rolled"]);
    for folder in &refusing {
        fs::set_permissions(folder, fs::Permissions::from_mode(0o755))?;
    }
    let added = added?;

    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("restored data.toml and data.lock"),
        "{stderr}"
    );
    assert!(!sandbox.work().join("data.toml").exists(), "{stderr}");
    assert!(!dataset.join(".gitignore").exists(), "{stderr}");
    Ok(())
}

/// sync removed the earlier file itself, so once a failed replacement can go ahead, the next sync
/// places the pin without saying someone deleted or moved the file.
#[cfg(unix)]
#[test]
fn a_failed_replacement_is_placed_later_without_blaming_anyone() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !files_refuse_reads(&sandbox)? {
        return skipped("permissions", "this disk keeps no file from being read");
    }
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V2, "\"v2\"")],
    )?;
    copy_iris(&sandbox, &server)?;
    let upgraded = sandbox.run(&["lock", "--upgrade", "iris"])?;
    assert_eq!(
        upgraded.status.code(),
        Some(0),
        "{}",
        text(&upgraded.stderr)?
    );
    // a copy reads the new bytes, which the cache now refuses
    let object = crate::support::object(&sandbox.cache(), V2);
    fs::set_permissions(&object, fs::Permissions::from_mode(0o000))?;
    let failed = sandbox.run(&["sync", "--offline"]);
    fs::set_permissions(&object, fs::Permissions::from_mode(0o444))?;
    let failed = failed?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    assert_eq!(failed.status.code(), Some(50), "{}", text(&failed.stderr)?);
    assert!(!placed.exists(), "{}", text(&failed.stderr)?);

    let synced = sandbox.run(&["--json", "sync", "--offline"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    let placement = &document["result"]["placement"];
    assert_eq!(placement["put_back"], serde_json::json!([]), "{document}");
    assert_eq!(placement["placed"], 1, "{document}");
    assert_eq!(fs::read(&placed)?, V2, "{document}");
    Ok(())
}

/// a file sync cannot take away for its pin, as one another program holds open on windows, stays as
/// it was, and the other files it replaces still get theirs rather than being left deleted.
#[cfg(unix)]
#[test]
fn a_file_that_cannot_make_way_leaves_the_other_replacements_placed() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    // copy = true replaces a placed hardlink, while a clone or a copy only turns writable
    if !sandbox.links()?.hardlinks_read_only() {
        return skipped(
            "placed-hardlinks",
            "this disk places no read-only file as a hardlink",
        );
    }
    if !folders_refuse_files(&sandbox)? {
        return skipped("permissions", "this disk keeps no folder from new files");
    }
    let bytes = zip_of(&[
        ("README.txt", b"read me"),
        ("nested/deep/c.bin", b"deep"),
        ("tables/t.csv", b"a,b\n"),
    ])?;
    let server = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!("[datasets.held]\nref = \"{}\"\n", server.url),
    )?;
    let placed = sandbox.run(&["sync"])?;
    assert_eq!(placed.status.code(), Some(0), "{}", text(&placed.stderr)?);
    fs::write(
        &manifest,
        format!("[datasets.held]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    // the file that sorts last cannot be removed, so the others were taken away before it failed
    let dataset = sandbox.work().join("data").join("held");
    let refusing = dataset.join("tables");
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o555))?;

    let synced = sandbox.run(&["sync"]);
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o755))?;
    let synced = synced?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(50), "{stderr}");
    assert!(stderr.contains("t.csv"), "{stderr}");
    for (path, bytes) in [
        ("README.txt", &b"read me"[..]),
        ("nested/deep/c.bin", &b"deep"[..]),
    ] {
        let copy = dataset.join(path);
        assert_eq!(
            fs::read(&copy).ok().as_deref(),
            Some(bytes),
            "{path}: {stderr}"
        );
        assert!(
            !fs::metadata(&copy)?.permissions().readonly(),
            "{path}: {stderr}"
        );
    }
    assert_eq!(fs::read(refusing.join("t.csv"))?, b"a,b\n", "{stderr}");
    let again = sandbox.run(&["sync"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    assert!(
        !fs::metadata(refusing.join("t.csv"))?
            .permissions()
            .readonly()
    );
    Ok(())
}

/// the list is rolled back whole, so the files placed for each of its datasets go too.
#[cfg(unix)]
#[test]
fn a_rolled_back_list_takes_the_files_it_placed_with_it() -> Result {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return skipped("unprivileged", "root ignores file permissions");
    }
    let sandbox = Sandbox::new()?;
    if !folders_refuse_files(&sandbox)? {
        return skipped("permissions", "this disk keeps no folder from new files");
    }
    let bytes = zip_of(&[("a.csv", b"first"), ("sub/b.csv", b"other")])?;
    let archive = Server::start("/files.zip", vec![file(&bytes, "\"v1\"")])?;
    let plain = Server::start("/files/c.csv", vec![file(b"c\n", "\"c1\"")])?;

    // a folder the second dataset needs that refuses new files, so its second file cannot be placed
    let refusing = sandbox.work().join("data").join("files").join("sub");
    fs::create_dir_all(&refusing)?;
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o555))?;

    let added = sandbox.run(&["add", &plain.url, &archive.url]);
    fs::set_permissions(&refusing, fs::Permissions::from_mode(0o755))?;
    let added = added?;

    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(50), "{stderr}");
    assert!(!sandbox.work().join("data.toml").exists(), "{stderr}");
    for placed in ["data/c/c.csv", "data/files/a.csv"] {
        assert!(!sandbox.work().join(placed).exists(), "{placed}: {stderr}");
    }
    Ok(())
}

/// without the count, a sync that downloads nothing reads as one that found nothing in the cache.
/// `-v` says it in words.
#[test]
fn the_counters_tell_files_placed_from_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let added = sandbox.run(&["--json", "add", &server.url])?;
    let report: serde_json::Value = serde_json::from_slice(&added.stdout)?;
    assert_eq!(report["work"]["placed_from_cache"], 0, "{report}");
    assert!(report["work"].get("cache_hits").is_none(), "{report}");
    for (name, args) in [
        ("clone", &["--json", "sync"][..]),
        ("other", &["-v", "sync"][..]),
    ] {
        let clone = sandbox.folder(name)?;
        for file in ["data.toml", "data.lock"] {
            fs::copy(sandbox.work().join(file), clone.join(file))?;
        }
        let synced = sandbox.command_in(&clone, args).output()?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        if name == "clone" {
            let report: serde_json::Value = serde_json::from_slice(&synced.stdout)?;
            assert_eq!(report["work"]["placed_from_cache"], 1, "{report}");
            assert_eq!(report["work"]["requests"], 0, "{report}");
        } else {
            let stderr = text(&synced.stderr)?;
            assert!(stderr.contains("1 placed from the cache"), "{stderr}");
        }
    }
    Ok(())
}

/// status hashes only files sizes and times cannot vouch for, so an edit that kept a copy's size
/// and time passes unseen, and the last line must not claim the bytes match.
#[test]
fn status_says_it_compared_sizes_and_times() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    quietly_changed_copy(&sandbox, &server)?;

    let status = sandbox.run(&["status"])?;

    assert_eq!(
        text(&status.stderr)?,
        "         Ok everything matches data.lock by size and time\n"
    );
    Ok(())
}

/// a copy of iris, older than the state, changed in place under the same size and time, as a
/// failing disk or a tool that restores times leaves it.
fn quietly_changed_copy(sandbox: &Sandbox, server: &Server) -> Result<std::path::PathBuf> {
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\ncopy = true\n", server.url),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let state = sandbox.work().join(".fetchloom").join("state");
    sandbox.wait_for_a_later_time(fs::metadata(&state)?.modified()?)?;
    fs::write(&state, fs::read(&state)?)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut edited = V1.to_vec();
    edited[0] = b'S';
    let modified = fs::metadata(&placed)?.modified()?;
    edit(&placed, &edited)?;
    set_time(&placed, modified)?;
    Ok(placed)
}

/// the change shows in no listing, so a plain sync finds nothing to do; `sync --verify` puts the
/// pinned file back without --force.
#[test]
fn verify_finds_and_puts_back_a_change_that_kept_its_size_and_time() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let placed = quietly_changed_copy(&sandbox, &server)?;

    let checked = sandbox.run(&["--json", "status", "--verify"])?;
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );
    let verified = sandbox.run(&["status", "--verify"])?;
    let told = text(&verified.stdout)?;
    assert!(
        told.contains(
            "Damaged data/iris/iris.csv · its bytes changed though its size and time did not"
        ),
        "{told}"
    );
    let hints = text(&verified.stderr)?;
    assert!(hints.contains("`fetchloom sync --verify`"), "{hints}");
    let plain = text(&sandbox.run(&["sync"])?.stderr)?;
    assert_eq!(
        plain,
        "         Ok all pinned files are in place by size and time, nothing to do\n"
    );
    let repaired = sandbox.run(&["sync", "--verify"])?;
    let stderr = text(&repaired.stderr)?;
    assert_eq!(repaired.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert!(
        stderr.contains(
            "data/iris/iris.csv was put back as pinned: its bytes had changed though its size and time had not"
        ),
        "{stderr}"
    );
    let kept = sandbox.run(&["status", "--verify"])?;
    let rows = text(&kept.stdout)?;
    assert!(
        rows.contains(
            "Extra data/iris/iris.csv.changed · a changed copy sync kept of data/iris/iris.csv"
        ),
        "{rows}"
    );
    delete(&placed.with_file_name("iris.csv.changed"))?;
    let after = sandbox.run(&["status", "--verify"])?;
    assert_eq!(
        text(&after.stderr)?,
        "         Ok everything matches data.lock, checked byte for byte\n"
    );
    let again = sandbox.run(&["sync", "--verify"])?;
    assert_eq!(
        text(&again.stderr)?,
        "         Ok all pinned files are in place, checked byte for byte; nothing to do\n"
    );
    assert_eq!(server.requests().len(), 1, "the cache still held iris");
    Ok(())
}

/// a sync stopped once it kept a changed link's copy beside its file says where the copy is,
/// though a run that ends early tells no placement.
#[cfg(unix)]
#[test]
fn a_stopped_sync_says_where_it_kept_a_changed_copy() -> Result {
    use crate::support::{Running, wait_until};
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(_) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    let release = sandbox.folder("hold")?.join("release");
    let held = sandbox.folder("hold")?.join("release.held");
    let mut command = sandbox.command(&["sync"]);
    command.env("FETCHLOOM_TEST_HOLD_KEPT", &release);
    let running = Running::start(&mut command)?;
    wait_until("the changed copy to be kept", || Ok(held.exists()))?;
    running.interrupt()?;
    running.wait_for("Stopping")?;
    fs::write(&release, b"")?;
    let output = running.finish()?;

    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let kept = sandbox
        .work()
        .join("data")
        .join("iris")
        .join("iris.csv.changed");
    assert!(kept.exists(), "{stderr}");
    assert!(
        stderr.contains(
            "the changed copy of data/iris/iris.csv is kept as data/iris/iris.csv.changed"
        ),
        "{stderr}"
    );
    Ok(())
}

/// a fresh clone's `sync --locked --verify` checks the cached bytes it places: an object changed
/// under its size and time, as a cache restored from elsewhere can hold, is fetched again, never
/// placed.
#[test]
fn verify_checks_the_cached_bytes_a_fresh_clone_places() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    sandbox.allow(&server.url);
    fs::write(
        sandbox.work().join("data.toml"),
        format!("[datasets.iris]\nref = \"{}\"\n", server.url),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let object = crate::support::object(&sandbox.cache(), V1);
    let modified = fs::metadata(&object)?.modified()?;
    let mut changed = V1.to_vec();
    changed[0] = b'S';
    edit(&object, &changed)?;
    set_time(&object, modified)?;
    let mut sealed = fs::metadata(&object)?.permissions();
    sealed.set_readonly(true);
    fs::set_permissions(&object, sealed)?;
    let placed = clone.join("data").join("iris").join("iris.csv");

    let offline = sandbox
        .command_in(&clone, &["sync", "--locked", "--verify", "--offline"])
        .output()?;
    let stderr = text(&offline.stderr)?;
    assert_eq!(offline.status.code(), Some(20), "{stderr}");
    assert!(!placed.exists(), "{stderr}");
    assert!(
        stderr
            .contains("the cache's copy of data/iris/iris.csv changed, so it is downloaded again"),
        "{stderr}"
    );
    let verified = sandbox
        .command_in(&clone, &["sync", "--locked", "--verify"])
        .output()?;

    let stderr = text(&verified.stderr)?;
    assert_eq!(verified.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert_eq!(
        server.requests().len(),
        2,
        "the changed copy is fetched again"
    );
    Ok(())
}

/// the bytes of a copy `sync --verify` puts back may be someone's edit that kept its size and
/// time, so they stay beside it, as a changed link's do.
#[test]
fn verify_keeps_the_changed_bytes_of_a_copy_it_puts_back() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let placed = quietly_changed_copy(&sandbox, &server)?;
    let changed = fs::read(&placed)?;
    let planned = sandbox.run(&["sync", "--verify", "--dry-run"])?;
    let stderr = text(&planned.stderr)?;
    assert!(
        stderr.contains("the changed copy would be kept as data/iris/iris.csv.changed"),
        "{stderr}"
    );

    let repaired = sandbox.run(&["sync", "--verify"])?;

    let stderr = text(&repaired.stderr)?;
    assert_eq!(repaired.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    let kept = placed.with_file_name("iris.csv.changed");
    assert_eq!(fs::read(&kept)?, changed, "{stderr}");
    assert!(
        stderr.contains("the changed copy is kept as data/iris/iris.csv.changed"),
        "{stderr}"
    );
    Ok(())
}

/// a copy whose size or time changed is the project's own, so it stays until --force.
#[test]
fn verify_keeps_an_edit_that_shows() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    let placed = quietly_changed_copy(&sandbox, &server)?;
    let mut grown = V1.to_vec();
    grown.extend_from_slice(b"6.3,edited\n");
    edit(&placed, &grown)?;

    let kept = sandbox.run(&["sync", "--verify"])?;

    let stderr = text(&kept.stderr)?;
    assert_eq!(kept.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("data/iris/iris.csv has changed, so it stays"),
        "{stderr}"
    );
    assert_eq!(fs::read(&placed)?, grown);
    Ok(())
}

/// the change keeps the size and time and the file is sealed again, so only hashing sees it;
/// `sync --verify` marks the copy, downloads the file again and puts the link back.
#[test]
fn verify_puts_back_a_link_whose_cached_copy_quietly_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![file(V1, "\"v1\""), file(V1, "\"v1\"")],
    )?;
    let Some(_) = shared_and_edited(&sandbox, &server)? else {
        return Ok(());
    };
    keep_time(&sandbox.work())?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let mut sealed = fs::metadata(&placed)?.permissions();
    sealed.set_readonly(true);
    fs::set_permissions(&placed, sealed)?;
    let checked = sandbox.run(&["--json", "status", "--verify"])?;
    let report: serde_json::Value = serde_json::from_slice(&checked.stdout)?;
    assert_eq!(
        report["result"]["damaged"],
        serde_json::json!(["data/iris/iris.csv"]),
        "{report}"
    );

    let repaired = sandbox.run(&["sync", "--verify"])?;

    let stderr = text(&repaired.stderr)?;
    assert_eq!(repaired.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&placed)?, V1, "{stderr}");
    assert_eq!(
        server.requests().len(),
        2,
        "the changed copy is fetched again"
    );
    Ok(())
}

/// a sync never deletes outside the data folder, so the old one keeps what fetchloom placed, and
/// the sync and its dry run name the person's own files there, so deleting it loses none unseen.
#[test]
fn a_changed_data_dir_leaves_the_old_folder_and_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let work = sandbox.work();
    for notes in [
        "data/iris/my-notes.txt",
        "data/readme.txt",
        "data/scratch/plot.py",
        "data/scratch/todo.md",
    ] {
        fs::create_dir_all(work.join(notes).parent().ok_or("no folder")?)?;
        fs::write(work.join(notes), b"mine\n")?;
    }
    let manifest = work.join("data.toml");
    fs::write(
        &manifest,
        format!(
            "[project]\ndata_dir = \"inputs/raw\"\n\n{}",
            fs::read_to_string(&manifest)?
        ),
    )?;

    let said = "data still holds data/iris/iris.csv, a file fetchloom placed before data_dir changed to inputs/raw, and 4 files it did not place: data/iris/my-notes.txt, data/readme.txt, data/scratch/plot.py and 1 more. fetchloom leaves them all there; move the 4 files fetchloom did not place out of data before you delete data";
    for args in [&["--json", "sync", "--dry-run"][..], &["--json", "sync"]] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let warnings = crate::support::warned(&json(&output)?).join("\n");
        assert!(warnings.contains(said), "{warnings}");
    }
    assert_eq!(
        fs::read(work.join("data").join("iris").join("iris.csv"))?,
        V1
    );
    assert_eq!(fs::read(work.join("data/iris/my-notes.txt"))?, b"mine\n");
    assert_eq!(fs::read(work.join("inputs/raw/iris/iris.csv"))?, V1);
    Ok(())
}

/// both fail with `data.not_ready`, naming the file and what to do, and the strict sync keeps the
/// edit as a plain sync does.
#[test]
fn a_kept_edit_fails_a_strict_sync_and_status_check_until_it_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    let edited = b"sepal_length,species\n999,user-edited\n";
    edit(&placed, edited)?;
    for args in [
        &["--json", "sync", "--offline", "--strict"][..],
        &["--json", "status", "--check"][..],
    ] {
        let output = sandbox.run(args)?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(1), "{args:?}: {document}");
        let error = &document["errors"][0];
        assert_eq!(error["kind"], "data.not_ready", "{args:?}: {document}");
        assert_eq!(
            error["details"],
            serde_json::json!([
                "data/iris/iris.csv changed since fetchloom placed it: save what you need, then `fetchloom sync --force` replaces it"
            ]),
            "{args:?}: {document}"
        );
        assert_eq!(
            error["fix"],
            serde_json::Value::Null,
            "{args:?}: {document}"
        );
        assert!(document["result"].is_object(), "{args:?}: {document}");
        assert_eq!(fs::read(&placed)?, edited, "{args:?}");
    }
    let forced = sandbox.run(&["sync", "--offline", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    for args in [
        &["sync", "--offline", "--strict"][..],
        &["status", "--check"][..],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    assert_eq!(fs::read(&placed)?, V1);
    Ok(())
}

/// copies need room on the data folder's disk, not just the cache's: on exFAT a sync that cannot
/// fit them asks first, naming the disk, places nothing with nobody to ask, and `--yes` goes on.
#[cfg(debug_assertions)]
#[test]
fn copies_the_data_folders_disk_cannot_hold_ask_first() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 2048];
    let server = Server::start("/files/big.bin", vec![file(&body, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let placed = sandbox.work().join("data").join("big").join("big.bin");
    delete(&placed)?;
    let sync = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_FILESYSTEM", "exfat")
            .env("FETCHLOOM_TEST_FREE", "1000")
            .output()
    };
    let refused = sync(&["--json", "sync"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr)?);
    let error = &json(&refused)?["errors"][0];
    assert_eq!(error["kind"], "unconfirmed", "{error}");
    assert_eq!(
        error["message"],
        format!(
            "this sync needs at least 2.0 KiB on the disk holding the data folder at {}, which has 1000 B free",
            sandbox.work().join("data").display()
        )
    );
    assert!(
        error["help"]
            .as_str()
            .is_some_and(|help| help.contains("--yes")),
        "{error}"
    );
    assert!(!placed.exists(), "nothing was placed");
    let output = sync(&["sync", "--yes"])?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(fs::read(&placed)?, body);
    Ok(())
}

/// an add of a link whose server states its size weighs the copy placing makes on the data folder's
/// disk before the body comes, as it weighs the cache's: on exFAT beside the cache, the download
/// and its copy need twice the file, so with room for one and a half no byte moves.
#[cfg(debug_assertions)]
#[test]
fn an_add_weighs_the_data_folders_disk_before_downloading() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'x'; 2048];
    let server = Server::start("/files/big.bin", vec![file(&body, "\"v1\"")])?;
    let output = sandbox
        .command(&["--json", "add", &server.url])
        .env("FETCHLOOM_TEST_FILESYSTEM", "exfat")
        .env("FETCHLOOM_TEST_FREE", "3072")
        .output()?;
    let document = json(&output)?;
    assert_eq!(output.status.code(), Some(1), "{document}");
    assert_eq!(document["work"]["bytes_down"], 0, "{document}");
    let message = document["errors"][0]["message"]
        .as_str()
        .unwrap_or_default();
    assert!(
        message.starts_with("placing ")
            && message.ends_with(&format!(
                "big.bin needs at least 4.0 KiB on the disk holding the data folder at {}, which has 3.0 KiB free",
                sandbox.work().join("data").display()
            )),
        "{document}"
    );
    Ok(())
}

/// a file in the way of a folder data.lock needs keeps what it blocks unplaced, and `--json`
/// names the file in the way as `blocked_by`.
#[test]
fn a_blocked_placement_names_its_blocker_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![file(V1, "\"v1\"")])?;
    crate::support::ok(&sandbox.run(&["add", &server.url])?)?;
    delete_folder(&sandbox.work().join("data/iris"))?;
    fs::write(sandbox.work().join("data/iris"), b"mine\n")?;
    let document = json(&sandbox.run(&["--json", "sync"])?)?;
    assert_eq!(
        document["result"]["placement"]["kept"],
        serde_json::json!([{
            "path": "data/iris/iris.csv",
            "reason": "blocked",
            "blocked_by": "data/iris"
        }]),
        "{document}"
    );
    Ok(())
}

/// a sync that places files says `Placing <n> files` on stderr as it starts placing them, once,
/// just before what it placed, n the files those lines count: the one line it adds to what it said
/// before. stdout says nothing, `-q` says nothing, and `--json` writes no line on stderr.
#[test]
fn a_placing_sync_says_what_it_places_as_it_starts() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    for name in ["a.csv", "b.csv", "c.csv"] {
        fs::write(flowers.join(name), name)?;
    }
    let added = sandbox.run(&["add", &flowers.display().to_string()])?;
    let stderr = text(&added.stderr)?;
    assert_eq!(added.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Placing"), "{stderr}");
    let data = sandbox.work().join("data");
    let placed = |args: &[&str]| -> Result<std::process::Output> {
        crate::support::delete_folder(&data)?;
        let synced = sandbox.run(args)?;
        assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
        assert_eq!(fs::read(data.join("flowers/b.csv"))?, b"b.csv");
        Ok(synced)
    };
    let synced = placed(&["sync"])?;
    assert_eq!(
        ways_left_out(&text(&synced.stderr)?),
        "    Placing 3 files\n     Placed data/flowers · 3 files\n  hint: to keep the data folder somewhere else than data/, set data_dir in data.toml\n     Synced 1 dataset into data\n"
    );
    assert_eq!(text(&synced.stdout)?, "");
    let quiet = placed(&["-q", "sync"])?;
    assert_eq!(text(&quiet.stderr)?, "");
    let documented = placed(&["--json", "sync"])?;
    assert_eq!(text(&documented.stderr)?, "");
    assert_eq!(json(&documented)?["result"]["placement"]["placed"], 3);
    Ok(())
}

/// a dry run, a sync with nothing to place and a remove say no such line.
#[test]
fn only_a_placing_sync_says_what_it_places_as_it_starts() -> Result {
    let sandbox = Sandbox::new()?;
    let flowers = sandbox.folder("flowers")?;
    fs::write(flowers.join("a.csv"), "a")?;
    let run = |args: &[&str]| -> Result<String> {
        let ran = sandbox.run(args)?;
        let stderr = text(&ran.stderr)?;
        assert_eq!(ran.status.code(), Some(0), "{args:?}: {stderr}");
        Ok(stderr)
    };
    run(&["add", &flowers.display().to_string()])?;
    let data = sandbox.work().join("data");
    crate::support::delete_folder(&data)?;
    let dry = run(&["sync", "--dry-run"])?;
    assert!(dry.contains("Would"), "{dry}");
    assert!(!dry.contains("Placing"), "{dry}");
    assert!(run(&["sync"])?.contains("Placing 1 file"));
    let settled = run(&["sync"])?;
    assert!(!settled.contains("Placing"), "{settled}");
    let removed = run(&["remove", "flowers", "--yes"])?;
    assert!(!removed.contains("Placing"), "{removed}");
    Ok(())
}

/// an edit of a file copy = true placed is what copy = true is for, so the warning that sync keeps
/// it never says to set copy = true again.
#[test]
fn an_edit_of_a_copy_is_never_told_to_set_copy() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", vec![file(V1, "\"v1\"")])?;
    copy_iris(&sandbox, &server)?;
    fs::write(sandbox.work().join("data/iris/iris.csv"), V2)?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("warning: data/iris/iris.csv has changed, so it stays\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("set copy = true"), "{stderr}");
    assert!(
        stderr.contains("copy = true lets you edit it here"),
        "{stderr}"
    );
    Ok(())
}
