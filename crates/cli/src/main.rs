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
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::{IoMode, Policy};
use fetchloom_engine::seam::store::Store as _;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use fetchloom_sources as _;
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

    let running = Span::start();
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
        EventPayload::RunEnd {
            duration_ms: running.elapsed_ms(),
        },
    ));
    code
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
    let remote = run::is_remote(reference);
    let environment = ProcessEnvironment;
    let policy = get_policy(transfer, parsed, resolved, &environment, observer, sequence);
    let (source, named) = match resolve_places(reference, transfer, &policy) {
        Ok(places) => places,
        Err(error) => {
            return reporter.report(&error);
        }
    };

    let (manifest, is_dataset) = match resolve_manifest(reference, &source, remote) {
        Ok(found) => found,
        Err(error) => return reporter.report(&error),
    };
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
        work,
        platform,
        durability,
        scratch: _scratch,
        held,
    } = match open_for(
        resolved,
        transfer,
        &destination,
        &policy,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(opened) => opened,
        Err(code) => return code,
    };

    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return reporter.report(&error),
    };
    let selection = selection_of(transfer);
    let pinned = match held_to_lock(&lock_path, &dataset, &manifest, &selection, transfer.locked) {
        Ok(pinned) => pinned,
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
    let (source, named) = match resolve_places(reference, transfer, &policy) {
        Ok(places) => places,
        Err(error) => return reporter.report(&error),
    };
    let dataset = run::dataset_name(reference, &source);
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
    let work = Arc::new(WorkCounter::new());
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
        work,
        platform,
        durability,
        scratch: _scratch,
        held,
    } = match open_for(
        resolved,
        transfer,
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
    let manifest = run::synthesized_manifest(&plan.dataset, artifact.source.as_str());
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
fn resolve_manifest(
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
        run::synthesized_manifest(&run::dataset_name(reference, source), reference)
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
    if remote && run::is_container(reference) {
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
    reference: &str,
    transfer: &surface::TransferFlags,
    policy: &dyn Policy,
) -> Result<(PathBuf, Option<PathBuf>), fetchloom_engine::error::Error> {
    run::allowed_offline(reference, policy)?;
    let source = if run::is_remote(reference) {
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
    work: Arc<WorkCounter>,
    platform: NativePlatform,
    held: Option<Box<fetchloom_cache::Cache<NativePlatform>>>,
    durability: DurabilityTier,
    scratch: Option<Scratch>,
}

/// Opens the pool, the counter, the platform, and the cache one run needs.
fn open_for(
    resolved: &settings::Settings,
    transfer: &surface::TransferFlags,
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
    let work = Arc::new(WorkCounter::new());
    let platform = NativePlatform::new(Arc::clone(&work));
    let mut scratch = None;
    let mut held = if transfer.no_cache {
        None
    } else {
        let root = match run::resolve_path(policy.cache_directory().unwrap_or(Path::new("."))) {
            Ok(root) => root,
            Err(error) => return Err(reporter.report(&error)),
        };
        match open_cache(&root, policy, &work, &processor, observer, sequence) {
            Ok(held) => held,
            Err(refused) => return Err(reporter.report(&refused)),
        }
    };
    if held.is_none() {
        let root = scratch_beside(destination);
        let _ = std::fs::remove_dir_all(&root);
        held = match open_cache(&root, policy, &work, &processor, observer, sequence) {
            Ok(held) => held,
            Err(refused) => return Err(reporter.report(&refused)),
        };
        scratch = Some(Scratch { root });
    }
    Ok(Opened {
        processor,
        work,
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
