//! `fetchloom lock` where a git merge left both sides in data.toml or data.lock, merged a dataset
//! at a time, each side whole from git where it can tell, with nothing resolved or downloaded.

use std::fmt::Write as _;
use std::path::Path;

use super::{Changes, Report, dry_run_done};
use crate::cli::Context;
use crate::cli::args;
use crate::error::{Error, Kind};
use crate::output::Warning;
use crate::project::Project;
use crate::project::git::{self, Operation};
use crate::project::lock::merge::markers::{self, Sides};
use crate::project::lock::merge::{self, Choices, Merged, Side, Summary};
use crate::project::lock::{self, Lock};
use crate::project::manifest::{self, Manifest};
use crate::units;

/// data.toml and data.lock as a merge left them, with sides for each holding conflict lines.
struct Conflicted {
    manifest: String,
    /// none when the project has no data.lock.
    lock: Option<String>,
    manifest_sides: Option<Sides>,
    lock_sides: Option<Sides>,
}

impl Conflicted {
    /// whether git's conflict lines name the sides as `git stash` does.
    fn stashed(&self) -> bool {
        [&self.manifest_sides, &self.lock_sides]
            .into_iter()
            .flatten()
            .any(|sides| sides.stashed)
    }

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
    let program = std::env::var_os("PATH").and_then(|path| git::find(Some(&path), &project.root));
    if conflicted.manifest_sides.is_none() && conflicted.lock_sides.is_none() {
        if choices.is_empty() {
            return Ok(None);
        }
        return Err(nothing_to_choose(
            &conflicted,
            program.as_deref(),
            &project.root,
        ));
    }
    let (manifest, lock) = sides(program.as_deref(), &project.root, &conflicted);
    let told = |line: &str| context.output.detail(line);
    let committed = |side: Side, part: &str| {
        git::in_commit(
            program.as_deref()?,
            &project.root,
            part,
            side == Side::Theirs,
        )
    };
    let merged = merge::merge(
        &manifest,
        lock.as_ref(),
        &choices,
        &project.root,
        &told,
        program.is_some().then_some(&committed),
    )
    .map_err(|error| {
        whose(
            in_project(error, project),
            program.as_deref(),
            &project.root,
            conflicted.stashed(),
        )
    })?;
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
    let operation = program
        .as_deref()
        .and_then(|git| git::operation(git, &project.root));
    tell(
        context,
        project,
        &merged,
        conflicted.files(),
        args.dry.dry_run,
        operation,
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
        waiting: Vec::new(),
    }))
}

/// the refusal of `--ours` or `--theirs` where neither file holds git's conflict lines, as after a
/// side was chosen: while git is still in the merge, or still holds both sides of either file, as
/// after `git stash pop`, `git checkout -m` brings the lines back.
fn nothing_to_choose(conflicted: &Conflicted, git: Option<&Path>, root: &Path) -> Error {
    let error = Error::new(
        Kind::Usage,
        "--ours and --theirs choose a side in a merge, and neither data.toml nor data.lock holds git's conflict lines",
    );
    let Some(git) = git else {
        return error.help(format!(
            "run `{}` without them",
            crate::suggested::command("lock")
        ));
    };
    let held = || {
        [manifest::FILE, lock::FILE]
            .into_iter()
            .any(|file| git::stages(git, root, file).is_some())
    };
    let why = match git::operation(git, root) {
        Some(operation) => {
            let operation = match operation {
                Operation::Merge => "merge",
                Operation::Rebase => "rebase",
                Operation::CherryPick => "cherry-pick",
                Operation::Revert => "revert",
            };
            format!("the git {operation} is still under way")
        }
        None if held() => "git still holds both sides of the conflict".to_owned(),
        None => {
            return error.help(format!(
                "run `{}` without them",
                crate::suggested::command("lock")
            ));
        }
    };
    let files = if conflicted.lock.is_some() {
        "data.toml data.lock"
    } else {
        "data.toml"
    };
    error
        .detail(format!(
            "{why}, so a side chosen before can be chosen again"
        ))
        .help(format!(
            "bring back git's conflict lines with `{}`, then run the same command again",
            crate::suggested::git(root, &format!("checkout -m {files}"))
        ))
}

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

/// none when data.toml cannot be read, which the usual reader then explains.
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

/// data.lock's sides come whole from git's index when `git` has them, so no pin mixes both sides.
/// data.toml keeps its conflict lines' sides, which hold what git merged itself and edits since.
fn sides(git: Option<&Path>, root: &Path, conflicted: &Conflicted) -> (Sides, Option<Sides>) {
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

fn same(text: &str, whole: Option<[String; 2]>) -> Sides {
    Sides {
        ours: text.to_owned(),
        theirs: text.to_owned(),
        whole,
        ..Sides::default()
    }
}

/// `error`, a merge that needs a choice, with whose each side is and its commit, since a rebase
/// turns git's ours and theirs around from the branch a person is on. any other error stays as
/// it is.
fn whose(mut error: Error, git: Option<&Path>, root: &Path, stashed: bool) -> Error {
    let Some(git) = git.filter(|_| error.kind == Kind::Unconfirmed) else {
        return error;
    };
    let ours = "the branch you are on";
    let (our_role, their_role, theirs) = match git::operation(git, root) {
        Some(operation) => {
            let (our_role, their_role) = match operation {
                Operation::Merge => (ours, "the branch you are merging"),
                Operation::Rebase => (
                    "the branch you are rebasing onto",
                    "your commit being replayed",
                ),
                Operation::CherryPick => (ours, "the commit you are picking"),
                Operation::Revert => (ours, "the commit before the one you are reverting"),
            };
            (our_role, their_role, Some(operation.theirs()))
        }
        // git records no commit for the stash it could not apply whole
        None if stashed => (ours, "your stashed changes", None),
        None => return error,
    };
    for (side, role, commit) in [
        ("ours", our_role, Some("HEAD")),
        ("theirs", their_role, theirs),
    ] {
        let mut line = format!("{side} is {role}");
        if let Some(commit) = commit.and_then(|commit| git::commit(git, root, commit)) {
            let _ = write!(line, ": {}", units::clip(&commit, SUBJECT, "\u{2026}"));
        }
        error = error.detail(line);
    }
    error
}

/// the most columns a commit's short name and subject take in a message.
const SUBJECT: usize = 72;

/// `error` with the project beside what the merge found, so `--json` names both.
fn in_project(error: Error, project: &Project) -> Error {
    let Some(result) = error.result.clone() else {
        return error;
    };
    let mut result = *result;
    result["project"] = serde_json::json!(project.root);
    error.with_result(result)
}

/// writes what changed, data.lock when `rendered`, and returns the parts of lists it wrote anew.
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
    if let Some(lock) = &merged.lock {
        // the lists first, so data.lock never names a part that is not there yet, and a part the
        // git merge deleted, which the side kept still names, comes back
        written = project.write_lists(lock)?;
        if let Some(text) = rendered {
            project.write(lock::FILE, text.as_bytes())?;
            project.sweep_lists(lock);
        }
    }
    Ok(written)
}

/// what the merge did, then what is left: `operation` is what git is in, which says how to finish.
fn tell(
    context: &Context,
    project: &Project,
    merged: &Merged,
    files: &str,
    dry_run: bool,
    operation: Option<Operation>,
) {
    let output = &context.output;
    let verb = if dry_run { "Would merge" } else { "Merged" };
    output.done(verb, &format!("{files} \u{b7} {}", facts(&merged.summary)));
    let unpinned = &merged.summary.unpinned;
    if !unpinned.is_empty() {
        let (names, are, them) = match unpinned.as_slice() {
            [one] => (one.clone(), "is", "it"),
            many => (many.join(", "), "are", "them"),
        };
        output.warn_with(
            Warning::MergeUnpinned,
            &format!(
                "{names} {are} not pinned, as neither side of data.lock pins {them} as data.toml now asks"
            ),
            &format!(
                "run `{}`, which pins {them}",
                crate::suggested::command("sync")
            ),
        );
    }
    let manifest = Manifest::parse(&merged.manifest).ok();
    if dry_run {
        dry_run_done(output, None);
        return;
    }
    let data_dir = manifest.map_or_else(
        || manifest::DATA_DIR.to_owned(),
        |manifest| manifest.data_dir,
    );
    // git keeps both files unmerged until they are added, whatever fetchloom wrote
    let mut files = vec![manifest::FILE];
    if merged.lock.is_some() {
        files.push(lock::FILE);
    }
    // a part git's merge brought in that the merge then swept is a deletion to add too
    if super::names_lists(project) {
        files.push(lock::lists::FOLDER);
    }
    let git = |words: &str| crate::suggested::git(&project.root, words);
    let add = format!("`{}`", git(&format!("add {}", files.join(" "))));
    let finish = match operation {
        Some(Operation::Merge) => format!("{add} and `{}` to finish the merge", git("commit")),
        Some(Operation::Rebase) => format!(
            "{add} and `{}` to finish the rebase",
            git("rebase --continue")
        ),
        Some(Operation::CherryPick) => format!(
            "{add} and `{}` to finish the cherry-pick",
            git("cherry-pick --continue")
        ),
        Some(Operation::Revert) => format!(
            "{add} and `{}` to finish the revert",
            git("revert --continue")
        ),
        None => format!("{add} to mark the conflict resolved"),
    };
    output.hint(&format!(
        "run `{}` to update {data_dir}/, then {finish}",
        crate::suggested::command("sync")
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
