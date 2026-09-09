//! The `plan` and `apply` commands: resolving a run to a file, and executing that file.

use crate::command::explain::tuning_for;
use crate::command::get::{finish_get, held_to_lock, resolve_places, selection_of};
use crate::command::{Opened, open_cache, open_for, thread_budget};
use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::terminal::Streams;
use crate::{Reporter, locked, planning, policy, run, settings, surface};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::work::WorkCounter;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn inert_on_plan(transfer: &surface::TransferFlags) -> Option<&'static str> {
    if transfer.force {
        return Some("--force");
    }
    if transfer.adopt {
        return Some("--adopt");
    }
    None
}

#[must_use]
pub fn run_plan(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    if let Some(flag) = inert_on_plan(transfer) {
        eprintln!("{flag} changes what a run does to a destination, and plan writes to none");
        return ExitCode::Usage;
    }
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let json = parsed.global.json;
    let work = Arc::new(WorkCounter::new());
    let adapters = run::adapters_for(&work, &settings::limits_for(resolved));
    let environment = ProcessEnvironment;
    let policy = policy::CommandLinePolicy::new(
        resolved.clone(),
        transfer,
        Streams::detect(),
        parsed.global.yes,
        &environment,
        &policy::NativeCredentialStore,
        observer,
        sequence,
        &policy::StdinPrompter,
    );
    if let Err(error) = run::allowed_offline(reference, &policy) {
        return reporter.report(&error);
    }
    let (source, named) = match resolve_places(&adapters, reference, transfer) {
        Ok(places) => places,
        Err(error) => return reporter.report(&error),
    };
    let dataset = run::dataset_name(&adapters, reference, &source);
    let destination = match named {
        Some(named) => named,
        None => match run::resolve_path(&PathBuf::from(".").join(&dataset)) {
            Ok(destination) => destination,
            Err(error) => return reporter.report(&error),
        },
    };
    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return reporter.report(&error),
    };
    let pinned = match locked::pinned(&lock_path, &dataset, Some(locked::Requirement::Plan)) {
        Ok(pinned) => pinned,
        Err(error) => return reporter.report(&error),
    };
    let root = match run::resolve_path(policy.cache_directory().unwrap_or(Path::new("."))) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let held = if transfer.no_cache {
        None
    } else {
        match open_cache(
            &root,
            &policy,
            &work,
            &Arc::new(processor),
            observer,
            sequence,
        ) {
            Ok(held) => held,
            Err(refused) => return reporter.report(&refused),
        }
    };
    let plan = match planning::build(
        pinned.as_ref(),
        &dataset,
        reference,
        &destination,
        held.as_deref(),
    ) {
        Ok(plan) => plan,
        Err(error) => return reporter.report(&error),
    };
    observer.emit(&Event::new(sequence, EventPayload::PlanReady));
    let written = if json {
        serde_json::to_string(&plan).map_err(|reason| reason.to_string())
    } else {
        plan.render().map_err(|reason| reason.to_string())
    };
    match written {
        Ok(body) => {
            println!("{body}");
            ExitCode::Success
        }
        Err(reason) => {
            eprintln!("the plan could not be written: {reason}");
            ExitCode::Usage
        }
    }
}

#[must_use]
pub fn run_apply(
    plan_path: &std::path::Path,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let (plan, artifact, destination) = match read_plan(plan_path, transfer) {
        Ok(read) => read,
        Err(error) => return reporter.report(&error),
    };
    let work = Arc::new(WorkCounter::new());
    let adapters = run::adapters_for(&work, &settings::limits_for(resolved));
    let environment = ProcessEnvironment;
    let policy = policy::CommandLinePolicy::new(
        resolved.clone(),
        transfer,
        Streams::detect(),
        parsed.global.yes,
        &environment,
        &policy::NativeCredentialStore,
        observer,
        sequence,
        &policy::StdinPrompter,
    );
    let Opened {
        processor,
        platform,
        durability,
        scratch: _scratch,
        held,
    } = match open_for(
        resolved,
        transfer,
        &work,
        &destination,
        &policy,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(opened) => opened,
        Err(code) => return code,
    };
    let tuning = tuning_for(resolved, &policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: processor.as_ref(),
        digester: &digester,
        platform: &platform,
        durability,
        cache: held.as_deref(),
        work: &work,
        extract: !transfer.no_extract,
        verify: policy.verification(),
        tuning: &tuning,
        policy: &policy,
        adapters: &adapters,
    };
    let selection = fetchloom_engine::selection::Selection {
        include: artifact.select.clone(),
        exclude: Vec::new(),
        layout: artifact.layout,
    };
    let produced = apply_produce(
        &with,
        &policy,
        &plan,
        &artifact,
        &destination,
        &selection,
        transfer,
        observer,
        sequence,
    );
    let result = match produced {
        Ok(result) => result,
        Err(error) => return reporter.report(&error),
    };
    let manifest = run::synthesized_manifest(&adapters, &plan.dataset, artifact.source.as_str());
    let resolved = run::resolved_object(&result, &selection);
    finish_get(
        &result,
        held.as_deref(),
        &manifest,
        &resolved,
        &policy,
        None,
        &reporter,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "each argument names a piece of what applying a plan produces"
)]
pub(crate) fn apply_produce(
    with: &run::Materialization<'_>,
    policy: &dyn fetchloom_engine::seam::policy::Policy,
    plan: &fetchloom_engine::plan::Plan,
    artifact: &fetchloom_engine::plan::PlanArtifact,
    destination: &Path,
    selection: &fetchloom_engine::selection::Selection,
    transfer: &surface::TransferFlags,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<run::RunResult, fetchloom_engine::error::Error> {
    let cached = with
        .cache
        .is_some_and(|cache| cache.contains(artifact.digest).unwrap_or(false));
    if cached {
        return run::materialize_cached(
            with,
            artifact.digest,
            artifact.size,
            &plan.dataset,
            destination,
            selection,
            transfer.force,
            transfer.adopt,
            &artifact.source,
            observer,
            sequence,
        );
    }
    run::allowed_offline(artifact.source.as_str(), policy)?;
    run::materialize_remote(
        with,
        artifact.source.as_str(),
        destination,
        selection,
        transfer.force,
        transfer.adopt,
        Some(artifact.digest),
        observer,
        sequence,
    )
}

pub(crate) fn lock_path_of(
    transfer: &surface::TransferFlags,
) -> Result<PathBuf, fetchloom_engine::error::Error> {
    run::resolve_path(
        &transfer
            .lock
            .clone()
            .unwrap_or_else(|| PathBuf::from("fetchloom.lock")),
    )
}

pub(crate) fn read_plan(
    plan_path: &std::path::Path,
    transfer: &surface::TransferFlags,
) -> Result<
    (
        fetchloom_engine::plan::Plan,
        fetchloom_engine::plan::PlanArtifact,
        PathBuf,
    ),
    fetchloom_engine::error::Error,
> {
    let plan = planning::read(plan_path, &fetchloom_engine::limits::Limits::default())?;
    let artifact = planning::only_artifact(&plan)?.clone();
    let destination = run::resolve_path(transfer.output.as_deref().unwrap_or(&plan.destination))?;
    Ok((plan, artifact, destination))
}

pub(crate) fn locked_selection(
    transfer: &surface::TransferFlags,
    dataset: &str,
    manifest: &fetchloom_engine::manifest::Manifest,
) -> Result<
    (
        PathBuf,
        fetchloom_engine::selection::Selection,
        Option<fetchloom_engine::lock::LockedDataset>,
    ),
    fetchloom_engine::error::Error,
> {
    let lock_path = lock_path_of(transfer)?;
    let selection = selection_of(transfer);
    let pinned = held_to_lock(&lock_path, dataset, manifest, &selection, transfer.locked)?;
    Ok((lock_path, selection, pinned))
}
