//! papers and scholarly data: arXiv's API, its OAI-PMH interface and the mirror of its pdfs, and
//! semantic scholar's API, through a server on this machine that stands in for each host.
#![cfg(debug_assertions)]

use std::fs;

use crate::support::{
    Result, Routes, Sandbox, Server, file, json, ok, reply, sha256_hex, text, without_copy_warning,
};

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

/// arXiv is searched only when named, its papers by title first, each with its first author and
/// year.
#[test]
fn arxiv_is_searched_by_title_when_named() -> Result {
    let sandbox = Sandbox::new()?;
    let api = Server::start("/", vec![xml(PAPER), xml(NONE)])?;
    let output = sandbox
        .command(&["search", "attention is all you need", "--source", "arxiv"])
        .env("FETCHLOOM_TEST_ARXIV_API", &api.url)
        .output()?;
    let stderr = text(&output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("Searching arXiv for \"attention is all you need\""),
        "{stderr}"
    );
    assert_eq!(
        text(&output.stdout)?,
        "arxiv:1706.03762\tAttention Is All You Need\n"
    );
    let asked = api.requests();
    assert!(
        asked[0].contains("search_query=ti%3a%22attention+is+all+you+need%22"),
        "{asked:?}"
    );
    Ok(())
}

const S2_OPEN: &[u8] = include_bytes!("../fixtures/sources/s2/paper-open.json");
const S2_CLOSED: &[u8] = include_bytes!("../fixtures/sources/s2/paper-closed.json");
const S2_SEARCH: &[u8] = include_bytes!("../fixtures/sources/s2/search.json");
const S2_FIELDS: &str =
    "fields=paperId%2CcorpusId%2CexternalIds%2Curl%2Ctitle%2Cyear%2Cauthors%2CopenAccessPdf";

/// where the stand-in for semantic scholar's API answers about the paper `id`.
fn s2_paper(id: &str) -> String {
    format!("/api.semanticscholar.org/graph/v1/paper/{id}?{S2_FIELDS}")
}

fn s2_json(body: &[u8]) -> Vec<crate::support::Part> {
    reply("200 OK", &[("Content-Type", "application/json")], body)
}

/// semantic scholar's answer for the PLOS paper, its pdf on `routes`, which stand in for the
/// publisher.
fn s2_open(routes: &Routes) -> Result<Vec<u8>> {
    let mut answer: serde_json::Value = serde_json::from_slice(S2_OPEN)?;
    answer["openAccessPdf"]["url"] = format!("{}/plos/printable.pdf", routes.url).into();
    Ok(serde_json::to_vec(&answer)?)
}

/// a paper is found with no key, named by its title and pinned at the pdf semantic scholar links,
/// with the license it states; a teammate's sync downloads that link and asks semantic scholar
/// nothing.
#[test]
fn a_semantic_scholar_paper_is_pinned_at_its_open_pdf_without_a_key() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(Vec::new())?;
    sandbox.allow(&routes.url);
    routes.change(vec![
        (
            s2_paper("DOI:10.1371/journal.pcbi.1003285"),
            s2_json(&s2_open(&routes)?),
        ),
        ("/plos/printable.pdf".to_owned(), file(PDF, "\"p\"")),
    ]);
    let output = command(
        &sandbox,
        &routes,
        &["add", "s2:DOI:10.1371/journal.pcbi.1003285"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        !without_copy_warning(&stderr).contains("warning"),
        "{stderr}"
    );
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains(
            "[datasets.ten-simple-rules]\nref = \"s2:DOI:10.1371/journal.pcbi.1003285\"\n"
        ),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for expected in [
        "resolved = \"s2:12b056fd90c1300c44c4a8cff9f01f0c7a75cab4\"".to_owned(),
        "title = \"Ten Simple Rules for Reproducible Computational Research\"".to_owned(),
        "license = \"CC-BY\"".to_owned(),
        "doi = \"10.1371/journal.pcbi.1003285\"".to_owned(),
        format!(
            "{{ path = \"journal.pcbi.1003285.pdf\", size = {}, sha256 = \"{}\", url = \"{}/plos/printable.pdf\" }}",
            PDF.len(),
            sha256_hex(PDF),
            routes.url
        ),
    ] {
        assert!(lock.contains(&expected), "{expected}\n{lock}");
    }
    assert_eq!(
        fs::read(work.join("data/ten-simple-rules/journal.pcbi.1003285.pdf"))?,
        PDF
    );
    assert!(
        routes
            .requests()
            .iter()
            .all(|head| !head.contains("x-api-key")),
        "no key went"
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
    assert_eq!(routes.targets()[listed..], ["/plos/printable.pdf"]);
    Ok(())
}

/// a paper semantic scholar links no pdf of is refused, naming the page that says where it is.
#[test]
fn a_semantic_scholar_paper_without_an_open_pdf_is_refused_with_its_page() -> Result {
    let sandbox = Sandbox::new()?;
    let id = "240bf8044739451452d2c30780d7d9f031d2d89c";
    let routes = Routes::start(vec![(s2_paper(id), s2_json(S2_CLOSED))])?;
    let output = command(&sandbox, &routes, &["--json", "add", &format!("s2:{id}")]).output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "reference.unresolved");
    assert_eq!(
        error["message"],
        format!("semantic scholar links no open-access pdf of \"Deep learning\" (s2:{id})")
    );
    assert_eq!(
        error["help"],
        format!(
            "its page, https://www.semanticscholar.org/paper/{id}, says where it is published, which may need a subscription"
        )
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// every program without a key shares one pool of semantic scholar's requests, so a refusal for
/// load ends with a suggestion of a key, whose rate is its own.
#[test]
fn a_busy_semantic_scholar_suggests_a_key() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        s2_paper("CorpusId:2180281"),
        reply(
            "429 Too Many Requests",
            &[("Content-Type", "application/json")],
            br#"{"message": "Too Many Requests. Please wait and try again or apply for a key for higher rate limits. https://www.semanticscholar.org/product/api#api-key-form", "code": "429"}"#,
        ),
    )])?;
    let output = command(&sandbox, &routes, &["--json", "add", "s2:CorpusId:2180281"])
        .env("FETCHLOOM_RETRIES", "0")
        .output()?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "network.rate_limited");
    assert_eq!(
        error["help"],
        "every program without a key shares one pool of semantic scholar's requests: run `fetchloom --json add s2:CorpusId:2180281` again later, or get a free key at https://www.semanticscholar.org/product/api#api-key-form and sign in with `fetchloom auth login s2`, for a rate of your own"
    );
    Ok(())
}

/// semantic scholar is searched only when named, by relevance, each paper by its corpus id.
#[test]
fn semantic_scholar_is_searched_when_named() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![(
        format!(
            "/api.semanticscholar.org/graph/v1/paper/search?query=attention+is+all+you+need&limit=3&{S2_FIELDS}"
        ),
        s2_json(S2_SEARCH),
    )])?;
    let output = command(
        &sandbox,
        &routes,
        &["search", "attention is all you need", "--source", "s2"],
    )
    .output()?;
    let stderr = text(&output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains("Searching semantic scholar for \"attention is all you need\""),
        "{stderr}"
    );
    let stdout = text(&output.stdout)?;
    assert_eq!(
        stdout.lines().next(),
        Some("s2:CorpusId:13756489\tAttention is All you Need"),
        "{stdout}"
    );
    assert_eq!(stdout.lines().count(), 3, "{stdout}");
    Ok(())
}

const S2_RELEASE: &[u8] = include_bytes!("../fixtures/sources/s2/release.json");
const S2_DATASET: &[u8] = include_bytes!("../fixtures/sources/s2/dataset.json");
/// where the stand-in for semantic scholar's API answers about the newest release, and lists the
/// links to its tldrs files.
const S2_RELEASE_AT: &str = "/api.semanticscholar.org/datasets/v1/release/latest";
const S2_LINKS_AT: &str = "/api.semanticscholar.org/datasets/v1/release/2026-11-10/dataset/tldrs";
/// a key that stands in for the one semantic scholar gives.
const S2_KEY: &str = "s2-placeholder-key";

/// the names of the recorded dataset's first `count` files.
fn s2_names(count: usize) -> Result<Vec<String>> {
    let answer: serde_json::Value = serde_json::from_slice(S2_DATASET)?;
    Ok(answer["files"]
        .as_array()
        .into_iter()
        .flatten()
        .take(count)
        .filter_map(|link| {
            let link = link.as_str()?;
            let path = link.split('?').next()?;
            Some(path.rsplit('/').next()?.to_owned())
        })
        .collect())
}

/// the API's answer that links each of `names`, signed with `signature` and expiring at `expires`,
/// and where the stand-in for S3 serves each of those links.
fn s2_links(names: &[String], signature: &str, expires: u64) -> (Vec<u8>, Vec<String>) {
    let query = format!(
        "AWSAccessKeyId=PLACEHOLDER&Signature={signature}&x-amz-security-token=PLACEHOLDER&Expires={expires}"
    );
    let files: Vec<String> = names
        .iter()
        .map(|name| {
            format!("https://ai2-s2ag.s3.amazonaws.com/staging/2026-11-10/tldrs/{name}?{query}")
        })
        .collect();
    let targets = names
        .iter()
        .map(|name| format!("/ai2-s2ag.s3.amazonaws.com/staging/2026-11-10/tldrs/{name}?{query}"))
        .collect();
    let body = serde_json::to_vec(&serde_json::json!({"name": "tldrs", "files": files}))
        .unwrap_or_default();
    (body, targets)
}

/// how many requests asked for `target`.
fn asked(routes: &Routes, target: &str) -> usize {
    routes
        .targets()
        .iter()
        .filter(|asked| asked.as_str() == target)
        .count()
}

/// without a key the release still names the dataset, so the refusal names the key's page and
/// the sign-in, and nothing changes.
#[test]
fn a_semantic_scholar_dataset_without_a_key_is_refused_naming_the_sign_in() -> Result {
    let sandbox = Sandbox::new()?;
    let routes = Routes::start(vec![
        (S2_RELEASE_AT.to_owned(), s2_json(S2_RELEASE)),
        (
            S2_LINKS_AT.to_owned(),
            reply(
                "401 Unauthorized",
                &[("Content-Type", "application/json")],
                br#"{"error":"A valid API key is required"}"#,
            ),
        ),
    ])?;
    let output = command(&sandbox, &routes, &["--json", "add", "s2:tldrs"]).output()?;
    assert_eq!(output.status.code(), Some(40), "{}", text(&output.stderr)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "auth.required");
    assert_eq!(
        error["message"],
        "semantic scholar gives the files of s2:tldrs@2026-11-10 only for an API key"
    );
    assert_eq!(
        error["help"],
        "get a free key at https://www.semanticscholar.org/product/api#api-key-form, then sign in with `fetchloom auth login s2`, or set S2_API_KEY"
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

/// data.lock keeps each file by the API's listing and the file's name, never a signed link, so a
/// teammate's sync asks the API for fresh links and downloads each by its name.
#[test]
fn a_semantic_scholar_dataset_is_pinned_by_its_files_names_and_each_sync_asks_fresh_links() -> Result
{
    let sandbox = Sandbox::new()?;
    let names = s2_names(2)?;
    let (links, targets) = s2_links(&names, "PLACEHOLDER", 4_102_444_800);
    let parts: [&[u8]; 2] = [b"first part\n", b"second part\n"];
    let mut served = vec![
        (S2_RELEASE_AT.to_owned(), s2_json(S2_RELEASE)),
        (S2_LINKS_AT.to_owned(), s2_json(&links)),
    ];
    for (target, part) in targets.iter().zip(parts) {
        served.push((target.clone(), file(part, "\"s3\"")));
    }
    let routes = Routes::start(served)?;
    let output = command(&sandbox, &routes, &["add", "s2:tldrs"])
        .env("S2_API_KEY", S2_KEY)
        .output()?;
    let (stdout, stderr) = (text(&output.stdout)?, text(&output.stderr)?);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        !stdout.contains(S2_KEY) && !stderr.contains(S2_KEY),
        "{stderr}"
    );
    let work = sandbox.work();
    let manifest = fs::read_to_string(work.join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.tldrs]\nref = \"s2:tldrs\"\n"),
        "{manifest}"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    for expected in [
        "resolved = \"s2:tldrs@2026-11-10\"".to_owned(),
        "title = \"A short natural-language summary of the contents of a paper.\"".to_owned(),
        "license = \"ODC-By-1.0\"".to_owned(),
        "access = \"login\"".to_owned(),
        format!(
            "url = \"https://api.semanticscholar.org/datasets/v1/release/2026-11-10/dataset/tldrs#{}\"",
            names[0]
        ),
    ] {
        assert!(lock.contains(&expected), "{expected}\n{lock}");
    }
    for secret in ["Signature", "PLACEHOLDER", "Expires", "amazonaws"] {
        assert!(
            !lock.contains(secret) && !manifest.contains(secret),
            "{secret}"
        );
    }
    assert_eq!(fs::read(work.join("data/tldrs").join(&names[0]))?, parts[0]);
    assert_eq!(fs::read(work.join("data/tldrs").join(&names[1]))?, parts[1]);

    let teammate = sandbox.folder("teammate")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(work.join(name), teammate.join(name))?;
    }
    let listed = routes.targets().len();
    let synced = command(&sandbox, &routes, &["sync", "--locked"])
        .current_dir(&teammate)
        .env("FETCHLOOM_CACHE", sandbox.folder("teammate-cache")?)
        .env("S2_API_KEY", S2_KEY)
        .output()?;
    ok(&synced)?;
    let mut asked_since = routes.targets()[listed..].to_vec();
    asked_since.sort();
    let mut expected = vec![
        S2_LINKS_AT.to_owned(),
        targets[0].clone(),
        targets[1].clone(),
    ];
    expected.sort();
    assert_eq!(asked_since, expected, "one listing, then each file by name");
    Ok(())
}

/// a link past its `Expires` is never used: the listing is asked again, and its fresh link
/// downloads the file.
#[test]
fn a_semantic_scholar_link_past_its_expiry_is_asked_for_again() -> Result {
    let sandbox = Sandbox::new()?;
    let names = s2_names(1)?;
    let (stale, stale_targets) = s2_links(&names, "PLACEHOLDER", 1_700_000_000);
    let (fresh, fresh_targets) = s2_links(&names, "PLACEHOLDER", 4_102_444_800);
    let routes = Routes::start(vec![
        (S2_RELEASE_AT.to_owned(), s2_json(S2_RELEASE)),
        (S2_LINKS_AT.to_owned(), s2_json(&fresh)),
        (fresh_targets[0].clone(), file(b"part\n", "\"s3\"")),
    ])?;
    routes.turns(S2_LINKS_AT, vec![s2_json(&stale)]);
    let output = command(&sandbox, &routes, &["add", "s2:tldrs"])
        .env("S2_API_KEY", S2_KEY)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(asked(&routes, S2_LINKS_AT), 2);
    assert_eq!(asked(&routes, &stale_targets[0]), 0);
    assert_eq!(asked(&routes, &fresh_targets[0]), 1);
    Ok(())
}

/// S3's answer for a link whose time ran out, as AWS words it.
const S3_EXPIRED: &[u8] = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Error><Code>AccessDenied</Code><Message>Request has expired</Message><Expires>2026-11-20T13:36:33Z</Expires><ServerTime>2026-11-20T13:37:05Z</ServerTime><RequestId>VBVWVFN0V1TNHWXR</RequestId></Error>";

/// adds `count` files of the recorded dataset, at most `jobs` requests at once, whose first links
/// S3 calls expired, as it does once the session that signed them ends: each download asks the
/// listing again and its fresh link downloads the file. a download that kept its place while it
/// asked would wait for ever once every place is taken, so the run has a short deadline.
fn expired_links_are_asked_for_again(count: usize, jobs: &str) -> Result {
    let sandbox = Sandbox::new()?;
    let names = s2_names(count)?;
    let (old, old_targets) = s2_links(&names, "PLACEHOLDER", 4_102_444_800);
    let (new, new_targets) = s2_links(&names, "PLACEHOLDER-AGAIN", 4_102_444_800);
    let mut served = vec![
        (S2_RELEASE_AT.to_owned(), s2_json(S2_RELEASE)),
        (S2_LINKS_AT.to_owned(), s2_json(&new)),
    ];
    for (at, (old, new)) in old_targets.iter().zip(&new_targets).enumerate() {
        served.push((
            old.clone(),
            reply(
                "403 Forbidden",
                &[("Content-Type", "application/xml")],
                S3_EXPIRED,
            ),
        ));
        served.push((
            new.clone(),
            file(format!("part {at}\n").as_bytes(), "\"s3\""),
        ));
    }
    let routes = Routes::start(served)?;
    routes.turns(S2_LINKS_AT, vec![s2_json(&old)]);
    let output = command(&sandbox, &routes, &["add", "s2:tldrs"])
        .env("S2_API_KEY", S2_KEY)
        .env("FETCHLOOM_JOBS", jobs)
        .deadline(std::time::Duration::from_secs(30))
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    // downloads asking at once may each ask the listing again
    let listed = asked(&routes, S2_LINKS_AT);
    assert!((2..=count + 1).contains(&listed), "{listed} listings");
    for target in old_targets.iter().chain(&new_targets) {
        assert_eq!(asked(&routes, target), 1, "{target}");
    }
    for (at, name) in names.iter().enumerate() {
        assert_eq!(
            fs::read(sandbox.work().join("data/tldrs").join(name))?,
            format!("part {at}\n").as_bytes()
        );
    }
    Ok(())
}

#[test]
fn a_semantic_scholar_link_s3_calls_expired_is_asked_for_again() -> Result {
    expired_links_are_asked_for_again(1, "1")
}

#[test]
fn semantic_scholar_links_expiring_together_are_asked_for_again() -> Result {
    expired_links_are_asked_for_again(2, "2")
}
