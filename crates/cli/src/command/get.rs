//! The `get` command: resolving a reference, materializing it, and recording what it produced.

use crate::command::explain::tuning_for;
use crate::command::plan::locked_selection;
use crate::command::{Opened, Requested, get_policy, open_for, open_request};
use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::{Reporter, locked, run, settings, surface};
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::event::Sequence;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use std::path::PathBuf;
use std::sync::Arc;

#[must_use]
pub fn run_get(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let work = Arc::new(WorkCounter::new());
    let limits = settings::limits_for(resolved);
    let adapters = run::adapters_for(&work, &limits);
    let environment = ProcessEnvironment;
    let policy = get_policy(transfer, parsed, resolved, &environment, observer, sequence);

    let Requested {
        reference: named_reference,
        source,
        named,
        manifest,
        is_dataset,
        remote,
    } = match open_request(
        reference, transfer, &adapters, resolved, &policy, &limits, observer, sequence,
    ) {
        Ok(opened) => opened,
        Err(error) => return reporter.report(&error),
    };
    let reference = named_reference.as_str();
    let dataset = manifest.name.clone();
    let accepted_terms = match run::assert_terms(&policy, manifest.license.as_ref()) {
        Ok(accepted) => accepted,
        Err(error) => return reporter.report(&error),
    };
    let destination = match destination_for(named, &dataset) {
        Ok(destination) => destination,
        Err(error) => return reporter.report(&error),
    };

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

    let (lock_path, selection, pinned) = match locked_selection(transfer, &dataset, &manifest) {
        Ok(held) => held,
        Err(error) => return reporter.report(&error),
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
    let produced = resolve_and_publish(
        &with,
        &Request {
            reference,
            remote,
            is_dataset,
            source: &source,
            destination: &destination,
            manifest: &manifest,
            selection: &selection,
            transfer,
            pinned: pinned.as_ref(),
        },
        observer,
        sequence,
    );

    record(
        &produced,
        &Recording {
            manifest: &manifest,
            dataset: &dataset,
            lock_path: &lock_path,
            pinned: pinned.as_ref(),
            locked: transfer.locked,
            cache: held.as_deref(),
            policy: &policy,
            json: parsed.global.json,
            accepted_terms,
            selected: !transfer.select.is_empty() || !transfer.exclude.is_empty(),
        },
        observer,
        sequence,
    )
}

pub(crate) struct Request<'a> {
    reference: &'a str,
    remote: bool,
    is_dataset: bool,
    source: &'a std::path::Path,
    destination: &'a std::path::Path,
    manifest: &'a fetchloom_engine::manifest::Manifest,
    selection: &'a fetchloom_engine::selection::Selection,
    transfer: &'a surface::TransferFlags,
    pinned: Option<&'a fetchloom_engine::lock::LockedDataset>,
}

pub(crate) fn resolve_and_publish(
    with: &run::Materialization<'_>,
    request: &Request<'_>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> run::DatasetRun {
    if request.is_dataset {
        let base = request
            .source
            .parent()
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        return run::materialize_manifest(
            with,
            request.manifest,
            &base,
            request.destination,
            request.transfer.force,
            request.transfer.adopt,
            request.pinned,
            observer,
            sequence,
        );
    }
    let outcome = materialize(
        with,
        request.reference,
        request.remote,
        request.source,
        request.destination,
        request.selection,
        request.transfer,
        request.pinned,
        observer,
        sequence,
    );
    let resolved = outcome
        .as_ref()
        .map(|result| run::resolved_object(result, request.selection))
        .unwrap_or_default();
    run::DatasetRun { resolved, outcome }
}

pub(crate) struct Recording<'a> {
    manifest: &'a fetchloom_engine::manifest::Manifest,
    dataset: &'a str,
    lock_path: &'a std::path::Path,
    pinned: Option<&'a fetchloom_engine::lock::LockedDataset>,
    locked: bool,
    cache: Option<&'a fetchloom_cache::Cache<NativePlatform>>,
    policy: &'a dyn fetchloom_engine::seam::policy::Policy,
    json: bool,
    accepted_terms: Option<fetchloom_engine::license::Acceptance>,
    selected: bool,
}

fn unpinned(produced: &run::DatasetRun) -> locked::Unpinned {
    match &produced.outcome {
        Err(_) => locked::Unpinned::RunFailed,
        Ok(result) => match result.artifact {
            None => locked::Unpinned::Tree,
            Some(_) => locked::Unpinned::InteropUnknown,
        },
    }
}

pub(crate) fn record(
    produced: &run::DatasetRun,
    into: &Recording<'_>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let tree = produced.outcome.as_ref().ok().map(|result| result.tree);
    let settled = locked::resolved(into.manifest, &produced.resolved, tree).and_then(|recorded| {
        locked::settle(
            into.lock_path,
            into.dataset,
            into.pinned,
            recorded.as_ref().ok_or_else(|| unpinned(produced)),
            into.locked,
            observer,
            sequence,
        )
    });
    let reporter = Reporter::new(into.json, observer, sequence);
    if let Err(error) = settled {
        return reporter.report(&error);
    }
    match &produced.outcome {
        Ok(result) => {
            crate::hint::record(|observed| {
                observed.lock_written = Some(into.lock_path.display().to_string());
                if !into.selected {
                    observed.entries_taken_whole = Some(result.entries);
                }
            });
            finish_get(
                result,
                into.cache,
                into.manifest,
                &produced.resolved,
                into.policy,
                into.accepted_terms,
                &reporter,
            )
        }
        Err(error) => reporter.report(error),
    }
}

pub(crate) fn destination_for(
    named: Option<PathBuf>,
    dataset: &str,
) -> Result<PathBuf, fetchloom_engine::error::Error> {
    match named {
        Some(named) => Ok(named),
        None => run::resolve_path(&PathBuf::from(".").join(dataset)),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the flags, the lock, and the two observers each name a contract behavior of their own"
)]
pub(crate) fn materialize(
    with: &run::Materialization<'_>,
    reference: &str,
    remote: bool,
    source: &std::path::Path,
    destination: &std::path::Path,
    selection: &fetchloom_engine::selection::Selection,
    transfer: &surface::TransferFlags,
    pinned: Option<&fetchloom_engine::lock::LockedDataset>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<run::RunResult, fetchloom_engine::error::Error> {
    if remote && run::is_container(with.adapters, reference) {
        return run::materialize_remote_container(
            with,
            reference,
            destination,
            selection,
            transfer.force,
            transfer.adopt,
            observer,
            sequence,
        );
    }
    if remote {
        return run::materialize_remote(
            with,
            reference,
            destination,
            selection,
            transfer.force,
            transfer.adopt,
            pinned
                .filter(|_| transfer.locked)
                .and_then(|entry| entry.artifacts.values().next())
                .map(|artifact| artifact.digest),
            observer,
            sequence,
        );
    }
    run::materialize_local(
        with,
        source,
        destination,
        selection,
        transfer.force,
        transfer.adopt,
        pinned
            .filter(|_| transfer.locked)
            .and_then(|entry| entry.artifacts.values().next())
            .map(|artifact| artifact.digest),
        observer,
        sequence,
    )
}

pub(crate) fn resolve_places(
    adapters: &Adapters,
    reference: &str,
    transfer: &surface::TransferFlags,
    policy: &dyn Policy,
) -> Result<(PathBuf, Option<PathBuf>), fetchloom_engine::error::Error> {
    run::allowed_offline(reference, policy)?;
    let source = if run::is_served(adapters, reference) {
        PathBuf::from(run::remote_name(reference))
    } else {
        run::local_path(reference)?
    };
    let named = match transfer.output.clone() {
        Some(requested) => Some(run::resolve_path(&requested)?),
        None => None,
    };
    Ok((source, named))
}

pub(crate) fn held_to_lock(
    lock_path: &std::path::Path,
    dataset: &str,
    manifest: &fetchloom_engine::manifest::Manifest,
    selection: &fetchloom_engine::selection::Selection,
    is_locked: bool,
) -> Result<Option<fetchloom_engine::lock::LockedDataset>, fetchloom_engine::error::Error> {
    let pinned = locked::pinned(
        lock_path,
        dataset,
        is_locked.then_some(locked::Requirement::LockedRun),
    )?;
    if is_locked && let Some(pinned) = pinned.as_ref() {
        pinned.check_request(manifest.digest()?, manifest.release.as_deref(), selection)?;
    }
    Ok(pinned)
}

pub(crate) fn finish_get(
    result: &run::RunResult,
    cache: Option<&fetchloom_cache::Cache<NativePlatform>>,
    manifest: &fetchloom_engine::manifest::Manifest,
    artifacts: &[run::ResolvedArtifact],
    policy: &dyn fetchloom_engine::seam::policy::Policy,
    accepted_terms: Option<fetchloom_engine::license::Acceptance>,
    reporter: &Reporter<'_>,
) -> ExitCode {
    let mut result = result.clone();
    if let Some(cache) = cache {
        match run::write_receipt(
            cache,
            manifest,
            artifacts,
            &result,
            policy.verification(),
            accepted_terms,
        ) {
            Ok(trust) => result.trust = trust,
            Err(error) => return reporter.report(&error),
        }
    }
    if !policy.accepts(result.trust) {
        return reporter.report(&fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::PolicyTrustRefused,
            format!(
                "run it again with --verify never to accept it, because this run can claim only that the bytes are {}",
                result.trust.label()
            ),
        ));
    }
    let result = &result;
    if reporter.json() {
        match serde_json::to_string(result) {
            Ok(body) => println!("{body}"),
            Err(error) => {
                eprintln!("the result could not be written: {error}");
                return ExitCode::Usage;
            }
        }
    } else {
        println!(
            "{}  {}  {}",
            crate::style::accent(&result.tree.to_string()),
            crate::style::dimmed(&format!("{} entries", result.entries)),
            result.destination.display()
        );
    }
    ExitCode::Success
}

pub(crate) fn selection_of(
    transfer: &surface::TransferFlags,
) -> fetchloom_engine::selection::Selection {
    fetchloom_engine::selection::Selection {
        include: transfer
            .select
            .iter()
            .map(|text| fetchloom_engine::selection::Glob::new(text.clone()))
            .collect(),
        exclude: transfer
            .exclude
            .iter()
            .map(|text| fetchloom_engine::selection::Glob::new(text.clone()))
            .collect(),
        layout: transfer
            .layout
            .map_or(fetchloom_engine::selection::Layout::Keep, |arg| arg.0),
    }
}
