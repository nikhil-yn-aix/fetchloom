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
        stderr
            .contains("hint: `fetchloom build --dry-run` shows the commands that would build them"),
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
            "built",
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

/// a recipe that makes `tidy` from clean, which [`CLEAN`] makes from iris.
const TIDY: &str = "\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"python3\", \"tidy.py\", \"{clean}\", \"--out={out}\"]\nenv = { SEED = \"1\" }\n";

/// a dry run says, in the order steps would run, each step's command with its folders in place,
/// what it reads and where its result goes, and runs nothing.
#[test]
fn a_dry_run_shows_commands_and_runs_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!("{}{TIDY}", fs::read_to_string(&manifest)?),
    )?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = sandbox.run(&["build", "--dry-run"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let stderr = text(&output.stderr)?;
    for line in [
        "  Would run clean · not built yet\n",
        "            python3 clean.py <build>/in/iris <build>/out\n",
        "            reads iris and clean.py; writes data/clean\n",
        "  Would run tidy · not built yet; clean is not built yet\n",
        "            python3 tidy.py <build>/in/clean --out=<build>/out\n",
        "            reads clean, with SEED=1; writes data/tidy\n",
        "         Ok dry run, nothing was run or changed\n",
    ] {
        assert!(stderr.contains(line), "{line:?} in\n{stderr}");
    }
    assert!(stderr.find("clean · not built") < stderr.find("tidy · not built"));
    let document = json(&sandbox.run(&["--json", "build", "-n", "tidy"])?)?;
    let result = &document["result"];
    assert_eq!(document["command"], "build");
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["selected"], serde_json::json!(["tidy"]));
    let steps = result["steps"].as_array().ok_or("no steps")?;
    let names: Vec<&str> = steps
        .iter()
        .filter_map(|step| step["name"].as_str())
        .collect();
    assert_eq!(names, ["clean", "tidy"], "a step comes after what it reads");
    let clean = &steps[0];
    assert_eq!(clean["action"], "would_run");
    assert_eq!(clean["changes"], serde_json::json!(["not built yet"]));
    assert_eq!(
        clean["run"],
        serde_json::json!(["python3", "clean.py", "<build>/in/iris", "<build>/out"])
    );
    assert_eq!(clean["inputs"][0]["name"], "iris");
    assert_eq!(clean["inputs"][0]["folder"], "<build>/in/iris");
    assert_eq!(clean["code"], serde_json::json!(["clean.py"]));
    assert_eq!(clean["output"], "data/clean");
    assert!(
        clean["key"]
            .as_str()
            .is_some_and(|key| key.starts_with("sha256:")),
        "{clean}"
    );
    let tidy = &steps[1];
    assert_eq!(tidy["key"], Value::Null, "its input is not pinned yet");
    assert_eq!(tidy["env"], serde_json::json!({"SEED": "1"}));
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert!(!sandbox.work().join("data").join("clean").exists());
    Ok(())
}

/// a step data.lock pins as built now does not run again, until something it is made from
/// moves.
#[test]
fn a_dry_run_tells_up_to_date_steps_from_changed_ones() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    pin_clean(&sandbox, CODE, &[])?;
    let output = sandbox.run(&["--json", "build", "--dry-run", "clean"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let document = json(&output)?;
    let step = &document["result"]["steps"][0];
    assert_eq!(step["action"], "up_to_date");
    assert_eq!(step["changes"], serde_json::json!([]));
    let stderr = text(&sandbox.run(&["build", "--dry-run"])?.stderr)?;
    assert!(stderr.contains(" Up to date clean\n"), "{stderr}");
    assert!(
        stderr.contains("         Ok every recipe is built as data.lock pins it; dry run, nothing was run or changed\n"),
        "{stderr}"
    );
    fs::write(sandbox.work().join("clean.py"), "print('edited')\n")?;
    let stderr = text(&sandbox.run(&["build", "--dry-run"])?.stderr)?;
    assert!(
        stderr.contains("  Would run clean · clean.py changed\n"),
        "{stderr}"
    );
    Ok(())
}

/// build runs only recipes, and names them right.
#[test]
fn build_names_only_recipes() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    for (args, code, said) in [
        (
            &["build", "--dry-run", "iris"][..],
            2,
            "iris is fetched, not built; `fetchloom sync` places it",
        ),
        (
            &["build", "--dry-run", "claen"],
            2,
            "data.toml has no dataset called claen",
        ),
        (
            &["build"],
            2,
            "this fetchloom shows what building would run, and cannot run recipes yet",
        ),
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        let stderr = text(&output.stderr)?;
        assert!(stderr.contains(said), "{args:?}: {stderr}");
    }
    let document = json(&sandbox.run(&["--json", "build", "clean"])?)?;
    assert_eq!(
        document["errors"][0]["fix"], "fetchloom build --dry-run clean",
        "a dry run runs nothing, so a script may run it"
    );
    Ok(())
}

/// only `fetchloom build` may run a project's code: no other command runs a step, here one that
/// would leave a file behind.
#[cfg(unix)]
#[test]
fn no_other_command_runs_a_step() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    added(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let recipe = "\n[datasets.marked]\nfrom = [\"iris\"]\nrun = [\"sh\", \"-c\", \"touch ran\", \"{iris}\", \"{out}\"]\n";
    fs::write(
        &manifest,
        format!("{}{recipe}", fs::read_to_string(&manifest)?),
    )?;
    for args in [
        &["sync"][..],
        &["sync", "--locked"],
        &["lock"],
        &["lock", "--check"],
        &["lock", "--upgrade"],
        &["status"],
        &["status", "--verify"],
        &["list"],
        &["export"],
        &["path", "iris"],
        &["path", "marked"],
        &["build", "--dry-run"],
        &["build"],
        &["remove", "marked", "--dry-run"],
    ] {
        sandbox.run(args)?;
        assert!(
            !sandbox.work().join("ran").exists(),
            "{args:?} ran the step"
        );
    }
    Ok(())
}
