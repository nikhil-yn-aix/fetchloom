//! The `why` command: the resolution, the source choice, and the trust reasoning.

use crate::command::thread_budget;
use crate::surface::CommandLine;
use crate::{Reporter, cache, run, settings};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::event::Sequence;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::work::WorkCounter;
use std::io::Write;
use std::sync::Arc;

#[must_use]
pub fn run_why(
    reference: &str,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let work = Arc::new(WorkCounter::new());
    let adapters = run::adapters_for(&work, &settings::limits_for(resolved));
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let held = match cache::open(
        &root,
        DurabilityTier::Normal,
        fetchloom_engine::verification::VerificationPolicy::Fingerprint,
        IoMode::Auto,
        fetchloom_engine::compression::CompressionChoice::Auto,
        Arc::clone(&work),
        Arc::new(processor),
    ) {
        cache::Opened::Ready(held) => Some(held),
        cache::Opened::Degraded { .. } | cache::Opened::Refused(_) => None,
    };
    let explained = match crate::why::explain(reference, &adapters, held.as_deref()) {
        Ok(explained) => explained,
        Err(error) => return reporter.report(&error),
    };
    let mut stdout = std::io::stdout().lock();
    if parsed.global.json {
        match serde_json::to_string(&explained) {
            Ok(written) => {
                let _ = writeln!(stdout, "{written}");
            }
            Err(reason) => {
                eprintln!("the explanation could not be written: {reason}");
                return ExitCode::Usage;
            }
        }
    } else {
        let _ = write!(stdout, "{}", explained.render());
    }
    let _ = stdout.flush();
    ExitCode::Success
}
