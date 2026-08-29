//! The composition root: the only place the seams are wired together.

use fetchloom_cli::{config, explain, run, settings, surface, terminal};

#[cfg(test)]
use fetchloom_faults as _;
use serde as _;
#[cfg(test)]
use tempfile as _;
use toml as _;

use std::io::Write;
use std::path::PathBuf;

use clap::{CommandFactory, Parser};
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_platform::NativePlatform;

use fetchloom_cli::observer::{EventStream, Fanout, Renderer};
use fetchloom_cli::settings::{Environment, ProcessEnvironment};
use fetchloom_cli::surface::{Command, CommandLine, Shell};
use fetchloom_cli::terminal::Streams;

#[cfg(all(target_env = "musl", feature = "mimalloc"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    let code = execute();
    std::process::ExitCode::from(u8::try_from(code.code()).unwrap_or(1))
}

fn execute() -> ExitCode {
    let parsed = match CommandLine::try_parse() {
        Ok(parsed) => parsed,
        Err(error) => {
            let _ = error.print();
            return ExitCode::Usage;
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

    let resolved = settings::resolve_all(&parsed.global, &discovered, &environment);
    let display = terminal::resolve_display(
        resolved.display.value,
        parsed.global.quiet,
        streams,
        &environment,
    );

    let sequence = Sequence::new();
    let mut sinks: Vec<Box<dyn Observer>> = vec![Box::new(Renderer::new(
        display.mode,
        !parsed.global.no_animation,
    ))];
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

    observer.emit(&Event::new(&sequence, EventPayload::RunStart));
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
        EventPayload::RunEnd { duration_ms: 0 },
    ));
    code
}

fn dispatch(
    parsed: &CommandLine,
    resolved: &settings::Settings,
    discovered: &config::Discovered,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    match &parsed.command {
        Command::Explain { key } => run_explain(resolved, discovered, key.as_deref()),
        Command::Completions { shell } => write_completions(*shell),
        Command::Verify { target } => run_verify(target, parsed.global.json),
        Command::Get {
            references,
            transfer,
        } => run_get(references, transfer, parsed, resolved, observer, sequence),
    }
}

fn run_explain(
    resolved: &settings::Settings,
    discovered: &config::Discovered,
    key: Option<&str>,
) -> ExitCode {
    let rows = explain::rows(resolved);
    if let Some(key) = key {
        let Some(row) = rows.iter().find(|row| row.key == key) else {
            eprintln!("{key} is not a setting this build has");
            return ExitCode::Usage;
        };
        println!("{} = {} ({})", row.key, row.value, row.origin);
        return ExitCode::Success;
    }
    for line in explain::file_lines(discovered) {
        println!("{line}");
    }
    for row in rows {
        println!("{} = {} ({})", row.key, row.value, row.origin);
    }
    ExitCode::Success
}

fn write_completions(shell: Shell) -> ExitCode {
    let generator = match shell {
        Shell::Bash => clap_complete::aot::Shell::Bash,
        Shell::Elvish => clap_complete::aot::Shell::Elvish,
        Shell::Fish => clap_complete::aot::Shell::Fish,
        Shell::Powershell => clap_complete::aot::Shell::PowerShell,
        Shell::Zsh => clap_complete::aot::Shell::Zsh,
    };
    let mut command = CommandLine::command();
    let mut stdout = std::io::stdout();
    clap_complete::aot::generate(generator, &mut command, "fetchloom", &mut stdout);
    let _ = stdout.flush();
    ExitCode::Success
}

fn run_verify(target: &str, json: bool) -> ExitCode {
    let path = match run::local_path(target) {
        Ok(path) => path,
        Err(error) => return report(&error, json),
    };
    match run::verify_tree(&path) {
        Ok((tree, entries)) => {
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
        Err(error) => report(&error, json),
    }
}

fn run_get(
    references: &[String],
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let json = parsed.global.json;
    if references.len() != 1 {
        eprintln!("this build takes exactly one reference at a time");
        return ExitCode::Usage;
    }
    if let Err(error) = run::allowed_offline(&references[0], resolved.offline.value) {
        observer.emit(&Event::new(
            sequence,
            EventPayload::Failure {
                error: error.clone(),
            },
        ));
        return report(&error, json);
    }
    let source = match run::local_path(&references[0]) {
        Ok(source) => source,
        Err(error) => return report(&error, json),
    };
    let destination = transfer
        .output
        .clone()
        .unwrap_or_else(|| run::default_destination(&source));

    let durability = match transfer.durability {
        Some(surface::DurabilityChoice::Strict) => DurabilityTier::Strict,
        Some(surface::DurabilityChoice::Normal) | None => DurabilityTier::Normal,
        Some(surface::DurabilityChoice::Fast) => DurabilityTier::Fast,
    };

    let budget = ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        parsed
            .global
            .threads
            .and_then(|value| usize::try_from(value).ok())
            .and_then(std::num::NonZeroUsize::new),
    );
    let Ok(processor) = Processor::new(budget) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };

    let platform = NativePlatform::new();

    match run::materialize_local(
        &processor,
        &platform,
        durability,
        &source,
        &destination,
        observer,
        sequence,
    ) {
        Ok(result) => {
            if json {
                match serde_json::to_string(&result) {
                    Ok(body) => println!("{body}"),
                    Err(error) => {
                        eprintln!("the result could not be written: {error}");
                        return ExitCode::Usage;
                    }
                }
            } else {
                println!(
                    "{}  {} entries  {}",
                    result.tree,
                    result.entries,
                    result.destination.display()
                );
            }
            ExitCode::Success
        }
        Err(error) => report(&error, json),
    }
}

fn report(error: &fetchloom_engine::error::Error, json: bool) -> ExitCode {
    if json {
        match serde_json::to_string(error) {
            Ok(body) => println!("{body}"),
            Err(_) => eprintln!("{error}"),
        }
    } else {
        eprintln!("{error}");
        eprintln!("next: {}", error.next_action());
    }
    ExitCode::from(error.layer())
}
