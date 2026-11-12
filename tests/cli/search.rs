//! catalog discovery and safe unattended behavior against local catalog responses.
#![cfg(debug_assertions)]

use crate::support::{Result, Sandbox, Server, command_help, json, reply, text};
use std::process::Command;

const HF: &[u8] = include_bytes!("../fixtures/sources/search/hf.json");
const KAGGLE: &[u8] = include_bytes!("../fixtures/sources/search/kaggle.json");
const ZENODO: &[u8] = include_bytes!("../fixtures/sources/search/zenodo.json");
const ENA: &[u8] = include_bytes!("../fixtures/sources/search/ena.json");
const GEO_SEARCH: &[u8] = include_bytes!("../fixtures/sources/search/geo-search.json");
const GEO_SUMMARY: &[u8] = include_bytes!("../fixtures/sources/search/geo-summary.json");
const DANDI: &[u8] = include_bytes!("../fixtures/sources/search/dandi.json");
const OPENNEURO: &[u8] = include_bytes!("../fixtures/sources/search/openneuro.json");

/// what ENA, GEO, dandi and openneuro answer a search that finds nothing, by the variable that
/// stands in for each.
const NOTHING_MORE: [(&str, &[&[u8]]); 4] = [
    ("FETCHLOOM_TEST_ENA_API", &[br#"{"entries":[]}"#]),
    (
        "FETCHLOOM_TEST_GEO_API",
        &[br#"{"esearchresult":{"idlist":[]}}"#],
    ),
    ("FETCHLOOM_TEST_DANDI_API", &[br#"{"results":[]}"#]),
    (
        "FETCHLOOM_TEST_OPENNEURO_API",
        &[br#"{"data":{"advancedSearch":{"edges":[]}}}"#],
    ),
];

/// a catalog that answers each request with the next of `answers`, as JSON.
fn serve(answers: &[&[u8]]) -> Result<Server> {
    Server::start(
        "/",
        answers
            .iter()
            .map(|body| reply("200 OK", &[("Content-Type", "application/json")], body))
            .collect(),
    )
}

/// ENA, GEO, dandi and openneuro, each answering `answers`, by the variable that stands in for
/// each.
fn others(answers: [(&'static str, &[&[u8]]); 4]) -> Result<Vec<(&'static str, Server)>> {
    answers
        .into_iter()
        .map(|(variable, answers)| Ok((variable, serve(answers)?)))
        .collect()
}

struct Catalogs {
    hf: Server,
    kaggle: Server,
    zenodo: Server,
    others: Vec<(&'static str, Server)>,
}

impl Catalogs {
    /// hugging face, kaggle and zenodo answering these, and the other catalogs finding nothing.
    fn new(hf: &[u8], kaggle: &[u8], zenodo: &[u8]) -> Result<Self> {
        Ok(Self {
            hf: serve(&[hf])?,
            kaggle: serve(&[kaggle])?,
            zenodo: serve(&[zenodo])?,
            others: others(NOTHING_MORE)?,
        })
    }

    fn command(&self, sandbox: &Sandbox, args: &[&str]) -> Command {
        let mut command = sandbox.command(args);
        command
            .env("FETCHLOOM_TEST_HF_API", &self.hf.url)
            .env("FETCHLOOM_TEST_KAGGLE_API", &self.kaggle.url)
            .env("FETCHLOOM_TEST_ZENODO_API", &self.zenodo.url);
        for (variable, catalog) in &self.others {
            command.env(variable, &catalog.url);
        }
        command
    }
}

#[cfg(target_os = "linux")]
#[test]
fn search_at_a_terminal_says_how_to_add_a_match() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let command = catalogs.command(&sandbox, &["search", "iris"]);
    let Some(screen) = sandbox.on_a_terminal_running(&command)? else {
        return Ok(());
    };
    assert!(
        screen.contains(
            "next: `fetchloom add hf:datasets/scikit-learn/iris` puts the first match in a project"
        ),
        "{screen}"
    );
    // no row is numbered, since `fetchloom add 1` would search for "1"
    assert!(
        screen.contains("\n            hf:datasets/scikit-learn/iris "),
        "{screen}"
    );
    assert!(!screen.contains("          1 "), "{screen}");
    Ok(())
}

/// with `TERM=dumb`, the way a screen reader's user runs fetchloom, plain words list the matches
/// first, in rows that fit the terminal, then say why nothing was added, and exit 10.
#[cfg(target_os = "linux")]
#[test]
fn a_dumb_terminal_lists_the_matches_first_in_its_width() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let mut command = catalogs.command(&sandbox, &["add", "iris"]);
    command.env("TERM", "dumb");
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &command, "rows 40 cols 60", "", "")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(10), "{screen}");
    let found = screen
        .find("Found 9 datasets for \"iris\"; add one by its id:\n")
        .ok_or_else(|| format!("no list first: {screen}"))?;
    let error = screen.find("error: ").ok_or("no error")?;
    assert!(found < error, "{screen}");
    let rows: Vec<&str> = screen[found..error]
        .lines()
        .skip(1)
        .filter(|line| line.starts_with("            "))
        .collect();
    assert!(rows.len() >= 9, "{screen}");
    assert!(rows.iter().all(|row| row.chars().count() <= 60), "{screen}");
    assert!(
        rows.iter().any(|row| row
            .trim_start()
            .starts_with("hf:datasets/scikit-learn/iris ")),
        "{screen}"
    );
    Ok(())
}

#[test]
fn unattended_add_refuses_to_guess_even_with_yes() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let output = catalogs
        .command(&sandbox, &["add", "iris", "--yes", "--json"])
        .output()?;
    assert_eq!(
        output.status.code(),
        Some(10),
        "stderr: {}\nstdout: {}",
        text(&output.stderr)?,
        text(&output.stdout)?
    );
    let document = json(&output)?;
    assert_eq!(document["command"], "add");
    assert_eq!(
        document["errors"][0]["kind"], "reference.ambiguous",
        "{document}"
    );
    assert_eq!(
        document["result"]["candidates"].as_array().map(Vec::len),
        Some(9),
        "{document}"
    );
    assert_eq!(
        document["result"]["candidates"][0]["reference"],
        "hf:datasets/scikit-learn/iris"
    );
    assert!(
        document["errors"][0]["help"]
            .as_str()
            .unwrap_or_default()
            .contains("fetchloom add hf:datasets/scikit-learn/iris")
    );
    assert_eq!(output.stderr, b"");
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join("data.lock").exists());
    Ok(())
}

#[test]
fn ambiguous_add_leaves_an_existing_manifest_unchanged() -> Result {
    let sandbox = Sandbox::new()?;
    let manifest = "# keep this comment\n[datasets]\n";
    std::fs::write(sandbox.work().join("data.toml"), manifest)?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let output = catalogs.command(&sandbox, &["add", "iris"]).output()?;
    assert_eq!(output.status.code(), Some(10));
    assert!(text(&output.stderr)?.contains("will not guess which dataset you mean"));
    assert_eq!(
        std::fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    Ok(())
}

#[test]
fn unattended_get_suggests_get_and_accepts_an_owner_name_as_words() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let output = catalogs
        .command(&sandbox, &["get", "uciml/iris", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(10));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "reference.ambiguous");
    assert!(
        document["errors"][0]["help"]
            .as_str()
            .unwrap_or_default()
            .contains("fetchloom get hf:datasets/scikit-learn/iris")
    );
    assert!(catalogs.kaggle.requests()[0].contains("search=uciml%2firis"));
    assert!(!sandbox.work().join("iris").exists());
    Ok(())
}

#[test]
fn search_prints_references_on_stdout_even_when_quiet() -> Result {
    let sandbox = Sandbox::new()?;
    for quiet in [false, true] {
        let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
        let args = if quiet {
            vec!["search", "iris", "-q"]
        } else {
            vec!["search", "iris"]
        };
        let output = catalogs.command(&sandbox, &args).output()?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        let stdout = text(&output.stdout)?;
        assert_eq!(
            stdout.lines().count(),
            9,
            "stderr: {}",
            text(&output.stderr)?
        );
        let first: Vec<&str> = stdout
            .lines()
            .filter_map(|line| line.split('\t').next())
            .take(3)
            .collect();
        assert_eq!(
            first,
            [
                "hf:datasets/scikit-learn/iris",
                "kaggle:uciml/iris",
                "zenodo:23233774"
            ]
        );
        assert!(
            stdout.contains("kaggle:uciml/iris\tIris Species\t3.6 KiB\n"),
            "{stdout}"
        );
        assert!(stdout.contains("hf:datasets/scikit-learn/iris\tscikit-learn/iris\t18.1 KiB\n"));
        let stderr = text(&output.stderr)?;
        if quiet {
            assert_eq!(stderr, "");
        } else {
            assert!(stderr.contains(
                "Searching hugging face, kaggle, zenodo, ENA, GEO, dandi and openneuro for \"iris\""
            ));
            assert!(stderr.contains("Found"));
            for hint in [
                "each site's first 3 matches are shown; `--limit 20` shows up to 20 from each",
                "sizes are as each site lists them, and a zipped download is larger once unpacked",
            ] {
                assert!(stderr.contains(hint), "{stderr}");
            }
            // nobody watches stderr here, so stdout alone carries the list
            assert!(!stderr.contains("Iris Species"));
        }
    }
    Ok(())
}

#[test]
fn source_and_limit_narrow_the_search() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let output = catalogs
        .command(
            &sandbox,
            &[
                "search", "iris", "--source", "kaggle", "--limit", "2", "--json",
            ],
        )
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let candidates = document["result"]["candidates"].as_array().cloned();
    assert_eq!(candidates.as_ref().map(Vec::len), Some(2), "{document}");
    assert_eq!(catalogs.hf.requests().len(), 0);
    assert_eq!(catalogs.zenodo.requests().len(), 0);
    let output = catalogs
        .command(&sandbox, &["search", "iris", "--source", "figshare"])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    Ok(())
}

/// ENA, GEO, dandi and openneuro, each answering the matches recorded from it.
fn finding() -> Result<Vec<(&'static str, Server)>> {
    others([
        ("FETCHLOOM_TEST_ENA_API", &[ENA]),
        ("FETCHLOOM_TEST_GEO_API", &[GEO_SEARCH, GEO_SUMMARY]),
        ("FETCHLOOM_TEST_DANDI_API", &[DANDI]),
        ("FETCHLOOM_TEST_OPENNEURO_API", &[OPENNEURO]),
    ])
}

/// ENA, GEO, dandi and openneuro are searched with the others, each through its own API: GEO
/// finds series, then asks for their titles, and openneuro gets the header its API asks of a
/// query sent by GET.
#[test]
fn ena_geo_dandi_and_openneuro_are_searched_with_the_others() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs {
        others: finding()?,
        ..Catalogs::new(b"[]", b"[]", br#"{"hits":{"hits":[]}}"#)?
    };
    let output = catalogs.command(&sandbox, &["search", "iris"]).output()?;
    let stderr = text(&output.stderr)?;
    assert!(output.status.success(), "{stderr}");
    assert!(
        stderr.contains(
            "Searching hugging face, kaggle, zenodo, ENA, GEO, dandi and openneuro for \"iris\""
        ),
        "{stderr}"
    );
    let stdout = text(&output.stdout)?;
    let references: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.split('\t').next())
        .collect();
    assert_eq!(
        references,
        [
            "ena:SRP455996",
            "geo:GSE318345",
            "dandi:000623",
            "openneuro:ds001037",
            "ena:SRP630995",
            "geo:GSE311178",
            "dandi:000044",
            "openneuro:ds007046",
            "ena:SRP266745",
            "geo:GSE320313",
            "dandi:001361",
        ],
        "{stderr}"
    );
    let asked = |at: usize| catalogs.others[at].1.requests();
    assert!(
        asked(0)[0].starts_with(
            "get /ebisearch/ws/rest/sra-study?query=iris&size=3&format=json&fields=name%2cdescription "
        ),
        "{:?}",
        asked(0)
    );
    let geo = asked(1);
    assert_eq!(geo.len(), 2, "{geo:?}");
    assert!(
        geo[0].starts_with("get /entrez/eutils/esearch.fcgi?db=gds&term=iris+and+gse%5betyp%5d&"),
        "{geo:?}"
    );
    assert!(
        geo[1].starts_with(
            "get /entrez/eutils/esummary.fcgi?db=gds&id=200318345%2c200311178%2c200320313&"
        ),
        "{geo:?}"
    );
    assert!(
        asked(2)[0].starts_with("get /api/dandisets/?search=iris&page_size=3&ordering=-stars "),
        "{:?}",
        asked(2)
    );
    let openneuro = asked(3);
    assert!(
        openneuro[0].starts_with("get /crn/graphql?query="),
        "{openneuro:?}"
    );
    assert!(
        openneuro[0].contains("\r\napollo-require-preflight: true\r\n"),
        "{openneuro:?}"
    );
    Ok(())
}

/// `--source` names ENA, GEO, dandi and openneuro as references do, and each match carries the
/// facts its catalog gives.
#[test]
fn source_names_ena_geo_dandi_and_openneuro_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs {
        others: finding()?,
        ..Catalogs::new(HF, KAGGLE, ZENODO)?
    };
    let output = catalogs
        .command(
            &sandbox,
            &[
                "search",
                "iris",
                "--source",
                "ena,geo",
                "--source",
                "dandi,openneuro",
                "--json",
            ],
        )
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let candidates = &document["result"]["candidates"];
    assert_eq!(candidates.as_array().map(Vec::len), Some(11), "{document}");
    assert_eq!(
        candidates[0],
        serde_json::json!({
            "reference": "ena:SRP455996", "title": "Iris germanica Assembly", "source": "ENA",
            "size": null, "downloads": null, "note": null,
        })
    );
    assert_eq!(candidates[1]["source"], "GEO");
    assert_eq!(
        candidates[2],
        serde_json::json!({
            "reference": "dandi:000623",
            "title": "Data for: Multimodal single-neuron, intracranial EEG, and fMRI brain responses during movie watching in human patients",
            "source": "dandi", "size": 27_744_749_325_u64, "downloads": null, "note": null,
        })
    );
    assert_eq!(
        candidates[3],
        serde_json::json!({
            "reference": "openneuro:ds001037", "title": "The brain of Chris",
            "source": "openneuro", "size": 8_600_815, "downloads": 42, "note": null,
        })
    );
    for catalog in [&catalogs.hf, &catalogs.kaggle, &catalogs.zenodo] {
        assert_eq!(catalog.requests(), Vec::<String>::new());
    }
    Ok(())
}

#[test]
fn search_json_carries_metadata_and_joins_words() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    let output = catalogs
        .command(&sandbox, &["search", "fashion", "mnist", "--json"])
        .output()?;
    assert!(output.status.success(), "{}", text(&output.stderr)?);
    assert_eq!(output.stderr, b"");
    let document = json(&output)?;
    assert_eq!(document["command"], "search");
    assert_eq!(document["status"], "ok");
    assert_eq!(
        document["result"]["candidates"][1],
        serde_json::json!({
            "reference":"kaggle:uciml/iris", "title":"Iris Species", "source":"kaggle",
            "size":3687, "downloads":958_555, "note":null,
        })
    );
    assert!(catalogs.hf.requests()[0].contains("search=fashion+mnist"));
    assert!(catalogs.zenodo.requests()[0].contains("q=title%3a%28fashion+and+mnist%29"));
    Ok(())
}

#[test]
fn zenodo_preserves_whitespace_in_quoted_words() -> Result {
    let sandbox = Sandbox::new()?;
    for words in ["fashion mnist", "fashion\t mnist", "fashion\u{a0}mnist"] {
        let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
        let output = catalogs
            .command(&sandbox, &["search", words, "--source", "zenodo", "--json"])
            .output()?;
        assert!(output.status.success(), "{}", text(&output.stderr)?);
        assert_eq!(json(&output)?["status"], "ok");
        let requests = catalogs.zenodo.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0].contains("q=title%3a%28fashion+and+mnist%29"),
            "{words:?}: {requests:?}"
        );
        assert_eq!(catalogs.hf.requests(), Vec::<String>::new());
        assert_eq!(catalogs.kaggle.requests(), Vec::<String>::new());
    }
    Ok(())
}

#[test]
fn zero_matches_explain_how_to_try_again() -> Result {
    let sandbox = Sandbox::new()?;
    for command in ["add", "get", "search"] {
        let catalogs = Catalogs::new(b"[]", b"[]", br#"{"hits":{"hits":[]}}"#)?;
        let output = catalogs
            .command(&sandbox, &[command, "nothing", "--json"])
            .output()?;
        assert_eq!(output.status.code(), Some(10));
        let document = json(&output)?;
        assert_eq!(document["errors"][0]["kind"], "reference.unresolved");
        assert_eq!(
            document["errors"][0]["message"],
            "found no dataset matching \"nothing\""
        );
        assert!(
            document["errors"][0]["help"]
                .as_str()
                .unwrap_or_default()
                .contains("try other words")
        );
    }
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

#[test]
fn a_failed_catalog_is_a_warning_in_json() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, b"broken json", ZENODO)?;
    let output = catalogs
        .command(&sandbox, &["search", "iris", "--json"])
        .output()?;
    assert!(output.status.success());
    let document = json(&output)?;
    assert_eq!(
        document["result"]["candidates"].as_array().map(Vec::len),
        Some(6),
        "{document}"
    );
    assert!(
        document["warnings"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .starts_with("kaggle search failed:")
    );
    assert_eq!(document["warnings"][0]["kind"], "search.partial");
    Ok(())
}

/// the first ctrl-c while one catalog still searches ends add there, as it ends search: a stop,
/// exit 130, with no warning about the catalog it cut short and no choice from the matches the
/// others found.
#[cfg(unix)]
#[test]
fn a_first_stop_while_a_catalog_searches_ends_add() -> Result {
    use std::io::BufRead as _;

    use crate::support::Part;

    let sandbox = Sandbox::new()?;
    let (hf, kaggle) = (serve(&[HF])?, serve(&[KAGGLE])?);
    let (_release, held) = std::sync::mpsc::channel();
    let zenodo = Server::start("/", vec![vec![Part::Wait(held)]])?;
    let others = others(NOTHING_MORE)?;
    let mut command = sandbox.command(&["-vv", "add", "iris"]);
    for (variable, catalog) in &others {
        command.env(variable, &catalog.url);
    }
    let mut child = command
        .env("FETCHLOOM_TEST_HF_API", &hf.url)
        .env("FETCHLOOM_TEST_KAGGLE_API", &kaggle.url)
        .env("FETCHLOOM_TEST_ZENODO_API", &zenodo.url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let stream = child.stderr.take().ok_or("no stderr")?;
    let (heard, lines) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut told = String::new();
        for line in std::io::BufReader::new(stream)
            .lines()
            .map_while(std::io::Result::ok)
        {
            let _ = heard.send(line.clone());
            told.push_str(&line);
            told.push('\n');
        }
        told
    });
    let mut answers = 0;
    while answers < 2 {
        let line = lines.recv_timeout(std::time::Duration::from_mins(1))?;
        if line.contains("net::api: answer") {
            answers += 1;
        }
    }
    zenodo.wait_for_requests(1)?;
    let sent = std::process::Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()?;
    assert!(sent.success());
    let status = child.wait()?;
    let stderr = reader.join().map_err(|_| "reading stderr failed")?;
    assert_eq!(status.code(), Some(130), "{stderr}");
    assert!(stderr.contains("Stopped"), "{stderr}");
    assert!(!stderr.contains("warning:"), "{stderr}");
    assert!(!sandbox.work().join("data.toml").exists(), "{stderr}");
    Ok(())
}

#[test]
fn offline_search_uses_the_existing_offline_error() -> Result {
    let sandbox = Sandbox::new()?;
    for command in ["add", "get", "search"] {
        let output = sandbox.run(&[command, "iris", "--offline", "--json"])?;
        assert_eq!(output.status.code(), Some(20));
        assert_eq!(json(&output)?["errors"][0]["kind"], "offline.missing");
    }
    Ok(())
}

#[test]
fn search_has_lowercase_help_and_is_a_help_topic() -> Result {
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["search", "--help"])?;
    assert!(output.status.success());
    assert!(text(&output.stdout)?.contains("usage: fetchloom search [options] <words>..."));
    assert_eq!(output.stdout, sandbox.run(&["help", "search"])?.stdout);
    let help = text(&output.stdout)?;
    command_help().bind(|| insta::assert_snapshot!(help));
    Ok(())
}

#[test]
fn even_one_match_needs_a_person_to_choose() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(
        br#"[{"id":"scikit-learn/iris"}]"#,
        b"[]",
        br#"{"hits":{"hits":[]}}"#,
    )?;
    let output = catalogs
        .command(&sandbox, &["add", "iris", "--yes", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(10));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "reference.ambiguous");
    assert_eq!(
        document["result"]["candidates"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(!sandbox.work().join("data.toml").exists());
    Ok(())
}

#[test]
fn server_titles_are_clipped_and_cannot_control_the_terminal() -> Result {
    let sandbox = Sandbox::new()?;
    let kaggle = serde_json::to_vec(&serde_json::json!([{
        "ref":"uciml/iris", "title": format!("iris\x1b[2J {}", "long ".repeat(40))
    }]))?;
    let catalogs = Catalogs::new(b"[]", &kaggle, br#"{"hits":{"hits":[]}}"#)?;
    let output = catalogs.command(&sandbox, &["get", "iris"]).output()?;
    assert_eq!(output.status.code(), Some(10));
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains('\x1b'));
    // a control character in a title becomes a space, so it neither acts nor shifts the columns
    assert!(stderr.contains("iris [2J long"), "{stderr}");
    assert!(!stderr.contains(&"long ".repeat(15)));
    Ok(())
}

#[test]
fn prefixed_references_and_bare_dois_are_not_search_words() -> Result {
    let sandbox = Sandbox::new()?;
    for reference in ["zenodo:abc", "https://example.org/iris", "10.5281/zenodo.1"] {
        let output = sandbox.run(&["get", reference, "--offline"])?;
        assert!(!output.status.success());
        assert!(!text(&output.stderr)?.contains("Searching"));
    }
    Ok(())
}

#[test]
fn a_local_path_or_name_is_refused_without_a_request() -> Result {
    let sandbox = Sandbox::new()?;
    let catalogs = Catalogs::new(HF, KAGGLE, ZENODO)?;
    std::fs::write(sandbox.work().join("flowers.csv"), b"a,b\n")?;
    std::fs::create_dir(sandbox.work().join("iris"))?;
    let absolute = sandbox.work().join("flowers.csv").display().to_string();
    // add imports what a path names, and finds nothing at these
    for (args, code, kind) in [
        (["add", "~/flowers.csv"], 50, "local.io"),
        (["add", "C:\\data\\flowers.csv"], 50, "local.io"),
        (["get", absolute.as_str()], 10, "reference.invalid"),
        (["get", "./flowers.csv"], 10, "reference.invalid"),
        (["get", "file:///etc/hostname"], 10, "reference.invalid"),
        (["search", "./flowers.csv"], 2, "usage"),
        (["add", "iris"], 2, "usage"),
        (["get", "flowers.csv"], 2, "usage"),
    ] {
        let output = catalogs
            .command(&sandbox, &[&["--json"], &args[..]].concat())
            .output()?;
        let document = json(&output)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}: {document}");
        assert_eq!(document["errors"][0]["kind"], kind, "{args:?}");
        assert_eq!(document["work"]["requests"], 0, "{args:?}");
    }
    let path = json(
        &catalogs
            .command(&sandbox, &["--json", "get", "./flowers.csv"])
            .output()?,
    )?;
    assert_eq!(
        path["errors"][0]["message"],
        "\"./flowers.csv\" is a path on this machine, and fetchloom downloads data from links, DOIs and ids"
    );
    let name = json(
        &catalogs
            .command(&sandbox, &["--json", "add", "iris"])
            .output()?,
    )?;
    assert_eq!(
        name["errors"][0]["message"],
        "\"iris\" is a folder here, so fetchloom did not search for it"
    );
    assert_eq!(
        name["errors"][0]["help"],
        "run `fetchloom add ./iris` to pin it, or `fetchloom search iris` to search"
    );
    assert_eq!(
        name["errors"][0]["fix"],
        serde_json::Value::Null,
        "which of the two the person meant is theirs to say"
    );
    for catalog in [&catalogs.hf, &catalogs.kaggle, &catalogs.zenodo]
        .into_iter()
        .chain(catalogs.others.iter().map(|(_, catalog)| catalog))
    {
        assert_eq!(catalog.requests(), Vec::<String>::new());
    }
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join(".fetchloom").exists());
    Ok(())
}
