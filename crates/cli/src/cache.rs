//! The cache commands, and the one place a cache is opened.

use std::io::IsTerminal;
use std::path::Path;
use std::sync::Arc;

use fetchloom_cache::Cache;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;

use crate::surface::CacheCommand;

/// A cache this run may use, or why it may not.
pub enum Opened {
    /// The cache is open and usable.
    Ready(Box<Cache<NativePlatform>>),
    /// The cache cannot be used, and the run continues without one.
    Degraded {
        /// What the platform said.
        reason: String,
    },
    /// The cache was written in a format this build does not read.
    Refused(Box<Error>),
}

/// Opens the cache at a root, deciding what an unusable one means.
#[must_use]
pub fn open(
    root: &Path,
    tier: DurabilityTier,
    policy: VerificationPolicy,
    work: Arc<WorkCounter>,
    processor: Arc<fetchloom_engine::pool::Processor>,
) -> Opened {
    let platform = NativePlatform::new(Arc::clone(&work));
    match Cache::open(root, platform, tier, policy, work, processor) {
        Ok(held) => Opened::Ready(Box::new(held)),
        Err(refused)
            if refused.kind() == ErrorKind::CacheFormatMismatch
                || refused.kind() == ErrorKind::CacheLockingUnsupported =>
        {
            Opened::Refused(Box::new(refused))
        }
        Err(refused) => Opened::Degraded {
            reason: refused.next_action().to_owned(),
        },
    }
}

/// Opens the cache for a cache command, where there is nothing to degrade to.
///
/// # Errors
///
/// Fails when the cache cannot be opened for any reason.
pub fn require(
    root: &Path,
    work: Arc<WorkCounter>,
    processor: Arc<fetchloom_engine::pool::Processor>,
) -> Result<Cache<NativePlatform>, Error> {
    let platform = NativePlatform::new(Arc::clone(&work));
    Cache::open(
        root,
        platform,
        DurabilityTier::Normal,
        VerificationPolicy::Fingerprint,
        work,
        processor,
    )
}

/// Emits the degrade event a run makes when it cannot use its cache.
pub fn report_degrade(observer: &dyn Observer, sequence: &Sequence, root: &Path, reason: &str) {
    observer.emit(&Event::new(
        sequence,
        EventPayload::Degrade {
            requested: format!("the cache at {}", root.display()),
            used: "no cache, so nothing is retained".to_owned(),
            reason: reason.to_owned(),
        },
    ));
}

/// Runs one cache command.
#[must_use]
pub fn run(
    root: &Path,
    command: &CacheCommand,
    processor: Arc<fetchloom_engine::pool::Processor>,
    reporter: &crate::Reporter<'_>,
    yes: bool,
) -> ExitCode {
    let held = match require(root, Arc::new(WorkCounter::new()), processor) {
        Ok(held) => held,
        Err(refused) if matches!(command, CacheCommand::Clear) => {
            let _ = refused;
            return clear(root, None, reporter, yes);
        }
        Err(refused) => return reporter.report(&refused),
    };

    match command {
        CacheCommand::Status => report_status(&held, reporter),
        CacheCommand::Ls => report_list(&held, reporter),
        CacheCommand::Verify => report_verify(&held, reporter),
        CacheCommand::Pin { digest } => change_pin(&held, digest, true, reporter),
        CacheCommand::Unpin { digest } => change_pin(&held, digest, false, reporter),
        CacheCommand::Export { bundle } => report_bundle(&held.export(bundle), reporter),
        CacheCommand::Import { bundle } => {
            let read = fetchloom_cache::bundle::BundleReader::open(bundle)
                .and_then(|reader| held.import(bundle, reader));
            report_bundle(&read, reporter)
        }
        CacheCommand::Repair => report_rebuild(&held, reporter),
        CacheCommand::Prune => report_prune(&held, reporter),
        CacheCommand::Clear => {
            let counted = held.status().ok();
            drop(held);
            clear(root, counted.as_ref(), reporter, yes)
        }
    }
}

fn report_status(held: &Cache<NativePlatform>, reporter: &crate::Reporter<'_>) -> ExitCode {
    match held.status() {
        Ok(found) => {
            if reporter.json() {
                print_json(&found);
            } else {
                println!("{}", found.root.display());
                println!("objects     {:>12}", found.objects);
                println!("bytes       {:>12}", found.bytes);
                println!("partials    {:>12}", found.partials);
                println!("pins        {:>12}", found.pins);
                println!("quarantined {:>12}", found.quarantined);
            }
            ExitCode::Success
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn report_list(held: &Cache<NativePlatform>, reporter: &crate::Reporter<'_>) -> ExitCode {
    match held.list() {
        Ok(found) => {
            if reporter.json() {
                let rendered: Vec<String> =
                    found.iter().map(std::string::ToString::to_string).collect();
                print_json(&rendered);
            } else {
                for digest in found {
                    let size = held.size_of(digest).unwrap_or_default();
                    let pinned = if held.layout().pin_of(digest).exists() {
                        "pinned"
                    } else {
                        ""
                    };
                    println!("{digest}  {size:>12}  {pinned}");
                }
            }
            ExitCode::Success
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn report_verify(held: &Cache<NativePlatform>, reporter: &crate::Reporter<'_>) -> ExitCode {
    match fetchloom_cache::verify::run(held) {
        Ok(found) => {
            if reporter.json() {
                print_json(&found);
            } else {
                println!("verified    {:>12}", found.verified);
                println!("quarantined {:>12}", found.quarantined.len());
                println!("held        {:>12}", found.held);
                for digest in &found.quarantined {
                    println!("{digest} was quarantined");
                }
            }
            if found.quarantined.is_empty() {
                ExitCode::Success
            } else {
                ExitCode::Cache
            }
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn change_pin(
    held: &Cache<NativePlatform>,
    digest: &str,
    pin: bool,
    reporter: &crate::Reporter<'_>,
) -> ExitCode {
    let parsed = match digest
        .parse::<fetchloom_engine::digest::Digest>()
        .map_err(|reason| reason.to_string())
        .and_then(|found| ContentDigest::try_from(found).map_err(|reason| reason.to_string()))
    {
        Ok(parsed) => parsed,
        Err(reason) => {
            let refused = Error::new(
                ErrorKind::CacheCorrupt,
                format!("name an object the way cache ls prints it: {reason}"),
            );
            return reporter.report(&refused);
        }
    };
    let outcome = if pin {
        held.pin(parsed)
    } else {
        held.unpin(parsed)
    };
    match outcome {
        Ok(()) => {
            if !reporter.json() {
                println!("{parsed} is {}", if pin { "pinned" } else { "not pinned" });
            }
            ExitCode::Success
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn report_rebuild(held: &Cache<NativePlatform>, reporter: &crate::Reporter<'_>) -> ExitCode {
    match fetchloom_cache::rebuild::run(held) {
        Ok(found) => {
            if reporter.json() {
                print_json(&found);
            } else {
                println!("trees       {:>12}", found.trees_rebuilt);
                println!("records     {:>12}", found.records_rebuilt);
                println!("locks       {:>12}", found.locks_released);
                println!("orphans     {:>12}", found.orphans_removed);
                println!("held        {:>12}", found.held);
                for digest in &found.needing_a_source {
                    println!("{digest} needs a source: run repair {digest}");
                }
            }
            if found.needing_a_source.is_empty() {
                ExitCode::Success
            } else {
                ExitCode::Cache
            }
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn report_prune(held: &Cache<NativePlatform>, reporter: &crate::Reporter<'_>) -> ExitCode {
    match held.prune(fetchloom_cache::prune::GRACE) {
        Ok(found) => {
            if reporter.json() {
                print_json(&found);
            } else {
                println!("removed     {:>12}", found.removed);
                println!("bytes       {:>12}", found.bytes_removed);
                println!("kept        {:>12}", found.kept);
                println!("quarantined {:>12}", found.quarantined_removed);
                if found.skipped_other_owner > 0 {
                    println!(
                        "skipped     {:>12}  another user created them",
                        found.skipped_other_owner
                    );
                }
            }
            ExitCode::Success
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn clear(
    root: &Path,
    found: Option<&fetchloom_engine::seam::store::CacheStatus>,
    reporter: &crate::Reporter<'_>,
    yes: bool,
) -> ExitCode {
    if !yes {
        if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
            let refused = Error::new(
                ErrorKind::PolicyTermsRequired,
                "run it again with --yes, because clearing the cache needs an answer and this run has nowhere to ask".to_owned(),
            );
            return reporter.report(&refused);
        }
        match found {
            Some(found) => eprintln!(
                "{} objects and {} bytes will be removed from {}, and fetching them again may take hours.",
                found.objects,
                found.bytes,
                root.display()
            ),
            None => eprintln!(
                "{} will be removed, and fetching again may take hours. It was written in a format this build does not read, so what it holds cannot be counted.",
                root.display()
            ),
        }
        eprint!("Remove them? [y/N] ");
        let mut answer = String::new();
        if std::io::stdin().read_line(&mut answer).is_err()
            || !answer.trim().eq_ignore_ascii_case("y")
        {
            eprintln!("nothing was removed");
            return ExitCode::Success;
        }
    }

    match fetchloom_cache::clear(root) {
        Ok(()) => {
            if !reporter.json() {
                match found {
                    Some(found) => {
                        println!("removed {} objects from {}", found.objects, root.display());
                    }
                    None => println!("removed {}", root.display()),
                }
            }
            ExitCode::Success
        }
        Err(refused) => reporter.report(&refused),
    }
}

fn print_json<T: serde::Serialize>(body: &T) {
    match serde_json::to_string(body) {
        Ok(rendered) => println!("{rendered}"),
        Err(reason) => eprintln!("the result could not be written: {reason}"),
    }
}

/// Reports what an export or an import moved.
fn report_bundle(
    outcome: &Result<fetchloom_cache::bundle::BundleReport, Error>,
    reporter: &crate::Reporter<'_>,
) -> ExitCode {
    match outcome {
        Ok(report) => {
            if reporter.json() {
                let body = serde_json::json!({
                    "status": "bundled",
                    "objects": report.objects,
                    "bytes": report.bytes,
                    "already_held": report.already_held,
                });
                println!("{body}");
            } else {
                println!(
                    "{} objects  {} bytes  {} already held",
                    report.objects, report.bytes, report.already_held
                );
            }
            ExitCode::Success
        }
        Err(refused) => reporter.report(refused),
    }
}
