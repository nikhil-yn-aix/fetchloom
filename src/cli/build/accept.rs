//! `fetchloom build --accept`: pins the rebuild a build kept of a recipe, the files it made that
//! differ from what data.lock pins for the same key, without running anything, and places it.
//! it chooses which files a result holds, so only a person runs it: no error names it as a fix.

use std::collections::BTreeSet;
use std::sync::Arc;

use super::{Built, Did, Done, done, forget, pin, selected, sized, whole};
use crate::cache::Cache;
use crate::cli::project::{Run, committed, ignore_data, sync_all};
use crate::cli::sync::place::Seen;
use crate::cli::sync::{Loop, Scope, Upgrade};
use crate::cli::{Context, open_cache};
use crate::digest;
use crate::error::{Error, Kind};
use crate::project::Project;
use crate::project::lock::Lock;
use crate::project::manifest::Manifest;
use crate::project::state::{Candidate, State};
use crate::recipe::Step;

/// pins the kept rebuild of each recipe `names` asks for, or of every recipe with one when it
/// names none, then places them. nothing changes unless every one of them can be pinned.
///
/// # Errors
///
/// `usage` when a recipe has no kept rebuild, or one whose key or pin is not data.lock's now, or
/// whose files the cache no longer holds.
pub(super) async fn accept(
    context: &Context,
    project: &Project,
    names: &[String],
) -> Result<Built, Error> {
    let (text, manifest) = project.manifest()?;
    let read = project.lock_and_text()?;
    let (state, _) = project.state();
    let wanted = wanted(&manifest, &state, names)?;
    let old = read
        .as_ref()
        .map(|(lock, _)| lock.clone())
        .unwrap_or_default();
    let cache = open_cache()?;
    let mut accepted = Vec::new();
    for name in &wanted {
        accepted.push(checked(project, &manifest, &old, &cache, &state, name)?);
    }
    let scope: BTreeSet<String> = wanted.iter().cloned().collect();
    let work =
        Loop::new(context, project, Arc::new(context.net()), cache).within(Scope::Only(scope));
    let (mut lock, before) = work
        .relock(
            &manifest,
            read.map(|(lock, _)| lock),
            &Upgrade::Nothing,
            Vec::new(),
        )
        .await?;
    let mut report = Built {
        project: project.root.clone(),
        selected: wanted.clone(),
        datasets: wanted.clone(),
        lock_changed: true,
        ..Built::default()
    };
    for (step, candidate) in accepted {
        report.steps.push(take(&work, &mut lock, &step, candidate)?);
    }
    let written = work.write_rendered(&before, &lock, lock.render())?;
    work.project.sweep_lists(&lock);
    forget(project, &wanted)?;
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: Some(digest::sha256(written.as_bytes())),
    };
    let outcome = sync_all(&work, &manifest, Some(lock), None, Run::planned(&seen)).await?;
    ignore_data(&work, &manifest)?;
    tell(context, project, &manifest, &wanted);
    report.placement = Some(outcome.placed);
    Ok(report)
}

/// the recipes `names` asks to accept, or every recipe with a kept rebuild when it names none.
fn wanted(manifest: &Manifest, state: &State, names: &[String]) -> Result<Vec<String>, Error> {
    if !names.is_empty() {
        return Ok(names.to_vec());
    }
    let wanted: Vec<String> = selected(manifest, names)
        .into_iter()
        .filter(|name| state.candidates.contains_key(name))
        .collect();
    if wanted.is_empty() {
        return Err(Error::new(Kind::Usage, "there is no kept rebuild to accept").help(
            "`fetchloom build --check` runs each step again and keeps the files of one that differs from data.lock",
        ));
    }
    Ok(wanted)
}

/// the recipe `name` as it stands now and its kept rebuild, once the rebuild answers it: its key
/// is the one the recipe makes now, data.lock still pins the result it was kept beside, and the
/// cache holds its files.
fn checked<'s>(
    project: &Project,
    manifest: &Manifest,
    lock: &Lock,
    cache: &Cache,
    state: &'s State,
    name: &str,
) -> Result<(Step, &'s Candidate), Error> {
    let candidate = state.candidates.get(name).ok_or_else(|| {
        Error::new(
            Kind::Usage,
            format!("there is no kept rebuild of {name} to accept"),
        )
        .help(again(name))
    })?;
    let step = Step::read(&project.root, manifest, lock, name)?;
    let agrees = step.as_ref().and_then(Step::key) == Some(candidate.key)
        && lock
            .result(name)
            .is_some_and(|pinned| pinned.key == candidate.key && pinned.tree == candidate.pinned)
        && crate::cache::tree_hash(&candidate.files) == candidate.tree;
    let step = match step {
        Some(step) if agrees => step,
        _ => {
            return Err(Error::new(
                Kind::Usage,
                format!(
                    "the kept rebuild of {name} came from another recipe or pin than data.lock has now"
                ),
            )
            .help(again(name)));
        }
    };
    if !whole(cache, &candidate.files) {
        return Err(Error::new(
            Kind::Usage,
            format!("the files of the kept rebuild of {name} are no longer in the cache"),
        )
        .help(again(name)));
    }
    Ok((step, candidate))
}

/// pins `candidate` in `lock` as the result of `step`, and records it in the cache as the newest
/// result of its key, the one a later build reuses.
fn take(
    work: &Loop<'_>,
    lock: &mut Lock,
    step: &Step,
    candidate: &Candidate,
) -> Result<Done, Error> {
    let name = &step.name;
    work.cache
        .record_result(&candidate.key, &candidate.files)
        .map_err(|err| {
            Error::io(
                format_args!("could not record the accepted result of {name} in the cache"),
                &err,
            )
        })?;
    work.context.output.step(
        "Accepted",
        &format!(
            "{name} · {} in place of the files data.lock pinned",
            sized(&candidate.files)
        ),
    );
    pin(
        lock,
        step,
        candidate.key,
        candidate.tree,
        candidate.files.clone(),
    );
    Ok(done(
        Did::Accepted,
        name,
        candidate.key,
        candidate.tree,
        &candidate.files,
    ))
}

/// the closing lines of an accept of `accepted`: what it pinned, the recipes made from them that
/// need building again, and the commit.
fn tell(context: &Context, project: &Project, manifest: &Manifest, accepted: &[String]) {
    let output = &context.output;
    match accepted {
        [name] => output.done("Ok", &format!("pinned {name}'s kept rebuild in data.lock")),
        many => output.done(
            "Ok",
            &format!(
                "pinned the kept rebuilds of {} in data.lock",
                crate::units::count(many.len() as u64, "recipe", "recipes")
            ),
        ),
    }
    for (reader, read) in readers(manifest, accepted) {
        output.hint(&format!(
            "{reader} reads {read}, so `fetchloom build {reader}` builds it again from the accepted files"
        ));
    }
    output.next(&format!("commit {}", committed(project)));
}

/// what to run for a new kept rebuild of `name`.
fn again(name: &str) -> String {
    format!(
        "`fetchloom build --check {name}` runs its step again as data.lock pins it now, and keeps its files when they differ"
    )
}

/// each recipe that reads one of `accepted` and is not accepted itself, with the one it reads.
fn readers<'a>(manifest: &'a Manifest, accepted: &'a [String]) -> Vec<(&'a str, &'a str)> {
    manifest
        .datasets
        .iter()
        .filter(|(name, _)| !accepted.contains(name))
        .filter_map(|(name, entry)| {
            let read = entry
                .recipe()?
                .from
                .iter()
                .find(|input| accepted.contains(input))?;
            Some((name.as_str(), read.as_str()))
        })
        .collect()
}
