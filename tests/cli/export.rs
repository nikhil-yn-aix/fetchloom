//! exports: the checksums of the files a project places and the links of the files it downloads,
//! for other tools, read from data.lock and the cache without asking any server.

use std::fs;
use std::process::Command;

use crate::support::{Result, Routes, Sandbox, file, json, sha256_hex, text, zip_of};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const A: &[u8] = b"a\n";
const C: &[u8] = b"c\n";

fn pack() -> Result<Vec<u8>> {
    zip_of(&[("pack/a.csv", A), ("pack/b/c.txt", C)])
}

/// a server on this machine for iris.csv and pack.zip, with the zip's bytes.
fn server() -> Result<(Routes, Vec<u8>)> {
    let zip = pack()?;
    let routes = Routes::start(vec![
        ("/iris.csv".to_owned(), file(IRIS, "\"i1\"")),
        ("/pack.zip".to_owned(), file(&zip, "\"p1\"")),
    ])?;
    Ok((routes, zip))
}

fn added(sandbox: &Sandbox, routes: &Routes, files: &[&str]) -> Result {
    for name in files {
        let added = sandbox.run(&["add", &format!("{}/{name}", routes.url)])?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    Ok(())
}

#[test]
fn sha256_lists_each_placed_file_with_archive_members_for_sha256sum() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv", "pack.zip"])?;
    let asked = routes.requests().len();
    let output = sandbox.run(&["export"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let sums = text(&output.stdout)?;
    assert_eq!(
        sums,
        format!(
            "{}  data/iris/iris.csv\n{}  data/pack/a.csv\n{}  data/pack/b/c.txt\n",
            sha256_hex(IRIS),
            sha256_hex(A),
            sha256_hex(C)
        )
    );
    assert_eq!(text(&output.stderr)?, "");
    assert_eq!(routes.requests().len(), asked, "export asks no server");
    // coreutils checks the list from the project's folder, where linux has it
    fs::write(sandbox.work().join("sums"), &sums)?;
    if let Ok(checked) = Command::new("sha256sum")
        .args(["--check", "--strict", "sums"])
        .current_dir(sandbox.work())
        .output()
    {
        assert!(checked.status.success(), "{}", text(&checked.stderr)?);
        assert_eq!(text(&checked.stdout)?.lines().count(), 3);
    }
    Ok(())
}

#[test]
fn links_and_aria2_name_each_downloaded_file_and_never_an_archive_member() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    added(&sandbox, &routes, &["iris.csv", "pack.zip"])?;
    let links = sandbox.run(&["export", "--format", "links"])?;
    assert_eq!(links.status.code(), Some(0), "{}", text(&links.stderr)?);
    let url = &routes.url;
    assert_eq!(
        text(&links.stdout)?,
        format!(
            "{url}/iris.csv {}\n{url}/pack.zip {}\n",
            sha256_hex(IRIS),
            sha256_hex(&zip)
        )
    );
    let aria2 = sandbox.run(&["export", "--format", "aria2", "pack"])?;
    assert_eq!(aria2.status.code(), Some(0), "{}", text(&aria2.stderr)?);
    assert_eq!(
        text(&aria2.stdout)?,
        format!(
            "{url}/pack.zip\n  out=pack/pack.zip\n  checksum=sha-256={}\n",
            sha256_hex(&zip)
        )
    );
    Ok(())
}

/// this lets `aria2c -d data` fetch the data folder again without fetchloom.
#[test]
fn aria2_puts_each_file_in_the_folder_its_dataset_is_placed_in() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    sandbox.allow(&routes.url);
    let url = &routes.url;
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.iris]\nref = \"{url}/iris.csv\"\npath = \"raw\"\n\n[datasets.pack]\nref = \"{url}/pack.zip\"\npath = \".\"\n"
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(sandbox.work().join("data/raw/iris.csv"))?, IRIS);

    let aria2 = sandbox.run(&["export", "--format", "aria2"])?;

    assert_eq!(aria2.status.code(), Some(0), "{}", text(&aria2.stderr)?);
    assert_eq!(
        text(&aria2.stdout)?,
        format!(
            "{url}/iris.csv\n  out=raw/iris.csv\n  checksum=sha-256={}\n{url}/pack.zip\n  out=pack.zip\n  checksum=sha-256={}\n",
            sha256_hex(IRIS),
            sha256_hex(&zip)
        )
    );
    Ok(())
}

#[test]
fn json_lists_placed_files_and_downloaded_ones_with_their_hashes() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    added(&sandbox, &routes, &["iris.csv", "pack.zip"])?;
    let output = sandbox.run(&["--json", "export", "pack"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let result = &json(&output)?["result"];
    assert_eq!(
        result["placed"],
        serde_json::json!([
            {"dataset": "pack", "path": "data/pack/a.csv", "size": A.len(), "sha256": sha256_hex(A), "unpacked_from": "pack.zip"},
            {"dataset": "pack", "path": "data/pack/b/c.txt", "size": C.len(), "sha256": sha256_hex(C), "unpacked_from": "pack.zip"},
        ])
    );
    assert_eq!(
        result["sources"],
        serde_json::json!([
            {"dataset": "pack", "path": "pack.zip", "size": zip.len(), "sha256": sha256_hex(&zip), "url": format!("{}/pack.zip", routes.url)},
        ])
    );
    for gap in ["unlocked", "not_unpacked", "no_link"] {
        assert_eq!(result[gap], serde_json::json!([]), "{gap}");
    }
    assert_eq!(result["file"], serde_json::Value::Null);
    let unknown = sandbox.run(&["export", "pak"])?;
    assert_eq!(unknown.status.code(), Some(2));
    assert!(
        text(&unknown.stderr)?.contains("fetchloom export pack"),
        "{}",
        text(&unknown.stderr)?
    );
    Ok(())
}

/// text refuses rather than leave the archive's files out, and links need no unpacking.
#[test]
fn an_archive_never_unpacked_here_stops_text_and_is_named_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, zip) = server()?;
    added(&sandbox, &routes, &["pack.zip"])?;
    let cold = sandbox.folder("cold")?;
    let run = |args: &[&str]| sandbox.command(args).env("FETCHLOOM_CACHE", &cold).output();
    let refused = run(&["export"])?;
    assert_eq!(
        refused.status.code(),
        Some(20),
        "{}",
        text(&refused.stderr)?
    );
    assert_eq!(text(&refused.stdout)?, "", "no partial list");
    let stderr = text(&refused.stderr)?;
    assert!(
        stderr.contains("error: the files inside data/pack/pack.zip are not known on this machine"),
        "{stderr}"
    );
    assert!(stderr.contains("`fetchloom sync`"), "{stderr}");
    let listed = run(&["--json", "export"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let result = &json(&listed)?["result"];
    assert_eq!(
        result["not_unpacked"],
        serde_json::json!(["data/pack/pack.zip"])
    );
    assert_eq!(result["placed"], serde_json::json!([]));
    assert_eq!(result["sources"][0]["sha256"], sha256_hex(&zip));
    let links = run(&["export", "--format", "links"])?;
    assert_eq!(links.status.code(), Some(0), "{}", text(&links.stderr)?);
    assert_eq!(
        text(&links.stdout)?,
        format!("{}/pack.zip {}\n", routes.url, sha256_hex(&zip))
    );
    assert!(!cold.join("objects").exists(), "export makes no cache");
    Ok(())
}

#[test]
fn a_dataset_not_pinned_yet_stops_text_and_is_named_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let manifest = sandbox.work().join("data.toml");
    let before = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{before}\n[datasets.later]\nref = \"{}/pack.zip\"\n",
            routes.url
        ),
    )?;
    let refused = sandbox.run(&["export", "--format", "links"])?;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused.stderr)?);
    assert_eq!(text(&refused.stdout)?, "");
    let stderr = text(&refused.stderr)?;
    assert!(stderr.contains("later is not in data.lock yet"), "{stderr}");
    let listed = sandbox.run(&["--json", "export"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let result = &json(&listed)?["result"];
    assert_eq!(result["unlocked"], serde_json::json!(["later"]));
    assert_eq!(result["placed"][0]["path"], "data/iris/iris.csv");
    Ok(())
}

/// no source holds imported files, so they have no link; the warning counts them once, and
/// `--json` names them under `no_link`.
#[test]
fn imported_files_are_left_out_of_links_once_and_named_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let folder = sandbox.folder("flowers")?;
    for name in ["a.txt", "b.txt", "c.txt"] {
        fs::write(folder.join(name), name)?;
    }
    let imported = sandbox.run(&["add", "../flowers"])?;
    assert_eq!(
        imported.status.code(),
        Some(0),
        "{}",
        text(&imported.stderr)?
    );
    let url = &routes.url;
    let left_out = "warning: the list leaves out 3 files of flowers: they were imported from a folder on this machine, so no link holds them\n  help: share them through a folder mirror, which `fetchloom push --to <folder>` fills\n";
    for (format, expected) in [
        ("links", format!("{url}/iris.csv {}\n", sha256_hex(IRIS))),
        (
            "aria2",
            format!(
                "{url}/iris.csv\n  out=iris/iris.csv\n  checksum=sha-256={}\n",
                sha256_hex(IRIS)
            ),
        ),
    ] {
        let output = sandbox.run(&["export", "--format", format])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        assert_eq!(text(&output.stdout)?, expected, "{format}");
        let stderr = text(&output.stderr)?;
        assert!(stderr.starts_with(left_out), "{format}: {stderr}");
        assert_eq!(stderr.matches("warning:").count(), 1, "{format}: {stderr}");
    }
    let written = sandbox.run(&["--json", "export", "--format", "links", "-o", "links.txt"])?;
    assert_eq!(written.status.code(), Some(0), "{}", text(&written.stderr)?);
    let document = json(&written)?;
    assert_eq!(
        document["result"]["no_link"],
        serde_json::json!(["flowers/a.txt", "flowers/b.txt", "flowers/c.txt"])
    );
    assert_eq!(crate::support::kinds(&document), ["export.left_out"]);
    assert_eq!(
        fs::read_to_string(sandbox.work().join("links.txt"))?,
        format!("{url}/iris.csv {}\n", sha256_hex(IRIS))
    );
    Ok(())
}

/// fetchloom never pins such a link, so one was edited in, and its line break could add an option
/// to aria2's input; no output shows it. a link with a secret stops sooner, as data.lock is read.
#[test]
fn a_link_one_line_cannot_hold_is_never_written() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let lock = sandbox.work().join("data.lock");
    let pinned = fs::read_to_string(&lock)?;
    let link = format!("{}/iris.csv", routes.url);
    fs::write(
        &lock,
        pinned.replace(
            &format!("url = \"{link}\""),
            &format!("url = \"{link}\\n  dir=export-canary\""),
        ),
    )?;
    let refused = sandbox.run(&["export", "--format", "aria2"])?;
    assert_eq!(
        refused.status.code(),
        Some(50),
        "{}",
        text(&refused.stderr)?
    );
    assert_eq!(text(&refused.stdout)?, "");
    let stderr = text(&refused.stderr)?;
    assert!(
        stderr.contains(
            "the link data.lock gives iris/iris.csv holds a space or a control character"
        ),
        "{stderr}"
    );
    let listed = sandbox.run(&["--json", "export"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let document = json(&listed)?;
    assert_eq!(
        document["result"]["sources"][0]["url"],
        serde_json::Value::Null
    );
    assert_eq!(
        document["result"]["no_link"],
        serde_json::json!(["iris/iris.csv"])
    );
    let listing = sandbox.run(&["--json", "list"])?;
    assert_eq!(
        json(&listing)?["result"]["datasets"][0]["locked_files"][0].get("url"),
        Some(&serde_json::Value::Null)
    );
    for output in [&refused, &listed, &listing] {
        for stream in [&output.stdout, &output.stderr] {
            assert!(
                !text(stream)?.contains("export-canary"),
                "{}",
                text(stream)?
            );
        }
    }
    let sums = sandbox.run(&["export"])?;
    assert_eq!(sums.status.code(), Some(0), "checksums need no link");
    Ok(())
}

#[test]
fn output_writes_the_file_whole_and_replaces_one_only_with_force() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let expected = format!("{}  data/iris/iris.csv\n", sha256_hex(IRIS));
    let sums = sandbox.work().join("sums.txt");
    let written = sandbox.run(&["export", "-o", "sums.txt"])?;
    assert_eq!(written.status.code(), Some(0), "{}", text(&written.stderr)?);
    assert_eq!(text(&written.stdout)?, "");
    assert_eq!(text(&written.stderr)?, "   Exported 1 file to sums.txt\n");
    assert_eq!(fs::read_to_string(&sums)?, expected);
    fs::write(&sums, "mine\n")?;
    let kept = sandbox.run(&["--json", "export", "-o", "sums.txt"])?;
    assert_eq!(kept.status.code(), Some(50));
    let error = &json(&kept)?["errors"][0];
    assert_eq!(error["kind"], "local.not_empty");
    assert!(
        error["help"]
            .as_str()
            .unwrap_or_default()
            .contains("--force"),
        "{error}"
    );
    assert_eq!(fs::read_to_string(&sums)?, "mine\n");
    let forced = sandbox.run(&["--json", "export", "-o", "sums.txt", "--force"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(json(&forced)?["result"]["file"], sums.display().to_string());
    assert_eq!(fs::read_to_string(&sums)?, expected);
    let mut names: Vec<String> = fs::read_dir(sandbox.work())?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect::<Result<_>>()?;
    names.sort();
    assert_eq!(
        names,
        [".fetchloom", "data", "data.lock", "data.toml", "sums.txt"],
        "nothing is left beside it"
    );
    let folder = sandbox.run(&["export", "-o", "data"])?;
    assert_eq!(folder.status.code(), Some(2), "{}", text(&folder.stderr)?);
    let force_alone = sandbox.run(&["export", "--force"])?;
    assert_eq!(force_alone.status.code(), Some(2));
    Ok(())
}

/// the mark comes once the inventory is checked, and the export ends with its closing line and
/// exit 0, never saying it stops while it writes the file anyway.
#[cfg(unix)]
#[test]
fn a_ctrl_c_past_the_exports_mark_lets_it_write_the_file() -> Result {
    let sandbox = Sandbox::new()?;
    let (routes, _) = server()?;
    added(&sandbox, &routes, &["iris.csv"])?;
    let (running, release) = crate::support::held_at_its_mark(
        &sandbox,
        &mut sandbox.command(&["export", "-o", "sums.txt"]),
    )?;
    assert!(
        !sandbox.work().join("sums.txt").exists(),
        "the mark comes before the write"
    );
    let stderr = crate::support::finished_anyway(running, &release)?;
    assert!(
        stderr.contains("   Exported 1 file to sums.txt\n"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("sums.txt"))?,
        format!("{}  data/iris/iris.csv\n", sha256_hex(IRIS))
    );
    Ok(())
}

/// zenodo's answer for record `id`, which holds iris.csv and has a DOI.
fn record(id: u64) -> Result<Vec<crate::support::Part>> {
    let record = serde_json::json!({
        "id": id,
        "doi": format!("10.5281/zenodo.{id}"),
        "metadata": { "title": "DatasetIris", "license": { "id": "cc-by-4.0" }, "access_right": "open" },
        "files": [{ "key": "iris.csv", "size": IRIS.len(), "checksum": format!("md5:{}", crate::support::md5_hex(IRIS)) }],
    });
    Ok(crate::support::reply(
        "200 OK",
        &[("Content-Type", "application/json")],
        &serde_json::to_vec(&record)?,
    ))
}

/// a CSL-JSON answer, as a registration agency sends it.
fn csl(item: &serde_json::Value) -> Result<Vec<crate::support::Part>> {
    Ok(crate::support::reply(
        "200 OK",
        &[("Content-Type", "application/vnd.citationstyles.csl+json")],
        &serde_json::to_vec(item)?,
    ))
}

/// doi.org sending a request on to `to`, as it does for a DOI's registration agency.
fn sent_on(to: &str) -> Vec<crate::support::Part> {
    crate::support::reply(
        "302 Found",
        &[("Location", to), ("Content-Length", "0")],
        b"",
    )
}

/// a project of three datasets on hosts `routes` stands in for: two zenodo records with DOIs,
/// one an article's, and a plain link without one.
fn cited_project(sandbox: &Sandbox) -> Result<Routes> {
    let routes = Routes::start(vec![
        ("/zenodo.org/api/records/101".to_owned(), record(101)?),
        ("/zenodo.org/api/records/202".to_owned(), record(202)?),
        (
            "/zenodo.org/api/records/101/files/iris.csv/content".to_owned(),
            file(IRIS, "\"i\""),
        ),
        (
            "/zenodo.org/api/records/202/files/iris.csv/content".to_owned(),
            file(IRIS, "\"i\""),
        ),
        ("/data.example.org/uci/a.csv".to_owned(), file(A, "\"a\"")),
        (
            "/doi.org/10.5281%2Fzenodo.101".to_owned(),
            sent_on("https://data.crosscite.org/10.5281%2Fzenodo.101"),
        ),
        (
            "/data.crosscite.org/10.5281%2Fzenodo.101".to_owned(),
            csl(&serde_json::json!({
                "type": "dataset",
                "title": "DatasetIris",
                "author": [{ "family": "Montero Loaiza", "given": "David" }],
                "issued": { "date-parts": [[2020, 1, 15]] },
                "publisher": "Zenodo",
                "DOI": "10.5281/ZENODO.101",
            }))?,
        ),
        (
            "/doi.org/10.5281%2Fzenodo.202".to_owned(),
            sent_on("https://api.crossref.org/v1/works/10.5281%2Fzenodo.202/transform"),
        ),
        (
            "/api.crossref.org/v1/works/10.5281%2Fzenodo.202/transform".to_owned(),
            csl(&serde_json::json!({
                "type": "article-journal",
                "title": ["Irises, {measured}"],
                "author": [{ "family": "Fisher", "given": "R. A." }],
                "issued": { "date-parts": [["1936"]] },
                "container-title": "Annals of Eugenics",
                "publisher": "Wiley",
            }))?,
        ),
    ])?;
    for (reference, name) in [
        ("zenodo:101", "iris"),
        ("zenodo:202", "paper"),
        ("https://data.example.org/uci/a.csv", "uci"),
    ] {
        let added = sandbox
            .command(&["add", reference, "--as", name])
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()?;
        assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    }
    Ok(routes)
}

/// three pinned datasets, two with DOIs: bibtex has two entries from the DOIs' records, asked of
/// doi.org once each with an Accept header and no other, and one from data.lock marked incomplete.
#[test]
fn bibtex_cites_from_each_doi_record_and_marks_one_from_the_lock_incomplete() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = cited_project(&sandbox)?;
    let asked = routes.requests().len();
    let output = sandbox
        .command(&["export", "--format", "bibtex", "-o", "data.bib"])
        .env("FETCHLOOM_TEST_HOSTS", &routes.url)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Exported 3 citations to data.bib"),
        "{stderr}"
    );
    assert!(
        stderr.contains("warning: uci has no DOI, so its entry is built from data.lock alone and marked incomplete"),
        "{stderr}"
    );
    let bib = fs::read_to_string(sandbox.work().join("data.bib"))?;
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let day = lock
        .split("locked = \"")
        .nth(1)
        .and_then(|rest| rest.get(..10))
        .unwrap_or_default();
    assert_eq!(
        bib,
        format!(
            "@misc{{iris,\n  author = {{Montero Loaiza, David}},\n  title = {{DatasetIris}},\n  publisher = {{Zenodo}},\n  year = {{2020}},\n  doi = {{10.5281/zenodo.101}},\n  url = {{https://doi.org/10.5281/zenodo.101}},\n  urldate = {{{day}}},\n  copyright = {{cc-by-4.0}},\n}}\n\n@article{{paper,\n  author = {{Fisher, R. A.}},\n  title = {{Irises, \\textbraceleft{{}}measured\\textbraceright{{}}}},\n  journal = {{Annals of Eugenics}},\n  publisher = {{Wiley}},\n  year = {{1936}},\n  doi = {{10.5281/zenodo.202}},\n  url = {{https://doi.org/10.5281/zenodo.202}},\n  urldate = {{{day}}},\n  copyright = {{cc-by-4.0}},\n}}\n\n@misc{{uci,\n  title = {{uci}},\n  url = {{https://data.example.org/uci/a.csv}},\n  urldate = {{{day}}},\n  note = {{Incomplete: built from data.lock, which gives no author, year or title.}},\n}}\n"
        )
    );
    let asked: Vec<String> = routes.requests()[asked..].to_vec();
    let doi: Vec<&String> = asked
        .iter()
        .filter(|head| head.starts_with("get /doi.org/"))
        .collect();
    assert_eq!(doi.len(), 2, "{asked:#?}");
    for head in doi {
        let accepts: Vec<&str> = head
            .lines()
            .filter(|line| line.starts_with("accept:"))
            .collect();
        assert_eq!(
            accepts,
            ["accept: application/vnd.citationstyles.csl+json"],
            "{head}"
        );
    }
    Ok(())
}

/// offline, every entry comes from data.lock, the run says doi.org was not asked, asks nothing
/// and exits 0; csl and ris carry the same entries, and the statement each file's SHA-256.
#[test]
fn offline_citations_come_from_the_lock_and_say_doi_org_was_not_asked() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = cited_project(&sandbox)?;
    let asked = routes.requests().len();
    let output = sandbox.run(&["--offline", "export", "--format", "ris"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("offline, so doi.org was not asked for the DOI metadata of iris and paper: their entries are built from data.lock alone"),
        "{stderr}"
    );
    let ris = text(&output.stdout)?;
    assert_eq!(ris.matches("TY  - DATA\n").count(), 3, "{ris}");
    assert_eq!(ris.matches("ER  - \n").count(), 3, "{ris}");
    assert!(ris.contains("DO  - 10.5281/zenodo.101\n"), "{ris}");
    assert!(
        ris.contains("N1  - Incomplete: built from data.lock, which gives no author or year. License: cc-by-4.0.\n"),
        "{ris}"
    );
    let output = sandbox.run(&["--offline", "--json", "export", "--format", "csl"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let citations = &document["result"]["citations"];
    assert_eq!(citations[0]["from"], "data.lock", "{citations}");
    assert_eq!(citations[0]["unasked"], "offline, so doi.org was not asked");
    assert_eq!(
        citations[0]["missing"],
        serde_json::json!(["author", "year"])
    );
    let output = sandbox.run(&["--offline", "export", "--format", "csl"])?;
    let items: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(items[0]["id"], "iris", "{items}");
    assert_eq!(items[0]["DOI"], "10.5281/zenodo.101");
    let output = sandbox.run(&["--offline", "export", "--format", "statement"])?;
    let statement = text(&output.stdout)?;
    assert!(
        statement.starts_with("Data availability\n\nThis work uses 3 datasets."),
        "{statement}"
    );
    for line in [
        "iris: DatasetIris, https://doi.org/10.5281/zenodo.101, 1 file:\n".to_owned(),
        "uci: https://data.example.org/uci/a.csv, 1 file:\n".to_owned(),
        format!("{}  iris.csv\n", sha256_hex(IRIS)),
        format!("{}  a.csv\n", sha256_hex(A)),
    ] {
        assert!(statement.contains(&line), "{line} in\n{statement}");
    }
    assert_eq!(routes.requests().len(), asked, "offline asks nothing");
    Ok(())
}

/// a dataset its source's MD5 pins, which no sync downloaded yet, is cited as any other, as a
/// citation needs no SHA-256, and the statement gives that MD5 and says why.
#[test]
fn citations_take_a_dataset_pinned_by_its_md5_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        ("/zenodo.org/api/records/101".to_owned(), record(101)?),
        (
            "/zenodo.org/api/records/101/files/iris.csv/content".to_owned(),
            file(IRIS, "\"i\""),
        ),
        ("/zenodo.org/api/records/303".to_owned(), record(303)?),
    ])?;
    for (args, name) in [
        (&["add", "zenodo:101", "--as", "iris"][..], "iris"),
        (&["add", "zenodo:303", "--as", "late", "--no-sync"], "late"),
    ] {
        let added = sandbox
            .command(args)
            .env("FETCHLOOM_TEST_HOSTS", &routes.url)
            .output()?;
        assert_eq!(
            added.status.code(),
            Some(0),
            "{name}: {}",
            text(&added.stderr)?
        );
    }
    let output = sandbox.run(&["--offline", "export", "--format", "bibtex"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    let bib = text(&output.stdout)?;
    for key in ["@misc{iris,", "@misc{late,"] {
        assert!(bib.contains(key), "{key} in\n{bib}");
    }
    assert!(bib.contains("doi = {10.5281/zenodo.303}"), "{bib}");
    let output = sandbox.run(&["--offline", "export", "--format", "statement"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let statement = text(&output.stdout)?;
    for line in [
        "the SHA-256 checksum of every file used, or for a file not downloaded yet the MD5 its source states, so that".to_owned(),
        format!("{}  iris.csv\n", sha256_hex(IRIS)),
        "late: DatasetIris, https://doi.org/10.5281/zenodo.303, 1 file, not downloaded yet, so by the MD5 its source states:\n".to_owned(),
        format!("{}  iris.csv\n", crate::support::md5_hex(IRIS)),
    ] {
        assert!(statement.contains(&line), "{line} in\n{statement}");
    }
    Ok(())
}
