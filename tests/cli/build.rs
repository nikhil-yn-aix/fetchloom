//! recipes declared in data.toml against servers on this machine: what each command does with
//! them, that none but `fetchloom build` runs a step, and what a build runs, pins and places,
//! with the step program `examples/recipe_step.rs`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::export::sha256;
use crate::project::{IRIS, PATH, added};
use crate::support::{Result, Running, Sandbox, Server, file, json, text, wait_until};

/// a recipe that makes `clean` from iris with clean.py.
const CLEAN: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"python3\", \"clean.py\", \"{iris}\", \"{out}\"]\ncode = [\"clean.py\"]\n";

/// a project with iris added from `server` and the recipe [`CLEAN`] beside it, with its code.
pub(crate) fn with_recipe(sandbox: &Sandbox, server: &Server) -> Result {
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
pub(crate) const CODE: &[u8] = b"import sys\nprint(sys.argv)\n";

/// pins `clean` in data.lock as built from iris with clean.py holding `code` and the variables
/// `env`, one file in its result. the key is made here as the recipe key scheme defines it, apart
/// from fetchloom's own code: the JSON of the scheme, each input's content, the run, each code
/// file's hash and the variables.
pub(crate) fn pin_clean(sandbox: &Sandbox, code: &[u8], env: &[(&str, &str)]) -> Result {
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
    let checked = sandbox.run(&["lock", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    // the pin was written by hand, so no build made its files here
    let synced = sandbox.run(&["sync", "--locked"])?;
    assert_eq!(synced.status.code(), Some(60));
    assert!(
        text(&synced.stderr)?.contains("clean's pinned result is not on this machine or a mirror"),
        "{}",
        text(&synced.stderr)?
    );
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
    with(
        &sandbox,
        &server,
        &format!("{COPY_CODE}env = {{ SEED = \"1\" }}\n"),
    )?;
    fs::write(sandbox.work().join("clean.py"), "print('before')\n")?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    fs::write(sandbox.work().join("clean.py"), "print('after')\n")?;
    let manifest = sandbox.work().join("data.toml");
    let text_now = fs::read_to_string(&manifest)?;
    fs::write(&manifest, text_now.replace("SEED = \"1\"", "SEED = \"2\""))?;
    let output = sandbox.run(&["status"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let stdout = text(&output.stdout)?;
    assert!(
        stdout.contains("Unlocked clean · clean.py changed; env changed\n"),
        "{stdout}"
    );
    let stderr = text(&output.stderr)?;
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

/// a recipe cannot read a dataset data.toml no longer has, so the dataset stays until every
/// recipe that reads it goes, and the refusal names each of them.
#[test]
fn a_dataset_a_recipe_reads_is_not_removed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    let path = sandbox.work().join("data.toml");
    let count = "\n[datasets.count]\nfrom = [\"iris\"]\nrun = [\"python3\", \"count.py\", \"{iris}\", \"{out}\"]\n";
    fs::write(&path, format!("{}{count}", fs::read_to_string(&path)?))?;
    let refused = |said: &str, help: &str| -> Result {
        let manifest = fs::read_to_string(&path)?;
        let refused = sandbox.run(&["remove", "iris"])?;
        assert_eq!(refused.status.code(), Some(2), "{}", text(&refused.stderr)?);
        let stderr = text(&refused.stderr)?;
        assert!(stderr.contains(&format!("error: {said}\n")), "{stderr}");
        assert!(stderr.contains(&format!("help: {help}\n")), "{stderr}");
        assert_eq!(fs::read_to_string(&path)?, manifest);
        Ok(())
    };
    refused(
        "clean and count are made from iris",
        "remove clean and count first, or take iris out of their from in data.toml",
    )?;
    let removed = sandbox.run(&["remove", "clean"])?;
    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!fs::read_to_string(&path)?.contains("clean"));
    refused(
        "count is made from iris",
        "remove count first, or take iris out of its from in data.toml",
    )
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
        "    May run tidy · not built yet; clean is not built yet\n",
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
    assert_eq!(
        tidy["action"], "may_run",
        "what clean makes is known once it ran"
    );
    assert_eq!(tidy["key"], Value::Null, "its input is not pinned yet");
    assert_eq!(tidy["env"], serde_json::json!({"SEED": "1"}));
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert!(!sandbox.work().join("data").join("clean").exists());
    Ok(())
}

/// a step data.lock pins as built now, whose result the cache holds, does not run again, until
/// something it is made from moves.
#[test]
fn a_dry_run_tells_up_to_date_steps_from_changed_ones() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with_recipe(&sandbox, &server)?;
    pin_clean(&sandbox, CODE, &[])?;
    let result = object(&sandbox, b"done");
    fs::create_dir_all(result.parent().ok_or("no folder")?)?;
    fs::write(&result, b"done")?;
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
            "data.toml has no dataset called \"claen\"",
        ),
        (&["build", "iris"], 2, "iris is fetched, not built"),
    ] {
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(code), "{args:?}");
        let stderr = text(&output.stderr)?;
        assert!(stderr.contains(said), "{args:?}: {stderr}");
    }
    Ok(())
}

/// only `fetchloom build` may run a project's code: no other command runs a step, here one that
/// would leave a file behind.
#[cfg(unix)]
#[test]
fn no_other_command_runs_a_step() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![
            file(IRIS, "\"v1\""),
            file(IRIS, "\"v1\""),
            file(IRIS, "\"v1\""),
        ],
    )?;
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
        &["remove", "marked", "--dry-run"],
        &["add", &server.url, "--as", "again"],
    ] {
        sandbox.run(args)?;
        assert!(
            !sandbox.work().join("ran").exists(),
            "{args:?} ran the step"
        );
    }
    Ok(())
}

/// `fetchloom` with `args` in `folder`, with the step program on PATH as `recipe-step`.
pub(crate) fn building_in(sandbox: &Sandbox, folder: &Path, args: &[&str]) -> Result<Command> {
    let bin = sandbox.folder("bin")?;
    let program = bin.join(format!("recipe-step{}", std::env::consts::EXE_SUFFIX));
    if !program.is_file() {
        fs::copy(crate::support::recipe_step()?, &program)?;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let paths = std::iter::once(bin).chain(std::env::split_paths(&path));
    let mut command = sandbox.command_in(folder, args);
    command.env("PATH", std::env::join_paths(paths)?);
    Ok(command)
}

/// [`building_in`] the work folder, run to its end.
pub(crate) fn build(sandbox: &Sandbox, args: &[&str]) -> Result<std::process::Output> {
    Ok(building_in(sandbox, &sandbox.work(), args)?.output()?)
}

/// a project with iris added from `server` and the recipes `recipes`, as data.toml tables.
pub(crate) fn with(sandbox: &Sandbox, server: &Server, recipes: &str) -> Result {
    added(sandbox, server)?;
    let manifest = sandbox.work().join("data.toml");
    let text = fs::read_to_string(&manifest)?;
    fs::write(&manifest, format!("{text}{recipes}"))?;
    Ok(())
}

/// a recipe that makes `clean` from iris by copying its files.
const COPY: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"copy\", \"{iris}\", \"{out}\"]\n";

/// [`COPY`], with clean.py as its code.
const COPY_CODE: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"copy\", \"{iris}\", \"{out}\"]\ncode = [\"clean.py\"]\n";

/// a recipe that makes `clean` from iris with the step program in `mode`, with its arguments.
fn recipe(mode: &[&str]) -> String {
    let words: Vec<String> = mode.iter().map(|word| format!("\"{word}\"")).collect();
    format!(
        "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", {}, \"{{iris}}\", \"{{out}}\"]\n",
        words.join(", ")
    )
}

/// how many times a step ran in `folder`, the folder steps run in.
pub(crate) fn runs(folder: &Path) -> Result<usize> {
    match fs::read_to_string(folder.join("runs")) {
        Ok(runs) => Ok(runs.lines().count()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.into()),
    }
}

/// every file below `folder` with its bytes, by its path there, to compare a folder before and
/// after a command.
fn files(folder: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut todo = vec![folder.to_path_buf()];
    while let Some(at) = todo.pop() {
        let Ok(entries) = fs::read_dir(&at) else {
            continue;
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                todo.push(entry.path());
            } else {
                let path = entry
                    .path()
                    .strip_prefix(folder)?
                    .to_string_lossy()
                    .replace('\\', "/");
                files.insert(path, fs::read(entry.path())?);
            }
        }
    }
    Ok(files)
}

/// a build runs the step in the project folder, pins what it wrote with its key and tree, keeps
/// the list of its files beside data.lock and places them in the data folder.
#[test]
fn build_pins_and_places_a_result() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(
        runs(&sandbox.work())?,
        1,
        "the step runs in the project folder"
    );
    let placed = sandbox.work().join("data").join("clean").join("iris.csv");
    assert_eq!(fs::read(&placed)?, IRIS);
    let tree = sha256(format!("iris.csv\t{}\t{}\n", IRIS.len(), sha256(IRIS)).as_bytes());
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains("name = \"clean\""), "{lock}");
    assert!(
        lock.contains(&format!("tree = \"sha256:{tree}\"")),
        "{lock}"
    );
    assert!(
        fs::read_dir(sandbox.work().join("data.lock.d"))?.count() == 1,
        "the result's files are listed beside data.lock"
    );
    let document = json(&output)?;
    let step = &document["result"]["steps"][0];
    assert_eq!(step["name"], "clean");
    assert_eq!(step["action"], "ran");
    assert_eq!(step["tree"], format!("sha256:{tree}"));
    assert_eq!(step["files"], 1);
    assert_eq!(step["log"], ".fetchloom/builds/clean.log");
    assert_eq!(document["result"]["lock_changed"], true);
    assert_eq!(document["result"]["datasets"], serde_json::json!(["clean"]));
    assert_eq!(document["result"]["placement"]["placed"], 1);
    assert!(sandbox.work().join(".fetchloom/builds/clean.log").is_file());
    for args in [&["lock", "--check"][..], &["sync", "--locked"]] {
        let output = sandbox.run(args)?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    Ok(())
}

/// a file a step writes enters the cache as one the build wrote, unless the cache held its
/// bytes already, as it holds iris's for a step that copies iris.
#[test]
fn a_build_counts_what_its_steps_add_to_the_cache() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "made"]))?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["work"]["objects_written"], 1);
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("\"write\", \"clean.csv\", \"made\"", "\"copy\""),
    )?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["work"]["objects_written"], 0);
    Ok(())
}

/// a build of what data.lock pins as built now, with its files in the cache, runs nothing and
/// leaves data.lock as it is.
#[test]
fn a_second_build_runs_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let again = build(&sandbox, &["--json", "build", "clean"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stdout)?);
    assert_eq!(runs(&sandbox.work())?, 1);
    let document = json(&again)?;
    assert_eq!(document["result"]["steps"][0]["action"], "up_to_date");
    assert_eq!(document["result"]["lock_changed"], false);
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    let told = text(&build(&sandbox, &["build"])?.stderr)?;
    assert!(told.contains(" Up to date clean\n"), "{told}");
    Ok(())
}

/// a folder mirror at `root` holding the objects of `bytes`, as pushing fills one.
fn mirror_holding(root: &Path, bytes: &[&[u8]]) -> Result {
    fs::create_dir_all(root)?;
    fs::write(root.join("fetchloom-mirror"), "fetchloom mirror 1\n")?;
    for bytes in bytes {
        let hex = sha256(bytes);
        let fan = root.join("objects").join(&hex[..2]);
        fs::create_dir_all(&fan)?;
        fs::write(fan.join(&hex[2..]), bytes)?;
    }
    Ok(())
}

/// the object of `bytes` in the sandbox's cache.
fn object(sandbox: &Sandbox, bytes: &[u8]) -> PathBuf {
    let hex = sha256(bytes);
    sandbox
        .cache()
        .join("objects")
        .join(&hex[..2])
        .join(&hex[2..])
}

/// where sync places `file` of `dataset` in the work folder's data folder, which a run takes a
/// file's bytes from when the cache lacks them.
fn placed(sandbox: &Sandbox, dataset: &str, file: &str) -> PathBuf {
    sandbox.work().join("data").join(dataset).join(file)
}

/// a result data.lock pins for the key building it now would make, whose files the cache and
/// the data folder lost, comes from a mirror rather than from running its step again.
#[test]
fn a_pinned_result_the_cache_lacks_comes_from_a_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "made"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let mirror = sandbox.folder("mirror")?;
    mirror_holding(&mirror, &[b"made"])?;
    crate::support::delete(&object(&sandbox, b"made"))?;
    crate::support::delete(&placed(&sandbox, "clean", "clean.csv"))?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = building_in(&sandbox, &sandbox.work(), &["--json", "build"])?
        .env("FETCHLOOM_MIRRORS", &mirror)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(document["result"]["steps"][0]["action"], "fetched");
    assert_eq!(document["work"]["mirror_objects"], 1);
    assert_eq!(runs(&sandbox.work())?, 1, "the step did not run again");
    assert!(object(&sandbox, b"made").is_file());
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    Ok(())
}

/// each step's action in the `--json` of a dry run, then in that of the build after it, as
/// `<name> <action>`, with `mirror` set as a mirror for both.
fn foreseen_then_done(
    sandbox: &Sandbox,
    mirror: Option<&Path>,
) -> Result<(Vec<String>, Vec<String>)> {
    let actions = |args: &[&str]| -> Result<Vec<String>> {
        let mut command = building_in(sandbox, &sandbox.work(), args)?;
        if let Some(mirror) = mirror {
            command.env("FETCHLOOM_MIRRORS", mirror);
        }
        let output = command.output()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}: {}",
            text(&output.stdout)?
        );
        Ok(json(&output)?["result"]["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|step| format!("{} {}", step["name"], step["action"]).replace('"', ""))
            .collect())
    };
    Ok((
        actions(&["--json", "build", "--dry-run"])?,
        actions(&["--json", "build"])?,
    ))
}

/// a project whose `clean` writes the file `made` and whose `tidy` copies it, built, and a folder
/// mirror that holds `made`.
fn made_and_mirrored(sandbox: &Sandbox, server: &Server) -> Result<PathBuf> {
    let tidy = "\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"recipe-step\", \"copy\", \"{clean}\", \"{out}\"]\n";
    with(
        sandbox,
        server,
        &format!("{}{tidy}", recipe(&["write", "clean.csv", "made"])),
    )?;
    assert_eq!(build(sandbox, &["build"])?.status.code(), Some(0));
    let mirror = sandbox.folder("mirror")?;
    mirror_holding(&mirror, &[b"made"])?;
    Ok(mirror)
}

/// takes `made`, the one file of both results of [`made_and_mirrored`], out of the cache, and
/// with `placed_too` out of the data folder, which holds it for each.
fn lose_made(sandbox: &Sandbox, placed_too: bool) -> Result {
    crate::support::delete(&object(sandbox, b"made"))?;
    if placed_too {
        for dataset in ["clean", "tidy"] {
            crate::support::delete(&placed(sandbox, dataset, "clean.csv"))?;
        }
    }
    Ok(())
}

/// a dry run takes each step as the build after it does: a pinned result the cache lacks comes
/// from the data folder or a mirror that holds it, or its step runs, a result built before from
/// the same inputs is reused, and a step that reads what a changed step makes may run. its action
/// is the one the build then reports, as in a fresh clone, where every pinned result is missing.
#[test]
fn a_dry_run_says_what_the_build_then_does() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = made_and_mirrored(&sandbox, &server)?;
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    let (made, other) = ("made", "other");
    for (case, lost, mirrored, writes, foreseen, done) in [
        (
            "the cache and the data folder lost both results",
            Some(true),
            false,
            made,
            "clean would_run, tidy up_to_date",
            "clean ran, tidy up_to_date",
        ),
        (
            "the data folder holds what the cache lost",
            Some(false),
            false,
            made,
            "clean would_fetch, tidy up_to_date",
            "clean fetched, tidy up_to_date",
        ),
        (
            "a mirror holds what the cache and the data folder lost",
            Some(true),
            true,
            made,
            "clean would_fetch, tidy up_to_date",
            "clean fetched, tidy up_to_date",
        ),
        (
            "the cache holds both results",
            None,
            false,
            made,
            "clean up_to_date, tidy up_to_date",
            "clean up_to_date, tidy up_to_date",
        ),
        (
            "clean's run changed",
            None,
            false,
            other,
            "clean would_run, tidy may_run",
            "clean ran, tidy ran",
        ),
        (
            "clean's run changed back",
            None,
            false,
            made,
            "clean would_reuse, tidy would_reuse",
            "clean reused, tidy reused",
        ),
    ] {
        fs::write(
            &manifest,
            written.replace("\"made\"", &format!("\"{writes}\"")),
        )?;
        if let Some(placed_too) = lost {
            lose_made(&sandbox, placed_too)?;
        }
        let (said, did) = foreseen_then_done(&sandbox, mirrored.then_some(mirror.as_path()))?;
        assert_eq!(
            (said.join(", "), did.join(", ")),
            (foreseen.into(), done.into()),
            "{case}"
        );
    }
    Ok(())
}

/// a dry run shows the command of a step whose pinned result is nowhere, and says where a result
/// a mirror holds would come from, with no command, since nothing runs.
#[test]
fn a_dry_run_shows_what_runs_and_where_a_result_comes_from() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let mirror = made_and_mirrored(&sandbox, &server)?;
    lose_made(&sandbox, true)?;
    let told = text(&build(&sandbox, &["build", "--dry-run"])?.stderr)?;
    for line in [
        "  Would run clean · its pinned result is not on this machine or a mirror\n",
        "            recipe-step write clean.csv made <build>/in/iris <build>/out\n",
        " Up to date tidy\n",
    ] {
        assert!(told.contains(line), "{line:?} in\n{told}");
    }
    let told = text(
        &building_in(&sandbox, &sandbox.work(), &["build", "--dry-run"])?
            .env("FETCHLOOM_MIRRORS", &mirror)
            .output()?
            .stderr,
    )?;
    let fetch = format!(
        "Would fetch clean · 1 file, 4 B as data.lock pins it, from {}\n",
        mirror.display()
    );
    assert!(told.contains(&fetch), "{fetch:?} in\n{told}");
    assert!(!told.contains("recipe-step write"), "{told}");
    Ok(())
}

/// a step a build runs again for a pin whose files the cache and the data folder lost, which
/// makes the same files, leaves data.lock as it is, and the build says the result matches it
/// rather than that it pinned it.
#[test]
fn a_rebuild_that_makes_the_pinned_files_says_it_matches() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "made"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    crate::support::delete(&object(&sandbox, b"made"))?;
    crate::support::delete(&placed(&sandbox, "clean", "clean.csv"))?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = build(&sandbox, &["build"])?;
    let told = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{told}");
    assert_eq!(runs(&sandbox.work())?, 2, "the step ran again");
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert!(
        told.contains("         Ok clean's result matches data.lock\n"),
        "{told}"
    );
    assert!(!told.contains("pinned"), "{told}");
    Ok(())
}

/// a step reads the bytes data.lock pins, never an edit in the data folder: a build that keeps an
/// edit of a dataset a recipe reads says that the step reads the pinned bytes, naming the file
/// and how to build from the fix, and never offers `sync --force`, which would undo the fix.
#[test]
fn a_build_says_its_steps_read_the_pins_not_an_edit() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("[datasets.iris]\n", "[datasets.iris]\ncopy = true\n"),
    )?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let edited = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::edit(&edited, b"sepal_length,species\n5.2,setosa\n")?;
    let output = build(&sandbox, &["build"])?;
    let told = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{told}");
    assert!(
        told.contains(
            "warning: data/iris/iris.csv has changed, so it stays, but clean reads the bytes data.lock pins for iris, not this edit\n  help: to build from the fix, make it in a step of clean, or copy the file out of data/ and add it as a dataset of its own with `fetchloom add <copy> --as <name>` for clean to read\n"
        ),
        "{told}"
    );
    assert!(!told.contains("--force"), "{told}");
    assert_eq!(fs::read(&edited)?, b"sepal_length,species\n5.2,setosa\n");
    let made = sandbox.work().join("data").join("clean").join("iris.csv");
    assert_eq!(fs::read(made)?, IRIS);
    Ok(())
}

/// an edit made in place through a hardlink changed the cache's copy a step reads, so the build
/// must fetch its pinned bytes again. when it cannot, as offline, its error says why it needed
/// them, as a sync's does, and the edit stays.
#[test]
fn a_build_that_cannot_fetch_a_changed_cache_copy_again_says_why() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    sandbox.allow(&server.url);
    let added = json(&sandbox.run(&["--json", "add", &server.url])?)?;
    if added["work"]["hardlinks"] != 1 {
        return Ok(());
    }
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        format!("{}{COPY}", fs::read_to_string(&manifest)?),
    )?;
    let edited = placed(&sandbox, "iris", "iris.csv");
    crate::support::edit(&edited, b"changed in place\n")?;
    let output = build(&sandbox, &["--json", "--offline", "build"])?;
    assert_eq!(output.status.code(), Some(20), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, _, details, _, _) = error(&document);
    assert_eq!(kind, "offline.missing");
    assert!(
        details.contains(&"data/iris/iris.csv is linked to the cache's copy, which was changed, so its pinned bytes must be fetched again"),
        "{details:?}"
    );
    assert_eq!(fs::read(&edited)?, b"changed in place\n");
    assert_eq!(runs(&sandbox.work())?, 0);
    Ok(())
}

/// a rebuild keeps an edit of the result it replaces, which `copy = true` allows, and says the
/// file holds an earlier version than data.lock pins now. its help names only the recipe whose
/// file stays, never iris, which has `place = false`.
#[test]
fn a_rebuild_that_keeps_an_edit_names_its_recipe_alone() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let made = |text: &str| {
        recipe(&["write", "clean.csv", text])
            .replace("[datasets.clean]\n", "[datasets.clean]\ncopy = true\n")
    };
    with(&sandbox, &server, &made("one"))?;
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("[datasets.iris]\n", "[datasets.iris]\nplace = false\n"),
    )?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let edited = sandbox.work().join("data").join("clean").join("clean.csv");
    crate::support::edit(&edited, b"one, edited\n")?;
    let written = fs::read_to_string(&manifest)?;
    fs::write(&manifest, written.replace(&made("one"), &made("two")))?;
    let output = build(&sandbox, &["build"])?;
    let told = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{told}");
    assert!(
        told.contains("warning: data/clean/clean.csv has changed, so it stays, but it is an edit of an earlier version of clean, not what data.lock pins now\n  help: `fetchloom sync --force clean` replaces it\n"),
        "{told}"
    );
    assert_eq!(fs::read(&edited)?, b"one, edited\n");
    Ok(())
}

/// a step that fails stops the build with its exit code and last lines, and leaves data.lock,
/// its list and the data folder byte for byte as they were.
#[test]
fn a_failed_step_keeps_lock_and_data() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let manifest = sandbox.work().join("data.toml");
    let text_before = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        text_before.replace("\"copy\"", "\"fail\", \"3\""),
    )?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let parts = files(&sandbox.work().join("data.lock.d"))?;
    let data = files(&sandbox.work().join("data"))?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, details, help, fix) = error(&document);
    assert_eq!(kind, "build.failed");
    assert!(
        message.starts_with("clean failed: recipe-step exited with code 3 after "),
        "{message}"
    );
    assert_eq!(details.len(), 20, "{details:?}");
    assert!(details.contains(&"working on part 30"), "{details:?}");
    assert!(
        help.contains(".fetchloom/builds/clean.log, and data.lock and data/clean are as they were"),
        "{help}"
    );
    assert_eq!(fix, &Value::Null);
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert_eq!(files(&sandbox.work().join("data.lock.d"))?, parts);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    let log = fs::read_to_string(sandbox.work().join(".fetchloom/builds/clean.log"))?;
    for line in ["working on part 1\n", "something went wrong in part 30\n"] {
        assert!(log.contains(line), "{log}");
    }
    Ok(())
}

/// a failed step's output reaches a person once: as the step runs, or, with `-q`, which hides it
/// then, in the error.
#[test]
fn a_failed_step_shows_its_output_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["fail", "3"]))?;
    for args in [&["build"][..], &["-q", "build"]] {
        let output = build(&sandbox, args)?;
        assert_eq!(output.status.code(), Some(60), "{args:?}");
        let told = text(&output.stderr)?;
        assert!(
            told.contains("error: clean failed: recipe-step exited with code 3 after "),
            "{told}"
        );
        // stdout's last line is among the last 20 however the two outputs interleave
        assert_eq!(
            told.matches("working on part 30\n").count(),
            1,
            "{args:?}: {told}"
        );
    }
    Ok(())
}

/// a step that writes nothing into its result folder has made no result.
#[test]
fn an_empty_result_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["nothing"]))?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, _, _) = error(&document);
    assert_eq!(kind, "build.failed");
    assert_eq!(message, "clean wrote nothing into {out}");
    assert!(!fs::read_to_string(sandbox.work().join("data.lock"))?.contains("clean"));
    Ok(())
}

/// a result holds plain files only, so a link the step wrote is refused, never followed.
#[cfg(unix)]
#[test]
fn a_link_in_the_result_is_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["link"]))?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(60));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("clean wrote a link at latest, and results hold plain files only"),
        "{stderr}"
    );
    Ok(())
}

/// two names a disk that ignores case takes for one stop the build here, not on a teammate's
/// machine.
#[cfg(target_os = "linux")]
#[test]
fn case_twins_are_refused() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["case"]))?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(60));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("which are one name on disks that ignore case"),
        "{stderr}"
    );
    assert!(
        stderr.contains("Data.csv") && stderr.contains("data.csv"),
        "{stderr}"
    );
    Ok(())
}

/// a step reads the files data.lock pins, copied from the cache, never the data folder, where a
/// person may have changed them.
#[test]
fn inputs_come_from_pins_not_the_data_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    // a file of its own, so the edit cannot reach the cache through a link
    let placed = sandbox.work().join("data").join("iris").join("iris.csv");
    crate::support::delete(&placed)?;
    fs::write(&placed, b"edited by hand\n")?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("data").join("clean").join("iris.csv"))?,
        IRIS
    );
    assert_eq!(fs::read(&placed)?, b"edited by hand\n", "the edit stays");
    Ok(())
}

/// a zip of two files, with `images/x.png` inside.
fn zipped() -> Result<Vec<u8>> {
    use std::io::Write as _;
    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, bytes) in [("images/x.png", &b"x"[..]), ("labels.csv", b"file,label\n")] {
        archive.start_file(path, zip::write::SimpleFileOptions::default())?;
        archive.write_all(bytes)?;
    }
    Ok(archive.finish()?.into_inner())
}

/// the paths of the files below `folder`, sorted.
fn names(folder: &Path) -> Result<Vec<String>> {
    Ok(files(folder)?.into_keys().collect())
}

/// a members table that leaves a file of an archive out leaves it out of the step too: the step
/// never sees it, and its key differs from the one of the whole archive.
#[test]
fn a_members_table_keeps_a_file_from_the_step() -> Result {
    let sandbox = Sandbox::new()?;
    let set = sandbox.folder("set")?;
    fs::write(set.join("set.zip"), zipped()?)?;
    let added = sandbox.run(&["add", "../set"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    let copy = "\n[datasets.clean]\nfrom = [\"set\"]\nrun = [\"recipe-step\", \"copy\", \"{set}\", \"{out}\"]\n";
    fs::write(&manifest, format!("{plain}{copy}"))?;
    let whole = build(&sandbox, &["--json", "build"])?;
    assert_eq!(whole.status.code(), Some(0), "{}", text(&whole.stdout)?);
    let result = sandbox.work().join("data").join("clean");
    let all = names(&result)?;
    assert!(all.iter().any(|name| name.ends_with("x.png")), "{all:?}");

    fs::write(
        &manifest,
        format!(
            "{plain}\n[datasets.set.members.\"set.zip\"]\nexclude = [\"images/x.png\"]\n{copy}"
        ),
    )?;
    let filtered = build(&sandbox, &["--json", "build"])?;
    assert_eq!(
        filtered.status.code(),
        Some(0),
        "{}",
        text(&filtered.stdout)?
    );
    assert_eq!(runs(&sandbox.work())?, 2);
    let kept = names(&result)?;
    assert!(!kept.iter().any(|name| name.ends_with("x.png")), "{kept:?}");
    assert_eq!(kept.len(), all.len() - 1, "{kept:?}");
    let key = |output: &std::process::Output| -> Result<Value> {
        Ok(json(output)?["result"]["steps"][0]["key"].clone())
    };
    assert_ne!(key(&filtered)?, key(&whole)?);
    Ok(())
}

/// a build on a machine whose cache lacks an imported input, and whose data folder lacks it
/// too, says how to bring it there, since no link fetches it.
#[test]
fn an_import_the_cache_lacks_names_the_add_that_brings_it() -> Result {
    let sandbox = Sandbox::new()?;
    let set = sandbox.folder("set")?;
    fs::write(set.join("labels.csv"), "file,label\n")?;
    let added = sandbox.run(&["add", "../set"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let manifest = sandbox.work().join("data.toml");
    let plain = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        format!(
            "{plain}\n[datasets.clean]\nfrom = [\"set\"]\nrun = [\"recipe-step\", \"copy\", \"{{set}}\", \"{{out}}\"]\n"
        ),
    )?;
    crate::support::delete(&sandbox.work().join("data").join("set").join("labels.csv"))?;
    let elsewhere = sandbox.folder("another cache")?;
    let output = building_in(&sandbox, &sandbox.work(), &["--json", "build"])?
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    assert_eq!(output.status.code(), Some(10), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, help, _) = error(&document);
    assert_eq!(kind, "reference.unresolved");
    assert_eq!(
        message,
        "set was imported from files, and this machine's cache does not hold them"
    );
    assert_eq!(
        help,
        "copy them to data/set and build again, or run `fetchloom add <folder> --as set` where they are"
    );
    assert_eq!(runs(&sandbox.work())?, 0);
    Ok(())
}

/// the code of a recipe is in its key, so a build after an edit runs the step again and pins
/// the new key.
#[test]
fn editing_code_makes_build_pin_again() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY_CODE)?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    let first = json(&build(&sandbox, &["--json", "build"])?)?;
    fs::write(sandbox.work().join("clean.py"), "print(2)\n")?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let second = json(&output)?;
    assert_eq!(runs(&sandbox.work())?, 2);
    let (before, after) = (&first["result"]["steps"][0], &second["result"]["steps"][0]);
    assert_eq!(after["action"], "ran");
    assert_ne!(after["key"], before["key"]);
    assert_eq!(after["tree"], before["tree"]);
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&format!(
        "key = \"{}\"",
        after["key"].as_str().unwrap_or_default()
    )));
    assert_eq!(sandbox.run(&["lock", "--check"])?.status.code(), Some(0));
    Ok(())
}

/// the warnings of the json document `value` about recipe code git does not have.
fn code_warnings(value: &Value) -> Vec<&str> {
    value["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|warning| warning.contains(", code of "))
        .collect()
}

/// a commit that leaves out a recipe's code leaves a clone unable to build it from the same
/// code, so every build that succeeds warns of code git does not have: here a new file in a code
/// folder, after which the step made the same files and no new list joined data.lock.d, and
/// again in the build after it, which finds the result up to date.
#[test]
fn a_build_warns_of_recipe_code_git_does_not_have() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let lib = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"copy\", \"{iris}\", \"{out}\"]\ncode = [\"lib\"]\n";
    with(&sandbox, &server, lib)?;
    let lib = sandbox.work().join("lib");
    fs::create_dir(&lib)?;
    fs::write(lib.join("a.py"), "a = 1\n")?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    crate::merge::ok(&sandbox, &["add", "."])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "clean"])?;
    fs::write(lib.join("new.py"), "b = 2\n")?;
    for action in ["ran", "up_to_date"] {
        let output = build(&sandbox, &["--json", "build"])?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        assert_eq!(document["result"]["steps"][0]["action"], action);
        assert_eq!(
            code_warnings(&document),
            [
                "git does not have lib/new.py, code of clean; run `git add lib/new.py`, then commit it, since a clone without it cannot build clean from the same code"
            ]
        );
    }
    assert_eq!(
        fs::read_dir(sandbox.work().join("data.lock.d"))?.count(),
        1,
        "the step made the same files"
    );
    Ok(())
}

/// code a recipe names that the project lacks, as in a clone of a commit that left it out, is
/// to be brought back: taking it out of the recipe changes the recipe for everyone.
#[test]
fn missing_code_is_brought_back_before_it_leaves_the_recipe() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY_CODE)?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, help, _) = error(&document);
    assert_eq!(kind, "project.invalid");
    assert_eq!(
        message,
        "clean cannot be built: its code clean.py is not in the project"
    );
    assert_eq!(
        help,
        "bring clean.py back: `git restore clean.py` if git has it, else ask whoever built clean to commit it; take it out of code in data.toml only once the step no longer reads it"
    );
    assert_eq!(runs(&sandbox.work())?, 0);
    Ok(())
}

/// a recipe made from another takes that one's tree, so an upstream rebuild that makes the same
/// files leaves the downstream pin, and its step, alone.
#[test]
fn an_upstream_rebuild_with_the_same_tree_keeps_the_downstream_pin() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let tidy = "\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"recipe-step\", \"write\", \"tidy.csv\", \"done\", \"{clean}\", \"{out}\"]\n";
    with(&sandbox, &server, &format!("{COPY_CODE}{tidy}"))?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let pinned = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let tidy_pin = |lock: &str| lock[lock.find("name = \"tidy\"").unwrap_or_default()..].to_owned();
    fs::write(sandbox.work().join("clean.py"), "print(2)\n")?;
    let output = build(&sandbox, &["--json", "build", "tidy"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let actions: Vec<&str> = document["result"]["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|step| step["action"].as_str())
        .collect();
    assert_eq!(actions, ["ran", "up_to_date"]);
    let runs = fs::read_to_string(sandbox.work().join("runs"))?;
    assert_eq!(runs, "copy\nwrite\ncopy\n");
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert_eq!(tidy_pin(&lock), tidy_pin(&pinned));
    Ok(())
}

/// a key holds nothing of where the project is, so a moved project's results stay built.
#[test]
fn a_moved_project_keeps_its_keys() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let moved = sandbox.folder("moved")?.join("project");
    fs::rename(sandbox.work(), &moved)?;
    let checked = sandbox.command_in(&moved, &["lock", "--check"]).output()?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let output = building_in(&sandbox, &moved, &["--json", "build"])?.output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["result"]["steps"][0]["action"], "up_to_date");
    assert_eq!(runs(&moved)?, 1);
    assert_eq!(fs::read(moved.join("data.lock"))?, lock);
    Ok(())
}

/// a second build of a key another build is running waits for it and takes its result, so the
/// step runs once.
#[test]
fn two_builds_of_one_key_run_it_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let marker = sandbox.folder("hold")?.join("started");
    let release = marker.with_file_name("release");
    let (marker_text, release_text) = (marker.to_string_lossy(), release.to_string_lossy());
    with(
        &sandbox,
        &server,
        &recipe(&[
            "hold",
            &marker_text.replace('\\', "\\\\"),
            &release_text.replace('\\', "\\\\"),
        ]),
    )?;
    let other = crate::project::clone(&sandbox, "other")?;
    let first = Running::start(&mut building_in(&sandbox, &sandbox.work(), &["build"])?)?;
    wait_until("the first step to start", || Ok(marker.exists()))?;
    let second = Running::start(&mut building_in(&sandbox, &other, &["build"])?)?;
    second.wait_for("Waiting for another fetchloom building clean")?;
    fs::write(&release, "go")?;
    let first = first.finish()?;
    assert_eq!(first.status.code(), Some(0), "{}", text(&first.stderr)?);
    let second = second.finish()?;
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stderr)?);
    let told = text(&second.stderr)?;
    assert!(
        told.contains("Reused clean · built before from the same inputs"),
        "{told}"
    );
    assert_eq!(runs(&sandbox.work())?, 1);
    assert_eq!(runs(&other)?, 0);
    assert_eq!(
        fs::read(other.join("data").join("clean").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// a lab's shared cache often lets its group write the files fetchloom makes. a result recorded
/// there is still one only its owner may change, so a clone building the same key reuses it.
#[cfg(unix)]
#[test]
fn a_result_recorded_under_a_group_umask_is_reused() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    let other = crate::project::clone(&sandbox, "other")?;
    for folder in [sandbox.work(), other.clone()] {
        let output = with_umask(&building_in(&sandbox, &folder, &["build"])?, "002").output()?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    }
    assert_eq!(runs(&sandbox.work())?, 1);
    assert_eq!(runs(&other)?, 0, "the clone reused the result");
    Ok(())
}

/// `command`, run by `sh` with the file mode creation mask `mask`.
#[cfg(unix)]
fn with_umask(command: &Command, mask: &str) -> Command {
    let mut shell = Command::new("sh");
    shell
        .arg("-c")
        .arg(format!("umask {mask} && exec \"$0\" \"$@\""))
        .arg(command.get_program())
        .args(command.get_args());
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => {
                shell.env(name, value);
            }
            None => {
                shell.env_remove(name);
            }
        }
    }
    if let Some(folder) = command.get_current_dir() {
        shell.current_dir(folder);
    }
    shell
}

/// a step's log is for its owner alone, and keeps only the latest 16 MiB of its output.
#[cfg(unix)]
#[test]
fn the_log_is_private_and_bounded() -> Result {
    use std::os::unix::fs::PermissionsExt;
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let bytes = (17 << 20).to_string();
    with(&sandbox, &server, &recipe(&["say", &bytes]))?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let log = sandbox.work().join(".fetchloom/builds/clean.log");
    assert_eq!(fs::metadata(&log)?.permissions().mode() & 0o777, 0o600);
    let kept = fs::read(&log)?;
    assert!(kept.len() <= 16 << 20, "{}", kept.len());
    assert!(kept.ends_with(b"to fill its log\n"));
    Ok(())
}

/// a build says how much room the steps it runs may take on the disk holding the cache, and asks
/// first when that disk has less free above `build_floor`: with no one to ask it stops with
/// `unconfirmed`, exit 1, before anything runs, and `--yes` goes on.
#[test]
fn a_build_short_of_room_asks_first() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY_CODE)?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    fs::write(sandbox.work().join("clean.py"), "print(2)\n")?;
    // the result data.lock pinned last is as big as iris, 32 B, so the rerun may take more than
    // the 20 B free above a floor of 1 B, whether or not the disk clones files
    let short = |args: &[&str]| -> Result<std::process::Output> {
        Ok(building_in(&sandbox, &sandbox.work(), args)?
            .env("FETCHLOOM_TEST_FREE", "20")
            .env("FETCHLOOM_BUILD_FLOOR", "1")
            .output()?)
    };
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = short(&["--json", "build"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, _, _) = error(&document);
    assert_eq!(kind, "unconfirmed");
    assert!(
        message.starts_with("this build needs at least "),
        "{message}"
    );
    assert_eq!(runs(&sandbox.work())?, 1, "nothing ran");
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    let output = short(&["--yes", "build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let told = text(&output.stderr)?;
    assert!(told.contains("      Needs up to "), "{told}");
    assert!(
        told.contains(" on the disk holding the cache, which has 20 B free\n"),
        "{told}"
    );
    assert_eq!(runs(&sandbox.work())?, 2);
    Ok(())
}

/// a disk holding the cache with less free than `build_floor` would stop a step at its first
/// look, so a build refuses before any step runs, with `--yes` too. it names what frees room and
/// the floor itself, never `--yes`, and data.lock and the data folder stay as they were.
#[test]
fn a_build_below_the_floor_stops_before_any_step() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    let pinned = pins(&sandbox.work())?;
    let data = files(&sandbox.work().join("data"))?;
    for args in [&["--json", "build"][..], &["--json", "--yes", "build"]] {
        let output = building_in(&sandbox, &sandbox.work(), args)?
            .env("FETCHLOOM_TEST_FREE", "1000")
            .env("FETCHLOOM_BUILD_FLOOR", "1M")
            .output()?;
        assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        let (kind, message, _, help, _) = error(&document);
        assert_eq!(kind, "local.io");
        assert_eq!(
            message,
            "the disk holding the cache has 1000 B free, less than the 1.0 MiB build_floor keeps free, so no step can run"
        );
        assert!(help.contains("lower build_floor"), "{help}");
        assert!(!help.contains("--yes"), "{help}");
        assert_eq!(runs(&sandbox.work())?, 0, "{args:?}");
    }
    assert_eq!(pins(&sandbox.work())?, pinned);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    Ok(())
}

/// sends `signal`, such as `-TERM`, to the process `pid` with `kill`.
#[cfg(unix)]
fn signal(pid: u32, signal: &str) -> Result {
    let sent = Command::new("kill")
        .args([signal, &pid.to_string()])
        .status()?;
    if sent.success() {
        Ok(())
    } else {
        Err(format!("kill {signal} failed").into())
    }
}

/// the stops a test can send a build: ctrl-c, and on linux and macos the SIGTERM a scheduler
/// sends a job it ends.
#[cfg(unix)]
const STOPS: [&str; 2] = ["-INT", "-TERM"];
#[cfg(windows)]
const STOPS: [&str; 1] = ["ctrl-c"];

/// a project whose recipe runs the step program in `mode`, `leave-child` or `start-child`, with
/// the server it adds iris from, and the lock and the marker of the process the step starts.
fn leaving_a_child(mode: &str) -> Result<(Sandbox, Server, PathBuf, PathBuf)> {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let held = sandbox.folder("held")?;
    let (lock, marker) = (held.join("lock"), held.join("locked"));
    with(
        &sandbox,
        &server,
        &recipe(&[mode, &word(&lock), &word(&marker)]),
    )?;
    Ok((sandbox, server, lock, marker))
}

/// a stop, by ctrl-c or by the SIGTERM a scheduler sends a job it ends, ends the step and what
/// it started, on windows even a process that ignores ctrl-c, and leaves data.lock and the data
/// folder as they were.
#[test]
fn a_stopped_step_leaves_no_process_behind() -> Result {
    for stop in STOPS {
        let (sandbox, _server, lock, marker) = leaving_a_child("leave-child")?;
        let pinned = pins(&sandbox.work())?;
        let data = files(&sandbox.work().join("data"))?;
        let command = building_in(&sandbox, &sandbox.work(), &["build"])?;
        #[cfg(windows)]
        let command = own_console(command);
        let mut running = crate::project::Background::start(command)?;
        wait_until("the step's child to take its lock", || Ok(marker.exists()))?;
        #[cfg(unix)]
        signal(running.child.id(), stop)?;
        #[cfg(windows)]
        ctrl_c(&sandbox, running.child.id())?;
        running.child.wait()?;
        lock_freed(&lock)?;
        let (code, stderr) = running.finish()?;
        assert_eq!(code, Some(130), "{stop}: {stderr}");
        assert_eq!(pins(&sandbox.work())?, pinned, "{stop}");
        assert_eq!(files(&sandbox.work().join("data"))?, data, "{stop}");
    }
    Ok(())
}

/// a build that copies iris into `clean`, run with its first mark of finishing work held until
/// the file `release` appears, once it wrote `<release>.held`, and what data.toml and data.lock
/// held before it.
#[cfg(unix)]
fn held_after_the_mark(
    sandbox: &Sandbox,
    release: &Path,
) -> Result<(Running, BTreeMap<String, Vec<u8>>)> {
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(sandbox, &server, COPY)?;
    let pinned = pins(&sandbox.work())?;
    let mut command = building_in(sandbox, &sandbox.work(), &["build"])?;
    command.env("FETCHLOOM_TEST_HOLD_FINISH", release);
    let running = Running::start(&mut command)?;
    let mut held = release.as_os_str().to_owned();
    held.push(".held");
    wait_until("the build to mark its work as finishing", || {
        Ok(Path::new(&held).exists())
    })?;
    Ok((running, pinned))
}

/// a SIGTERM once the build has run its steps and marked its work as finishing, right before it
/// writes data.lock, no longer stops it: it says it finishes what it started, never that it is
/// stopping, then pins and places the result and ends with its closing line.
#[cfg(unix)]
#[test]
fn a_term_after_the_build_marks_its_work_lets_it_finish() -> Result {
    let sandbox = Sandbox::new()?;
    let release = sandbox.folder("hold")?.join("release");
    let (running, pinned) = held_after_the_mark(&sandbox, &release)?;
    assert_eq!(
        pins(&sandbox.work())?,
        pinned,
        "the mark comes before data.lock is written"
    );
    running.signal("-TERM")?;
    running.wait_for("Finishing")?;
    fs::write(&release, b"")?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("  Finishing what it started (ctrl-c again to quit at once)\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("Stopping"), "{stderr}");
    assert!(
        stderr.contains("         Ok pinned clean's result in data.lock\n"),
        "{stderr}"
    );
    assert_eq!(fs::read(sandbox.work().join("data/clean/iris.csv"))?, IRIS);
    Ok(())
}

/// a second SIGTERM quits a build at once, even past its mark, says so in one line and exits 130.
#[cfg(unix)]
#[test]
fn a_second_term_quits_a_build_at_once() -> Result {
    let sandbox = Sandbox::new()?;
    let release = sandbox.folder("hold")?.join("release");
    let (running, _) = held_after_the_mark(&sandbox, &release)?;
    // only keeps a build that waits for the hold from holding the test for good
    let deadline = release.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let _ = fs::write(deadline, b"");
    });
    running.signal("-TERM")?;
    running.wait_for("Finishing")?;
    running.signal("-TERM")?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert!(!release.exists(), "the build waited for its hold: {stderr}");
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(stderr.ends_with("       Quit at once\n"), "{stderr}");
    Ok(())
}

/// a SIGTERM once `--accept` has marked its work as finishing, right before it writes data.lock,
/// lets it pin and place the kept rebuild and end with its closing line, never saying it stops.
#[cfg(unix)]
#[test]
fn a_term_after_accept_marks_its_work_lets_it_finish() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    kept_rebuild(&sandbox, &server, &recipe(&["random"]))?;
    let pinned = pins(&sandbox.work())?;
    let release = sandbox.folder("hold")?.join("release");
    let mut command = building_in(&sandbox, &sandbox.work(), &["build", "--accept"])?;
    command.env("FETCHLOOM_TEST_HOLD_FINISH", &release);
    let running = Running::start(&mut command)?;
    let mut held = release.as_os_str().to_owned();
    held.push(".held");
    wait_until("the accept to mark its work as finishing", || {
        Ok(Path::new(&held).exists())
    })?;
    assert_eq!(
        pins(&sandbox.work())?,
        pinned,
        "the mark comes before data.lock is written"
    );
    running.signal("-TERM")?;
    running.wait_for("Finishing")?;
    fs::write(&release, b"")?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Stopping"), "{stderr}");
    assert!(
        stderr.contains("         Ok pinned clean's kept rebuild in data.lock\n"),
        "{stderr}"
    );
    assert_ne!(pins(&sandbox.work())?, pinned);
    Ok(())
}

/// a SIGTERM while a step runs stops the build: it says so before any line of the stopped
/// build, ends the step, and leaves data.lock and the data folder as they were.
#[cfg(unix)]
#[test]
fn a_term_during_a_step_is_told_before_the_build_stops() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let held = sandbox.folder("held")?;
    let (marker, release) = (held.join("started"), held.join("release"));
    with(
        &sandbox,
        &server,
        &recipe(&["hold", &word(&marker), &word(&release)]),
    )?;
    let pinned = pins(&sandbox.work())?;
    let data = files(&sandbox.work().join("data"))?;
    let running = Running::start(&mut building_in(&sandbox, &sandbox.work(), &["build"])?)?;
    wait_until("the step to start", || Ok(marker.exists()))?;
    running.signal("-TERM")?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    let told: Vec<&str> = stderr.lines().collect();
    let stopping = told
        .iter()
        .position(|line| *line == "   Stopping (press ctrl-c again to quit at once)");
    let stopped = told.iter().position(|line| line.starts_with("    Stopped"));
    assert!(
        matches!((stopping, stopped), (Some(stopping), Some(stopped)) if stopping < stopped),
        "{stderr}"
    );
    assert!(!release.exists());
    assert_eq!(pins(&sandbox.work())?, pinned);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    Ok(())
}

/// a step that ends ends what it started and left running too, so nothing writes while its
/// result is taken in.
#[test]
fn a_finished_step_leaves_no_process_behind() -> Result {
    let (sandbox, _server, lock, _) = leaving_a_child("start-child")?;
    let mut running =
        crate::project::Background::start(building_in(&sandbox, &sandbox.work(), &["build"])?)?;
    running.child.wait()?;
    lock_freed(&lock)?;
    let (code, stderr) = running.finish()?;
    assert_eq!(code, Some(0), "{stderr}");
    Ok(())
}

/// on windows a build killed outright, as task manager or a cancelled CI job ends one, takes its
/// step and all the step started along, since the step's job closes with fetchloom.
#[cfg(windows)]
#[test]
fn a_killed_build_leaves_no_process_behind() -> Result {
    let (sandbox, _server, lock, marker) = leaving_a_child("leave-child")?;
    let mut running =
        crate::project::Background::start(building_in(&sandbox, &sandbox.work(), &["build"])?)?;
    wait_until("the step's child to take its lock", || Ok(marker.exists()))?;
    running.child.kill()?;
    running.child.wait()?;
    lock_freed(&lock)?;
    running.finish()?;
    Ok(())
}

/// waits until the process the step program started in `lock` mode ends and frees `lock`. a
/// test waits for this before it reads fetchloom's stderr to its end, since on windows that
/// process inherits fetchloom's own handles, its stderr among them.
fn lock_freed(lock: &Path) -> Result {
    wait_until("the step's child to end and free its lock", || {
        Ok(fs::OpenOptions::new()
            .write(true)
            .open(lock)?
            .try_lock()
            .is_ok())
    })
}

/// `path` as a word of [`recipe`], which data.toml reads as a quoted string.
fn word(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "\\\\")
}

/// the windows process creation flag that gives a console program a console of its own, without
/// a window.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `command` with a console of its own, so the ctrl-c [`ctrl_c`] presses there reaches no other
/// process.
#[cfg(windows)]
fn own_console(mut command: Command) -> Command {
    std::os::windows::process::CommandExt::creation_flags(&mut command, CREATE_NO_WINDOW);
    command
}

/// presses ctrl-c in the console of the process `pid`, which needs a console of its own, as
/// [`own_console`] gives it. windows has no command for this and the crate forbids unsafe code,
/// so powershell calls the console functions.
#[cfg(windows)]
fn ctrl_c(sandbox: &Sandbox, pid: u32) -> Result {
    let script = sandbox.folder("console")?.join("ctrl-c.ps1");
    fs::write(&script, CTRL_C)?;
    let mut powershell = Command::new("powershell");
    powershell
        .args(["-NoLogo", "-NoProfile", "-NonInteractive"])
        .args(["-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg(pid.to_string())
        .stdin(std::process::Stdio::null());
    let output = powershell.output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "powershell could not press ctrl-c ({}): {}",
            output.status,
            text(&output.stderr)?
        )
        .into())
    }
}

/// a powershell script that presses ctrl-c in the console of the process its argument names: it
/// leaves its own console for that one, ignores ctrl-c itself, and sends it to every process
/// there.
#[cfg(windows)]
const CTRL_C: &str = r#"param([uint32] $Id)
Add-Type -Name Console -Namespace FetchloomTests -MemberDefinition @'
[DllImport("kernel32.dll")] public static extern bool FreeConsole();
[DllImport("kernel32.dll")] public static extern bool AttachConsole(uint id);
[DllImport("kernel32.dll")] public static extern bool SetConsoleCtrlHandler(System.IntPtr handler, bool add);
[DllImport("kernel32.dll")] public static extern bool GenerateConsoleCtrlEvent(uint signal, uint group);
'@
[void] [FetchloomTests.Console]::FreeConsole()
if (-not [FetchloomTests.Console]::AttachConsole($Id)) { exit 2 }
[void] [FetchloomTests.Console]::SetConsoleCtrlHandler([IntPtr]::Zero, $true)
if (-not [FetchloomTests.Console]::GenerateConsoleCtrlEvent(0, 0)) { exit 3 }
exit 0
"#;

/// a step that runs fetchloom on its own project fails at once, rather than waiting for the
/// build that runs it.
#[test]
fn a_step_calling_fetchloom_on_its_project_fails_fast() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let fetchloom = crate::support::executable()
        .to_string_lossy()
        .replace('\\', "\\\\");
    with(&sandbox, &server, &recipe(&["call", &fetchloom]))?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, details, _, _) = error(&document);
    assert_eq!(kind, "build.failed");
    assert!(message.contains("exited with code 2"), "{message}");
    assert!(
        details
            .iter()
            .any(|line| line.contains("runs inside `fetchloom build` of this project")),
        "{details:?}"
    );
    Ok(())
}

/// the objects a cache at `root` holds, by their SHA-256.
fn objects(root: &Path) -> Result<std::collections::BTreeSet<String>> {
    Ok(files(&root.join("objects"))?
        .into_keys()
        .map(|path| path.replace('/', ""))
        .collect())
}

/// a rebuild of an unchanged recipe that makes other files is refused: data.lock keeps the pin
/// it has, since only a person may choose the new files. the rebuild is kept for them: status
/// lists it and cache gc keeps its files.
#[test]
fn a_rebuild_with_other_files_keeps_the_pin_and_the_rebuild() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["random"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    // a machine whose cache never held the result, nor its data folder
    crate::support::delete(&placed(&sandbox, "clean", "random.bin"))?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let data = files(&sandbox.work().join("data"))?;
    let elsewhere = sandbox.folder("another cache")?;
    let there = |args: &[&str]| -> Result<std::process::Output> {
        Ok(building_in(&sandbox, &sandbox.work(), args)?
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()?)
    };
    let output = there(&["--json", "build"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    assert_eq!(runs(&sandbox.work())?, 2);
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    let status = json(&there(&["--json", "status"])?)?;
    assert_eq!(kept_names(&status), ["clean"]);
    let told = text(&there(&["status"])?.stdout)?;
    assert!(
        told.contains("       Kept clean · a rebuild that differs from data.lock; `fetchloom build --accept clean` pins it\n"),
        "{told}"
    );
    let document = json(&output)?;
    let (kind, message, details, help, fix) = error(&document);
    assert_eq!(kind, "build.differs");
    assert_eq!(message, "clean differs from data.lock: 0 of 1 files match");
    assert_eq!(details, ["changed random.bin"]);
    assert!(
        help.contains("data.lock and data/clean are as they were"),
        "{help}"
    );
    assert!(help.contains("`fetchloom build --accept clean`"), "{help}");
    assert_eq!(fix, &Value::Null, "only a person accepts a rebuild");
    let step = &document["result"]["steps"][0];
    assert_eq!(step["action"], "ran");
    assert_eq!(step["check"]["changed"], serde_json::json!(["random.bin"]));
    assert_ne!(step["candidate"], step["tree"]);
    let kept = objects(&elsewhere)?;
    let gc = there(&["cache", "gc", "--now"])?;
    assert_eq!(gc.status.code(), Some(0), "{}", text(&gc.stderr)?);
    assert_eq!(objects(&elsewhere)?, kept, "gc keeps the rebuild's files");
    Ok(())
}

/// a step that reads a file outside its code, as a program or library of the machine it runs on,
/// makes other files where that file differs, and the same ones each time. the build keeps the
/// pin and the rebuild, and its help asks for a look at the versions and `status -v` before
/// `--accept`, rather than calling the step random.
#[test]
fn a_rebuild_with_another_tool_version_is_compared_before_it_is_accepted() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let tool = sandbox.work().join("tool");
    fs::create_dir(&tool)?;
    fs::write(tool.join("version.txt"), "1.0\n")?;
    let versioned = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"copy\", \"tool\", \"{iris}\", \"{out}\"]\n";
    with(&sandbox, &server, versioned)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    fs::write(tool.join("version.txt"), "2.0\n")?;
    crate::support::delete(&object(&sandbox, b"1.0\n"))?;
    crate::support::delete(&placed(&sandbox, "clean", "version.txt"))?;
    let pinned = pins(&sandbox.work())?;
    let mut kept = Vec::new();
    for _ in 0..2 {
        let output = build(&sandbox, &["--json", "build"])?;
        assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        let (kind, _, details, help, _) = error(&document);
        assert_eq!(kind, "build.differs");
        assert_eq!(details, ["changed version.txt"]);
        assert!(!help.contains("not reproducible"), "{help}");
        assert!(
            help.contains("another version of a program or library"),
            "{help}"
        );
        let compare = help.find("`fetchloom status -v`").ok_or(help)?;
        let accept = help.find("`fetchloom build --accept clean`").ok_or(help)?;
        assert!(compare < accept, "{help}");
        kept.push(document["result"]["steps"][0]["candidate"].clone());
    }
    assert_eq!(kept[0], kept[1], "the step made the same files twice");
    assert_eq!(pins(&sandbox.work())?, pinned);
    Ok(())
}

/// the recipes with a kept rebuild in the `--json` document of a status.
fn kept_names(status: &Value) -> Vec<&str> {
    status["result"]["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|kept| kept["name"].as_str())
        .collect()
}

/// a project whose `clean` writes random bytes, built once, then checked, which keeps a rebuild
/// that differs from data.lock.
fn kept_rebuild(sandbox: &Sandbox, server: &Server, recipe: &str) -> Result {
    with(sandbox, server, recipe)?;
    assert_eq!(build(sandbox, &["build"])?.status.code(), Some(0));
    let output = build(sandbox, &["build", "--check"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stderr)?);
    Ok(())
}

/// `status -v` names each file of a kept rebuild that differs from data.lock with the cache file
/// that holds the rebuild's bytes, and `--json` gives the same with the file data.lock pins, so a
/// person can compare them before `--accept`, which pins those very bytes.
#[test]
fn status_names_where_the_files_of_a_kept_rebuild_are() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    kept_rebuild(&sandbox, &server, &recipe(&["random"]))?;
    let placed = sandbox.work().join("data").join("clean").join("random.bin");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    let kept = &document["result"]["candidates"][0];
    assert_eq!(kept["name"], "clean");
    let files = kept["files"].as_array().ok_or("no files")?;
    assert_eq!(files.len(), 1, "{kept}");
    assert_eq!(
        (&files[0]["path"], &files[0]["difference"]),
        (&Value::from("random.bin"), &Value::from("changed"))
    );
    let rebuilt = PathBuf::from(files[0]["file"].as_str().ok_or("no file")?);
    let pinned = PathBuf::from(files[0]["pinned"].as_str().ok_or("no pinned file")?);
    assert!(
        rebuilt.starts_with(sandbox.cache()),
        "{}",
        rebuilt.display()
    );
    assert_eq!(fs::read(&pinned)?, fs::read(&placed)?);
    assert_ne!(fs::read(&rebuilt)?, fs::read(&placed)?);
    let told = text(&sandbox.run(&["-v", "status"])?.stdout)?;
    let line = format!("            changed random.bin: {}\n", rebuilt.display());
    assert!(told.contains(&line), "{line:?} in\n{told}");
    let told = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        told.contains("  hint: `fetchloom status -v` lists where the files of each kept rebuild that differ from data.lock are\n"),
        "{told}"
    );
    assert_eq!(
        build(&sandbox, &["build", "--accept"])?.status.code(),
        Some(0)
    );
    assert_eq!(fs::read(&placed)?, fs::read(&rebuilt)?);
    Ok(())
}

/// a check runs a step again from the inputs data.lock pins and compares what it makes, file by
/// file, with the result data.lock pins, which a step that copies its input matches. data.lock
/// and the data folder stay as they were.
#[test]
fn check_reproduces_a_deterministic_step() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let pinned = pins(&sandbox.work())?;
    let data = files(&sandbox.work().join("data"))?;
    let output = build(&sandbox, &["--json", "build", "--check"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let result = &document["result"];
    assert_eq!(result["lock_changed"], false);
    assert_eq!(result["placement"], Value::Null);
    let step = &result["steps"][0];
    assert_eq!(step["action"], "ran");
    assert_eq!(
        step["check"],
        serde_json::json!({"matching": 1, "changed": [], "missing": [], "extra": []})
    );
    assert_eq!(step["candidate"], Value::Null);
    let told = text(&build(&sandbox, &["build", "--check"])?.stderr)?;
    assert!(
        told.contains("    Checked clean · 1 of 1 files match data.lock\n"),
        "{told}"
    );
    assert_eq!(runs(&sandbox.work())?, 3, "each check runs the step");
    assert_eq!(pins(&sandbox.work())?, pinned);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    Ok(())
}

/// a check of a step that is not reproducible fails with what differs and keeps the rebuild,
/// leaving data.lock and the data folder as they were, and `--accept` then pins those files,
/// without running anything, places them, and has nothing left to accept after.
#[test]
fn check_keeps_a_differing_rebuild_and_accept_pins_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["random"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let pinned = pins(&sandbox.work())?;
    let data = files(&sandbox.work().join("data"))?;
    let checked = build(&sandbox, &["--json", "build", "--check"])?;
    assert_eq!(
        checked.status.code(),
        Some(60),
        "{}",
        text(&checked.stdout)?
    );
    assert_eq!(pins(&sandbox.work())?, pinned);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    let document = json(&checked)?;
    let (kind, message, details, _, fix) = error(&document);
    assert_eq!(kind, "build.differs");
    assert_eq!(message, "clean differs from data.lock: 0 of 1 files match");
    assert_eq!(details, ["changed random.bin"]);
    assert_eq!(fix, &Value::Null);
    let kept = document["result"]["steps"][0]["candidate"].clone();
    assert!(
        kept.as_str()
            .is_some_and(|tree| tree.starts_with("sha256:"))
    );
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(kept_names(&status), ["clean"]);
    let placed = sandbox.work().join("data/clean/random.bin");
    let before = fs::read(&placed)?;
    let output = build(&sandbox, &["--json", "build", "--accept", "clean"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(runs(&sandbox.work())?, 2, "accepting runs nothing");
    let document = json(&output)?;
    let step = &document["result"]["steps"][0];
    assert_eq!(step["action"], "accepted");
    assert_eq!(step["tree"], kept);
    assert_eq!(document["result"]["lock_changed"], true);
    assert_ne!(
        fs::read(&placed)?,
        before,
        "the data folder holds the accepted files"
    );
    let lock = fs::read_to_string(sandbox.work().join("data.lock"))?;
    assert!(lock.contains(&format!("tree = {kept}")), "{lock}");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(status["result"]["candidates"], serde_json::json!([]));
    let again = build(&sandbox, &["--json", "build", "--accept", "clean"])?;
    assert_eq!(again.status.code(), Some(2), "{}", text(&again.stdout)?);
    let refused = json(&again)?;
    let (kind, message, _, _, _) = error(&refused);
    assert_eq!(kind, "usage");
    assert_eq!(message, "there is no kept rebuild of clean to accept");
    let rebuilt = build(&sandbox, &["--json", "build"])?;
    assert_eq!(
        json(&rebuilt)?["result"]["steps"][0]["action"],
        "up_to_date"
    );
    Ok(())
}

/// a check runs every step from the inputs data.lock pins, so a step after one that differs is
/// checked against the result data.lock pins for what it reads, not the rebuild, and every step
/// is reported.
#[test]
fn checks_report_every_step() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let tidy = "\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"recipe-step\", \"copy\", \"{clean}\", \"{out}\"]\n";
    let noise = "\n[datasets.noise]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"random\", \"{iris}\", \"{out}\"]\n";
    with(
        &sandbox,
        &server,
        &format!("{}{tidy}{noise}", recipe(&["random"])),
    )?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let output = build(&sandbox, &["--json", "build", "--check"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let steps = document["result"]["steps"].as_array().ok_or("no steps")?;
    let names: Vec<&str> = steps
        .iter()
        .filter_map(|step| step["name"].as_str())
        .collect();
    assert_eq!(names, ["clean", "noise", "tidy"]);
    for differing in [&steps[0], &steps[1]] {
        assert_eq!(
            differing["check"]["changed"],
            serde_json::json!(["random.bin"])
        );
    }
    assert_eq!(steps[2]["check"]["matching"], 1);
    assert_eq!(steps[2]["candidate"], Value::Null);
    let (_, message, details, help, _) = error(&document);
    assert_eq!(message, "clean and noise differ from data.lock");
    assert_eq!(
        details,
        [
            "clean: 0 of 1 files match data.lock",
            "changed random.bin",
            "noise: 0 of 1 files match data.lock",
            "changed random.bin"
        ]
    );
    assert!(
        help.contains("once you choose them, `fetchloom build --accept clean noise` pins them"),
        "{help}"
    );
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(kept_names(&status), ["clean", "noise"]);
    Ok(())
}

/// a check runs a step from the result data.lock pins for each recipe it reads, never from a
/// rebuild: where that result is in no cache, data folder or mirror, as on a fresh machine after
/// the check of the recipe made other files, the step is skipped, and the error says so.
#[test]
fn a_check_skips_a_step_whose_pinned_input_is_nowhere() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    let tidy = "\n[datasets.tidy]\nfrom = [\"clean\"]\nrun = [\"recipe-step\", \"copy\", \"{clean}\", \"{out}\"]\n";
    with(&sandbox, &server, &format!("{}{tidy}", recipe(&["random"])))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    // tidy copies clean, so the data folder holds clean's result twice
    for dataset in ["clean", "tidy"] {
        crate::support::delete(&placed(&sandbox, dataset, "random.bin"))?;
    }
    let elsewhere = sandbox.folder("another cache")?;
    let output = building_in(&sandbox, &sandbox.work(), &["--json", "build", "--check"])?
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let steps = document["result"]["steps"].as_array().ok_or("no steps")?;
    assert!(steps[0]["candidate"].is_string(), "{document}");
    assert_eq!(steps[1]["action"], "skipped");
    assert_eq!(steps[1]["check"], Value::Null);
    let (kind, _, details, _, _) = error(&document);
    assert_eq!(kind, "build.differs");
    assert!(
        details.contains(&"tidy was not checked: it reads a result data.lock pins that is not on this machine or a mirror"),
        "{details:?}"
    );
    assert_eq!(runs(&sandbox.work())?, 3, "tidy did not run in the check");
    Ok(())
}

/// a check compares with what data.lock pins, so a recipe never built, or changed since it was,
/// is `lock.outdated`, exit 1, before any step runs.
#[test]
fn a_check_refuses_drift_before_running() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY_CODE)?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    let output = build(&sandbox, &["--json", "build", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, _, _) = error(&document);
    assert_eq!(kind, "lock.outdated");
    assert_eq!(message, "data.lock is out of date: clean is not built yet");
    assert_eq!(runs(&sandbox.work())?, 0);
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    fs::write(sandbox.work().join("clean.py"), "print(2)\n")?;
    let output = build(&sandbox, &["--json", "build", "--check"])?;
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, details, _, _) = error(&document);
    assert_eq!(kind, "lock.outdated");
    assert_eq!(
        message,
        "data.lock is out of date: clean changed since it was built"
    );
    assert_eq!(details, ["clean: clean.py changed"]);
    assert_eq!(runs(&sandbox.work())?, 1);
    Ok(())
}

/// `--locked` never writes data.lock: a recipe never built or changed since, or a fetched
/// dataset data.toml changed, is `lock.outdated` before anything is fetched or run, and so is a
/// build that would change a pin anyway, as by dropping one of a recipe data.toml no longer has.
#[test]
fn locked_build_never_writes_data_lock() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let extra = "\n[datasets.extra]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"copy\", \"{iris}\", \"{out}\"]\n";
    with(&sandbox, &server, &format!("{COPY_CODE}{extra}"))?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    let refused = |args: &[&str], said: &str| -> Result {
        let lock = fs::read(sandbox.work().join("data.lock"))?;
        let runs_before = runs(&sandbox.work())?;
        let output = build(&sandbox, args)?;
        assert_eq!(output.status.code(), Some(1), "{}", text(&output.stdout)?);
        let document = json(&output)?;
        let (kind, message, _, _, _) = error(&document);
        assert_eq!(kind, "lock.outdated");
        assert_eq!(message, said);
        assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
        assert_eq!(runs(&sandbox.work())?, runs_before, "{args:?} ran a step");
        Ok(())
    };
    let locked = ["--json", "build", "--locked", "clean"];
    refused(&locked, "data.lock is out of date: clean is not built yet")?;
    refused(
        &["--json", "build", "--locked", "--dry-run"],
        "data.lock is out of date: clean and extra need building",
    )?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    fs::write(sandbox.work().join("clean.py"), "print(2)\n")?;
    refused(
        &locked,
        "data.lock is out of date: clean changed since it was built",
    )?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    let manifest = sandbox.work().join("data.toml");
    let text_now = fs::read_to_string(&manifest)?;
    fs::write(&manifest, text_now.replace(extra, ""))?;
    refused(
        &locked,
        "data.lock is out of date: building would change what it pins of extra",
    )?;
    fs::write(&manifest, &text_now)?;
    let requests = server.requests().len();
    fs::write(
        &manifest,
        text_now.replacen(
            "[datasets.iris]\n",
            "[datasets.iris]\nselect = \"*.csv\"\n",
            1,
        ),
    )?;
    refused(&locked, "data.lock is out of date with data.toml")?;
    assert_eq!(server.requests().len(), requests, "nothing was fetched");
    fs::write(&manifest, &text_now)?;
    let lock = fs::read(sandbox.work().join("data.lock"))?;
    let output = build(&sandbox, &locked)?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    assert_eq!(json(&output)?["result"]["steps"][0]["action"], "up_to_date");
    assert_eq!(fs::read(sandbox.work().join("data.lock"))?, lock);
    let accept = build(&sandbox, &["build", "--locked", "--accept"])?;
    assert_eq!(accept.status.code(), Some(2), "{}", text(&accept.stderr)?);
    Ok(())
}

/// `--prepare` puts in the cache everything a build of the recipes reads and runs nothing, as on
/// a login node, so a build without the network afterwards, as in a job, makes no request.
#[test]
fn prepare_fetches_inputs_so_an_offline_build_makes_no_requests() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    // a machine whose cache never held iris, nor its data folder
    crate::support::delete(&placed(&sandbox, "iris", "iris.csv"))?;
    let elsewhere = sandbox.folder("another cache")?;
    let there = |args: &[&str]| -> Result<std::process::Output> {
        Ok(building_in(&sandbox, &sandbox.work(), args)?
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()?)
    };
    let pinned = pins(&sandbox.work())?;
    let prepared = there(&["--json", "build", "--prepare"])?;
    assert_eq!(
        prepared.status.code(),
        Some(0),
        "{}",
        text(&prepared.stdout)?
    );
    let document = json(&prepared)?;
    assert_eq!(document["result"]["steps"][0]["action"], "prepared");
    assert_eq!(runs(&sandbox.work())?, 0, "preparing runs nothing");
    assert_eq!(server.requests().len(), 2, "iris came into the new cache");
    assert_eq!(pins(&sandbox.work())?, pinned);
    assert!(!sandbox.work().join("data").join("clean").exists());
    let built = there(&["--json", "--offline", "build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stdout)?);
    let document = json(&built)?;
    assert_eq!(document["result"]["steps"][0]["action"], "ran");
    assert_eq!(document["work"]["requests"], 0);
    assert_eq!(server.requests().len(), 2);
    Ok(())
}

/// a build of a step data.lock pins, whose result the cache holds, reads nothing, so `--prepare`
/// fetches nothing for it, while a check runs every step, so `--prepare --check` fetches what it
/// reads, and a check without the network then makes no request.
#[test]
fn prepare_check_fetches_what_a_check_reads() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "made"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    crate::support::delete(&object(&sandbox, IRIS))?;
    // an edit sync keeps, so placing needs no bytes of iris and the data folder gives none
    crate::support::edit(&placed(&sandbox, "iris", "iris.csv"), b"edited by hand\n")?;
    let prepared = build(&sandbox, &["--json", "build", "--prepare"])?;
    assert_eq!(
        prepared.status.code(),
        Some(0),
        "{}",
        text(&prepared.stdout)?
    );
    assert_eq!(
        json(&prepared)?["result"]["steps"][0]["action"],
        "up_to_date"
    );
    assert_eq!(server.requests().len(), 1, "a build of clean reads nothing");
    let prepared = build(&sandbox, &["--json", "build", "--prepare", "--check"])?;
    assert_eq!(
        prepared.status.code(),
        Some(0),
        "{}",
        text(&prepared.stdout)?
    );
    assert_eq!(json(&prepared)?["result"]["steps"][0]["action"], "prepared");
    assert_eq!(server.requests().len(), 2, "a check of clean reads iris");
    let checked = build(&sandbox, &["--json", "--offline", "build", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stdout)?);
    assert_eq!(json(&checked)?["work"]["requests"], 0);
    assert_eq!(
        runs(&sandbox.work())?,
        2,
        "only the build and the check ran"
    );
    Ok(())
}

/// a build places the datasets it took once its steps are done, so `--prepare` fetches a placed
/// dataset that no step reads, here since a mirror gives the result of the only step, and a
/// build without the network then makes no request.
#[test]
fn prepare_fetches_what_the_build_places() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "made"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let mirror = sandbox.folder("mirror")?;
    mirror_holding(&mirror, &[b"made"])?;
    let clone = sandbox.folder("clone")?;
    fs::create_dir(clone.join("data.lock.d"))?;
    pin_again(&clone, &pins(&sandbox.work())?)?;
    let elsewhere = sandbox.folder("another cache")?;
    let there = |args: &[&str]| -> Result<std::process::Output> {
        Ok(building_in(&sandbox, &clone, args)?
            .env("FETCHLOOM_CACHE", &elsewhere)
            .env("FETCHLOOM_MIRRORS", &mirror)
            .output()?)
    };
    let prepared = there(&["--json", "build", "--prepare"])?;
    assert_eq!(
        prepared.status.code(),
        Some(0),
        "{}",
        text(&prepared.stdout)?
    );
    assert_eq!(json(&prepared)?["result"]["steps"][0]["action"], "fetched");
    let built = there(&["--json", "--offline", "build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stdout)?);
    assert_eq!(json(&built)?["work"]["requests"], 0);
    assert_eq!(server.requests().len(), 2, "iris came once into each cache");
    Ok(())
}

/// `--prepare` ends by naming the build it prepared for, with the recipes named and `--check`
/// kept, and once data.lock pins every result as building it now would make, it names no step to
/// run.
#[cfg(target_os = "linux")]
#[test]
fn prepare_names_the_build_it_prepared_for() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    let told = |args: &[&str]| -> Result<Option<String>> {
        sandbox.on_a_terminal_running(&building_in(&sandbox, &sandbox.work(), args)?)
    };
    let Some(unbuilt) = told(&["build", "--prepare", "clean"])? else {
        return Ok(());
    };
    assert!(
        unbuilt
            .contains("  next: run `fetchloom --offline build clean` where the steps should run\n"),
        "{unbuilt}"
    );
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let checking = told(&["build", "--prepare", "--check", "clean"])?.unwrap_or_default();
    assert!(
        checking.contains(
            "  next: run `fetchloom --offline build --check clean` where the steps should run\n"
        ),
        "{checking}"
    );
    let built = told(&["build", "--prepare"])?.unwrap_or_default();
    assert!(built.contains(" Up to date clean\n"), "{built}");
    assert!(
        built.contains(
            "  next: run `fetchloom --offline build`, which places the results without running a step\n"
        ),
        "{built}"
    );
    assert_eq!(runs(&sandbox.work())?, 1, "only the build ran");
    Ok(())
}

/// a kept rebuild answers the recipe and the pin it was kept beside: after an edit of its code,
/// status says it is stale and `--accept` refuses it and changes nothing.
#[test]
fn accept_refuses_after_the_recipe_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let random = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"random\", \"{iris}\", \"{out}\"]\ncode = [\"clean.py\"]\n";
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    kept_rebuild(&sandbox, &server, random)?;
    let stale = || -> Result<Value> {
        Ok(json(&sandbox.run(&["--json", "status"])?)?["result"]["candidates"][0]["stale"].clone())
    };
    assert_eq!(stale()?, false);
    fs::write(sandbox.work().join("clean.py"), "print(2)\n")?;
    assert_eq!(stale()?, true);
    let status = sandbox.run(&["status"])?;
    // the row is on stdout and the hint on stderr
    let told = format!("{}{}", text(&status.stdout)?, text(&status.stderr)?);
    for line in [
        "       Kept clean · a rebuild from another recipe or pin than data.lock has now, so `fetchloom build --accept` refuses it\n",
        "  hint: `fetchloom build --check clean` runs its step again as data.lock pins it now, and keeps its files when they differ\n",
    ] {
        assert!(told.contains(line), "{line:?} in\n{told}");
    }
    let pinned = pins(&sandbox.work())?;
    let data = files(&sandbox.work().join("data"))?;
    let output = build(&sandbox, &["--json", "build", "--accept"])?;
    assert_eq!(output.status.code(), Some(2), "{}", text(&output.stdout)?);
    let refused = json(&output)?;
    let (kind, message, _, help, _) = error(&refused);
    assert_eq!(kind, "usage");
    assert_eq!(
        message,
        "the kept rebuild of clean came from another recipe or pin than data.lock has now"
    );
    assert!(help.contains("`fetchloom build --check clean`"), "{help}");
    assert_eq!(pins(&sandbox.work())?, pinned);
    assert_eq!(files(&sandbox.work().join("data"))?, data);
    let dry = build(&sandbox, &["build", "--accept", "--dry-run"])?;
    assert_eq!(dry.status.code(), Some(2), "{}", text(&dry.stderr)?);
    Ok(())
}

/// a kept rebuild goes with its recipe: a sync after data.toml lost the recipe, or `remove` of
/// it, drops the rebuild with the recipe's pin, so cache gc frees its files.
#[test]
fn a_kept_rebuild_goes_with_its_recipe() -> Result {
    for (edited, args) in [(true, &["sync"][..]), (false, &["remove", "clean"])] {
        let sandbox = Sandbox::new()?;
        let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
        let random = recipe(&["random"]);
        with(&sandbox, &server, &random)?;
        assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
        let built = objects(&sandbox.cache())?;
        assert_eq!(
            build(&sandbox, &["build", "--check"])?.status.code(),
            Some(60)
        );
        let kept: Vec<String> = objects(&sandbox.cache())?
            .difference(&built)
            .cloned()
            .collect();
        assert_eq!(kept.len(), 1, "the check keeps the file it made");
        if edited {
            let manifest = sandbox.work().join("data.toml");
            fs::write(
                &manifest,
                fs::read_to_string(&manifest)?.replace(&random, ""),
            )?;
        }
        let output = sandbox.run(args)?;
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
        let gc = sandbox.run(&["cache", "gc", "--now"])?;
        assert_eq!(gc.status.code(), Some(0), "{}", text(&gc.stderr)?);
        assert!(
            !objects(&sandbox.cache())?.contains(&kept[0]),
            "gc frees the kept rebuild once {args:?} dropped its recipe"
        );
    }
    Ok(())
}

/// sync places a pinned result from the cache and never runs its step; with the result in no
/// cache, it names the build that makes it, unless a download of the same sync brings its bytes.
#[test]
fn sync_places_a_result_and_never_builds_it() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let placed = sandbox.work().join("data").join("clean").join("iris.csv");
    crate::support::delete(&placed)?;
    let synced = build(&sandbox, &["sync", "clean"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(&placed)?, IRIS);
    crate::support::delete(&placed)?;
    let elsewhere = sandbox.folder("another cache")?;
    let output = building_in(&sandbox, &sandbox.work(), &["--json", "sync", "clean"])?
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, help, fix) = error(&document);
    assert_eq!(kind, "build.needed");
    assert_eq!(
        message,
        "clean's pinned result is not on this machine or a mirror"
    );
    assert_eq!(help, "run `fetchloom build clean` to make it here");
    assert_eq!(fix, &Value::Null, "building runs the project's code");
    // the step copies iris, so the download of iris brings the result's bytes too
    crate::support::delete(&sandbox.work().join("data").join("iris").join("iris.csv"))?;
    let output = building_in(&sandbox, &sandbox.work(), &["sync"])?
        .env("FETCHLOOM_CACHE", &elsewhere)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(
        !text(&output.stderr)?.contains("left out"),
        "{}",
        text(&output.stderr)?
    );
    assert_eq!(fs::read(&placed)?, IRIS);
    assert_eq!(runs(&sandbox.work())?, 1, "sync never runs a step");
    Ok(())
}

/// a sync of every dataset on a machine where neither the cache nor a mirror holds a pinned
/// result places the rest and leaves the result out with a warning naming its build, as an add
/// does, while `--locked` or naming the recipe stops.
#[test]
fn a_plain_sync_leaves_out_a_result_found_nowhere() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        PATH,
        vec![
            file(IRIS, "\"v1\""),
            file(IRIS, "\"v1\""),
            file(IRIS, "\"v1\""),
        ],
    )?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "done"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let data = sandbox.work().join("data");
    let (iris, clean) = (
        data.join("iris").join("iris.csv"),
        data.join("clean").join("clean.csv"),
    );
    crate::support::delete(&iris)?;
    crate::support::delete(&clean)?;
    let elsewhere = sandbox.folder("another cache")?;
    let there = |args: &[&str]| -> Result<std::process::Output> {
        Ok(building_in(&sandbox, &sandbox.work(), args)?
            .env("FETCHLOOM_CACHE", &elsewhere)
            .output()?)
    };
    let output = there(&["--json", "sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    assert_eq!(
        document["result"]["placement"]["unbuilt"],
        serde_json::json!(["clean"])
    );
    let warned = "clean's pinned result is not on this machine or a mirror, so it is left out; run `fetchloom build clean` to make it here";
    assert!(
        document["warnings"]
            .as_array()
            .is_some_and(|warnings| warnings.iter().any(|warning| warning == warned)),
        "{}",
        document["warnings"]
    );
    assert_eq!(fs::read(&iris)?, IRIS);
    assert!(!clean.exists());
    let again = there(&["sync"])?;
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr)?);
    let told = text(&again.stderr)?;
    assert!(told.contains("so it is left out\n"), "{told}");
    assert!(
        told.contains("Synced 1 dataset into data · 1 result left out until built\n"),
        "{told}"
    );
    let added = there(&["add", &server.url, "--as", "again"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    assert!(
        text(&added.stderr)?.contains("so it is left out\n"),
        "{}",
        text(&added.stderr)?
    );
    for args in [&["sync", "--locked"][..], &["sync", "clean"]] {
        let output = there(args)?;
        assert_eq!(
            output.status.code(),
            Some(60),
            "{args:?}: {}",
            text(&output.stderr)?
        );
    }
    assert_eq!(runs(&sandbox.work())?, 1, "sync never runs a step");
    Ok(())
}

/// what data.toml, data.lock and its list folder hold now, by path in the project.
fn pins(work: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut held: BTreeMap<String, Vec<u8>> = files(&work.join("data.lock.d"))?
        .into_iter()
        .map(|(path, bytes)| (format!("data.lock.d/{path}"), bytes))
        .collect();
    for name in ["data.toml", "data.lock"] {
        held.insert(name.to_owned(), fs::read(work.join(name))?);
    }
    Ok(held)
}

/// puts back what [`pins`] read.
fn pin_again(work: &Path, held: &BTreeMap<String, Vec<u8>>) -> Result {
    for (path, bytes) in held {
        fs::write(work.join(path), bytes)?;
    }
    Ok(())
}

/// a result left out takes along the file an older pin of it placed, so no script reads a
/// result data.lock no longer pins.
#[test]
fn a_left_out_result_retires_its_older_files() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "one"]))?;
    let work = sandbox.work();
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let first = pins(&work)?;
    let manifest = work.join("data.toml");
    let text_now = fs::read_to_string(&manifest)?;
    fs::write(&manifest, text_now.replace("\"one\"", "\"two\""))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let second = pins(&work)?;
    let placed = work.join("data").join("clean").join("clean.csv");
    pin_again(&work, &first)?;
    assert_eq!(sandbox.run(&["sync"])?.status.code(), Some(0));
    assert_eq!(fs::read(&placed)?, b"one");

    pin_again(&work, &second)?;
    let hex = sha256(b"two");
    crate::support::delete(
        &sandbox
            .cache()
            .join("objects")
            .join(&hex[..2])
            .join(&hex[2..]),
    )?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert!(
        text(&output.stderr)?.contains("so it is left out"),
        "{}",
        text(&output.stderr)?
    );
    assert!(!placed.exists(), "the older pin's file retires");
    assert_eq!(runs(&work)?, 2);
    Ok(())
}

/// `path` answers a built recipe's folder and files, and `export` lists them, as for any
/// dataset.
#[test]
fn path_and_export_answer_a_built_result() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, COPY)?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let folder = json(&sandbox.run(&["--json", "path", "clean"])?)?;
    assert_eq!(
        folder["result"]["path"],
        sandbox
            .work()
            .join("data")
            .join("clean")
            .to_string_lossy()
            .as_ref()
    );
    let file = sandbox.run(&["path", "clean/iris.csv"])?;
    assert_eq!(file.status.code(), Some(0), "{}", text(&file.stderr)?);
    let exported = sandbox.run(&["export"])?;
    assert_eq!(
        exported.status.code(),
        Some(0),
        "{}",
        text(&exported.stderr)?
    );
    assert!(
        text(&exported.stdout)?.contains(&format!("{}  data/clean/iris.csv", sha256(IRIS))),
        "{}",
        text(&exported.stdout)?
    );
    Ok(())
}

/// a step that changes its own code leaves a result no recipe answers, so the build fails.
#[test]
fn a_step_that_edits_its_code_fails() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let edits = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"write-code\", \"clean.py\", \"{iris}\", \"{out}\"]\ncode = [\"clean.py\"]\n";
    with(&sandbox, &server, edits)?;
    fs::write(sandbox.work().join("clean.py"), "print(1)\n")?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(60));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains("clean changed its own code while it ran: clean.py"),
        "{stderr}"
    );
    assert!(!fs::read_to_string(sandbox.work().join("data.lock"))?.contains("clean"));
    Ok(())
}

/// a step whose program is not there cannot start, and the build says where it looked: on PATH
/// for a bare name, and in the project folder for one with a folder in it, as a clone has none
/// before its environment is set up. once the project folder holds it, the same recipe builds.
#[test]
fn a_missing_program_cannot_start() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let missing = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"no-such-program-here\", \"{iris}\", \"{out}\"]\n";
    with(&sandbox, &server, missing)?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(60));
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(
            "clean cannot start: no program called no-such-program-here was found on PATH"
        ),
        "{stderr}"
    );
    let manifest = sandbox.work().join("data.toml");
    let written = fs::read_to_string(&manifest)?;
    fs::write(
        &manifest,
        written.replace("\"no-such-program-here\"", "\"env/bin/step\", \"copy\""),
    )?;
    let output = build(&sandbox, &["--json", "build"])?;
    assert_eq!(output.status.code(), Some(60), "{}", text(&output.stdout)?);
    let document = json(&output)?;
    let (kind, message, _, help, _) = error(&document);
    assert_eq!(kind, "build.failed");
    assert_eq!(
        message,
        "clean cannot start: the project folder has no env/bin/step"
    );
    assert!(
        help.contains("set up the project's environment so that env/bin/step is there"),
        "{help}"
    );
    assert!(!help.contains("full path"), "{help}");
    let bin = sandbox.work().join("env").join("bin");
    fs::create_dir_all(&bin)?;
    fs::copy(
        crate::support::recipe_step()?,
        bin.join(format!("step{}", std::env::consts::EXE_SUFFIX)),
    )?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    Ok(())
}

/// on windows a step's program is found through PATHEXT as a shell finds it, so a `.cmd` shim on
/// PATH, as npm and many installers leave, runs, and gets its arguments as written.
#[cfg(windows)]
#[test]
fn a_batch_shim_on_path_runs_with_its_arguments() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let shim = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-shim\", \"write\", \"said.txt\", \"50% & \\\"more\\\"\", \"{iris}\", \"{out}\"]\n";
    with(&sandbox, &server, shim)?;
    fs::write(
        sandbox.folder("bin")?.join("recipe-shim.cmd"),
        "@\"%~dp0recipe-step.exe\" %*\r\n",
    )?;
    let output = build(&sandbox, &["build"])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let said = sandbox.work().join("data").join("clean").join("said.txt");
    assert_eq!(fs::read_to_string(said)?, "50% & \"more\"");
    Ok(())
}

/// cache gc keeps the files of a result data.lock pins, which only a build could make again.
#[test]
fn gc_keeps_a_pinned_result() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, &recipe(&["write", "clean.csv", "made"]))?;
    assert_eq!(build(&sandbox, &["build"])?.status.code(), Some(0));
    let gc = sandbox.run(&["cache", "gc", "--now"])?;
    assert_eq!(gc.status.code(), Some(0), "{}", text(&gc.stderr)?);
    let made = sha256(b"made");
    let object = sandbox
        .cache()
        .join("objects")
        .join(&made[..2])
        .join(&made[2..]);
    assert!(object.is_file(), "{}", text(&gc.stderr)?);
    Ok(())
}
