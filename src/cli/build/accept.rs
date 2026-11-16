//! `fetchloom build --accept`: pins and places the rebuild a build kept of a recipe, running
//! nothing. it chooses which files a result holds, so only a person runs it: no error suggests it.

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

/// pins and places the kept rebuild of each recipe `names` asks for, or of every one with one,
/// changing nothing unless all can be pinned. `usage` refuses a missing rebuild, one whose key or
/// pin is not data.lock's now, or one whose files the cache no longer holds.
pub(super) async fn accept(
    context: &Context,
    project: &Project,
    names: &[String],
) -> Result<Built, Error> {
    let (text, manifest) = project.manifest()?;
    let read = project.lock_and_text()?;
    // as relocking will leave data.lock, a renamed recipe's pin and kept rebuild under its new name
    let (old, _) = crate::cli::sync::rename::forecast(
        &manifest,
        read.as_ref()
            .map(|(lock, _)| lock.clone())
            .unwrap_or_default(),
    );
    let (mut state, _) = project.state();
    state.carry_candidates(&manifest, &old);
    let wanted = wanted(&manifest, &state, names)?;
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
    for (step, candidate) in &accepted {
        report.steps.push(take(&work, &mut lock, step, candidate));
    }
    // so a CLI test can stop the accept between its last pin and its last look for a stop
    #[cfg(debug_assertions)]
    crate::stop::hold("FETCHLOOM_TEST_HOLD_ACCEPT");
    // the last look for a stop: once data.lock changes, the accept runs to its end
    if context.stop.is_asked() {
        return Err(super::stopped());
    }
    context.stop.finish();
    let written = work.write_rendered(&before, &lock, lock.render())?;
    for (step, candidate) in &accepted {
        record(&work.cache, step, candidate);
    }
    work.project.sweep_lists(&lock);
    forget(project, &wanted)?;
    let seen = Seen {
        manifest: digest::sha256(text.as_bytes()),
        lock: Some(digest::sha256(written.as_bytes())),
    };
    let outcome = sync_all(&work, &manifest, Some(lock), None, Run::planned(&seen)).await?;
    ignore_data(&work, &manifest, false)?;
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
        return Err(
            Error::new(Kind::Usage, "there is no kept rebuild to accept").help(format!(
                "`{}` runs each step again and keeps the files of one that differs from data.lock",
                crate::cli::project::command("build --check")
            )),
        );
    }
    Ok(wanted)
}

/// the recipe `name` as it stands and its kept rebuild, once the rebuild answers it: its key is the
/// recipe's now, data.lock still pins what it was kept beside, and the cache holds its files.
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
    let agrees = candidate.answers(step.as_ref().and_then(Step::key), lock.result(name));
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

/// pins `candidate` in `lock` as the result of `step`.
fn take(work: &Loop<'_>, lock: &mut Lock, step: &Step, candidate: &Candidate) -> Done {
    let name = &step.name;
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
    done(
        Did::Accepted,
        name,
        candidate.key,
        candidate.tree,
        &candidate.files,
    )
}

/// records `candidate`, which data.lock now pins as the result of `step`, in `cache` as the newest
/// result of its key, the one a build elsewhere with no pin reuses: only once data.lock pins it,
/// so an accept stopped or failed before never makes a rebuild no one accepted that result. the
/// project's own pin needs no record, so one that cannot be written is only logged.
fn record(cache: &Cache, step: &Step, candidate: &Candidate) {
    if let Err(err) = cache.record_result(&candidate.key, &candidate.files) {
        tracing::debug!(%err, recipe = step.name, "could not record an accepted result in the cache");
    }
}

/// the closing lines of an accept of `accepted`: what it pinned, the recipes made from them that
/// need building again, and the commit.
fn tell(context: &Context, project: &Project, manifest: &Manifest, accepted: &[String]) {
    let output = &context.output;
    match accepted {
        [name] => output.done(
            "Ok",
            &format!("pinned the kept rebuild of {name} in data.lock"),
        ),
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
            "{reader} reads {read}, so `{}` builds it again from the accepted files",
            crate::cli::project::command(&format!("build {reader}"))
        ));
    }
    output.next(&format!("commit {}", committed(project)));
}

/// what to run for a new kept rebuild of `name`.
pub(in crate::cli) fn again(name: &str) -> String {
    format!(
        "`{}` runs its step again as data.lock pins it now, and keeps its files when they differ",
        crate::cli::project::command(&format!("build --check {name}"))
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
