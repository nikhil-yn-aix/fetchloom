//! The composition root: the only place the seams are wired together.

use clap_complete as _;
use fetchloom_archive as _;
use fetchloom_cache as _;
#[cfg(test)]
use fetchloom_faults as _;
use fetchloom_platform as _;
use fetchloom_sources as _;
use fetchloom_view as _;
#[cfg(test)]
use flate2 as _;
#[cfg(all(test, unix))]
use rustix as _;
use serde as _;
use serde_json as _;
#[cfg(test)]
use tempfile as _;
use toml as _;
#[cfg(all(test, windows))]
use windows_sys as _;

use std::path::PathBuf;

use clap::Parser;
use fetchloom_engine::cancel;
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::seam::observer::Observer;

use fetchloom_cli::observer::{EventStream, Fanout, Renderer};
use fetchloom_cli::settings::{Environment, ProcessEnvironment};
use fetchloom_cli::surface::{Command, CommandLine};
use fetchloom_cli::terminal::Streams;
use fetchloom_cli::{Reporter, command, config, run, settings, surface, terminal};

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
        | Command::Status { .. }
        | Command::Diff { .. }
        | Command::Revert { .. }
        | Command::Promote { .. }
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
        Command::Status { target, verify } => command::tracked::run_status(
            target, false, *verify, parsed, resolved, observer, sequence,
        ),
        Command::Diff { target, verify } => command::tracked::run_status(
            target, true, *verify, parsed, resolved, observer, sequence,
        ),
        Command::Revert {
            target,
            entries,
            verify,
        } => command::tracked::run_revert(
            target, entries, *verify, parsed, resolved, observer, sequence,
        ),
        Command::Promote {
            target,
            output,
            force,
            lock,
        } => command::tracked::run_promote(
            target,
            output.as_deref(),
            lock,
            *force,
            parsed,
            resolved,
            observer,
            sequence,
        ),
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

fn warn_about_aggressive(resolved: &settings::Settings) {
    if !resolved.aggressive.value {
        return;
    }
    eprintln!(
        "--aggressive raises the transfers in flight for one host past the {} a run holds itself to, so a source may answer with a rate limit or refuse the run outright",
        fetchloom_engine::limits::Limits::default().connections_per_host
    );
}
