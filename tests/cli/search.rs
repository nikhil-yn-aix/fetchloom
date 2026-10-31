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
        assert!(
            stdout
                .starts_with("hf:datasets/scikit-learn/iris\nkaggle:uciml/iris\nzenodo:23233774\n")
        );
        let stderr = text(&output.stderr)?;
        if quiet {
            assert_eq!(stderr, "");
        } else {
            assert!(stderr.contains("Searching hugging face, kaggle and zenodo for \"iris\""));
            assert!(stderr.contains("Found"));
            assert!(stderr.contains("Iris Species"));
            assert!(stderr.contains("12k downloads"));
        }
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
    assert!(catalogs.zenodo.requests()[0].contains("q=title%3a%28fashion+mnist%29"));
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
    let output = catalogs.command(&sandbox, &["search", "iris"]).output()?;
    assert!(output.status.success());
    let stderr = text(&output.stderr)?;
    assert!(!stderr.contains('\x1b'));
    assert!(stderr.contains("\\u{1b}[2J"));
    assert!(!stderr.contains(&"long ".repeat(15)));
    assert_eq!(text(&output.stdout)?, "kaggle:uciml/iris\n");
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
