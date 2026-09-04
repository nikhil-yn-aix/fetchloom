//! The `repair` command: refetching the damaged ranges of a cached object.

use crate::command::plan::lock_path_of;
use crate::thread_budget;
use fetchloom_cli::settings::ProcessEnvironment;
use fetchloom_cli::surface::CommandLine;
use fetchloom_cli::terminal::Streams;
use fetchloom_cli::{Reporter, cache, locked, policy, run, settings, surface};
use fetchloom_engine::event::Sequence;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::work::WorkCounter;
use std::sync::Arc;

pub(crate) fn run_repair(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
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
    let held = match cache::require(&root, Arc::clone(&work), Arc::new(processor)) {
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
    let digest = match fetchloom_cli::repair::digest_for(&held, reference, pinned) {
        Ok(digest) => digest,
        Err(error) => return reporter.report(&error),
    };

    let outcome = fetchloom_cli::repair::Repair {
        cache: &held,
        location: reference,
        work: &work,
        observer,
        sequence,
    }
    .run(digest);
    fetchloom_cli::repair::report(&outcome, &reporter)
}
