//! `audit`: whether a mirror or its source still serves each file data.lock pins, asked for each
//! file's first byte, so nothing is downloaded, and whether a source moved on since the pin.

use crate::mirrors::{IRIS, cold, mirror_of, project, web_mirror_of};
use crate::support::{
    Part, Ranged, Result, Routes, Sandbox, file, hang_up, json, md5_hex, ok, reply, sha256_hex,
    text, workspace,
};

/// each of `targets` on `source` added as a dataset of its own, named for its file.
fn pinned(sandbox: &Sandbox, source: &Routes, targets: &[&str]) -> Result {
    sandbox.allow(&source.url);
    for target in targets {
        ok(&sandbox.run(&["add", &format!("{}{target}", source.url)])?)?;
    }
    Ok(())
}

/// the result `audit` gave for the dataset `name`.
fn dataset<'d>(document: &'d serde_json::Value, name: &str) -> Result<&'d serde_json::Value> {
    document["result"]["datasets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|dataset| dataset["name"] == name)
        .ok_or_else(|| format!("no result for {name}: {document}").into())
}

fn missing() -> Vec<crate::support::Part> {
    reply("404 Not Found", &[("Content-Length", "0")], b"")
}

/// with no data.lock, a check says once that nothing pins files to audit, as its error, with no
/// Ok line before it; a plain audit says it as its Ok line.
#[test]
fn a_check_without_data_lock_says_it_once_as_its_error() -> Result {
    let sandbox = Sandbox::new()?;
    std::fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.x]\nref = \"https://example.org/iris.csv\"\n",
    )?;
    let checked = sandbox.run(&["audit", "--check"])?;
    let stderr = text(&checked.stderr)?;
    assert_eq!(checked.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.starts_with("error: there is no data.lock, so nothing pins files to audit\n"),
        "{stderr}"
    );
    assert_eq!(
        stderr.matches("there is no data.lock").count(),
        1,
        "{stderr}"
    );
    let plain = sandbox.run(&["audit"])?;
    let stderr = text(&plain.stderr)?;
    assert_eq!(plain.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok there is no data.lock, so nothing pins files to audit"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn each_pinned_file_is_served_or_says_why_not() -> Result {
    let sandbox = Sandbox::new()?;
    let big = vec![7; 1 << 20];
    let source = Routes::start(vec![
        ("/files/moved.csv".to_owned(), file(b"a,b\n1,2\n", "\"m\"")),
        ("/files/locked.csv".to_owned(), file(b"c,d\n3,4\n", "\"l\"")),
        ("/files/grown.csv".to_owned(), file(b"e,f\n5,6\n", "\"g\"")),
        ("/files/big.bin".to_owned(), file(&big, "\"b\"")),
    ])?;
    pinned(
        &sandbox,
        &source,
        &[
            "/files/moved.csv",
            "/files/locked.csv",
            "/files/grown.csv",
            "/files/big.bin",
        ],
    )?;
    let parts = Ranged::start("/files/iris.csv", IRIS.to_vec(), 0)?;
    sandbox.allow(&parts.url);
    ok(&sandbox.run(&["add", &parts.url])?)?;
    // since the pins, one file is gone, one asks for a sign-in and one grew
    source.change(vec![
        ("/files/moved.csv".to_owned(), missing()),
        (
            "/files/locked.csv".to_owned(),
            reply("403 Forbidden", &[("Content-Length", "0")], b""),
        ),
        (
            "/files/grown.csv".to_owned(),
            file(b"e,f\n5,6\n7,8\n", "\"h\""),
        ),
    ]);
    let (asked, parted) = (source.requests().len(), parts.requests().len());
    let output = sandbox.run(&["--json", "audit"])?;
    ok(&output)?;
    let document = json(&output)?;
    let sources = &document["result"];
    assert_eq!(
        (&sources["files"], &sources["served"], &sources["unchecked"]),
        (&5.into(), &2.into(), &1.into()),
        "{sources}"
    );
    // a part of one byte, and a whole file from a server that pays the range no heed
    for name in ["iris", "big"] {
        let served = dataset(&document, name)?;
        assert_eq!(served["served"], 1, "{served}");
        assert_eq!(served["not_served"], serde_json::json!([]));
    }
    let outcome = |name: &str| -> Result<serde_json::Value> {
        Ok(dataset(&document, name)?["not_served"][0].clone())
    };
    let gone = outcome("moved")?;
    assert_eq!(gone["outcome"], "gone", "{gone}");
    assert_eq!(gone["file"], "moved/moved.csv");
    // a refusal shows nothing lost, so the file is not checked rather than not served
    let refused = dataset(&document, "locked")?["unchecked"][0].clone();
    assert_eq!(refused["outcome"], "refused", "{refused}");
    assert!(refused["help"].is_string(), "{refused}");
    let changed = outcome("grown")?;
    assert_eq!(changed["outcome"], "changed", "{changed}");
    assert_eq!(changed["size"], 12);
    // one request a file, each for its first byte, and no byte read but the one part's
    let heads = &source.requests()[asked..];
    assert_eq!(heads.len(), 4, "{heads:?}");
    for head in heads.iter().chain(&parts.requests()[parted..]) {
        assert!(
            head.to_ascii_lowercase().contains("range: bytes=0-0"),
            "{head}"
        );
    }
    assert_eq!(document["work"]["bytes_down"], 1, "{}", document["work"]);
    Ok(())
}

#[test]
fn the_files_no_place_serves_are_rows_under_their_dataset() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![
        ("/files/iris.csv".to_owned(), file(IRIS, "\"i\"")),
        ("/files/moved.csv".to_owned(), file(b"a,b\n1,2\n", "\"m\"")),
    ])?;
    pinned(&sandbox, &source, &["/files/iris.csv", "/files/moved.csv"])?;
    source.change(vec![("/files/moved.csv".to_owned(), missing())]);
    let output = sandbox.run(&["audit"])?;
    ok(&output)?;
    let port = source.url.rsplit(':').next().unwrap_or_default();
    assert_eq!(
        text(&output.stdout)?,
        format!(
            " Not served moved · 1 of 1 file\n            moved/moved.csv · gone: 127.0.0.1 has nothing at http://127.0.0.1:{port}/files/moved.csv (404)\n"
        )
    );
    let stderr = text(&output.stderr)?;
    for line in [
        "     Served iris · 1 file\n",
        "      Found 1 of 2 files in data.lock that no mirror or source serves\n",
        "`fetchloom lock --upgrade moved` pins what the source serves now",
    ] {
        assert!(stderr.contains(line), "{line} in\n{stderr}");
    }
    Ok(())
}

#[test]
fn a_clone_checks_its_sources_and_a_mirror_stands_in_for_a_lost_one() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    pinned(&sandbox, &source, &["/files/iris.csv"])?;
    // a fresh clone holds no data yet, which a check of its sources does not ask for
    let (folder, cache) = cold(&sandbox, "clone")?;
    let run = |arguments: &[&str], mirror: Option<&str>| {
        let mut command = sandbox.command_in(&folder, arguments);
        command.env("FETCHLOOM_CACHE", &cache);
        if let Some(mirror) = mirror {
            command.arg("--mirror").arg(mirror);
        }
        command.output()
    };
    assert_eq!(run(&["status", "--check"], None)?.status.code(), Some(1));
    ok(&run(&["audit", "--check"], None)?)?;
    source.change(vec![("/files/iris.csv".to_owned(), missing())]);
    let output = run(&["--json", "audit", "--check"], None)?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "data.not_ready");
    assert_eq!(
        error["message"],
        "no mirror or source serves 1 file data.lock pins"
    );
    assert!(
        error["details"][0]
            .as_str()
            .is_some_and(|line| line.starts_with("iris/iris.csv · gone: ")),
        "{error}"
    );
    let asked = source.requests().len();
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    let web = web_mirror_of(&sandbox)?;
    let (folder_mirror, at) = (
        root.to_string_lossy().into_owned(),
        format!("{}/m", web.url),
    );
    for mirror in [&folder_mirror, &at] {
        let output = run(&["--json", "audit", "--check"], Some(mirror))?;
        ok(&output)?;
        let document = json(&output)?;
        let iris = dataset(&document, "iris")?;
        assert_eq!(iris["on_mirrors"], 1, "{mirror}: {iris}");
        // its source is asked first, so a mirror that became the only copy says so
        assert_eq!(iris["lost_at_source"][0]["outcome"], "gone", "{iris}");
    }
    assert_eq!(source.requests().len(), asked + 2);
    let output = run(&["audit"], Some(&folder_mirror))?;
    ok(&output)?;
    let rows = text(&output.stdout)?;
    assert!(
        rows.contains("Source lost iris · 1 of 1 file, which only a mirror serves now\n"),
        "{rows}"
    );
    let objects: Vec<String> = web
        .requests()
        .into_iter()
        .filter(|head| head.contains("/m/objects/"))
        .collect();
    assert_eq!(objects.len(), 1, "{objects:?}");
    assert!(objects[0].to_ascii_lowercase().contains("range: bytes=0-0"));
    Ok(())
}

#[test]
fn a_host_that_gives_no_answer_is_asked_no_more() -> Result {
    let sandbox = Sandbox::new()?;
    let names = ["a", "b", "c", "d"];
    let target = |name: &str| format!("/files/{name}.csv");
    let source = Routes::start(
        names
            .iter()
            .map(|name| (target(name), file(name.as_bytes(), "\"x\"")))
            .collect(),
    )?;
    let targets: Vec<String> = names.iter().map(|name| target(name)).collect();
    let targets: Vec<&str> = targets.iter().map(String::as_str).collect();
    pinned(&sandbox, &source, &targets)?;
    source.change(
        names
            .iter()
            .map(|name| {
                let busy = reply("503 Service Unavailable", &[("Content-Length", "0")], b"");
                (target(name), busy)
            })
            .collect(),
    );
    let asked = source.requests().len();
    let output = sandbox
        .command(&["--json", "audit"])
        .env("FETCHLOOM_RETRIES", "0")
        .env("FETCHLOOM_JOBS", "1")
        .output()?;
    ok(&output)?;
    let document = json(&output)?;
    let mut unasked = 0;
    for name in names {
        let file = &dataset(&document, name)?["unchecked"][0];
        assert_eq!(file["outcome"], "unknown", "{file}");
        if file["message"] == "not asked, since 127.0.0.1 gave no answer for another file" {
            unasked += 1;
        }
    }
    // a file or two go out at once, and once one gave no answer the rest wait for nothing
    let sent = source.requests().len() - asked;
    assert!(sent < names.len(), "{sent} requests");
    assert_eq!(unasked, names.len() - sent);
    Ok(())
}

/// a source that gives the file without its size serves it; one that is busy, refuses or gives no
/// answer leaves the file not checked, which `--check` fails as a download of it would, never as
/// lost.
#[test]
fn a_file_not_checked_is_never_called_lost() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    pinned(&sandbox, &source, &["/files/iris.csv"])?;
    let check = || {
        sandbox
            .command(&["--json", "audit", "--check"])
            .env("FETCHLOOM_RETRIES", "0")
            .output()
    };
    // a whole file and no length, as a server paying the range no heed may send
    source.change(vec![(
        "/files/iris.csv".to_owned(),
        reply("200 OK", &[], IRIS),
    )]);
    let output = check()?;
    ok(&output)?;
    let iris = dataset(&json(&output)?, "iris")?.clone();
    assert_eq!(
        (&iris["served"], &iris["no_size"]),
        (&1.into(), &1.into()),
        "{iris}"
    );
    let told = text(&sandbox.run(&["audit"])?.stderr)?;
    assert!(
        told.contains("Served iris · 1 file · 1 answered without a size"),
        "{told}"
    );
    assert!(
        told.contains("Ok the file in data.lock is still served, without its size, and its source lists it as data.lock pins it"),
        "{told}"
    );
    for (status, code, kind) in [
        ("503 Service Unavailable", 20, "network.status"),
        ("403 Forbidden", 40, "auth.required"),
    ] {
        let answer = reply(status, &[("Content-Length", "0")], b"");
        source.change(vec![("/files/iris.csv".to_owned(), answer)]);
        let output = check()?;
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            text(&output.stderr)?
        );
        let error = &json(&output)?["errors"][0];
        assert_eq!(error["kind"], kind, "{error}");
        assert_eq!(
            error["message"],
            "could not check 1 file data.lock pins, which may still be served"
        );
    }
    source.change(vec![("/files/iris.csv".to_owned(), hang_up())]);
    let output = check()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    Ok(())
}

/// a file its source's digests pin before any download is asked about by its size too.
#[test]
fn a_file_not_downloaded_yet_is_asked_about_too() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    pinned(&sandbox, &source, &["/files/iris.csv"])?;
    let lock = sandbox.work().join("data.lock");
    let pins = std::fs::read_to_string(&lock)?;
    std::fs::write(
        &lock,
        pins.replacen("files = [", "downloaded = false\nfiles = [", 1),
    )?;
    let asked = source.requests().len();
    let output = sandbox.run(&["--json", "audit"])?;
    ok(&output)?;
    let document = json(&output)?;
    assert_eq!(document["result"]["files"], 1, "{document}");
    assert_eq!(dataset(&document, "iris")?["served"], 1);
    assert_eq!(source.requests().len(), asked + 1);
    source.change(vec![(
        "/files/iris.csv".to_owned(),
        file(b"a,b\n", "\"j\""),
    )]);
    let output = sandbox.run(&["--json", "audit"])?;
    ok(&output)?;
    let document = json(&output)?;
    let changed = &dataset(&document, "iris")?["not_served"][0];
    assert_eq!(
        (&changed["outcome"], &changed["size"]),
        (&"changed".into(), &4.into()),
        "{changed}"
    );
    Ok(())
}

/// an archive its source lost is served by a mirror that keeps the list of its files and the files,
/// as a push leaves them, without the archive itself.
#[test]
fn a_mirror_keeping_an_archives_files_serves_it() -> Result {
    let sandbox = Sandbox::new()?;
    let source = project(&sandbox)?;
    let zip = std::fs::read(workspace().join("tests/fixtures/archives/python.zip"))?;
    let hex = sha256_hex(&zip);
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    let object = root.join("objects").join(&hex[..2]).join(&hex[2..]);
    if object.exists() {
        std::fs::remove_file(object)?;
    }
    let web = web_mirror_of(&sandbox)?;
    web.change(vec![(
        format!("/m/objects/{}/{}", &hex[..2], &hex[2..]),
        missing(),
    )]);
    source.change(vec![("/files/dataset.zip".to_owned(), missing())]);
    for mirror in [
        root.to_string_lossy().into_owned(),
        format!("{}/m", web.url),
    ] {
        let output = sandbox
            .command(&["--json", "audit", "--check"])
            .arg("--mirror")
            .arg(&mirror)
            .output()?;
        ok(&output)?;
        let document = json(&output)?;
        let archive = dataset(&document, "dataset")?;
        assert_eq!(archive["on_mirrors"], 1, "{mirror}: {archive}");
        assert_eq!(archive["lost_at_source"][0]["outcome"], "gone", "{archive}");
    }
    Ok(())
}

#[test]
fn offline_asks_no_source() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/files/iris.csv".to_owned(), file(IRIS, "\"i\""))])?;
    pinned(&sandbox, &source, &["/files/iris.csv"])?;
    let asked = source.requests().len();
    let output = sandbox.run(&["--json", "--offline", "audit"])?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "offline.missing");
    assert_eq!(error["help"], "run it again without --offline");
    assert_eq!(source.requests().len(), asked);
    Ok(())
}

/// zenodo's answer for record `id`, a version of concept 100 holding iris.csv.
pub(crate) fn record(id: u64, license: &str) -> Result<Vec<Part>> {
    let record = serde_json::json!({
        "id": id,
        "conceptrecid": "100",
        "doi": format!("10.5281/zenodo.{id}"),
        "metadata": {
            "title": "DatasetIris",
            "license": { "id": license },
            "access_right": "open",
        },
        "files": [{ "key": "iris.csv", "size": IRIS.len(), "checksum": format!("md5:{}", md5_hex(IRIS)) }],
    });
    Ok(reply(
        "200 OK",
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(&record)?,
    ))
}

/// a dataset pinned through zenodo's concept id: once zenodo publishes a newer version, audit
/// names it with the upgrade that takes it, and a license changed on the pinned version shows too;
/// `--check` fails as data.lock out of date, naming the dataset and what moved.
#[test]
fn a_newer_zenodo_version_is_reported_with_the_upgrade_that_takes_it() -> Result {
    let sandbox = Sandbox::new()?;
    let content = "/zenodo.org/api/records/101/files/iris.csv/content";
    let routes = Routes::start(vec![
        (
            "/zenodo.org/api/records/100".to_owned(),
            record(101, "cc-by-4.0")?,
        ),
        (
            "/zenodo.org/api/records/101".to_owned(),
            record(101, "cc-by-4.0")?,
        ),
        (content.to_owned(), file(IRIS, "\"i\"")),
    ])?;
    let run = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()
    };
    ok(&run(&["add", "zenodo:100", "--as", "iris"])?)?;
    let output = run(&["audit"])?;
    ok(&output)?;
    assert_eq!(text(&output.stdout)?, "");
    assert!(
        text(&output.stderr)?
            .contains("Ok the file in data.lock is still served at its pinned size, and its source lists it as data.lock pins it"),
        "{}",
        text(&output.stderr)?
    );
    routes.change(vec![
        (
            "/zenodo.org/api/records/100".to_owned(),
            record(102, "cc-by-4.0")?,
        ),
        (
            "/zenodo.org/api/records/101".to_owned(),
            record(101, "cc0-1.0")?,
        ),
        (
            "/zenodo.org/api/records/102".to_owned(),
            record(102, "cc-by-4.0")?,
        ),
    ]);
    let output = run(&["audit"])?;
    ok(&output)?;
    assert_eq!(
        text(&output.stdout)?,
        "      Newer iris · zenodo:102, where data.lock pins zenodo:101\n    Changed iris · license cc-by-4.0 → cc0-1.0\n"
    );
    let stderr = text(&output.stderr)?;
    for line in [
        "     Served iris · 1 file\n",
        "      Found 1 dataset with a newer version or a change at their source\n",
        "`fetchloom lock --upgrade iris` pins what their sources list now",
    ] {
        assert!(stderr.contains(line), "{line} in\n{stderr}");
    }
    let output = run(&["--json", "audit", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let iris = dataset(&document, "iris")?;
    assert_eq!(iris["newer"], "zenodo:102", "{iris}");
    assert_eq!(
        iris["changed"],
        serde_json::json!([{ "field": "license", "pinned": "cc-by-4.0", "now": "cc0-1.0" }])
    );
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "lock.outdated", "{error}");
    assert_eq!(
        error["details"],
        serde_json::json!([
            "iris · newer version zenodo:102, where data.lock pins zenodo:101",
            "iris · license cc-by-4.0 → cc0-1.0"
        ])
    );
    Ok(())
}

/// a record its source deleted is named as gone, and fails the check with the dataset named.
#[test]
fn a_dataset_its_source_deleted_fails_the_check() -> Result {
    let sandbox = Sandbox::new()?;
    let content = "/zenodo.org/api/records/101/files/iris.csv/content";
    let routes = Routes::start(vec![
        (
            "/zenodo.org/api/records/101".to_owned(),
            record(101, "cc-by-4.0")?,
        ),
        (content.to_owned(), file(IRIS, "\"i\"")),
    ])?;
    let run = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()
    };
    ok(&run(&["add", "zenodo:101", "--as", "iris"])?)?;
    routes.change(vec![
        ("/zenodo.org/api/records/101".to_owned(), missing()),
        (content.to_owned(), missing()),
    ]);
    let output = run(&["--json", "audit", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(dataset(&document, "iris")?["unlisted"]["outcome"], "gone");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "data.not_ready", "{error}");
    let details: Vec<&str> = error["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        details[0].starts_with("iris/iris.csv · gone: "),
        "{details:?}"
    );
    assert_eq!(
        details[1],
        "iris · gone: zenodo:101 does not exist, or is not public"
    );
    Ok(())
}

/// a retry while asking about a file names it as its dataset knows it, as a download's retry
/// does, and not by its link's last part, which may be an id, a hash or `content`.
#[test]
fn a_retry_names_the_file_not_its_link() -> Result {
    let sandbox = Sandbox::new()?;
    let target = "/records/1/files/iris.csv/content";
    let named = || {
        let length = IRIS.len().to_string();
        let headers = [
            ("Content-Length", length.as_str()),
            ("Content-Disposition", "attachment; filename=\"iris.csv\""),
        ];
        reply("200 OK", &headers, IRIS)
    };
    let source = Routes::start(vec![(target.to_owned(), named())])?;
    sandbox.allow(&source.url);
    let link = format!("{}{target}", source.url);
    ok(&sandbox.run(&["add", &link, "--as", "iris"])?)?;
    let busy = reply(
        "503 Service Unavailable",
        &[("Content-Length", "0"), ("Retry-After", "0")],
        b"",
    );
    source.turns(target, vec![busy]);
    let output = sandbox.run(&["audit"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("trying iris/iris.csv again in"), "{stderr}");
    assert!(!stderr.contains("trying content again"), "{stderr}");
    Ok(())
}

/// an import has no source to lose a file, so a mirror that holds its files simply serves them,
/// and one that does not leaves them not served, as imported.
#[test]
fn an_import_a_mirror_holds_is_served_by_it() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = sandbox.folder("flowers")?;
    std::fs::write(folder.join("sepals.csv"), b"length,width\n5.1,3.5\n")?;
    ok(&sandbox.run(&["add", "../flowers"])?)?;
    let output = sandbox.run(&["--json", "audit"])?;
    ok(&output)?;
    let document = json(&output)?;
    let flowers = dataset(&document, "flowers")?;
    assert_eq!(flowers["not_served"][0]["outcome"], "imported", "{flowers}");
    let root = sandbox.folder("mirror")?;
    mirror_of(&sandbox, &root)?;
    let mirror = root.to_string_lossy().into_owned();
    let output = sandbox.run(&["--json", "audit", "--check", "--mirror", &mirror])?;
    ok(&output)?;
    let document = json(&output)?;
    let flowers = dataset(&document, "flowers")?;
    assert_eq!(
        (&flowers["served"], &flowers["on_mirrors"]),
        (&1.into(), &1.into()),
        "{flowers}"
    );
    assert_eq!(
        flowers["lost_at_source"],
        serde_json::json!([]),
        "{flowers}"
    );
    let output = sandbox.run(&["audit", "--mirror", &mirror])?;
    ok(&output)?;
    assert_eq!(text(&output.stdout)?, "");
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(&format!("     Served flowers · 1 file · 1 on {mirror}\n")),
        "{stderr}"
    );
    Ok(())
}

/// a link's file is checked by its size alone, so the run says so and never that nothing
/// changed: one digit changed in place keeps the size. its server's `Last-Modified` after
/// data.lock's `locked` names the file as possibly changed since the pin, a warning that leaves
/// `--check` passing; one from before the pin raises nothing.
#[test]
fn a_file_checked_by_size_alone_says_so_and_a_later_change_warns() -> Result {
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![(
        "/files/iris.csv".to_owned(),
        file(b"a,b\n1,2\n", "\"v1\""),
    )])?;
    pinned(&sandbox, &source, &["/files/iris.csv"])?;
    let path = sandbox.work().join("data.lock");
    let lock = std::fs::read_to_string(&path)?;
    let at = lock.find("locked = \"").ok_or("no locked time")? + "locked = \"".len();
    let pinned_at = "2026-06-01T00:00:00Z";
    std::fs::write(
        &path,
        format!(
            "{}{pinned_at}{}",
            &lock[..at],
            &lock[at + pinned_at.len()..]
        ),
    )?;
    for (modified, warned) in [
        ("Sun, 31 May 2026 23:00:00 GMT", false),
        ("Mon, 01 Jun 2026 01:00:00 GMT", true),
    ] {
        source.change(vec![(
            "/files/iris.csv".to_owned(),
            reply(
                "200 OK",
                &[("Content-Length", "8"), ("Last-Modified", modified)],
                b"a,b\n1,3\n",
            ),
        )]);
        let output = sandbox.run(&["audit"])?;
        ok(&output)?;
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.contains("Ok the file in data.lock is still served at its pinned size"),
            "{stderr}"
        );
        assert!(!stderr.contains("changed nothing"), "{stderr}");
        let output = sandbox.run(&["--json", "audit", "--check"])?;
        ok(&output)?;
        let document = json(&output)?;
        let changed: Vec<&serde_json::Value> = document["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|warning| warning["kind"] == "source.modified")
            .collect();
        if !warned {
            assert!(changed.is_empty(), "{modified}: {document}");
            continue;
        }
        assert_eq!(changed.len(), 1, "{document}");
        assert_eq!(
            changed[0]["message"],
            format!(
                "iris/iris.csv may have changed at its source since data.lock pinned it on {pinned_at}: its server says it was modified later, and only its size was compared"
            ),
            "{document}"
        );
    }
    Ok(())
}

/// a server that names no `ETag`, only `Last-Modified`, as python's `http.server`, answers 304 for
/// a file changed under its old date, as `cp -p` or `rsync -t` leave it. `audit` finds the change
/// by its size, and `lock --upgrade` agrees: it pins the new bytes. while the size holds, the
/// answer for the file's first byte, which states its date and size, stands and the upgrade
/// downloads nothing.
#[test]
fn audit_and_an_upgrade_agree_on_a_file_changed_under_its_old_date() -> Result {
    const DATE: &str = "Thu, 01 Jan 2026 00:00:00 GMT";
    let dated = |body: &[u8]| {
        reply(
            "200 OK",
            &[
                ("Content-Length", &body.len().to_string()),
                ("Last-Modified", DATE),
            ],
            body,
        )
    };
    let sandbox = Sandbox::new()?;
    let source = Routes::start(vec![("/f.csv".to_owned(), dated(b"a,b\n1,2\n"))])?;
    source.unmodified("/f.csv", DATE);
    pinned(&sandbox, &source, &["/f.csv"])?;
    let added = source.requests().len();
    let same = sandbox.run(&["lock", "--upgrade", "f"])?;
    assert_eq!(
        text(&same.stderr)?,
        "         Ok data.lock is already up to date, nothing to do\n"
    );
    let asked = &source.requests()[added..];
    let dated_ask = format!("if-modified-since: {}", DATE.to_ascii_lowercase());
    assert!(
        asked.iter().any(|head| head.contains("range: bytes=0-0")),
        "{asked:?}"
    );
    assert!(
        asked
            .iter()
            .all(|head| head.contains(&dated_ask) || head.contains("range: bytes=0-0")),
        "an upgrade of an unchanged file downloads nothing: {asked:?}"
    );
    let longer = b"a,b\n1,2\n3,4\n";
    source.change(vec![("/f.csv".to_owned(), dated(longer))]);
    let audited = sandbox.run(&["audit"])?;
    ok(&audited)?;
    let rows = text(&audited.stdout)?;
    assert!(
        rows.contains("f/f.csv · changed: 127.0.0.1 serves 12 B where data.lock pins 8 B"),
        "{rows}"
    );
    let upgraded = sandbox.run(&["lock", "--upgrade", "f"])?;
    ok(&upgraded)?;
    let lock = std::fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains(&sha256_hex(longer)),
        "{}",
        text(&upgraded.stderr)?
    );
    let again = sandbox.run(&["audit"])?;
    ok(&again)?;
    let stderr = text(&again.stderr)?;
    assert!(stderr.contains("     Served f · 1 file\n"), "{stderr}");
    Ok(())
}

/// a record data.toml pins by its version, `zenodo:101`, whose source names a newer one,
/// `zenodo:102`, gets a line naming that version and its page, with nothing for `--check` to
/// fail, since the person chose the version; a record that is its newest gets none.
#[test]
fn a_record_pinned_by_its_version_names_a_newer_one() -> Result {
    let sandbox = Sandbox::new()?;
    let content = "/zenodo.org/api/records/101/files/iris.csv/content";
    let latest = "/zenodo.org/api/records/101/versions/latest";
    let routes = Routes::start(vec![
        (
            "/zenodo.org/api/records/101".to_owned(),
            record(101, "cc-by-4.0")?,
        ),
        (latest.to_owned(), record(101, "cc-by-4.0")?),
        (content.to_owned(), file(IRIS, "\"i\"")),
    ])?;
    let run = |args: &[&str]| {
        sandbox
            .command(args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()
    };
    ok(&run(&["add", "zenodo:101", "--as", "iris"])?)?;
    let output = run(&["audit"])?;
    ok(&output)?;
    assert!(
        !text(&output.stderr)?.contains("Newest"),
        "{}",
        text(&output.stderr)?
    );
    routes.change(vec![
        (
            "/zenodo.org/api/records/101".to_owned(),
            record(101, "cc-by-4.0")?,
        ),
        (latest.to_owned(), record(102, "cc-by-4.0")?),
        (content.to_owned(), file(IRIS, "\"i\"")),
    ]);
    let output = run(&["audit"])?;
    ok(&output)?;
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("Newest iris · zenodo:102, at https://zenodo.org/records/102, while data.toml names zenodo:101: change its ref to take it"),
        "{stderr}"
    );
    let output = run(&["--json", "audit", "--check"])?;
    ok(&output)?;
    let document = json(&output)?;
    assert_eq!(
        dataset(&document, "iris")?["latest"],
        serde_json::json!({ "version": "zenodo:102", "link": "https://zenodo.org/records/102" }),
        "{document}"
    );
    assert_eq!(
        dataset(&document, "iris")?["newer"],
        serde_json::Value::Null
    );
    Ok(())
}

/// a checksum list states each file's SHA-256, so a list naming other bytes for a pinned file of
/// the same size says its source changed that file: audit names it and `--check` fails, rather
/// than saying the source lists it as data.lock pins it.
#[test]
fn a_list_stating_other_bytes_for_a_pinned_file_fails_the_check() -> Result {
    let sandbox = Sandbox::new()?;
    let (old, new): (&[u8], &[u8]) = (b"a,b\n1,2\n", b"a,b\n3,4\n");
    let sums = |bytes: &[u8]| format!("{}  f.csv\n", sha256_hex(bytes)).into_bytes();
    let source = Routes::start(vec![
        ("/lab/SHA256SUMS".to_owned(), file(&sums(old), "\"s1\"")),
        ("/lab/f.csv".to_owned(), file(old, "\"f1\"")),
    ])?;
    pinned(&sandbox, &source, &["/lab/SHA256SUMS"])?;
    source.change(vec![
        ("/lab/SHA256SUMS".to_owned(), file(&sums(new), "\"s2\"")),
        ("/lab/f.csv".to_owned(), file(new, "\"f2\"")),
    ]);
    let audited = sandbox.run(&["audit"])?;
    let stderr = text(&audited.stderr)?;
    assert!(!stderr.contains("as data.lock pins it"), "{stderr}");
    let checked = sandbox.run(&["--json", "audit", "--check"])?;
    let document = json(&checked)?;
    assert_eq!(checked.status.code(), Some(1), "{document}");
    assert_eq!(
        document["errors"][0]["kind"], "data.not_ready",
        "{document}"
    );
    let changed = &dataset(&document, "lab")?["not_served"][0];
    assert_eq!(changed["file"], "lab/f.csv", "{document}");
    assert_eq!(changed["outcome"], "changed", "{document}");
    Ok(())
}
