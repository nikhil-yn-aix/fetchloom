//! One module per command this build offers, and the plumbing every command opens.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform as _;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;

use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::terminal::Streams;
use crate::{Reporter, policy, run, settings, surface};

pub mod cache;
pub mod completions;
pub mod doctor;
pub mod explain;
pub mod get;
pub mod init;
pub mod inspect;
pub mod library;
pub mod plan;
pub mod repair;
pub mod tracked;
pub mod verify;
pub mod why;

pub(crate) fn write_json(value: &impl serde::Serialize) -> ExitCode {
    match serde_json::to_string(value) {
        Ok(rendered) => {
            println!("{rendered}");
            ExitCode::Success
        }
        Err(reason) => {
            eprintln!("the result could not be written: {reason}");
            ExitCode::Usage
        }
    }
}

pub(crate) fn thread_budget(resolved: &settings::Settings) -> ThreadBudget {
    ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        resolved
            .threads
            .value
            .and_then(|value| usize::try_from(value.get()).ok())
            .and_then(std::num::NonZeroUsize::new),
    )
}

fn report_clamp(
    budget: ThreadBudget,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) {
    if budget.origin() != fetchloom_engine::threads::BudgetOrigin::Clamped {
        return;
    }
    let requested = resolved
        .threads
        .value
        .map_or_else(String::new, |value| value.to_string());
    observer.emit(&Event::new(
        sequence,
        EventPayload::Degrade {
            requested: format!("{requested} threads for processor work"),
            used: format!("{} threads", budget.threads()),
            reason: format!(
                "the {} threads asked for from the {} are more than the {} this machine detected, and a ceiling above the budget is clamped to it",
                requested,
                resolved.threads.origin,
                budget.detected()
            ),
        },
    ));
}

pub(crate) fn open_cache(
    root: &std::path::Path,
    policy: &dyn fetchloom_engine::seam::policy::Policy,
    work: &Arc<WorkCounter>,
    processor: &Arc<Processor>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Option<Box<fetchloom_cache::Cache<NativePlatform>>>, Box<fetchloom_engine::error::Error>>
{
    match crate::cache::open(
        root,
        policy.durability(),
        policy.verification(),
        policy.io(),
        policy.compression(),
        Arc::clone(work),
        Arc::clone(processor),
    ) {
        crate::cache::Opened::Ready(held) => {
            for entry in held.take_degradations() {
                observer.emit(&Event::new(
                    sequence,
                    EventPayload::Degrade {
                        requested: entry.requested,
                        used: entry.used,
                        reason: entry.reason,
                    },
                ));
            }
            Ok(Some(held))
        }
        crate::cache::Opened::Refused(refused) => Err(refused),
        crate::cache::Opened::Degraded { reason } => {
            crate::cache::report_degrade(observer, sequence, root, &reason);
            Ok(None)
        }
    }
}

struct Scratch {
    root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch_beside(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!(".{name}.fetchloom-scratch"))
}

pub(crate) struct Opened {
    processor: Arc<Processor>,
    platform: NativePlatform,
    held: Option<Box<fetchloom_cache::Cache<NativePlatform>>>,
    durability: DurabilityTier,
    scratch: Option<Scratch>,
}

impl Opened {
    pub(crate) fn processor(&self) -> &Processor {
        self.processor.as_ref()
    }

    pub(crate) fn platform(&self) -> &NativePlatform {
        &self.platform
    }

    pub(crate) fn durability(&self) -> DurabilityTier {
        self.durability
    }

    pub(crate) fn cache(&self) -> Option<&fetchloom_cache::Cache<NativePlatform>> {
        self.held.as_deref()
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the settings, the flags, the counter, and the two observers each name a piece of what a run opens"
)]
pub(crate) fn open_for(
    resolved: &settings::Settings,
    transfer: &surface::TransferFlags,
    work: &Arc<WorkCounter>,
    destination: &Path,
    policy: &dyn fetchloom_engine::seam::policy::Policy,
    observer: &dyn Observer,
    sequence: &Sequence,
    reporter: &Reporter<'_>,
) -> Result<Opened, ExitCode> {
    let durability = policy.durability();
    let budget = thread_budget(resolved);
    report_clamp(budget, resolved, observer, sequence);
    let Ok(processor) = Processor::new(budget) else {
        eprintln!("the processor pool could not be built");
        return Err(ExitCode::Resource);
    };
    let processor = Arc::new(processor);
    let platform = NativePlatform::new(Arc::clone(work));
    let mut scratch = None;
    let mut held = if transfer.no_cache {
        None
    } else {
        let root = match run::resolve_path(policy.cache_directory().unwrap_or(Path::new("."))) {
            Ok(root) => root,
            Err(error) => return Err(reporter.report(&error)),
        };
        match open_cache(&root, policy, work, &processor, observer, sequence) {
            Ok(held) => held,
            Err(refused) => return Err(reporter.report(&refused)),
        }
    };
    if held.is_none() {
        let root = scratch_beside(destination);
        let _ = std::fs::remove_dir_all(&root);
        held = match open_cache(&root, policy, work, &processor, observer, sequence) {
            Ok(held) => held,
            Err(refused) => return Err(reporter.report(&refused)),
        };
        scratch = Some(Scratch { root });
    }
    Ok(Opened {
        processor,
        platform,
        held,
        durability,
        scratch,
    })
}

pub(crate) fn get_policy<'a>(
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    environment: &'a ProcessEnvironment,
    observer: &'a dyn Observer,
    sequence: &'a Sequence,
) -> policy::CommandLinePolicy<'a> {
    policy::CommandLinePolicy::new(
        resolved.clone(),
        transfer,
        Streams::detect(),
        parsed.global.yes,
        environment,
        &policy::NativeCredentialStore,
        observer,
        sequence,
        &policy::StdinPrompter,
    )
}

pub(crate) struct Requested {
    reference: String,
    source: PathBuf,
    named: Option<PathBuf>,
    manifest: fetchloom_engine::manifest::Manifest,
    is_dataset: bool,
    remote: bool,
}

#[expect(
    clippy::too_many_arguments,
    reason = "resolution reads the reference, the flags, the adapters, the settings, the policy, the limits, the work a routed request counts against, and both observers"
)]
pub(crate) fn open_request(
    reference: &str,
    transfer: &surface::TransferFlags,
    adapters: &Adapters,
    resolved: &settings::Settings,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
    work: &std::sync::Arc<fetchloom_engine::work::WorkCounter>,
    observer: &dyn Observer,
    sequence: &Sequence,
    answerable_offline: bool,
) -> Result<Requested, fetchloom_engine::error::Error> {
    if answerable_offline {
        run::forbid_when_offline(policy);
    } else {
        run::allowed_offline(reference, policy)?;
    }
    let describes_metadata = crate::resolve::is_metadata_document(reference);
    let reference = crate::resolve::resolve_reference(
        reference, adapters, resolved, policy, work, observer, sequence,
    )?;
    let remote = run::is_served(adapters, &reference);

    if describes_metadata {
        let named = match transfer.output.clone() {
            Some(at) => Some(run::resolve_path(&at)?),
            None => None,
        };
        return Ok(Requested {
            manifest: crate::resolve::manifest_from_metadata(&reference, adapters, policy, limits)?,
            reference,
            source: PathBuf::from("."),
            named,
            is_dataset: true,
            remote,
        });
    }

    let (source, named) = get::resolve_places(adapters, &reference, transfer)?;
    let (manifest, is_dataset) =
        crate::resolve::resolve_manifest(adapters, &reference, &source, remote)?;
    Ok(Requested {
        reference,
        source,
        named,
        manifest,
        is_dataset,
        remote,
    })
}

/// Every degradation the cache and the platform recorded while the run was
/// publishing, drained once at the end rather than at each of the places that
/// publish, so no path through a run can record one and never say it.
pub(crate) fn report_cache_degradations(
    held: Option<&fetchloom_cache::Cache<NativePlatform>>,
    platform: Option<&NativePlatform>,
    observer: &dyn Observer,
    sequence: &Sequence,
) {
    let recorded = held
        .map(fetchloom_cache::Cache::take_degradations)
        .unwrap_or_default()
        .into_iter()
        .chain(
            held.map(fetchloom_cache::Cache::take_platform_degradations)
                .unwrap_or_default(),
        )
        .chain(
            platform
                .map(fetchloom_platform::NativePlatform::take_degradations)
                .unwrap_or_default(),
        );
    for entry in recorded {
        observer.emit(&Event::new(
            sequence,
            EventPayload::Degrade {
                requested: entry.requested,
                used: entry.used,
                reason: entry.reason,
            },
        ));
    }
}
