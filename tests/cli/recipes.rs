//! recipes as every command but build sees them, against servers on this machine: what sync,
//! status and path do in each state of a pinned result, a dataset with `place = false` that a
//! recipe reads, and what `cache` and `cache gc` keep.

use std::fs;
use std::path::{Path, PathBuf};

use crate::build::{build, building_in, with};
use crate::project::{IRIS, PATH};
use crate::support::{Result, Sandbox, Server, file, text};

/// a recipe that makes `clean` from iris: the file clean.csv, holding `done`.
const WRITE: &str = "\n[datasets.clean]\nfrom = [\"iris\"]\nrun = [\"recipe-step\", \"write\", \"clean.csv\", \"done\", \"{iris}\", \"{out}\"]\n";

/// sets `place = false` on the dataset `name` in the data.toml of the project at `folder`.
fn unplace(folder: &Path, name: &str) -> Result {
    let manifest = folder.join("data.toml");
    let table = format!("[datasets.{name}]\n");
    let text = fs::read_to_string(&manifest)?;
    assert!(text.contains(&table), "{text}");
    fs::write(
        &manifest,
        text.replace(&table, &format!("{table}place = false\n")),
    )?;
    Ok(())
}

/// a fresh clone of the work folder's project in a folder called `name`, data.lock.d and all,
/// and an empty cache of its own beside it.
fn cloned(sandbox: &Sandbox, name: &str) -> Result<(PathBuf, PathBuf)> {
    let folder = crate::project::clone(sandbox, name)?;
    let parts = folder.join("data.lock.d");
    fs::create_dir_all(&parts)?;
    for entry in fs::read_dir(sandbox.work().join("data.lock.d"))? {
        let entry = entry?;
        fs::copy(entry.path(), parts.join(entry.file_name()))?;
    }
    Ok((folder, sandbox.folder(&format!("{name}-cache"))?))
}

/// a build of a recipe made from a dataset with `place = false` places only the result and
/// retires what was placed of the input; a clone whose cache lacks both fetches the input for
/// its step without placing it.
#[test]
fn a_build_places_only_the_result_of_an_unplaced_input() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")])?;
    with(&sandbox, &server, WRITE)?;
    unplace(&sandbox.work(), "iris")?;
    let built = build(&sandbox, &["build"])?;
    assert_eq!(built.status.code(), Some(0), "{}", text(&built.stderr)?);
    let told = text(&built.stderr)?;
    assert!(
        told.contains("    Retired data/iris · 1 file, since iris has place = false\n"),
        "{told}"
    );
    let data = sandbox.work().join("data");
    assert!(!data.join("iris").exists());
    assert_eq!(fs::read(data.join("clean").join("clean.csv"))?, b"done");
    let status = text(&sandbox.run(&["status"])?.stderr)?;
    assert!(
        status.contains(" Not placed iris · place = false in data.toml\n"),
        "{status}"
    );
    assert!(
        status.contains("Ok everything matches data.lock by size and time\n"),
        "{status}"
    );

    let (folder, cache) = cloned(&sandbox, "clone")?;
    let output = building_in(&sandbox, &folder, &["build"])?
        .env("FETCHLOOM_CACHE", &cache)
        .output()?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        server.requests().len(),
        2,
        "the clone fetched iris for its step"
    );
    assert!(!folder.join("data").join("iris").exists());
    assert_eq!(
        fs::read(folder.join("data").join("clean").join("clean.csv"))?,
        b"done"
    );
    Ok(())
}
