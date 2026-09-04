//! The composition root: the only place the seams are wired together.

use fetchloom_cli::{Reporter, cache, config, policy, run, settings, surface, terminal};

use fetchloom_cache as _;
#[cfg(test)]
use fetchloom_faults as _;
use serde as _;
#[cfg(test)]
use tempfile as _;
use toml as _;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Parser;
use fetchloom_archive as _;
use fetchloom_engine::cancel;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use fetchloom_sources as _;
use fetchloom_view as _;
#[cfg(test)]
use flate2 as _;
#[cfg(all(test, unix))]
use rustix as _;
#[cfg(all(test, windows))]
use windows_sys as _;

use fetchloom_cli::observer::{EventStream, Fanout, Renderer};
use fetchloom_cli::settings::{Environment, ProcessEnvironment};
use fetchloom_cli::surface::{Command, CommandLine};
use fetchloom_cli::terminal::Streams;

mod command;

#[cfg(all(target_env = "musl", feature = "mimalloc"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    listen_for_interrupts();
    let code = execute();
    std::process::ExitCode::from(u8::try_from(code.code()).unwrap_or(1))
}

fn listen_for_interrupts() {
    let _ = ctrlc::set_handler(|| {
        if cancel::interrupt() > 1 {
            cancel::stop();
        }
    });
}

fn execute() -> ExitCode {
    let parsed = match CommandLine::try_parse() {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = error.print();
            return if error.use_stderr() {
                ExitCode::Usage
            } else {
                ExitCode::Success
            };
        }
    };

    let environment = ProcessEnvironment;
    let streams = Streams::detect();

    let named = parsed
        .global
        .config
        .clone()
        .or_else(|| environment.get("FETCHLOOM_CONFIG").map(PathBuf::from));
    let working = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let discovered = match config::discover(&working, named.as_deref(), parsed.global.no_config) {
        Ok(discovered) => discovered,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::Usage;
        }
    };

    let transfer = transfer_flags(&parsed.command);
    let resolved = match settings::resolve_all(&parsed.global, &transfer, &discovered, &environment)
    {
        Ok(resolved) => resolved,
        Err(refused) => {
            eprintln!("{refused}");
            return ExitCode::Usage;
        }
    };
    warn_about_aggressive(&resolved);
    fetchloom_cli::style::colored(terminal::resolve_color(
        Some(resolved.color.value),
        streams,
        &environment,
    ));
    let display = terminal::resolve_display(
        resolved.display.value,
        parsed.global.quiet,
        streams,
        &environment,
    );

    let sequence = Sequence::new();
    let animate = !parsed.global.no_animation;
    let showing: Box<dyn Observer> = if display.mode == surface::DisplayMode::Live {
        Box::new(fetchloom_cli::observer::Live::new(animate))
    } else {
        Box::new(Renderer::new(display.mode, animate))
    };
    let mut sinks: Vec<Box<dyn Observer>> = vec![
        showing,
        Box::new(fetchloom_cli::logging::Log::new(resolved.log.value)),
    ];
    if let Some(target) = parsed.global.events.as_deref() {
        match EventStream::open(target) {
            Ok(stream) => sinks.push(Box::new(stream)),
            Err(error) => {
                eprintln!("could not open the event stream at {target}: {error}");
                return ExitCode::Usage;
            }
        }
    }
    let observer = Fanout::new(sinks);

    let running = Span::start();
    observer.emit(&Event::new(&sequence, EventPayload::RunStart));
    if resolved.log_clamped {
        observer.emit(&Event::new(
            &sequence,
            EventPayload::Degrade {
                requested: format!("a log level {} steps above info", parsed.global.verbose),
                used: format!("the {} level", resolved.log.value),
                reason: "debug is the highest level, so the request was clamped".to_owned(),
            },
        ));
    }
    if let (Some(requested), Some(reason)) = (display.requested, display.reason.as_ref()) {
        observer.emit(&Event::new(
            &sequence,
            EventPayload::Degrade {
                requested: format!("the {requested:?} view").to_lowercase(),
                used: format!("the {:?} view", display.mode).to_lowercase(),
                reason: reason.clone(),
            },
        ));
    }

    let code = dispatch(&parsed, &resolved, &discovered, &observer, &sequence);

    observer.emit(&Event::new(
        &sequence,
        EventPayload::RunEnd {
            duration_ms: running.elapsed_ms(),
        },
    ));
    offer_a_hint(&resolved, streams, &environment);
    code
}

fn offer_a_hint(resolved: &settings::Settings, streams: Streams, environment: &dyn Environment) {
    if !terminal::hints_permitted(!resolved.hints.value, streams, environment) {
        return;
    }
    let Some(hint) = fetchloom_cli::hint::taken().hint() else {
        return;
    };
    let Ok(root) = run::resolve_path(&resolved.cache_dir.value) else {
        return;
    };
    if fetchloom_cli::hint::already_said(&root, &hint.key) {
        return;
    }
    eprintln!("{}", hint.line);
    fetchloom_cli::hint::remember(&root, &hint.key);
}

fn transfer_flags(command: &Command) -> surface::TransferFlags {
    match command {
        Command::Get { transfer, .. }
        | Command::Plan { transfer, .. }
        | Command::Apply { transfer, .. }
        | Command::Repair { transfer, .. } => (**transfer).clone(),
        Command::Verify { .. }
        | Command::Init { .. }
        | Command::Watch { .. }
        | Command::Doctor
        | Command::Why { .. }
        | Command::Completions { .. }
        | Command::Cache { .. }
        | Command::Explain { .. } => surface::TransferFlags::default(),
    }
}

fn dispatch(
    parsed: &CommandLine,
    resolved: &settings::Settings,
    discovered: &config::Discovered,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    match &parsed.command {
        Command::Explain { key } => {
            command::explain::run_explain(resolved, discovered, key.as_deref(), parsed.global.json)
        }
        Command::Completions { shell } => command::completions::write_completions(*shell),
        Command::Doctor => command::doctor::run_doctor(resolved, discovered, parsed.global.json),
        Command::Why { reference } => {
            command::why::run_why(reference, parsed, resolved, observer, sequence)
        }
        Command::Watch { stream } => match fetchloom_cli::observer::watch(stream) {
            Ok(()) => ExitCode::Success,
            Err(reason) => {
                eprintln!("could not read the event stream at {stream}: {reason}");
                ExitCode::Usage
            }
        },
        Command::Verify { target } => {
            command::verify::run_verify(target, resolved, parsed.global.json, observer, sequence)
        }
        Command::Get {
            reference,
            transfer,
        } => command::get::run_get(reference, transfer, parsed, resolved, observer, sequence),
        Command::Plan {
            reference,
            transfer,
        } => command::plan::run_plan(reference, transfer, parsed, resolved, observer, sequence),
        Command::Apply { plan, transfer } => {
            command::plan::run_apply(plan, transfer, parsed, resolved, observer, sequence)
        }
        Command::Repair {
            reference,
            transfer,
        } => command::repair::run_repair(reference, transfer, parsed, resolved, observer, sequence),
        Command::Init {
            reference,
            output,
            force,
        } => command::init::run_init(
            reference,
            output.as_deref(),
            *force,
            parsed,
            resolved,
            observer,
            sequence,
        ),
        Command::Cache { command } => command::cache::run_cache(
            resolved,
            command,
            &Reporter::new(parsed.global.json, observer, sequence),
            parsed.global.yes,
        ),
    }
}

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
    match cache::open(
        root,
        policy.durability(),
        policy.verification(),
        policy.io(),
        Arc::clone(work),
        Arc::clone(processor),
    ) {
        cache::Opened::Ready(held) => {
            for entry in held.take_io_degradations() {
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
        cache::Opened::Refused(refused) => Err(refused),
        cache::Opened::Degraded { reason } => {
            cache::report_degrade(observer, sequence, root, &reason);
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
    reason = "resolution reads the reference, the flags, the adapters, the settings, the policy, the limits, and both observers"
)]
pub(crate) fn open_request(
    reference: &str,
    transfer: &surface::TransferFlags,
    adapters: &Adapters,
    resolved: &settings::Settings,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Requested, fetchloom_engine::error::Error> {
    run::allowed_offline(reference, policy)?;
    let describes_metadata = fetchloom_cli::resolve::is_metadata_document(reference);
    let reference = fetchloom_cli::resolve::resolve_reference(
        reference, adapters, resolved, policy, observer, sequence,
    )?;
    let remote = run::is_served(adapters, &reference);

    if describes_metadata {
        let named = match transfer.output.clone() {
            Some(at) => Some(run::resolve_path(&at)?),
            None => None,
        };
        return Ok(Requested {
            manifest: fetchloom_cli::resolve::manifest_from_metadata(
                &reference, adapters, policy, limits,
            )?,
            reference,
            source: PathBuf::from("."),
            named,
            is_dataset: true,
            remote,
        });
    }

    let (source, named) = command::get::resolve_places(adapters, &reference, transfer, policy)?;
    let (manifest, is_dataset) =
        fetchloom_cli::resolve::resolve_manifest(adapters, &reference, &source, remote)?;
    Ok(Requested {
        reference,
        source,
        named,
        manifest,
        is_dataset,
        remote,
    })
}

fn warn_about_aggressive(resolved: &settings::Settings) {
    if !resolved.aggressive.value {
        return;
    }
    eprintln!(
        "--aggressive raises the transfers in flight for one host past the {} a run holds itself to, so a source may answer with a rate limit or refuse the run outright",
        fetchloom_engine::limits::Limits::default().connections_per_host
    );
}
