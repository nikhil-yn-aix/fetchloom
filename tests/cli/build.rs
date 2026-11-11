//! recipes declared in data.toml against servers on this machine: what each command does with
//! them, and that none of them runs a step.

use std::fs;

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
