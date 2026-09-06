//! The `repair` command: refetching the damaged ranges of a cached object.

use crate::command::plan::lock_path_of;
use crate::command::thread_budget;
use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::terminal::Streams;
use crate::{Reporter, cache, locked, policy, run, settings, surface};
use fetchloom_engine::event::Sequence;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::work::WorkCounter;
use std::sync::Arc;

pub(crate) fn inert_on_repair(transfer: &surface::TransferFlags) -> Option<&'static str> {
    if transfer.output.is_some() {
        return Some("--output");
    }
    if !transfer.select.is_empty() {
        return Some("--select");
    }
    if !transfer.exclude.is_empty() {
        return Some("--exclude");
    }
    if transfer.layout.is_some() {
        return Some("--layout");
    }
    if transfer.no_extract {
        return Some("--no-extract");
    }
    if transfer.force {
        return Some("--force");
    }
    if transfer.adopt {
        return Some("--adopt");
    }
    None
}

#[must_use]
pub fn run_repair(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    if let Some(flag) = inert_on_repair(transfer) {
        eprintln!(
            "{flag} chooses what a run materializes, and repair restores bytes in the cache \
             and materializes nothing"
        );
        return ExitCode::Usage;
    }
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
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
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let work = Arc::new(WorkCounter::new());
    let held = match cache::require(
        &root,
        resolved.compress.value,
        Arc::clone(&work),
        Arc::new(processor),
    ) {
        Ok(held) => held,
        Err(refused) => return reporter.report(&refused),
    };

    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return reporter.report(&error),
    };
    let pinned = match locked::pinned(&lock_path, &run::remote_name(reference), None) {
        Ok(found) => found.and_then(|dataset| {
            dataset
                .artifacts
                .values()
                .next()
                .map(|artifact| artifact.digest)
        }),
        Err(error) => return reporter.report(&error),
    };
    let digest = match crate::repair::digest_for(&held, reference, pinned) {
        Ok(digest) => digest,
        Err(error) => return reporter.report(&error),
    };

    let outcome = crate::repair::Repair {
        cache: &held,
        location: reference,
        work: &work,
        observer,
        sequence,
    }
    .run(digest);
    crate::repair::report(&outcome, &reporter)
}
