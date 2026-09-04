//! The `cache` command and its subcommands.

use crate::thread_budget;
use fetchloom_cli::{Reporter, cache, run, settings, surface};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use std::sync::Arc;

pub(crate) fn run_cache(
    resolved: &settings::Settings,
    command: &surface::CacheCommand,
    reporter: &Reporter<'_>,
    yes: bool,
) -> ExitCode {
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    cache::run(&root, command, Arc::new(processor), reporter, yes)
}
