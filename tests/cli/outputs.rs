//! outputs: a folder of the project pinned where its pipeline writes it, shared by hash, placed
//! writable for a teammate, and never partly healed once it changed.

use std::fs;
use std::path::Path;

use crate::support::{Result, Sandbox, json, ok, text, warned};

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
    assert!(said.contains("fetchloom sync --force processed"), "{said}");
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

/// `--check` compares the whole tree and changes nothing: equal is exit 0, and a difference is
/// a kind and exit code of its own, with each difference in --json.
#[test]
fn check_compares_the_whole_tree_and_changes_nothing() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let pinned = fs::read_to_string(work.join("data.lock"))?;
    let same = sandbox.run(&["add", "data/processed", "--as", "processed", "--check"])?;
    let stderr = text(&same.stderr)?;
    assert_eq!(same.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("Ok data/processed holds the files processed pins"),
        "{stderr}"
    );
    rerun(&work)?;
    let objects = count_files(&sandbox.cache().join("objects"));
    let differs = sandbox.run(&[
        "--json",
        "add",
        "data/processed",
        "--as",
        "processed",
        "--check",
    ])?;
    let document = json(&differs)?;
    assert_eq!(differs.status.code(), Some(60), "{document}");
    assert_eq!(
        document["errors"][0]["kind"], "output.differs",
        "{document}"
    );
    let differences = &document["result"]["differences"];
    assert_eq!(
        differences["changed"],
        serde_json::json!(["iris_clean.csv"])
    );
    assert_eq!(differences["added"], serde_json::json!(["plot.png"]));
    assert_eq!(differences["removed"], serde_json::json!(["summary.txt"]));
    assert_eq!(fs::read_to_string(work.join("data.lock"))?, pinned);
    assert_eq!(count_files(&sandbox.cache().join("objects")), objects);
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
    Ok(())
}

/// removing an output untracks it: the author's files stay as they are.
#[test]
fn removing_an_output_keeps_the_authors_files() -> Result {
    let sandbox = Sandbox::new()?;
    let work = sandbox.work();
    processed(&work)?;
    ok(&sandbox.run(&["add", "data/processed", "--as", "processed"])?)?;
    let file = work.join("data/processed/iris_clean.csv");
    let before = look(&file)?;
    let removed = sandbox.run(&["remove", "processed", "--yes"])?;
    let stderr = text(&removed.stderr)?;
    assert_eq!(removed.status.code(), Some(0), "{stderr}");
    assert_eq!(look(&file)?, before, "{stderr}");
    assert_eq!(fs::read(work.join("data/processed/summary.txt"))?, SUMMARY);
    assert_eq!(fs::read_to_string(work.join("data.toml"))?, "");
    Ok(())
}
