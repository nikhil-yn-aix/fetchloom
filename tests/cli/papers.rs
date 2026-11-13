//! papers and scholarly data: arXiv's API, its OAI-PMH interface and the mirror of its pdfs,
//! through a server on this machine that stands in for each host.
#![cfg(debug_assertions)]

use std::fs;

use crate::support::{Result, Routes, Sandbox, file, json, ok, reply, sha256_hex, text};

/// the bytes of a paper's pdf, as far as the tests care.
const PDF: &[u8] = b"%PDF-1.5 attention\n";
/// another paper's pdf.
const OTHER: &[u8] = b"%PDF-1.5 another paper\n";

const PAPER: &[u8] = include_bytes!("../fixtures/sources/arxiv/paper.xml");
const LICENSE: &[u8] = include_bytes!("../fixtures/sources/arxiv/license.xml");
const NONE: &[u8] = include_bytes!("../fixtures/sources/arxiv/none.xml");

const API: &str = "/export.arxiv.org/api/query";
const LISTING: &str = "/storage.googleapis.com/storage/v1/b/arxiv-dataset/o";
const FIELDS: &str = "fields=items%28name%2Csize%2Cmd5Hash%2Cgeneration%29%2CnextPageToken";

/// a command whose requests to arXiv's hosts and to the mirror go to `routes`.
fn command(sandbox: &Sandbox, routes: &Routes, args: &[&str]) -> crate::support::Fetchloom {
    let mut command = sandbox.command(args);
    command
        .env("FETCHLOOM_TEST_SOURCES", &routes.url)
        .env("FETCHLOOM_TEST_BUCKETS", &routes.url);
    command
}

fn xml(body: &[u8]) -> Vec<crate::support::Part> {
    reply(
        "200 OK",
        &[("Content-Type", "application/atom+xml; charset=utf-8")],
        body,
    )
}

/// the mirror's listing of what it holds under `key`: each `(key, size)` at generation 7.
fn mirror(key: &str, objects: &[(&str, usize)]) -> (String, Vec<crate::support::Part>) {
    let items: Vec<serde_json::Value> = objects
        .iter()
        .map(|(key, size)| serde_json::json!({"name": key, "size": size.to_string(), "generation": "7"}))
        .collect();
    let body = serde_json::to_vec(&serde_json::json!({ "items": items })).unwrap_or_default();
    (
        format!("{LISTING}?prefix={}&{FIELDS}", key.replace('/', "%2F")),
        reply("200 OK", &[("Content-Type", "application/json")], &body),
    )
}

/// arXiv's answers for 1706.03762, at its newest version v7, and the mirror holding its pdf, or
/// not, in which case export.arxiv.org serves it.
fn paper(mirrored: bool) -> Result<Routes> {
    let key = "arxiv/arxiv/pdf/1706/1706.03762v7.pdf";
    let held: &[(&str, usize)] = if mirrored { &[(key, PDF.len())] } else { &[] };
    Routes::start(vec![
        (format!("{API}?id_list=1706.03762"), xml(PAPER)),
        (format!("{API}?id_list=1706.03762v7"), xml(PAPER)),
        (
            "/oaipmh.arxiv.org/oai?verb=GetRecord&identifier=oai%3AarXiv.org%3A1706.03762&metadataPrefix=arXivRaw".to_owned(),
            xml(LICENSE),
        ),
        mirror(key, held),
        (
            format!("/storage.googleapis.com/arxiv-dataset/{key}?generation=7"),
            file(PDF, "\"g7\""),
        ),
        (
            "/export.arxiv.org/pdf/1706.03762v7".to_owned(),
            file(PDF, "\"a7\""),
        ),
    ])
}

/// a paper takes its title as its name and pins its newest version, the mirror's copy at its
/// generation, with arXiv's license, which earns a warning; a teammate's sync asks nothing of
/// arXiv.
#[test]
fn an_arxiv_paper_is_named_by_its_title_and_pinned_from_the_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = paper(true)?;
    let output = command(&sandbox, &routes, &["add", "arXiv:1706.03762"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains(
            "the license http://arxiv.org/licenses/nonexclusive-distrib/1.0/ does not allow sharing copies"
        ),
        "{stderr}"
    );
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.attention-is-all-you]\nref = \"arxiv:1706.03762\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for expected in [
        "resolved = \"arxiv:1706.03762@v7\"".to_owned(),
        "title = \"Attention Is All You Need\"".to_owned(),
        "license = \"http://arxiv.org/licenses/nonexclusive-distrib/1.0/\"".to_owned(),
        "doi = \"10.48550/arXiv.1706.03762\"".to_owned(),
        format!(
            "{{ path = \"1706.03762v7.pdf\", size = {}, sha256 = \"{}\", url = \"https://storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/1706/1706.03762v7.pdf?generation=7\" }}",
            PDF.len(),
            sha256_hex(PDF)
        ),
    ] {
        assert!(lock.contains(&expected), "{expected}\n{lock}");
    }
    assert!(!lock.contains("access ="), "{lock}");
    assert_eq!(
        fs::read(work.join("data/attention-is-all-you/1706.03762v7.pdf"))?,
        PDF
    );

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = command(&sandbox, &routes, &["sync", "--locked"])
        .current_dir(&teammate)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .output()?;
    ok(&synced)?;
    assert_eq!(
        routes.targets()[listed..],
        [
            "/storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/1706/1706.03762v7.pdf?generation=7"
        ],
        "the lock alone says what to download"
    );
    Ok(())
}

/// a paper newer than the mirror comes from export.arxiv.org, the host arXiv keeps for programs.
#[test]
fn an_arxiv_paper_the_mirror_lacks_comes_from_export_arxiv_org() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = paper(false)?;
    let output = command(
        &sandbox,
        &routes,
        &[
            "add",
            "https://arxiv.org/pdf/1706.03762v7",
            "--as",
            "attention",
        ],
    )
    .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(
        lock.contains("url = \"https://export.arxiv.org/pdf/1706.03762v7\""),
        "{lock}"
    );
    assert!(
        routes
            .targets()
            .iter()
            .any(|target| target == "/export.arxiv.org/pdf/1706.03762v7")
    );
    Ok(())
}

/// a paper or a version arXiv does not have stops before anything is written.
#[test]
fn an_arxiv_paper_that_does_not_exist_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(format!("{API}?id_list=2401.99999"), xml(NONE))])?;
    let output = command(&sandbox, &routes, &["--json", "add", "arxiv:2401.99999"]).output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "reference.unresolved");
    assert_eq!(
        document["errors"][0]["message"],
        "arXiv has no paper arxiv:2401.99999"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    let routes = Routes::start(vec![(
        format!("{API}?id_list=1706.03762"),
        reply(
            "200 OK",
            &[("Content-Type", "text/html")],
            b"<html>busy</html>",
        ),
    )])?;
    let output = command(&sandbox, &routes, &["add", "arxiv:1706.03762"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(10), "{stderr}");
    assert!(
        stderr.contains("arXiv sent an answer about arxiv:1706.03762 that fetchloom cannot use: it is not an atom feed"),
        "{stderr}"
    );
    Ok(())
}

/// a month takes each paper's newest version in the mirror, named by the month.
#[test]
fn an_arxiv_month_takes_each_papers_newest_version_from_the_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let folder = "arxiv/arxiv/pdf/2401/";
    let routes = Routes::start(vec![
        mirror(
            folder,
            &[
                ("arxiv/arxiv/pdf/2401/2401.00001v1.pdf", PDF.len()),
                ("arxiv/arxiv/pdf/2401/2401.00002v1.pdf", PDF.len()),
                ("arxiv/arxiv/pdf/2401/2401.00002v2.pdf", OTHER.len()),
            ],
        ),
        (
            "/storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/2401/2401.00001v1.pdf?generation=7".to_owned(),
            file(PDF, "\"1\""),
        ),
        (
            "/storage.googleapis.com/arxiv-dataset/arxiv/arxiv/pdf/2401/2401.00002v2.pdf?generation=7".to_owned(),
            file(OTHER, "\"2\""),
        ),
    ])?;
    let output = command(&sandbox, &routes, &["add", "arxiv:2401"]).output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("arxiv:2401 · arXiv, January 2024 · 2 files"),
        "{stderr}"
    );
    let data = sandbox.work().join("data/arxiv-2401");
    assert_eq!(fs::read(data.join("2401.00001v1.pdf"))?, PDF);
    assert_eq!(fs::read(data.join("2401.00002v2.pdf"))?, OTHER);
    assert!(!data.join("2401.00002v1.pdf").exists());
    Ok(())
}

/// a category's month lists its papers through arXiv's API and downloads each from the mirror
/// where it holds the version, else from export.arxiv.org, saying how many come from there.
#[test]
fn an_arxiv_category_month_lists_through_the_api_and_downloads_from_the_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let feed = "<?xml version='1.0' encoding='UTF-8'?><feed xmlns=\"http://www.w3.org/2005/Atom\"><totalResults>2</totalResults><entry><id>http://arxiv.org/abs/2401.00001v1</id><title>One</title><published>2024-01-02T00:00:00Z</published></entry><entry><id>http://arxiv.org/abs/2401.00002v3</id><title>Two</title><published>2024-01-03T00:00:00Z</published></entry></feed>";
    let one = "arxiv/arxiv/pdf/2401/2401.00001v1.pdf";
    let routes = Routes::start(vec![
        (
            format!(
                "{API}?search_query=cat%3Acs.LG+AND+submittedDate%3A%5B202401010000+TO+202402010000%5D&sortBy=submittedDate&sortOrder=ascending&start=0&max_results=1000"
            ),
            xml(feed.as_bytes()),
        ),
        mirror("arxiv/arxiv/pdf/2401/", &[(one, PDF.len())]),
        (
            format!("/storage.googleapis.com/arxiv-dataset/{one}?generation=7"),
            file(PDF, "\"1\""),
        ),
        (
            "/export.arxiv.org/pdf/2401.00002v3".to_owned(),
            file(OTHER, "\"3\""),
        ),
    ])?;
    let dry = command(&sandbox, &routes, &["add", "arxiv:cs.lg/2401", "--dry-run"]).output()?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    for said in [
        "arxiv:cs.LG/2401 · arXiv cs.LG, January 2024 · 2 files",
        "the mirror does not hold 1 paper at its newest version yet, so it comes from export.arxiv.org, one every 3 seconds",
    ] {
        assert!(stderr.contains(said), "{said}\n{stderr}");
    }
    assert!(!sandbox.work().join("data.toml").exists());
    let output = command(&sandbox, &routes, &["add", "arxiv:cs.lg/2401"]).output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.cs-lg-2401]\nref = \"arxiv:cs.LG/2401\"\n"),
        "{manifest}"
    );
    let data = sandbox.work().join("data/cs-lg-2401");
    assert_eq!(fs::read(data.join("2401.00001v1.pdf"))?, PDF);
    assert_eq!(fs::read(data.join("2401.00002v3.pdf"))?, OTHER);
    Ok(())
}
