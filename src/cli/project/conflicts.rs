//! `fetchloom lock` in a project where a git merge left both sides in data.toml or data.lock:
//! both merged a dataset at a time, each side whole from git where it can tell, else from the
//! conflict lines, and nothing resolved or downloaded.

use super::{Changes, Report, committed, dry_run_done};
use crate::cli::Context;
use crate::cli::args;
use crate::error::{Error, Kind};
use crate::output::Warning;
use crate::project::lock::merge::markers::{self, Sides};
use crate::project::lock::merge::{self, Choices, Merged, Side, Summary};
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest};
use crate::project::{Project, git};

/// data.toml and data.lock as a merge left them, and the sides of each that holds git's conflict
/// lines.
struct Conflicted {
    manifest: String,
    /// none when the project has no data.lock.
    lock: Option<String>,
    manifest_sides: Option<Sides>,
    lock_sides: Option<Sides>,
}

impl Conflicted {
    /// the files that hold conflict lines, as a message names them.
    fn files(&self) -> &'static str {
        match (&self.manifest_sides, &self.lock_sides) {
            (Some(_), Some(_)) => "data.toml and data.lock",
            (Some(_), None) => "data.toml",
            (None, _) => "data.lock",
        }
    }
}

/// the merge `lock` makes when data.toml or data.lock holds git's conflict lines, written unless
/// `--dry-run`, or none when neither does.
pub(super) fn merge(
    context: &Context,
    project: &Project,
    args: &args::Lock,
) -> Result<Option<Report>, Error> {
    let choices = choices(args)?;
    let Some(conflicted) = read(project)? else {
        return Ok(None);
    };
    if conflicted.manifest_sides.is_none() && conflicted.lock_sides.is_none() {
        if choices.is_empty() {
            return Ok(None);
        }
        return Err(Error::new(
            Kind::Usage,
            "--ours and --theirs choose a side in a merge, and neither data.toml nor data.lock holds git's conflict lines",
        )
        .help("run `fetchloom lock` without them"));
    }
    let (manifest, lock) = sides(project, &conflicted);
    let told = |line: &str| context.output.detail(line);
    let merged = merge::merge(&manifest, lock.as_ref(), &choices, &project.root, &told)
        .map_err(|error| in_project(error, project))?;
    for warning in &merged.warnings {
        context.output.warn(Warning::MergeFormatting, warning);
    }
    let rendered = merged.lock.as_ref().map(Lock::render);
    let lock_changed = rendered.is_some() && rendered != conflicted.lock;
    if !args.dry.dry_run {
        // a stop asked while the sides were read leaves both files as they were
        context.stop.check()?;
        context.stop.finish();
        let changed = rendered.as_deref().filter(|_| lock_changed);
        let written = write(project, &merged, &conflicted.manifest, changed)?;
        context.wrote_lists(&project.root, written);
    }
    tell(
        context,
        project,
        &merged,
        conflicted.files(),
        args.dry.dry_run,
    );
    Ok(Some(Report {
        project: project.root.clone(),
        selected: None,
        datasets: merged.repinned,
        lock_changed,
        placement: None,
        dry_run: args.dry.dry_run,
        changes: args.dry.dry_run.then(Changes::default),
        merge: Some(merged.summary),
    }))
}

/// the side `--ours` and `--theirs` name for each dataset.
fn choices(args: &args::Lock) -> Result<Choices, Error> {
    let mut choices = Choices::new();
    for (names, side) in [(&args.ours, Side::Ours), (&args.theirs, Side::Theirs)] {
        for name in names {
            if choices
                .insert(name.clone(), side)
                .is_some_and(|other| other != side)
            {
                return Err(Error::new(
                    Kind::Usage,
                    format!("{name} is named by both --ours and --theirs"),
                )
                .help("keep one side of each dataset"));
            }
        }
    }
    Ok(choices)
}

/// data.toml and data.lock with the sides of each, or none when data.toml cannot be read, which
/// the usual reader then explains.
fn read(project: &Project) -> Result<Option<Conflicted>, Error> {
    let Ok(manifest) = std::fs::read_to_string(project.manifest_path()) else {
        return Ok(None);
    };
    let lock = std::fs::read_to_string(project.root.join(lock::FILE)).ok();
    let manifest_sides = markers::split(&manifest, manifest::FILE)?;
    let lock_sides = lock
        .as_deref()
        .map(|text| markers::split(text, lock::FILE))
        .transpose()?
        .flatten();
    Ok(Some(Conflicted {
        manifest,
        lock,
        manifest_sides,
        lock_sides,
    }))
}

/// the sides of data.toml and of data.lock. data.lock's come whole from git's index when git gives
/// them, base and all, so no pin mixes lines of both sides; else from its conflict lines. data.toml
/// keeps the sides its conflict lines give, which hold what git merged by itself and edits made
/// since, with git's whole sides beside them for a choice to take an entry from. its base is the
/// one its conflict lines show when it reads, else git's index's, else the one read around the
/// lines zdiff3 moved out of the conflicts. a file without conflict lines is the same on both
/// sides, data.toml with whole sides from the commits being merged.
fn sides(project: &Project, conflicted: &Conflicted) -> (Sides, Option<Sides>) {
    let program = std::env::var_os("PATH").and_then(|path| git::find(Some(&path), &project.root));
    let git = program.as_deref();
    let root = &project.root;
    let reads = |file: &'static str| move |base: &String| merge::reads(file, base);
    let manifest = match &conflicted.manifest_sides {
        Some(sides) => {
            let stages = git.and_then(|git| git::stages(git, root, manifest::FILE));
            let mut sides = sides.clone();
            if !sides.base.as_ref().is_some_and(reads(manifest::FILE)) {
                let trimmed = sides.trimmed.take().filter(reads(manifest::FILE));
                sides.base = stages
                    .as_ref()
                    .and_then(|stages| stages.base.clone())
                    .filter(reads(manifest::FILE))
                    .or(trimmed);
            }
            sides.whole = stages.map(|stages| [stages.ours, stages.theirs]);
            sides
        }
        None => same(
            &conflicted.manifest,
            git.and_then(|git| git::committed(git, root, manifest::FILE)),
        ),
    };
    let lock = conflicted.lock.as_deref().map(|text| {
        let Some(sides) = &conflicted.lock_sides else {
            return same(text, None);
        };
        let mut sides = sides.clone();
        if let Some(stages) = git.and_then(|git| git::stages(git, root, lock::FILE)) {
            sides.base = stages.base.filter(reads(lock::FILE));
            sides.whole = Some([stages.ours, stages.theirs]);
        } else if !sides.base.as_ref().is_some_and(reads(lock::FILE)) {
            sides.base = sides.trimmed.take().filter(reads(lock::FILE));
        }
        sides
    });
    (manifest, lock)
}

/// a file without conflict lines, the same on both sides, with each side whole as `whole` gives
/// it.
fn same(text: &str, whole: Option<[String; 2]>) -> Sides {
    Sides {
        ours: text.to_owned(),
        theirs: text.to_owned(),
        whole,
        ..Sides::default()
    }
}

/// `error` with the project beside what the merge found, so `--json` names both.
fn in_project(error: Error, project: &Project) -> Error {
    let Some(result) = error.result.clone() else {
        return error;
    };
    let mut result = *result;
    result["project"] = serde_json::json!(project.root);
    error.with_result(result)
}

/// writes the merged data.toml when it changed, then, when data.lock changed into `rendered`, the
/// lists it names and data.lock itself, and sweeps the lists no side needs any more. returns the
/// parts of lists it wrote anew.
fn write(
    project: &Project,
    merged: &Merged,
    manifest: &str,
    rendered: Option<&str>,
) -> Result<Vec<String>, Error> {
    if merged.manifest != manifest {
        project.write(manifest::FILE, merged.manifest.as_bytes())?;
    }
    let mut written = Vec::new();
    if let (Some(lock), Some(text)) = (&merged.lock, rendered) {
        // the lists first, so data.lock never names a part that is not there yet
        written = project.write_lists(lock)?;
        project.write(lock::FILE, text.as_bytes())?;
        project.sweep_lists(lock);
    }
    Ok(written)
}

/// what the merge did, or with `dry_run` would do, to `files`, and what comes next.
fn tell(context: &Context, project: &Project, merged: &Merged, files: &str, dry_run: bool) {
    let output = &context.output;
    let verb = if dry_run { "Would merge" } else { "Merged" };
    output.done(verb, &format!("{files} \u{b7} {}", facts(&merged.summary)));
    let unpinned = &merged.summary.unpinned;
    if !unpinned.is_empty() {
        let (names, are, them) = match unpinned.as_slice() {
            [one] => (one.clone(), "is", "it"),
            many => (many.join(", "), "are", "them"),
        };
        output.warn(Warning::MergeUnpinned, &format!(
            "{names} {are} not pinned, as neither side of data.lock pins {them} as data.toml now asks"
        ));
    }
    if dry_run {
        dry_run_done(output, None);
        return;
    }
    let data_dir = Manifest::parse(&merged.manifest).map_or_else(
        |_| manifest::DATA_DIR.to_owned(),
        |manifest| manifest.data_dir,
    );
    let sync = if unpinned.is_empty() {
        format!("run `fetchloom sync` to update {data_dir}/")
    } else {
        format!("run `fetchloom sync`, which pins them and updates {data_dir}/")
    };
    output.next(&format!(
        "{sync}, then commit {} to finish the merge",
        committed(project)
    ));
}

/// how the datasets came out, in a few words: `alpha from ours, beta from theirs, 1 the same`.
fn facts(summary: &Summary) -> String {
    let mut facts = Vec::new();
    for (names, how) in [
        (&summary.from_ours, "from ours"),
        (&summary.from_theirs, "from theirs"),
        (&summary.removed, "removed"),
    ] {
        if !names.is_empty() {
            facts.push(format!("{} {how}", names.join(", ")));
        }
    }
    if !summary.same.is_empty() {
        facts.push(format!("{} the same", summary.same.len()));
    }
    if facts.is_empty() {
        "no datasets".to_owned()
    } else {
        facts.join(", ")
    }
}
