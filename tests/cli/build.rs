//! recipes declared in data.toml against servers on this machine: what each command does with
//! them, and that none of them runs a step.

use std::fs;

use serde_json::Value;

use crate::export::sha256;
use crate::project::{IRIS, PATH, added};
use crate::support::{Result, Sandbox, Server, file, json, text};

/// a recipe that makes `clean` from iris with clean.py.
const CLEAN: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"python3\", \"clean.py\", \"{iris}\", \"{out}\"]\ncode = [\"clean.py\"]\n";

/// a project with iris added from `server` and the recipe [`CLEAN`] beside it, with its code.
fn with_recipe(sandbox: &Sandbox, server: &Server) -> Result {
    added(sandbox, server)?;
    let manifest = sandbox.work().join("data.toml");
    let text = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{text}{CLEAN}"))?;
    fs::write(
        sandbox.work().join("clean.py"),
        "import sys\nprint(sys.argv)\n",
    )?;
    Ok(())
}

/// the bytes of clean.py that [`pin_clean`] pins by default.
const CODE: &[u8] = b"import sys\nprint(sys.argv)\n";

/// pins `clean` in data.lock as built from iris with clean.py holding `code` and the variables
/// `env`, one file in its result. the key is made here as the recipe key scheme defines it, apart
/// from fetchloom's own code: the JSON of the scheme, each input's content, the run, each code
/// file's hash and the variables.
fn pin_clean(sandbox: &Sandbox, code: &[u8], env: &[(&str, &str)]) -> Result {
    let content = sha256(format!("iris.csv\t{}\t{}\n", IRIS.len(), sha256(IRIS)).as_bytes());
    let pairs: Vec<String> = env
        .iter()
        .map(|(name, value)| format!("[\"{name}\",\"{value}\"]"))
        .collect();
    let key = sha256(
        format!(
            "{{\"scheme\":\"fetchloom-recipe-1\",\"from\":[[\"iris\",\"sha256:{content}\"]],\"run\":[\"python3\",\"clean.py\",\"{{iris}}\",\"{{out}}\"],\"code\":[[\"clean.py\",\"sha256:{}\"]],\"env\":[{}]}}",
            sha256(code),
            pairs.join(",")
        )
        .as_bytes(),
    );
    let tree = format!("clean.csv\t4\t{}\n", sha256(b"done"));
    let part_text = format!("path\tsize\tsha256\n{tree}");
    let listed = sha256(part_text.as_bytes());
    let part = format!("data.lock.d/clean.{}.tsv.zst", &listed[..12]);
    fs::create_dir_all(sandbox.work().join("data.lock.d"))?;
    fs::write(
        sandbox.work().join(&part),
        zstd::encode_all(part_text.as_bytes(), 9)?,
    )?;
    let variables: Vec<String> = env
        .iter()
        .map(|(name, value)| format!("{name} = \"{value}\""))
        .collect();
    let env = if env.is_empty() {
        String::new()
    } else {
        format!("env = {{ {} }}\n", variables.join(", "))
    };
    let table = format!(
        "\n[[dataset]]\nname = \"clean\"\nfrom = [\n  {{ name = \"iris\", content = \"sha256:{content}\" }},\n]\nrun = [\"python3\", \"clean.py\", \"{{iris}}\", \"{{out}}\"]\ncode = [\n  {{ path = \"clean.py\", sha256 = \"{}\" }},\n]\n{env}key = \"sha256:{key}\"\nlocked = \"2026-11-10T00:00:00Z\"\ntree = \"sha256:{}\"\nfiles_in = [\"{part}\"]\nfile_count = 1\nfile_bytes = 4\nfiles_sha256 = \"sha256:{listed}\"\n",
        sha256(code),
        sha256(tree.as_bytes()),
    );
    let lock = sandbox.work().join("data.lock");
    fs::write(&lock, format!("{}{table}", fs::read_to_string(&lock)?))?;
    Ok(())
}

/// the kind, message, details, help and fix of the error in a `--json` document.
fn error(document: &Value) -> (&str, &str, Vec<&str>, &str, &Value) {
    let error = &document["errors"][0];
    let details = error["details"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    (
        error["kind"].as_str().unwrap_or_default(),
        error["message"].as_str().unwrap_or_default(),
        details,
        error["help"].as_str().unwrap_or_default(),
        &error["fix"],
    )
}

/// a recipe data.lock does not pin as built now fails the checks a script runs, with exit 60
/// and what moved, and never with a fix to run, since building runs the project's code.
#[test]
fn lock_check_and_sync_locked_report_recipe_drift() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    for args in [
        &["--json", "lock", "--check"][..],
        &["--json", "sync", "--locked"],
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(60),
            "{args:?}: {}",
            text(&output.stdout)?
        );
        let document = json(&output)?;
        let (kind, message, details, help, fix) = error(&document);
        assert_eq!(kind, "build.needed");
        assert_eq!(message, "data.lock is out of date: clean is not built yet");
        assert_eq!(details, ["clean: not built yet"]);
        assert_eq!(
            help,
            "run `fetchloom build clean`, then commit data.lock and data.lock.d"
        );
        assert_eq!(fix, &Value::Null, "building runs the project's code");
    }
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);

    pin_clean(&sandbox, CODE, &[])?;
    for args in [&["lock", "--check"][..], &["sync", "--locked"]] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    // a checkout with CRLF line endings is the same code
    fs::write(
        sandbox.work().join("clean.py"),
        b"import sys\r\nprint(sys.argv)\r\n",
    )?;
    assert_eq!(sandbox.run(&["lock", "--check"])?.status.code(), Some(0));

    fs::write(sandbox.work().join("clean.py"), "print('edited')\n")?;
    let edited = sandbox.run(&["--json", "lock", "--check"])?;
    assert_eq!(edited.status.code(), Some(60));
    let document = json(&edited)?;
    let (kind, message, details, _, _) = error(&document);
    assert_eq!(kind, "build.needed");
    assert_eq!(
        message,
        "data.lock is out of date: clean changed since it was built"
    );
    assert_eq!(details, ["clean: clean.py changed"]);
    Ok(())
}

/// status lists a recipe data.lock does not pin as built now with what moved, as it lists a
/// fetched dataset data.toml changed, and points at the dry run rather than at sync.
#[test]
fn status_names_what_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    pin_clean(&sandbox, b"print('before')\n", &[("SEED", "1")])?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("Unlocked clean · clean.py changed; env changed\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("hint: `fetchloom build --dry-run` shows the commands that would build the recipes again"),
        "{stderr}"
    );
    assert!(!stderr.contains("`fetchloom sync` places"), "{stderr}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(document["result"]["unlocked"], serde_json::json!(["clean"]));
    Ok(())
}

/// a recipe's result is not placed in the data folder yet, so `path` names the build it needs.
#[test]
fn path_of_a_recipe_needs_a_build() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let output = sandbox.run(&["--json", "path", "clean"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, help, fix) = error(&document);
    assert_eq!(kind, "build.needed");
    assert_eq!(
        message,
        "clean is made by a recipe, and its result is not in data/ yet"
    );
    assert_eq!(help, "run `fetchloom build clean`, which makes it");
    assert_eq!(fix, &Value::Null);
    Ok(())
}

/// sync fetches what data.toml's sources pin and asks nothing for a recipe, which only
/// `fetchloom build` makes.
#[test]
fn sync_leaves_a_recipe_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(server.requests().len(), 1, "only the add downloaded");
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert!(!sandbox.work().join("data").join("clean").exists());
    assert_eq!(
        fs::read(sandbox.work().join("data").join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

#[test]
fn list_names_what_a_recipe_is_made_from() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let listed = sandbox.run(&["list"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", text(&listed.stderr)?);
    let stdout = text(&listed.stdout)?;
    let first: Vec<&str> = stdout
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    assert_eq!(
        first,
        [
            "clean",
            "made",
            "from",
            "iris",
            "not",
            "locked",
            "yet",
            "data/clean"
        ],
        "{stdout}"
    );
    let document = json(&sandbox.run(&["--json", "list", "clean"])?)?;
    let clean = &document["result"]["datasets"][0];
    assert_eq!(clean["ref"], serde_json::Value::Null);
    assert_eq!(clean["from"], serde_json::json!(["iris"]));
    Ok(())
}

/// a recipe cannot read a dataset data.toml no longer has, so the dataset stays until the
/// recipe does.
#[test]
fn a_dataset_a_recipe_reads_is_not_removed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    let refused = sandbox.run(&["remove", "iris"])?;
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr)?);
    let stderr = text(&refused.stderr)?;
    assert!(stderr.contains("clean is made from iris"), "{stderr}");
    assert!(
        stderr.contains("remove clean first, or take iris out of its from in data.toml"),
        "{stderr}"
    );
    assert_eq!(
        fs::read_to_string(sandbox.work().join("data.toml"))?,
        manifest
    );
    let removed = sandbox.run(&["remove", "clean"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!fs::read_to_string(sandbox.work().join("data.toml"))?.contains("clean"));
    Ok(())
}
