//! catalog discovery and safe unattended behavior against local catalog responses.
#![cfg(debug_assertions)]

use crate::support::{Result, Sandbox, Server, json, reply, text};
use std::process::Command;

const HF: &[u8] = include_bytes!("../fixtures/sources/search/hf.json");
const KAGGLE: &[u8] = include_bytes!("../fixtures/sources/search/kaggle.json");
const ZENODO: &[u8] = include_bytes!("../fixtures/sources/search/zenodo.json");

struct Catalogs {
    hf: Server,
    kaggle: Server,
    zenodo: Server,
}

impl Catalogs {
    fn new(hf: &[u8], kaggle: &[u8], zenodo: &[u8]) -> Result<Self> {
        let serve = |body: &[u8]| {
            Server::start(
                "/",
                vec![reply(
                    "200 OK",
                    &[("Content-Type", "application/json")],
                    body,
                )],
            )
        };
        Ok(Self {
            hf: serve(hf)?,
            kaggle: serve(kaggle)?,
            zenodo: serve(zenodo)?,
        })
    }

    fn command(&self, sandbox: &Sandbox, args: &[&str]) -> Command {
        let mut command = sandbox.command(args);
        command
            .env("FETCHLOOM_TEST_HF_API", &self.hf.url)
            .env("FETCHLOOM_TEST_KAGGLE_API", &self.kaggle.url)
            .env("FETCHLOOM_TEST_ZENODO_API", &self.zenodo.url);
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
            assert!(stderr.contains("Searching hugging face, kaggle and zenodo for \"iris\""));
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
        document["warnings"][0]
            .as_str()
            .unwrap_or_default()
            .starts_with("kaggle search failed:")
    );
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
    insta::assert_snapshot!(text(&output.stdout)?);
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
    for catalog in [&catalogs.hf, &catalogs.kaggle, &catalogs.zenodo] {
        assert_eq!(catalog.requests(), Vec::<String>::new());
    }
    assert!(!sandbox.work().join("data.toml").exists());
    assert!(!sandbox.work().join(".fetchloom").exists());
    Ok(())
}
