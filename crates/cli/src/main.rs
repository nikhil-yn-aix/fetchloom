//! The composition root: the only place the seams are wired together.

use fetchloom_cli::{
    Reporter, cache, config, explain, locked, planning, policy, run, settings, surface, terminal,
};

use fetchloom_cache as _;
#[cfg(test)]
use fetchloom_faults as _;
use serde as _;
#[cfg(test)]
use tempfile as _;
use toml as _;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{CommandFactory, Parser};
use fetchloom_archive as _;
use fetchloom_engine::cancel;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::{IoMode, Policy};
use fetchloom_engine::seam::source::Source as _;
use fetchloom_engine::seam::store::Store as _;
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
use fetchloom_cli::surface::{Command, CommandLine, Shell};
use fetchloom_cli::terminal::Streams;

#[cfg(all(target_env = "musl", feature = "mimalloc"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> std::process::ExitCode {
    listen_for_interrupts();
    let code = execute();
    std::process::ExitCode::from(u8::try_from(code.code()).unwrap_or(1))
}

/// Arranges for an interrupt to stop the run and for a second one to end it.
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
    if resolved.aggressive.value {
        eprintln!(
            "--aggressive raises the transfers in flight for one host past the {} a run holds itself to, so a source may answer with a rate limit or refuse the run outright",
            fetchloom_engine::limits::Limits::default().connections_per_host
        );
    }
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
    offer_a_hint(&parsed, &resolved, streams, &environment);
    code
}

/// Prints at most one hint about something the user could have done
/// differently, after the result and never during a transfer.
fn offer_a_hint(
    parsed: &CommandLine,
    resolved: &settings::Settings,
    streams: Streams,
    environment: &dyn Environment,
) {
    if !terminal::hints_permitted(parsed.global.no_hints, streams, environment) {
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

/// Returns the materialization flags a command carries, or none when it takes
/// none.
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
            run_explain(resolved, discovered, key.as_deref(), parsed.global.json)
        }
        Command::Completions { shell } => write_completions(*shell),
        Command::Doctor => run_doctor(resolved, discovered, parsed.global.json),
        Command::Why { reference } => run_why(reference, parsed, resolved, observer, sequence),
        Command::Watch { stream } => match fetchloom_cli::observer::watch(stream) {
            Ok(()) => ExitCode::Success,
            Err(reason) => {
                eprintln!("could not read the event stream at {stream}: {reason}");
                ExitCode::Usage
            }
        },
        Command::Verify { target } => {
            run_verify(target, resolved, parsed.global.json, observer, sequence)
        }
        Command::Get {
            reference,
            transfer,
        } => run_get(reference, transfer, parsed, resolved, observer, sequence),
        Command::Plan {
            reference,
            transfer,
        } => run_plan(reference, transfer, parsed, resolved, observer, sequence),
        Command::Apply { plan, transfer } => {
            run_apply(plan, transfer, parsed, resolved, observer, sequence)
        }
        Command::Repair {
            reference,
            transfer,
        } => run_repair(reference, transfer, parsed, resolved, observer, sequence),
        Command::Init {
            reference,
            output,
            force,
        } => run_init(
            reference,
            output.as_deref(),
            *force,
            parsed,
            resolved,
            observer,
            sequence,
        ),
        Command::Cache { command } => run_cache(
            resolved,
            command,
            &Reporter::new(parsed.global.json, observer, sequence),
            parsed.global.yes,
        ),
    }
}

/// Refetches the damaged ranges of a cached object.
fn run_repair(
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

fn run_cache(
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

/// Returns what bounds a run's transfers, and whether measurement may move
/// them.
fn tuning_for(
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

/// Returns what this machine and this cache measured for the settings no level
/// supplies.
fn measured_for(resolved: &settings::Settings) -> explain::Measured {
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

fn run_explain(
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

/// Writes one value as JSON, which is the whole result of the command.
fn write_json(value: &impl serde::Serialize) -> ExitCode {
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

/// Returns the thread budget this machine detects.
fn detected_threads() -> u32 {
    u32::try_from(std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get))
        .unwrap_or(1)
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

fn run_verify(
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
    let Ok(processor) = Processor::new(ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    )) else {
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
    match run::verify_tree(&path, receipt.as_ref(), &emit) {
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

fn run_get(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let json = parsed.global.json;
    let work = Arc::new(WorkCounter::new());
    let limits = settings::limits_for(resolved);
    let adapters = run::adapters_for(&work, &limits);
    let environment = ProcessEnvironment;
    let policy = get_policy(transfer, parsed, resolved, &environment, observer, sequence);

    let Requested {
        reference: named_reference,
        source,
        named,
        manifest,
        is_dataset,
        remote,
    } = match open_request(
        reference, transfer, &adapters, resolved, &policy, &limits, observer, sequence,
    ) {
        Ok(opened) => opened,
        Err(error) => return reporter.report(&error),
    };
    let reference = named_reference.as_str();
    let dataset = manifest.name.clone();
    let accepted_terms = match run::assert_terms(&policy, manifest.license.as_ref()) {
        Ok(accepted) => accepted,
        Err(error) => return reporter.report(&error),
    };
    let destination = match destination_for(named, &dataset) {
        Ok(destination) => destination,
        Err(error) => return reporter.report(&error),
    };

    let Opened {
        processor,
        platform,
        durability,
        scratch: _scratch,
        held,
    } = match open_for(
        resolved,
        transfer,
        &work,
        &destination,
        &policy,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(opened) => opened,
        Err(code) => return code,
    };

    let (lock_path, selection, pinned) = match locked_selection(transfer, &dataset, &manifest) {
        Ok(held) => held,
        Err(error) => return reporter.report(&error),
    };

    let tuning = tuning_for(resolved, &policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: processor.as_ref(),
        digester: &digester,
        platform: &platform,
        durability,
        cache: held.as_deref(),
        work: &work,
        extract: !transfer.no_extract,
        verify: policy.verification(),
        tuning: &tuning,
        policy: &policy,
        adapters: &adapters,
    };
    let produced = resolve_and_publish(
        &with,
        &Request {
            reference,
            remote,
            is_dataset,
            source: &source,
            destination: &destination,
            manifest: &manifest,
            selection: &selection,
            transfer,
            pinned: pinned.as_ref(),
        },
        observer,
        sequence,
    );

    record(
        &produced,
        &Recording {
            manifest: &manifest,
            dataset: &dataset,
            lock_path: &lock_path,
            pinned: pinned.as_ref(),
            locked: transfer.locked,
            cache: held.as_deref(),
            policy: &policy,
            json,
            accepted_terms,
        },
        observer,
        sequence,
    )
}

/// Everything one materialization is asked for.
struct Request<'a> {
    /// The reference the user wrote.
    reference: &'a str,
    /// Whether it names a network location.
    remote: bool,
    /// Whether it names a manifest.
    is_dataset: bool,
    /// The path it resolved to.
    source: &'a std::path::Path,
    /// Where the run publishes.
    destination: &'a std::path::Path,
    /// The manifest the run resolves from.
    manifest: &'a fetchloom_engine::manifest::Manifest,
    /// The members the run takes.
    selection: &'a fetchloom_engine::selection::Selection,
    /// The flags that control materialization.
    transfer: &'a surface::TransferFlags,
    /// What the lock pins.
    pinned: Option<&'a fetchloom_engine::lock::LockedDataset>,
}

/// Resolves what a reference names and publishes it.
fn resolve_and_publish(
    with: &run::Materialization<'_>,
    request: &Request<'_>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> run::DatasetRun {
    if request.is_dataset {
        let base = request
            .source
            .parent()
            .map_or_else(|| PathBuf::from("."), std::path::Path::to_path_buf);
        return run::materialize_manifest(
            with,
            request.manifest,
            &base,
            request.destination,
            request.transfer.force,
            request.transfer.adopt,
            request.pinned,
            observer,
            sequence,
        );
    }
    let outcome = materialize(
        with,
        request.reference,
        request.remote,
        request.source,
        request.destination,
        request.selection,
        request.transfer,
        request.pinned,
        observer,
        sequence,
    );
    let resolved = outcome
        .as_ref()
        .map(|result| run::resolved_object(result, request.selection))
        .unwrap_or_default();
    run::DatasetRun { resolved, outcome }
}

/// Everything writing down a run needs to know.
struct Recording<'a> {
    /// The manifest the run resolved from.
    manifest: &'a fetchloom_engine::manifest::Manifest,
    /// The name the dataset is recorded under.
    dataset: &'a str,
    /// Where the lock is.
    lock_path: &'a std::path::Path,
    /// What the lock pinned before the run.
    pinned: Option<&'a fetchloom_engine::lock::LockedDataset>,
    /// Whether the run may only do what the lock states.
    locked: bool,
    /// The cache the receipt is kept in, when the run has one.
    cache: Option<&'a fetchloom_cache::Cache<NativePlatform>>,
    /// What the run is allowed to do.
    policy: &'a dyn fetchloom_engine::seam::policy::Policy,
    /// Whether the result is machine readable.
    json: bool,
    /// Whether the manifest's recorded terms were asserted, when it recorded
    /// any to assert.
    accepted_terms: Option<fetchloom_engine::license::Acceptance>,
}

/// Records what a run resolved and reports what it did.
fn record(
    produced: &run::DatasetRun,
    into: &Recording<'_>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let tree = produced.outcome.as_ref().ok().map(|result| result.tree);
    let settled = locked::resolved(into.manifest, &produced.resolved, tree).and_then(|recorded| {
        locked::settle(
            into.lock_path,
            into.dataset,
            into.pinned,
            recorded.as_ref(),
            into.locked,
            observer,
            sequence,
        )
    });
    let reporter = Reporter::new(into.json, observer, sequence);
    if let Err(error) = settled {
        return reporter.report(&error);
    }
    match &produced.outcome {
        Ok(result) => finish_get(
            result,
            into.cache,
            into.manifest,
            &produced.resolved,
            into.policy,
            into.accepted_terms,
            &reporter,
        ),
        Err(error) => reporter.report(error),
    }
}

/// Infers a manifest for a reference and writes it where it was asked for.
fn run_init(
    reference: &str,
    output: Option<&Path>,
    force: bool,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let limits = settings::limits_for(resolved);
    let work = Arc::new(WorkCounter::new());
    let adapters = run::adapters_for(&work, &limits);
    let environment = ProcessEnvironment;
    let policy = policy::CommandLinePolicy::new(
        resolved.clone(),
        &surface::TransferFlags::default(),
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
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));

    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let processor = Arc::new(processor);
    let inferred = if run::is_served(&adapters, reference) {
        infer_over_the_network(
            reference, &adapters, &policy, &work, &processor, resolved, &limits, &emit, observer,
            sequence, &reporter,
        )
    } else {
        match run::local_path(reference) {
            Ok(path) if path.is_dir() => Ok(fetchloom_cli::inference::from_directory(
                &path, &processor, &limits, &emit,
            )),
            Ok(path) => Ok(Err(fetchloom_engine::error::Error::new(
                fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
                format!(
                    "point init at a directory or a listing, because {} is one file and a manifest for it is one line",
                    path.display()
                ),
            ))),
            Err(error) => Ok(Err(error)),
        }
    };
    let inferred = match inferred {
        Ok(inferred) => inferred,
        Err(code) => return code,
    };
    let manifest = match inferred {
        Ok(manifest) => manifest,
        Err(error) => return reporter.report(&error),
    };
    let rendered = match if parsed.global.json {
        fetchloom_engine::document::canonical_json_of(&manifest)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    } else {
        fetchloom_engine::document::render_model(&manifest)
    } {
        Ok(rendered) => rendered,
        Err(error) => return reporter.report(&error),
    };
    match output {
        None => {
            let mut stdout = std::io::stdout().lock();
            let _ = write!(stdout, "{rendered}");
            let _ = stdout.flush();
            ExitCode::Success
        }
        Some(path) => {
            if path.exists() && !force {
                return reporter.report(&fetchloom_engine::error::Error::new(
                    fetchloom_engine::error::ErrorKind::DestinationModified,
                    format!(
                        "give --force to replace {}, because a manifest is edited after it is generated",
                        path.display()
                    ),
                ));
            }
            match std::fs::write(path, rendered.as_bytes()) {
                Ok(()) => ExitCode::Success,
                Err(reason) => reporter.report(&fetchloom_engine::error::Error::new(
                    fetchloom_engine::error::ErrorKind::DestinationUnrepresentable,
                    format!("make {} writable: {reason}", path.display()),
                )),
            }
        }
    }
}

/// Infers a manifest for a container over the network, reading every object's
/// bytes so that every digest it records is one it observed.
#[expect(
    clippy::too_many_arguments,
    reason = "inference needs the adapters, the policy, the counter, the pool, the settings, the limits, and both observers, each of which names one piece of what it does"
)]
fn infer_over_the_network(
    reference: &str,
    adapters: &Adapters,
    policy: &policy::CommandLinePolicy<'_>,
    work: &Arc<WorkCounter>,
    processor: &Arc<Processor>,
    resolved: &settings::Settings,
    limits: &fetchloom_engine::limits::Limits,
    emit: &dyn Fn(EventPayload),
    observer: &dyn Observer,
    sequence: &Sequence,
    reporter: &Reporter<'_>,
) -> Result<Result<fetchloom_engine::manifest::Manifest, fetchloom_engine::error::Error>, ExitCode>
{
    if !run::is_container(adapters, reference) {
        return Ok(Err(fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::ReferenceUnresolved,
            format!(
                "end {reference} with a slash so that it names a container, because a manifest for one object is one line and init walks a listing"
            ),
        )));
    }
    let root = match run::resolve_path(policy.cache_directory().unwrap_or(Path::new("."))) {
        Ok(root) => root,
        Err(error) => return Ok(Err(error)),
    };
    let held = match open_cache(&root, policy, work, processor, observer, sequence) {
        Ok(held) => held,
        Err(refused) => return Err(reporter.report(&refused)),
    };
    let platform = NativePlatform::new(Arc::clone(work));
    let tuning = tuning_for(resolved, policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: processor.as_ref(),
        digester: &digester,
        platform: &platform,
        durability: policy.durability(),
        cache: held.as_deref(),
        work,
        extract: false,
        verify: policy.verification(),
        tuning: &tuning,
        policy,
        adapters,
    };
    let placements = match run::infer_remote(&with, reference, observer, sequence) {
        Ok(placements) => placements,
        Err(error) => return Ok(Err(error)),
    };
    let read: Vec<fetchloom_cli::inference::Observed> = placements
        .into_iter()
        .map(|(path, content, size)| fetchloom_cli::inference::Observed {
            interop: held
                .as_deref()
                .and_then(|cache| cache.recorded_interop(content).ok().flatten()),
            path,
            content,
            size,
        })
        .collect();
    Ok(fetchloom_cli::inference::from_observed(
        reference, &read, limits, emit,
    ))
}

/// Reports what a run would do, moving no bytes.
fn inert_on_plan(transfer: &surface::TransferFlags) -> Option<&'static str> {
    if transfer.force {
        return Some("--force");
    }
    if transfer.adopt {
        return Some("--adopt");
    }
    None
}

fn run_plan(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    if let Some(flag) = inert_on_plan(transfer) {
        eprintln!("{flag} changes what a run does to a destination, and plan writes to none");
        return ExitCode::Usage;
    }
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let json = parsed.global.json;
    let work = Arc::new(WorkCounter::new());
    let adapters = run::adapters_for(&work, &settings::limits_for(resolved));
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
    let (source, named) = match resolve_places(&adapters, reference, transfer, &policy) {
        Ok(places) => places,
        Err(error) => return reporter.report(&error),
    };
    let dataset = run::dataset_name(&adapters, reference, &source);
    let destination = match named {
        Some(named) => named,
        None => match run::resolve_path(&PathBuf::from(".").join(&dataset)) {
            Ok(destination) => destination,
            Err(error) => return reporter.report(&error),
        },
    };
    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return reporter.report(&error),
    };
    let pinned = match locked::pinned(&lock_path, &dataset, Some(locked::Requirement::Plan)) {
        Ok(pinned) => pinned,
        Err(error) => return reporter.report(&error),
    };
    let root = match run::resolve_path(policy.cache_directory().unwrap_or(Path::new("."))) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let held = if transfer.no_cache {
        None
    } else {
        match open_cache(
            &root,
            &policy,
            &work,
            &Arc::new(processor),
            observer,
            sequence,
        ) {
            Ok(held) => held,
            Err(refused) => return reporter.report(&refused),
        }
    };
    let plan = match planning::build(
        pinned.as_ref(),
        &dataset,
        reference,
        &destination,
        held.as_deref(),
    ) {
        Ok(plan) => plan,
        Err(error) => return reporter.report(&error),
    };
    observer.emit(&Event::new(sequence, EventPayload::PlanReady));
    let written = if json {
        serde_json::to_string(&plan).map_err(|reason| reason.to_string())
    } else {
        plan.render().map_err(|reason| reason.to_string())
    };
    match written {
        Ok(body) => {
            println!("{body}");
            ExitCode::Success
        }
        Err(reason) => {
            eprintln!("the plan could not be written: {reason}");
            ExitCode::Usage
        }
    }
}

/// Executes a plan.
fn run_apply(
    plan_path: &std::path::Path,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let (plan, artifact, destination) = match read_plan(plan_path, transfer) {
        Ok(read) => read,
        Err(error) => return reporter.report(&error),
    };
    let work = Arc::new(WorkCounter::new());
    let adapters = run::adapters_for(&work, &settings::limits_for(resolved));
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
    let Opened {
        processor,
        platform,
        durability,
        scratch: _scratch,
        held,
    } = match open_for(
        resolved,
        transfer,
        &work,
        &destination,
        &policy,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(opened) => opened,
        Err(code) => return code,
    };
    let tuning = tuning_for(resolved, &policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: processor.as_ref(),
        digester: &digester,
        platform: &platform,
        durability,
        cache: held.as_deref(),
        work: &work,
        extract: !transfer.no_extract,
        verify: policy.verification(),
        tuning: &tuning,
        policy: &policy,
        adapters: &adapters,
    };
    let selection = fetchloom_engine::selection::Selection {
        include: artifact.select.clone(),
        exclude: Vec::new(),
        layout: artifact.layout,
    };
    let produced = apply_produce(
        &with,
        &policy,
        &plan,
        &artifact,
        &destination,
        &selection,
        transfer,
        observer,
        sequence,
    );
    let result = match produced {
        Ok(result) => result,
        Err(error) => return reporter.report(&error),
    };
    let manifest = run::synthesized_manifest(&adapters, &plan.dataset, artifact.source.as_str());
    let resolved = run::resolved_object(&result, &selection);
    finish_get(
        &result,
        held.as_deref(),
        &manifest,
        &resolved,
        &policy,
        None,
        &reporter,
    )
}

/// Produces the artifact a plan names, from the cache when it is already
/// held, or from its source otherwise.
#[expect(
    clippy::too_many_arguments,
    reason = "each argument names a piece of what applying a plan produces"
)]
fn apply_produce(
    with: &run::Materialization<'_>,
    policy: &dyn fetchloom_engine::seam::policy::Policy,
    plan: &fetchloom_engine::plan::Plan,
    artifact: &fetchloom_engine::plan::PlanArtifact,
    destination: &Path,
    selection: &fetchloom_engine::selection::Selection,
    transfer: &surface::TransferFlags,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<run::RunResult, fetchloom_engine::error::Error> {
    let cached = with
        .cache
        .is_some_and(|cache| cache.contains(artifact.digest).unwrap_or(false));
    if cached {
        return run::materialize_cached(
            with,
            artifact.digest,
            artifact.size,
            &plan.dataset,
            destination,
            selection,
            transfer.force,
            transfer.adopt,
            &artifact.source,
            observer,
            sequence,
        );
    }
    run::allowed_offline(artifact.source.as_str(), policy)?;
    run::materialize_remote(
        with,
        artifact.source.as_str(),
        destination,
        selection,
        transfer.force,
        transfer.adopt,
        Some(artifact.digest),
        observer,
        sequence,
    )
}

/// Returns the lock file this run reads and writes.
fn lock_path_of(
    transfer: &surface::TransferFlags,
) -> Result<PathBuf, fetchloom_engine::error::Error> {
    run::resolve_path(
        &transfer
            .lock
            .clone()
            .unwrap_or_else(|| PathBuf::from("fetchloom.lock")),
    )
}

/// Returns the manifest a reference resolves from, and whether it named one.
/// Returns the reference a run actually fetches, having walked the resolution
/// order: an explicit scheme, then a local path, then the configured sources.
///
/// # Errors
///
/// Fails with `reference.unresolved` when a name matches none of the configured
/// sources, and when none is configured.
fn resolve_reference(
    reference: &str,
    adapters: &Adapters,
    resolved: &settings::Settings,
    policy: &dyn Policy,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<String, fetchloom_engine::error::Error> {
    use fetchloom_cli::resolve;

    if resolve::has_explicit_scheme(reference) || !resolve::is_name(reference) {
        return Ok(reference.to_owned());
    }
    if std::path::Path::new(reference).exists() {
        return Ok(reference.to_owned());
    }
    let sources = &resolved.sources.value;
    let candidates = resolve::candidates(reference, sources);
    for candidate in &candidates {
        let Some((source, _)) = adapters.serving(candidate) else {
            continue;
        };
        let host = run::host_of(candidate);
        let credential = policy.credential(
            &fetchloom_engine::reference::Host::new(host),
            fetchloom_engine::credential::Necessity::Optional,
        )?;
        if source.probe(candidate, credential.as_ref()).is_ok() {
            observer.emit(&Event::new(
                sequence,
                EventPayload::ResolveAlias {
                    from: reference.to_owned(),
                    to: fetchloom_engine::redact::SafeUrl::new(candidate).to_string(),
                },
            ));
            return Ok(candidate.clone());
        }
    }
    Err(resolve::unmatched(reference, candidates.len()))
}

/// Reads the manifest a metadata document describes.
///
/// # Errors
///
/// Fails when the document cannot be fetched, or states something no manifest
/// can represent honestly.
fn manifest_from_metadata(
    reference: &str,
    adapters: &Adapters,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
) -> Result<fetchloom_engine::manifest::Manifest, fetchloom_engine::error::Error> {
    use fetchloom_engine::metadata::{Context, MetadataReader, croissant};

    let location = fetchloom_cli::resolve::metadata_location(reference)?;
    let bytes = read_document(&location, adapters, policy, limits)?;
    let name = run::remote_name(&location);
    croissant::Croissant.read(
        &bytes,
        &Context {
            base: &location,
            name: &name,
            limits,
        },
    )
}

/// Reads the bytes of one document a reference names, bounded by the manifest
/// size a run holds itself to.
fn read_document(
    location: &str,
    adapters: &Adapters,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
) -> Result<Vec<u8>, fetchloom_engine::error::Error> {
    use std::io::Read as _;

    run::allowed_offline(location, policy)?;
    let Some((source, _)) = adapters.serving(location) else {
        let path = run::local_path(location)?;
        return std::fs::read(&path).map_err(|reason| {
            fetchloom_engine::error::Error::new(
                fetchloom_engine::error::ErrorKind::ManifestInvalid,
                format!("make {} readable: {reason}", path.display()),
            )
        });
    };
    let credential = policy.credential(
        &fetchloom_engine::reference::Host::new(run::host_of(location)),
        fetchloom_engine::credential::Necessity::Optional,
    )?;
    let served = source.fetch(location, None, credential.as_ref())?;
    let mut bytes = Vec::new();
    served
        .body
        .take(limits.manifest_size)
        .read_to_end(&mut bytes)
        .map_err(|reason| {
            fetchloom_engine::error::Error::new(
                fetchloom_engine::error::ErrorKind::ManifestInvalid,
                format!("serve the document again, because it could not be read: {reason}"),
            )
        })?;
    Ok(bytes)
}

fn resolve_manifest(
    adapters: &Adapters,
    reference: &str,
    source: &std::path::Path,
    remote: bool,
) -> Result<(fetchloom_engine::manifest::Manifest, bool), fetchloom_engine::error::Error> {
    let read = if remote {
        None
    } else {
        run::manifest_at(source).transpose()?
    };
    let is_dataset = read.is_some();
    let manifest = read.unwrap_or_else(|| {
        run::synthesized_manifest(
            adapters,
            &run::dataset_name(adapters, reference, source),
            reference,
        )
    });
    Ok((manifest, is_dataset))
}

/// Returns the destination a run publishes to.
fn destination_for(
    named: Option<PathBuf>,
    dataset: &str,
) -> Result<PathBuf, fetchloom_engine::error::Error> {
    match named {
        Some(named) => Ok(named),
        None => run::resolve_path(&PathBuf::from(".").join(dataset)),
    }
}

/// Runs the materialization a reference names.
#[expect(
    clippy::too_many_arguments,
    reason = "the flags, the lock, and the two observers each name a contract behavior of their own"
)]
fn materialize(
    with: &run::Materialization<'_>,
    reference: &str,
    remote: bool,
    source: &std::path::Path,
    destination: &std::path::Path,
    selection: &fetchloom_engine::selection::Selection,
    transfer: &surface::TransferFlags,
    pinned: Option<&fetchloom_engine::lock::LockedDataset>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<run::RunResult, fetchloom_engine::error::Error> {
    if remote && run::is_container(with.adapters, reference) {
        return run::materialize_remote_container(
            with,
            reference,
            destination,
            selection,
            transfer.force,
            transfer.adopt,
            observer,
            sequence,
        );
    }
    if remote {
        return run::materialize_remote(
            with,
            reference,
            destination,
            selection,
            transfer.force,
            transfer.adopt,
            pinned
                .filter(|_| transfer.locked)
                .and_then(|entry| entry.artifacts.values().next())
                .map(|artifact| artifact.digest),
            observer,
            sequence,
        );
    }
    run::materialize_local(
        with,
        source,
        destination,
        selection,
        transfer.force,
        transfer.adopt,
        pinned
            .filter(|_| transfer.locked)
            .and_then(|entry| entry.artifacts.values().next())
            .map(|artifact| artifact.digest),
        observer,
        sequence,
    )
}

/// Returns the source a reference names and the destination it materializes to.
fn resolve_places(
    adapters: &Adapters,
    reference: &str,
    transfer: &surface::TransferFlags,
    policy: &dyn Policy,
) -> Result<(PathBuf, Option<PathBuf>), fetchloom_engine::error::Error> {
    run::allowed_offline(reference, policy)?;
    let source = if run::is_served(adapters, reference) {
        PathBuf::from(run::remote_name(reference))
    } else {
        run::local_path(reference)?
    };
    let named = match transfer.output.clone() {
        Some(requested) => Some(run::resolve_path(&requested)?),
        None => None,
    };
    Ok((source, named))
}

/// Returns what the lock pins for this run, having refused a locked run the
/// lock does not describe.
fn held_to_lock(
    lock_path: &std::path::Path,
    dataset: &str,
    manifest: &fetchloom_engine::manifest::Manifest,
    selection: &fetchloom_engine::selection::Selection,
    is_locked: bool,
) -> Result<Option<fetchloom_engine::lock::LockedDataset>, fetchloom_engine::error::Error> {
    let pinned = locked::pinned(
        lock_path,
        dataset,
        is_locked.then_some(locked::Requirement::LockedRun),
    )?;
    if is_locked && let Some(pinned) = pinned.as_ref() {
        pinned.check_request(manifest.digest()?, manifest.release.as_deref(), selection)?;
    }
    Ok(pinned)
}

/// Records what a run produced and reports it.
fn finish_get(
    result: &run::RunResult,
    cache: Option<&fetchloom_cache::Cache<NativePlatform>>,
    manifest: &fetchloom_engine::manifest::Manifest,
    artifacts: &[run::ResolvedArtifact],
    policy: &dyn fetchloom_engine::seam::policy::Policy,
    accepted_terms: Option<fetchloom_engine::license::Acceptance>,
    reporter: &Reporter<'_>,
) -> ExitCode {
    let mut result = result.clone();
    if let Some(cache) = cache {
        match run::write_receipt(
            cache,
            manifest,
            artifacts,
            &result,
            policy.verification(),
            accepted_terms,
        ) {
            Ok(trust) => result.trust = trust,
            Err(error) => return reporter.report(&error),
        }
    }
    if !policy.accepts(result.trust) {
        return reporter.report(&fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::PolicyTrustRefused,
            format!(
                "run it again with --verify never to accept it, because this run can claim only that the bytes are {}",
                result.trust.label()
            ),
        ));
    }
    let result = &result;
    if reporter.json() {
        match serde_json::to_string(result) {
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

fn selection_of(transfer: &surface::TransferFlags) -> fetchloom_engine::selection::Selection {
    fetchloom_engine::selection::Selection {
        include: transfer
            .select
            .iter()
            .map(|text| fetchloom_engine::selection::Glob::new(text.clone()))
            .collect(),
        exclude: transfer
            .exclude
            .iter()
            .map(|text| fetchloom_engine::selection::Glob::new(text.clone()))
            .collect(),
        layout: transfer
            .layout
            .map_or(fetchloom_engine::selection::Layout::Keep, |arg| arg.0),
    }
}

fn thread_budget(resolved: &settings::Settings) -> ThreadBudget {
    ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        resolved
            .threads
            .value
            .and_then(|value| usize::try_from(value.get()).ok())
            .and_then(std::num::NonZeroUsize::new),
    )
}

/// Says that a thread ceiling above what this machine detected was clamped to
/// it, because a ceiling that is silently ignored is a ceiling nobody set.
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

fn open_cache(
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

/// A cache directory that lives only for one run and is removed with it.
struct Scratch {
    root: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Returns the scratch cache directory a `--no-cache` run puts beside its
/// destination.
fn scratch_beside(destination: &Path) -> PathBuf {
    let name = destination.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    destination.with_file_name(format!(".{name}.fetchloom-scratch"))
}

/// What every materializing command opens before it moves a byte.
struct Opened {
    processor: Arc<Processor>,
    platform: NativePlatform,
    held: Option<Box<fetchloom_cache::Cache<NativePlatform>>>,
    durability: DurabilityTier,
    scratch: Option<Scratch>,
}

/// Opens the pool, the platform, and the cache one run needs.
#[expect(
    clippy::too_many_arguments,
    reason = "the settings, the flags, the counter, and the two observers each name a piece of what a run opens"
)]
fn open_for(
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

/// Reads a plan, its one artifact, and the destination the run publishes into.
///
/// # Errors
///
/// Fails when the plan cannot be read, when it names no artifact, and when the
/// destination cannot be resolved.
fn read_plan(
    plan_path: &std::path::Path,
    transfer: &surface::TransferFlags,
) -> Result<
    (
        fetchloom_engine::plan::Plan,
        fetchloom_engine::plan::PlanArtifact,
        PathBuf,
    ),
    fetchloom_engine::error::Error,
> {
    let plan = planning::read(plan_path, &fetchloom_engine::limits::Limits::default())?;
    let artifact = planning::only_artifact(&plan)?.clone();
    let destination = run::resolve_path(transfer.output.as_deref().unwrap_or(&plan.destination))?;
    Ok((plan, artifact, destination))
}

/// Builds the policy a get or an apply runs under.
fn get_policy<'a>(
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

/// Returns the lock a run writes, the members it selects, and what the lock
/// already pinned.
fn locked_selection(
    transfer: &surface::TransferFlags,
    dataset: &str,
    manifest: &fetchloom_engine::manifest::Manifest,
) -> Result<
    (
        PathBuf,
        fetchloom_engine::selection::Selection,
        Option<fetchloom_engine::lock::LockedDataset>,
    ),
    fetchloom_engine::error::Error,
> {
    let lock_path = lock_path_of(transfer)?;
    let selection = selection_of(transfer);
    let pinned = held_to_lock(&lock_path, dataset, manifest, &selection, transfer.locked)?;
    Ok((lock_path, selection, pinned))
}

/// Checks the environment and changes nothing.
fn run_doctor(
    resolved: &settings::Settings,
    discovered: &config::Discovered,
    json: bool,
) -> ExitCode {
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("{}", error.next_action());
            return ExitCode::Usage;
        }
    };
    let environment = ProcessEnvironment;
    let report = fetchloom_cli::doctor::run(
        discovered,
        &root,
        &environment,
        &policy::NativeCredentialStore,
    );
    let mut stdout = std::io::stdout().lock();
    if json {
        match serde_json::to_string(&report) {
            Ok(written) => {
                let _ = writeln!(stdout, "{written}");
            }
            Err(reason) => {
                eprintln!("the report could not be written: {reason}");
                return ExitCode::Usage;
            }
        }
    } else {
        let _ = write!(stdout, "{}", report.render());
    }
    let _ = stdout.flush();
    report.exit_code()
}

/// Explains a decision a run already made, inventing none.
fn run_why(
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
        Arc::clone(&work),
        Arc::new(processor),
    ) {
        cache::Opened::Ready(held) => Some(held),
        cache::Opened::Degraded { .. } | cache::Opened::Refused(_) => None,
    };
    let explained = match fetchloom_cli::why::explain(reference, &adapters, held.as_deref()) {
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

/// What the resolution order settled before a run touched a destination.
struct Requested {
    /// The reference actually fetched, after a name was resolved.
    reference: String,
    /// The path the reference names on this machine, when it names one.
    source: PathBuf,
    /// The destination the user named, when they named one.
    named: Option<PathBuf>,
    /// The manifest the run materializes.
    manifest: fetchloom_engine::manifest::Manifest,
    /// Whether that manifest was read rather than synthesized.
    is_dataset: bool,
    /// Whether the reference names a network location.
    remote: bool,
}

/// Walks the resolution order and reads the manifest a run materializes.
///
/// # Errors
///
/// Fails when the reference resolves to nothing, and when a metadata document
/// states something no manifest can represent.
#[expect(
    clippy::too_many_arguments,
    reason = "resolution reads the reference, the flags, the adapters, the settings, the policy, the limits, and both observers"
)]
fn open_request(
    reference: &str,
    transfer: &surface::TransferFlags,
    adapters: &Adapters,
    resolved: &settings::Settings,
    policy: &dyn Policy,
    limits: &fetchloom_engine::limits::Limits,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Requested, fetchloom_engine::error::Error> {
    let describes_metadata = fetchloom_cli::resolve::is_metadata_document(reference);
    let reference = resolve_reference(reference, adapters, resolved, policy, observer, sequence)?;
    let remote = run::is_served(adapters, &reference);

    if describes_metadata {
        let named = match transfer.output.clone() {
            Some(at) => Some(run::resolve_path(&at)?),
            None => None,
        };
        return Ok(Requested {
            manifest: manifest_from_metadata(&reference, adapters, policy, limits)?,
            reference,
            source: PathBuf::from("."),
            named,
            is_dataset: true,
            remote,
        });
    }

    let (source, named) = resolve_places(adapters, &reference, transfer, policy)?;
    let (manifest, is_dataset) = resolve_manifest(adapters, &reference, &source, remote)?;
    Ok(Requested {
        reference,
        source,
        named,
        manifest,
        is_dataset,
        remote,
    })
}
