//! The `verify` command: recomputing a materialized tree against its receipt.

use crate::command::{open_cache, thread_budget};
use crate::settings::ProcessEnvironment;
use crate::terminal::Streams;
use crate::{Reporter, cache, policy, run, settings, surface};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::work::WorkCounter;
use std::sync::Arc;

#[must_use]
pub fn run_verify(
    target: &str,
    resolved: &settings::Settings,
    json: bool,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(json, observer, sequence);
    let path = match run::local_path(target).and_then(|path| run::resolve_path(&path)) {
        Ok(path) => path,
        Err(error) => return reporter.report(&error),
    };
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    let work = Arc::new(WorkCounter::new());
    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let environment = ProcessEnvironment;
    let policy = policy::CommandLinePolicy::new(
        resolved.clone(),
        &surface::TransferFlags::default(),
        Streams::detect(),
        false,
        &environment,
        &policy::NativeCredentialStore,
        observer,
        sequence,
        &policy::StdinPrompter,
    );
    let held = match open_cache(
        &root,
        &policy,
        &work,
        &Arc::new(processor),
        observer,
        sequence,
    ) {
        Ok(held) => held,
        Err(refused) => {
            cache::report_degrade(observer, sequence, &root, refused.next_action());
            None
        }
    };
    let receipt = match held.as_deref().map(|cache| cache.read_receipt(&path)) {
        Some(Ok(receipt)) => receipt,
        Some(Err(error)) => return reporter.report(&error),
        None => None,
    };
    match run::verify_tree(&path, receipt.as_ref(), thread_budget(resolved), &emit) {
        Ok((tree, entries)) => {
            if let Some(recorded) = receipt.as_ref().and_then(|receipt| receipt.tree)
                && recorded != tree
            {
                let error = fetchloom_engine::error::Error::new(
                    fetchloom_engine::error::ErrorKind::IntegrityMismatch,
                    format!(
                        "fetch {} again, because it now holds {tree} where the run that wrote it reported {recorded}",
                        path.display()
                    ),
                );
                return reporter.report(&error);
            }
            if json {
                let body = serde_json::json!({
                    "status": "verified",
                    "tree": tree.to_string(),
                    "entries": entries,
                    "path": path,
                });
                println!("{body}");
            } else {
                println!("{tree}  {entries} entries  {}", path.display());
            }
            ExitCode::Success
        }
        Err(error) => reporter.report(&error),
    }
}
