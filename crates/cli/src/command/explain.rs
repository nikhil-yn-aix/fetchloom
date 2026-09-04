//! The `explain` command: the effective settings and where each came from.

use crate::command::{thread_budget, write_json};
use crate::{cache, config, explain, run, settings};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::policy::IoMode;
use fetchloom_engine::work::WorkCounter;
use std::sync::Arc;

pub(crate) fn tuning_for(
    resolved: &settings::Settings,
    policy: &dyn fetchloom_engine::seam::policy::Policy,
) -> run::Tuning {
    run::Tuning {
        ceilings: fetchloom_engine::tuning::Ceilings::resolve(
            thread_budget(resolved),
            policy.limits(),
            policy.concurrency(),
            policy.per_host(),
            policy.aggressive(),
        ),
        adapts: policy.adapts(),
        bandwidth: policy.bandwidth(),
    }
}

pub(crate) fn measured_for(resolved: &settings::Settings) -> explain::Measured {
    let budget = thread_budget(resolved);
    let ceilings = fetchloom_engine::tuning::Ceilings::resolve(
        budget,
        &fetchloom_engine::limits::Limits::default(),
        resolved.concurrency.value,
        resolved.per_host.value,
        resolved.aggressive.value,
    );
    let recorded = run::resolve_path(&resolved.cache_dir.value)
        .ok()
        .and_then(|root| {
            let work = Arc::new(WorkCounter::new());
            let pool = Processor::new(budget).ok()?;
            match cache::open(
                &root,
                DurabilityTier::Normal,
                fetchloom_engine::verification::VerificationPolicy::Fingerprint,
                IoMode::Buffered,
                work,
                Arc::new(pool),
            ) {
                cache::Opened::Ready(held) => Some(held),
                cache::Opened::Degraded { .. } | cache::Opened::Refused(_) => None,
            }
        })
        .map(|held| {
            held.measurements()
                .iter()
                .map(|(host, found)| found.describe(host))
                .collect()
        })
        .unwrap_or_default();
    explain::Measured {
        threads: detected_threads(),
        concurrency: ceilings.global.get(),
        per_host: ceilings.per_host.get(),
        recorded,
    }
}

#[must_use]
pub fn run_explain(
    resolved: &settings::Settings,
    discovered: &config::Discovered,
    key: Option<&str>,
    json: bool,
) -> ExitCode {
    let rows = explain::rows(resolved, &measured_for(resolved));
    if let Some(key) = key {
        let Some(row) = rows.iter().find(|row| row.key == key) else {
            eprintln!("{key} is not a setting this build has");
            return ExitCode::Usage;
        };
        if json {
            return write_json(row);
        }
        println!("{} = {} ({})", row.key, row.value, row.origin);
        return ExitCode::Success;
    }
    if json {
        return write_json(&explain::Report {
            files: explain::files(discovered),
            settings: rows,
        });
    }
    for line in explain::file_lines(discovered) {
        println!("{line}");
    }
    for row in rows {
        println!("{} = {} ({})", row.key, row.value, row.origin);
    }
    ExitCode::Success
}

pub(crate) fn detected_threads() -> u32 {
    u32::try_from(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
        .unwrap_or(1)
}
