//! outputs: a folder of the project pinned where its pipeline writes it, shared by hash, placed
//! writable for a teammate, and never partly healed once it changed.

use std::fs;
use std::path::Path;

use crate::project::{IRIS, PATH};
use crate::support::{Result, Routes, Running, Sandbox, file, json, ok, text, wait_until, warned};

const CLEAN: &[u8] = b"sepal,petal\n5.1,1.4\n";
const SUMMARY: &[u8] = b"150 rows\n";

/// the folder a pipeline writes, as cookiecutter's dataset.py writes data/processed.
fn processed(project: &Path) -> Result {
    fs::create_dir_all(project.join("data/processed"))?;
    fs::write(project.join("data/processed/iris_clean.csv"), CLEAN)?;
    fs::write(project.join("data/processed/summary.txt"), SUMMARY)?;
    Ok(())
}

/// what a check that a file stayed the author's compares: its bytes, length and modified time,
/// and on unix its inode and mode, which a link to the cache's copy or a sealed copy would change.
fn look(path: &Path) -> Result<(Vec<u8>, u64, std::time::SystemTime, u64, u32)> {
    let metadata = fs::symlink_metadata(path)?;
    #[cfg(unix)]
    let (inode, mode) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.ino(), metadata.mode())
    };
    #[cfg(not(unix))]
    let (inode, mode) = (0, 0);
    Ok((
        fs::read(path)?,
        metadata.len(),
        metadata.modified()?,
        inode,
        mode,
    ))
}

/// a rerun of the pipeline: one file rewritten, one gone, one new.
fn rerun(project: &Path) -> Result {
    let folder = project.join("data/processed");
    fs::write(
        folder.join("iris_clean.csv"),
        b"sepal,petal\n5.1,1.4\n4.9,1.4\n",
    )?;
    fs::remove_file(folder.join("summary.txt"))?;
    fs::write(folder.join("plot.png"), b"png")?;
    Ok(())
}

/// the paths of one kind status lists, as `--json` gives them.
fn listed(document: &serde_json::Value, kind: &str) -> Vec<String> {
    document["result"][kind]
        .as_array()
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// the pipeline's folder is pinned where it is: data.toml names it as the output's `at`, and its
/// files stay the author's, untouched and writable, with nothing left to place or hash.
#[test]
fn an_output_is_pinned_where_its_pipeline_wrote_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let files = [
        work.join("data/processed/iris_clean.csv"),
        work.join("data/processed/summary.txt"),
    ];
    let before = [look(&files[0])?, look(&files[1])?];
    let output = sandbox.run(&["add", "data/processed", "--as", "processed"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.processed]\nref = \"local\"\nat = \"data/processed\"\n"
    );
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("\nat = \"data/processed\"\n"), "{lock}");
    assert_eq!(lock.matches("{ path = ").count(), 2, "{lock}");
    assert!(
        stderr.contains("Added processed, its files are in data/processed"),
        "{stderr}"
    );
    // read once while pinning, so placing has nothing to read or adopt
    assert!(!stderr.contains("Adopted"), "{stderr}");
    assert_eq!(before, [look(&files[0])?, look(&files[1])?]);
    fs::OpenOptions::new().append(true).open(&files[0])?;
    let status = sandbox.run(&["--json", "status"])?;
    let document = json(&status)?;
    for kind in ["missing", "edited", "untracked", "extra", "outdated"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// a teammate's sync places the output where the author's pipeline wrote it, outside the data
/// folder too, as files of their own a pipeline may write over, never links to the cache's copy.
#[test]
fn a_teammate_gets_an_output_where_the_author_had_it_and_may_write_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        "[project]\ndata_dir = \"data/raw\"\n",
    )?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let placed = teammate.join("data/processed/iris_clean.csv");
    assert!(
        placed.is_file(),
        "where the author's code reads it: {stderr}"
    );
    assert!(!teammate.join("data/raw/processed").exists(), "{stderr}");
    assert_eq!(fs::read(&placed)?, CLEAN);
    assert_eq!(
        fs::read(teammate.join("data/processed/summary.txt"))?,
        SUMMARY
    );
    let metadata = fs::metadata(&placed)?;
    assert!(!metadata.permissions().readonly(), "{stderr}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(metadata.nlink(), 1, "a file of its own, not the cache's");
    }
    fs::write(&placed, b"rerun\n")?;
    let status = sandbox
        .command_in(&teammate, &["--json", "status"])
        .output()?;
    let document = json(&status)?;
    assert_eq!(
        listed(&document, "edited"),
        ["data/processed/iris_clean.csv"],
        "{document}"
    );
    Ok(())
}

/// an output's folder gets the .gitignore any dataset folder gets, outside the data folder too,
/// for the author and for a teammate. it is fetchloom's, never the output's: no check, pin,
/// status or sync takes it for a file of the folder.
#[test]
fn an_outputs_folder_gets_its_marker_and_never_takes_it_in() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/iris_clean.csv"), CLEAN)?;
    fs::write(work.join("results/summary.txt"), SUMMARY)?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    assert_eq!(fs::read(work.join("results/.gitignore"))?, b"*\n");
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert_eq!(lock.matches("{ path = ").count(), 2, "{lock}");
    let checked = sandbox.run(&["status", "--verify", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let again = sandbox.run(&["add", "results", "--as", "results"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("results is pinned as results holds it"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let synced = sandbox.command_in(&teammate, &["sync"]).output()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    assert_eq!(fs::read(teammate.join("results/.gitignore"))?, b"*\n");
    for project in [&work, &teammate] {
        let status = sandbox
            .command_in(project, &["--json", "status"])
            .output()?;
        let document = json(&status)?;
        for kind in ["missing", "edited", "extra", "yours"] {
            assert_eq!(
                listed(&document, kind),
                Vec::<String>::new(),
                "{kind}: {document}"
            );
        }
        let synced = sandbox.command_in(project, &["--json", "sync"]).output()?;
        let document = json(&synced)?;
        assert_eq!(synced.status.code(), Some(0), "{document}");
        assert!(
            warned(&document)
                .iter()
                .all(|said| !said.contains("changed since")),
            "{document}"
        );
    }
    Ok(())
}

/// git carries data.lock, never an output's bytes: a teammate whose cache and mirrors lack them
/// still gets every other dataset, and the run names the output once, as made on another machine,
/// with both ways on, ending as a sync does for a dataset no link fetches.
#[test]
fn a_teammate_without_an_outputs_bytes_gets_the_rest_and_both_ways_on() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let server = Routes::start(vec![(PATH.to_owned(), file(IRIS, "\"v1\""))])?;
    let link = format!("{}{PATH}", server.url);
    sandbox.allow(&link);
    ok(&sandbox.run(&["add", &link])?)?;
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let teammate = sandbox.folder("teammate")?;
    for file in ["data.toml", "data.lock"] {
        fs::copy(work.join(file), teammate.join(file))?;
    }
    let cache = sandbox.folder("teammate-cache")?;
    let sync = |args: &[&str]| {
        sandbox
            .command_in(&teammate, args)
            .env("FETCHLOOM_CACHE", &cache)
            .output()
    };
    let said = "processed was made on another machine, and its files are in no mirror this machine reaches";
    let synced = sync(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(10), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "reference.unresolved", "{document}");
    assert_eq!(error["message"], said, "{document}");
    assert_eq!(
        error["help"],
        "push them from the machine that made them with `fetchloom push --to <mirror>` and add that mirror here with `fetchloom settings set mirrors <mirror>`, or run your pipeline here, then `fetchloom add data/processed --as processed`"
    );
    assert_eq!(
        error["details"],
        serde_json::json!(["placed iris, whose files are all on this machine"])
    );
    assert_eq!(document["result"]["datasets"], serde_json::json!(["iris"]));
    assert_eq!(
        document["result"]["waiting"],
        serde_json::json!(["processed"])
    );
    assert_eq!(fs::read(teammate.join("data/iris/iris.csv"))?, IRIS);
    assert!(!teammate.join("data/processed").exists(), "{document}");
    let synced = sync(&["sync"])?;
    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(10), "{stderr}");
    assert_eq!(stderr.matches("processed").count(), 3, "{stderr}");
    assert!(stderr.contains(&format!("error: {said}\n")), "{stderr}");
    assert!(!stderr.contains("could not get"), "{stderr}");
    let status = text(&sync(&["status"])?.stderr)?;
    assert!(
        status.contains("hint: processed was made on another machine, so only a mirror that holds its files brings them here, or your pipeline run here, then `fetchloom add data/processed --as processed`"),
        "{status}"
    );
    let offline = json(&sync(&["--json", "--offline", "sync"])?)?;
    assert_eq!(offline["errors"][0]["kind"], "offline.missing", "{offline}");
    assert_eq!(offline["errors"][0]["message"], said, "{offline}");
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy();
    let pushed = json(&sync(&["--json", "push", "--to", &mirror, "--yes"])?)?;
    assert_eq!(pushed["errors"][0]["message"], said, "{pushed}");
    Ok(())
}

/// an output whose files changed, went or came is the author's work in progress: status names
/// each change, and sync leaves the whole folder as it is, naming the two ways on.
#[test]
fn a_changed_output_is_never_partly_healed() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    rerun(&work)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        listed(&document, "edited"),
        ["data/processed/iris_clean.csv"]
    );
    assert_eq!(listed(&document, "missing"), ["data/processed/summary.txt"]);
    assert_eq!(listed(&document, "extra"), ["data/processed/plot.png"]);
    let synced = sandbox.run(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    let warning = warned(&document)
        .into_iter()
        .find(|said| said.contains("processed"))
        .unwrap_or_default();
    assert!(
        warning.contains("data/processed changed since processed was pinned"),
        "{document}"
    );
    let said = document["warnings"].to_string();
    assert!(
        said.contains(
            "`fetchloom sync --force processed` to make it hold just the files processed pins"
        ),
        "{said}"
    );
    assert!(
        said.contains("fetchloom add data/processed --as processed"),
        "{said}"
    );
    let folder = work.join("data/processed");
    assert!(!folder.join("summary.txt").exists(), "never put back alone");
    assert_eq!(fs::read(folder.join("plot.png"))?, b"png");
    assert_eq!(
        fs::read(folder.join("iris_clean.csv"))?,
        b"sepal,petal\n5.1,1.4\n4.9,1.4\n"
    );
    let forced = sandbox.run(&["sync", "--force", "processed"])?;
    assert_eq!(forced.status.code(), Some(0), "{}", text(&forced.stderr)?);
    assert_eq!(fs::read(folder.join("iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(folder.join("summary.txt"))?, SUMMARY);
    assert!(!folder.join("plot.png").exists(), "the pinned tree, whole");
    assert_eq!(fs::read(folder.join(".gitignore"))?, b"*\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// after a rerun, the same add shows what changed and pins it in one step with --yes, keeping
/// the table's comments and settings; without a terminal or --yes it changes nothing.
#[test]
fn the_same_add_pins_a_rerun_in_one_step_keeping_the_table() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let table = "# made by src/dataset.py\n[datasets.processed]\nref = \"local\"\nat = \"data/processed\"\nexclude = \"*.log\" # logs stay out\n";
    fs::write(work.join("data.toml"), table)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, table);
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    rerun(&work)?;
    fs::write(work.join("data/processed/run.log"), b"log")?;
    let asked = sandbox.run(&["add", "data/processed", "--as", "processed"])?;
    let stderr = text(&asked.stderr)?;
    assert_eq!(asked.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("error: data/processed holds other files than processed pins"),
        "{stderr}"
    );
    for line in [
        "changed iris_clean.csv",
        "added plot.png",
        "removed summary.txt",
    ] {
        assert!(stderr.contains(line), "{line}: {stderr}");
    }
    assert!(
        !stderr.contains("run.log"),
        "an excluded file is no change: {stderr}"
    );
    assert!(
        stderr.contains("fetchloom add data/processed --as processed --yes"),
        "{stderr}"
    );
    assert!(
        stderr.contains(
            "`fetchloom sync --force processed` to make it hold just the files processed pins"
        ),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    let output = sandbox.run(&["add", "data/processed", "--as", "processed", "--yes"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Pinned processed · 1 file changed, 1 added, 1 removed"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, table);
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert!(lock.contains("path = \"plot.png\""), "{lock}");
    assert!(!lock.contains("summary.txt"), "{lock}");
    assert!(!lock.contains("run.log"), "{lock}");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// after a pipeline run, `status --verify --check` is the check a script runs on an output: it
/// hashes every file, so a rewrite that kept size and time shows, and it fails while the folder
/// holds other files than the output pins, naming the output once with what changed, as `sync
/// --strict --verify` does, which heals no part of it. a dry run of the same add shows what pinning
/// it again would change. none of them writes data.toml or data.lock.
#[test]
fn status_check_and_a_dry_run_judge_an_outputs_whole_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pinned = [
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    ];
    let check = ["--json", "status", "--verify", "--check"];
    let same = sandbox.run(&check)?;
    assert_eq!(same.status.code(), Some(0), "{}", text(&same.stdout)?);
    let clean = work.join("data/processed/iris_clean.csv");
    let modified = fs::metadata(&clean)?.modified()?;
    fs::write(&clean, b"sepal,petal\n5.1,1.5\n")?;
    fs::File::options()
        .write(true)
        .open(&clean)?
        .set_modified(modified)?;
    let unhashed = sandbox.run(&["status", "--check"])?;
    assert_eq!(
        unhashed.status.code(),
        Some(0),
        "only hashing tells: {}",
        text(&unhashed.stderr)?
    );
    let strict = sandbox.run(&["--json", "sync", "--strict", "--verify"])?;
    let synced = json(&strict)?;
    assert_eq!(strict.status.code(), Some(1), "{synced}");
    assert_eq!(synced["errors"][0]["kind"], "data.not_ready", "{synced}");
    assert_eq!(
        synced["errors"][0]["details"],
        serde_json::json!([
            "data/processed changed since processed was pinned, 1 file changed: `fetchloom sync --force processed` makes it hold just the files processed pins, or `fetchloom add data/processed --as processed` pins what it holds now"
        ]),
        "{synced}"
    );
    assert_eq!(
        fs::read(&clean)?,
        b"sepal,petal\n5.1,1.5\n",
        "never partly healed"
    );
    fs::remove_file(work.join("data/processed/summary.txt"))?;
    fs::write(work.join("data/processed/plot.png"), b"png")?;
    let refused = sandbox.run(&check)?;
    let document = json(&refused)?;
    assert_eq!(refused.status.code(), Some(1), "{document}");
    let error = &document["errors"][0];
    assert_eq!(error["kind"], "data.not_ready", "{document}");
    assert_eq!(
        error["details"],
        serde_json::json!([
            "data/processed changed since processed was pinned, 1 file changed, 1 added, 1 removed: `fetchloom sync --force processed` makes it hold just the files processed pins, or `fetchloom add data/processed --as processed` pins what it holds now"
        ]),
        "{document}"
    );
    let dry = sandbox.run(&["add", "data/processed", "--as", "processed", "--dry-run"])?;
    let stderr = text(&dry.stderr)?;
    assert_eq!(dry.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Would pin processed · 1 file changed, 1 added, 1 removed"),
        "{stderr}"
    );
    let listed = sandbox.run(&["-v", "add", "data/processed", "--as", "processed", "-n"])?;
    let stderr = text(&listed.stderr)?;
    for line in [
        "changed iris_clean.csv",
        "added plot.png",
        "removed summary.txt",
    ] {
        assert!(stderr.contains(line), "{line}: {stderr}");
    }
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, pinned[0]);
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned[1]);
    Ok(())
}

fn count_files(folder: &Path) -> usize {
    let Ok(entries) = fs::read_dir(folder) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                count_files(&entry.path())
            } else {
                1
            }
        })
        .sum()
}

/// every file in an output's folder is the output's: an add never takes in another dataset's
/// files, nor makes an output of the data folder or of the project, and changes nothing then.
#[test]
fn an_output_never_takes_in_another_datasets_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results/figures"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    fs::write(work.join("results/figures/f.png"), b"png")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let (toml, lock) = (
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    );
    for (folder, said) in [
        (
            "results/figures",
            "error: results/figures is in results, the folder of the output results, where every file is the output's",
        ),
        (
            ".",
            "error: . holds the whole project, so it cannot be an output's folder",
        ),
    ] {
        let refused = sandbox.run(&["add", folder, "--as", "other"])?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(2), "{folder}: {stderr}");
        assert!(stderr.contains(said), "{folder}: {stderr}");
        assert_eq!(fs::read_to_string(work.join("data.toml"))?, toml);
        assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    }
    Ok(())
}

/// an output keeps the files its pipeline wrote, so `--decompress`, which places single compressed
/// files unpacked, is refused for one, new or pinned, before anything is written.
#[test]
fn an_output_takes_no_decompress() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let said = "error: data/processed is an output, kept as its pipeline wrote it, so it takes no --decompress";
    let add = ["add", "data/processed", "--as", "processed", "--decompress"];
    let refused = sandbox.run(&add)?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains(said), "{stderr}");
    assert!(!work.join("data.toml").exists(), "{stderr}");
    ok(&sandbox.run(&add[..4])?)?;
    let pinned = [
        fs::read_to_string(work.join("data.toml"))?,
        fs::read_to_string(work.join("data.lock"))?,
    ];
    let refused = sandbox.run(&add)?;
    let stderr = text(&refused.stderr)?;
    assert_eq!(refused.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains(said), "{stderr}");
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, pinned[0]);
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned[1]);
    Ok(())
}

/// a cloned data.toml chooses where an output lands, so `at` is held to the data folder's rules
/// and kept apart from the cache, before anything is written.
#[test]
fn a_cloned_outputs_folder_is_held_to_the_data_folders_rules() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::write(
        work.join("data.toml"),
        "[datasets.out]\nref = \"local\"\nat = \".husky\"\n",
    )?;
    let output = sandbox.run(&["sync"])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(
            "error: at = \".husky\" in data.toml is refused because .husky is a folder other tools run files from"
        ),
        "{stderr}"
    );
    assert!(!work.join(".husky").exists());
    fs::write(
        work.join("data.toml"),
        "[datasets.out]\nref = \"local\"\nat = \".fetchloom-cache/objects\"\n",
    )?;
    let cached = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_CACHE", work.join(".fetchloom-cache"))
        .output()?;
    let stderr = text(&cached.stderr)?;
    assert_eq!(cached.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains(".fetchloom-cache/objects is inside the cache folder"),
        "{stderr}"
    );
    fs::write(
        work.join("data.toml"),
        "[datasets.out]\nref = \"local\"\nat = \"results\"\n",
    )?;
    let holding = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_CACHE", work.join("results").join("cache"))
        .output()?;
    let stderr = text(&holding.stderr)?;
    assert_eq!(holding.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.contains("is inside the output folder results, where datasets' files are placed"),
        "{stderr}"
    );
    assert!(!work.join("data.lock").exists(), "{stderr}");
    Ok(())
}

/// an add never pins a folder that is the cache, inside it or holding it as an output: refused
/// before data.toml or data.lock exist, and nothing copied.
#[test]
fn an_add_never_pins_the_cache_as_an_output() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let cache = work.join("results").join(".cache");
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    sandbox.make_cache(&cache)?;
    fs::create_dir_all(cache.join("tmp/sub"))?;
    fs::write(cache.join("tmp/sub/x.csv"), b"x\n")?;
    for (folder, said) in [
        ("results", "error: results holds the cache folder"),
        (
            "results/.cache/tmp/sub",
            "error: results/.cache/tmp/sub is inside the cache folder",
        ),
    ] {
        let refused = sandbox
            .command(&["add", folder, "--as", "out"])
            .env("FETCHLOOM_CACHE", &cache)
            .output()?;
        let stderr = text(&refused.stderr)?;
        assert_eq!(refused.status.code(), Some(2), "{folder}: {stderr}");
        assert!(stderr.contains(said), "{folder}: {stderr}");
        assert!(!work.join("data.toml").exists(), "{folder}: {stderr}");
        assert!(!work.join("data.lock").exists(), "{folder}: {stderr}");
    }
    Ok(())
}

/// removing an output untracks it: the author's files stay as they are, and of its folder's
/// marker, in the data folder or outside it, only one holding what fetchloom wrote goes, so git
/// sees the folder again. a .gitignore of the author's own, pinned with the output, stays too.
#[test]
fn removing_an_output_keeps_the_authors_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/summary.txt"), SUMMARY)?;
    fs::create_dir_all(work.join("figures"))?;
    fs::write(work.join("figures/plot.png"), b"png")?;
    fs::write(work.join("figures/.gitignore"), b"*.tmp\n")?;
    for (at, name) in [
        ("data/processed", "processed"),
        ("results", "results"),
        ("figures", "figures"),
    ] {
        ok(&sandbox.run(&["add", at, "--as", name])?)?;
    }
    assert_eq!(fs::read(work.join("results/.gitignore"))?, b"*\n");
    let changed = work.join("data/processed/.gitignore");
    fs::write(&changed, b"*\n!summary.txt\n")?;
    let file = work.join("data/processed/iris_clean.csv");
    let before = look(&file)?;
    let removed = sandbox.run(&["remove", "processed", "results", "figures", "--yes"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert_eq!(look(&file)?, before, "{stderr}");
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    assert_eq!(fs::read(work.join("results/summary.txt"))?, SUMMARY);
    assert!(!work.join("results/.gitignore").exists(), "{stderr}");
    assert_eq!(fs::read(&changed)?, b"*\n!summary.txt\n");
    assert_eq!(fs::read(work.join("figures/.gitignore"))?, b"*.tmp\n");
    assert_eq!(fs::read(work.join("figures/plot.png"))?, b"png");
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, "");
    Ok(())
}

/// a removed output's folder that a link took the place of on its way, as a moved folder may, is
/// no folder of the project: what the link leads to keeps its .gitignore.
#[cfg(unix)]
#[test]
fn removing_an_output_deletes_no_marker_through_a_link() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("out/results"))?;
    fs::write(work.join("out/results/summary.txt"), SUMMARY)?;
    ok(&sandbox.run(&["add", "out/results", "--as", "results"])?)?;
    assert_eq!(fs::read(work.join("out/results/.gitignore"))?, b"*\n");
    let elsewhere = sandbox.folder("elsewhere")?;
    fs::create_dir_all(elsewhere.join("results"))?;
    let theirs = elsewhere.join("results/.gitignore");
    fs::write(&theirs, b"*\n")?;
    fs::remove_dir_all(work.join("out"))?;
    std::os::unix::fs::symlink(&elsewhere, work.join("out"))?;
    let removed = sandbox.run(&["remove", "results", "--yes"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert_eq!(fs::read(&theirs)?, b"*\n", "{stderr}");
    Ok(())
}

/// what tools keep beside the files they work on, one of each kind, with the folder's one table.
const DROPPINGS: [&str; 12] = [
    ".table.csv.swp",
    ".table.csv.swo",
    "table.csv~",
    "#table.csv#",
    ".#notes.txt",
    ".~lock.table.csv#",
    "~$book.xlsx",
    ".nfs0000000001a2b3c400000002",
    ".fuse_hidden0000000a00000001",
    ".ipynb_checkpoints/table-checkpoint.csv",
    "__pycache__/m.cpython-312.pyc",
    "sub/.ipynb_checkpoints/x-checkpoint.csv",
];

/// `folder` holding table.csv and each of [`DROPPINGS`], and on unix emacs' lock, a link.
fn with_droppings(folder: &Path) -> Result {
    for dropping in DROPPINGS {
        let file = folder.join(dropping);
        fs::create_dir_all(file.parent().ok_or("a dropping outside any folder")?)?;
        fs::write(file, b"tool")?;
    }
    fs::write(folder.join("table.csv"), CLEAN)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink("someone@host.4242:1700000000", folder.join(".#table.csv"))?;
    Ok(())
}

/// the files editors, office suites, notebooks, python and network disks keep beside a person's
/// while they work are never data: a capture pins the folder without them, as an import from
/// outside the project does, and one line says what it skipped, no warning.
#[test]
fn a_pin_leaves_out_what_tools_keep_beside_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    let out = work.join("out");
    fs::create_dir_all(&out)?;
    with_droppings(&out)?;
    let mine = sandbox.folder("mine")?;
    with_droppings(&mine)?;
    for (args, name) in [
        (&["--json", "add", "out", "--as", "results"][..], "results"),
        (&["--json", "add", "../mine"], "mine"),
    ] {
        let added = sandbox.run(args)?;
        let document = json(&added)?;
        assert_eq!(added.status.code(), Some(0), "{name}: {document}");
        let skipped = document["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|warning| warning["kind"] == "import.skipped")
            .count();
        assert_eq!(skipped, 0, "{name}: {document}");
        let lock = fs::read_to_string(work.join("data.lock"))?;
        let pinned = lock
            .split("[[dataset]]")
            .find(|table| table.contains(&format!("name = \"{name}\"")));
        let pinned = pinned.ok_or("no table")?;
        assert_eq!(pinned.matches("{ path = ").count(), 1, "{name}: {lock}");
        assert!(pinned.contains("{ path = \"table.csv\""), "{name}: {lock}");
    }
    let human = sandbox.run(&["add", "out", "--as", "results", "--dry-run"])?;
    let stderr = text(&human.stderr)?;
    assert_eq!(human.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("Skipped #table.csv# and "), "{stderr}");
    assert!(
        stderr.contains(" more working files of editors and tools in out\n"),
        "{stderr}"
    );
    Ok(())
}

/// a dropping coming or going is no change: an output pinned while an editor had a file open is
/// unchanged once the editor closes, and once a notebook and an office suite leave theirs.
#[test]
fn a_dropping_coming_or_going_is_no_change_to_an_output() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    let folder = work.join("data/processed");
    fs::write(folder.join(".iris_clean.csv.swp"), b"vim")?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let lock = fs::read_to_string(work.join("data.lock"))?;
    assert_eq!(lock.matches("{ path = ").count(), 2, "{lock}");
    fs::remove_file(folder.join(".iris_clean.csv.swp"))?;
    fs::create_dir_all(folder.join(".ipynb_checkpoints"))?;
    fs::write(
        folder.join(".ipynb_checkpoints/iris_clean-checkpoint.csv"),
        CLEAN,
    )?;
    fs::write(folder.join(".~lock.summary.txt#"), b"lock")?;
    let checked = sandbox.run(&["status", "--verify", "--check"])?;
    assert_eq!(checked.status.code(), Some(0), "{}", text(&checked.stderr)?);
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "yours"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    let synced = sandbox.run(&["--json", "sync"])?;
    let document = json(&synced)?;
    assert_eq!(synced.status.code(), Some(0), "{document}");
    assert!(
        warned(&document)
            .iter()
            .all(|said| !said.contains("changed since")),
        "{document}"
    );
    let again = sandbox.run(&["add", "data/processed", "--as", "processed"])?;
    let stderr = text(&again.stderr)?;
    assert_eq!(again.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("processed is pinned as data/processed holds it"),
        "{stderr}"
    );
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, lock);
    Ok(())
}

/// a folder that changes while add reads it is no state it ever held whole: nothing is pinned and
/// the cache takes none of its files, whether a file came or one was rewritten at its size. a
/// tool's working file coming meanwhile, as an editor's swap file, is no change.
#[test]
fn a_folder_that_changes_while_it_is_read_is_not_pinned() -> Result {
    for (late, bytes, changes) in [
        ("late.csv", &b"late\n"[..], true),
        ("iris_clean.csv", &b"sepal,petal\n5.1,1.5\n"[..], true),
        (".iris_clean.csv.swp", &b"vim"[..], false),
    ] {
        let sandbox = Sandbox::new()?;
        let work = sandbox.work();
        processed(&work)?;
        let release = sandbox.folder("hold")?.join("release");
        let held = sandbox.folder("hold")?.join("release.held");
        let mut command = sandbox.command(&["add", "data/processed", "--as", "processed"]);
        command.env("FETCHLOOM_TEST_HOLD_CAPTURE", &release);
        let running = Running::start(&mut command)?;
        wait_until("add to read the folder", || Ok(held.exists()))?;
        fs::write(work.join("data/processed").join(late), bytes)?;
        fs::write(&release, b"")?;
        let output = running.finish()?;
        let stderr = text(&output.stderr)?;
        if !changes {
            assert_eq!(output.status.code(), Some(0), "{late}: {stderr}");
            let lock = fs::read_to_string(work.join("data.lock"))?;
            assert_eq!(lock.matches("{ path = ").count(), 2, "{late}: {lock}");
            continue;
        }
        assert_eq!(output.status.code(), Some(50), "{late}: {stderr}");
        assert!(
            stderr.contains(
                "error: data/processed changed while it was read: let the pipeline finish, then add again"
            ),
            "{late}: {stderr}"
        );
        assert!(!work.join("data.lock").exists(), "{late}: {stderr}");
        assert_eq!(count_files(&sandbox.cache().join("objects")), 0, "{late}");
    }
    Ok(())
}

/// an output has no source: audit calls a file of it no mirror holds local, and diff says when
/// an output moved, with the same files, as data.lock records where it lives.
#[test]
fn audit_and_diff_say_what_an_output_is() -> Result {
    let sandbox = Sandbox::new()?;
    if crate::merge::ok(&sandbox, &["init", "-q"])?.is_none() {
        return Ok(());
    }
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let document = json(&sandbox.run(&["--json", "audit"])?)?;
    let unserved = &document["result"]["datasets"][0]["not_served"];
    assert_eq!(unserved[0]["outcome"], "local", "{document}");
    assert_eq!(
        unserved[0]["message"],
        "it is an output made in this project, at data/processed, and no mirror holds it"
    );
    let audited = sandbox.run(&["audit"])?;
    let told = text(&audited.stdout)?;
    assert!(told.contains("Local processed · 2 of 2 files"), "{told}");
    crate::merge::ok(&sandbox, &["add", "-A"])?;
    crate::merge::ok(&sandbox, &["commit", "-qm", "pinned"])?;
    fs::rename(work.join("data/processed"), work.join("data/clean"))?;
    let moved = fs::read_to_string(work.join("data.toml"))?.replace("data/processed", "data/clean");
    fs::write(work.join("data.toml"), moved)?;
    ok(&sandbox.run(&["lock"])?)?;
    let diff = text(&sandbox.run(&["diff"])?.stdout)?;
    assert!(
        diff.contains("processed changed: at data/processed \u{2192} data/clean; the same files"),
        "{diff}"
    );
    let document = json(&sandbox.run(&["--json", "diff"])?)?;
    assert_eq!(
        document["result"]["datasets"][0]["fields"],
        serde_json::json!([{ "name": "at", "before": "data/processed", "after": "data/clean" }])
    );
    Ok(())
}

/// push sends the files an output pins, never what its folder holds since, and says so when the
/// folder changed.
#[test]
fn push_sends_an_outputs_pinned_files_and_says_when_its_folder_changed() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    fs::write(work.join("data/processed/plot.png"), b"png")?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    let pushed = sandbox.run(&["--json", "push", "--to", &mirror, "--yes"])?;
    let document = json(&pushed)?;
    assert_eq!(pushed.status.code(), Some(0), "{document}");
    assert_eq!(
        crate::support::warning(&document, "output.changed")["message"],
        "data/processed changed since processed was pinned: 1 file added, so push sends the files processed pins, not these",
        "{document}"
    );
    assert_eq!(
        document["result"]["objects"], 2,
        "the pinned files alone: {document}"
    );
    Ok(())
}

/// exclude takes a path in an output's folder, wherever it is: the pin leaves the file out, and
/// the author's file stays, no difference since its pattern leaves it out.
#[test]
fn exclude_leaves_an_outputs_file_out_and_keeps_it() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    fs::create_dir_all(work.join("results"))?;
    fs::write(work.join("results/r.csv"), b"r\n")?;
    fs::write(work.join("results/run.log"), b"log\n")?;
    ok(&sandbox.run(&["add", "results", "--as", "results"])?)?;
    let excluded = sandbox.run(&["exclude", "results/run.log"])?;
    assert_eq!(
        excluded.status.code(),
        Some(0),
        "{}",
        text(&excluded.stderr)?
    );
    assert_eq!(
        fs::read_to_string(work.join("data.toml"))?,
        "[datasets.results]\nref = \"local\"\nat = \"results\"\nexclude = [\"run.log\"]\n"
    );
    assert!(!fs::read_to_string(work.join("data.lock"))?.contains("run.log"));
    assert_eq!(fs::read(work.join("results/run.log"))?, b"log\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "unneeded"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    Ok(())
}

/// drop frees an output's bytes from the cache once a mirror keeps them, and leaves its folder,
/// the author's, as it is, still out of git since data.lock pins it; status counts none of its
/// files.
#[test]
fn dropping_an_output_keeps_its_folder() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let mirror = sandbox.folder("mirror")?;
    let mirror = mirror.to_string_lossy().into_owned();
    ok(&sandbox.run(&["push", "--to", &mirror, "--yes"])?)?;
    let dropped = sandbox.run(&["drop", "processed", "--mirror", &mirror])?;
    assert_eq!(dropped.status.code(), Some(0), "{}", text(&dropped.stderr)?);
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    let marker = work.join("data/processed/.gitignore");
    assert_eq!(fs::read(&marker)?, b"*\n");
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "unneeded"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    ok(&sandbox.run(&["sync"])?)?;
    assert_eq!(fs::read(work.join("data/processed/iris_clean.csv"))?, CLEAN);
    assert_eq!(fs::read(&marker)?, b"*\n");
    Ok(())
}

/// "changed" means content: a rerun that writes the same bytes is no change, and the status that
/// hashed the file to tell keeps its new look, so the next status reads none of it again.
#[test]
fn a_rewrite_with_the_same_bytes_is_no_change_and_is_hashed_once() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let clean = work.join("data/processed/iris_clean.csv");
    sandbox.wait_for_a_later_time(fs::metadata(&clean)?.modified()?)?;
    fs::write(&clean, CLEAN)?;
    let document = json(&sandbox.run(&["--json", "status"])?)?;
    for kind in ["missing", "edited", "extra", "untracked"] {
        assert_eq!(
            listed(&document, kind),
            Vec::<String>::new(),
            "{kind}: {document}"
        );
    }
    let modified = fs::metadata(&clean)?
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos();
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(work.join(".fetchloom/state"))?)?;
    let look = &state["files"]["data/processed/iris_clean.csv"]["look"];
    assert_eq!(
        look["modified"].as_u64().map(u128::from),
        Some(modified),
        "{state}"
    );
    Ok(())
}

/// at a terminal the same add shows what changed and asks: a no changes nothing, a yes pins it.
#[cfg(target_os = "linux")]
#[test]
fn the_same_add_asks_at_a_terminal_before_it_pins_a_rerun() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    rerun(&work)?;
    let asked = "pin what data/processed holds now as processed?";
    let add = sandbox.command(&["add", "data/processed", "--as", "processed"]);
    let Some((code, screen)) = crate::support::typed(&work, &add, "", asked, "n\r")? else {
        return Ok(());
    };
    assert!(
        screen.contains("Changed processed · 1 file changed, 1 added, 1 removed"),
        "{screen}"
    );
    assert!(screen.contains(&format!("{asked} [y/N]")), "{screen}");
    assert_eq!(code, Some(1), "{screen}");
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    let Some((code, screen)) = crate::support::typed(&work, &add, "", asked, "y\r")? else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen}");
    assert!(
        fs::read_to_string(work.join("data.lock"))?.contains("plot.png"),
        "{screen}"
    );
    Ok(())
}
