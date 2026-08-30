//! The composition root: the only place the seams are wired together.

use fetchloom_cli::{
    cache, config, explain, locked, planning, report, run, settings, surface, terminal,
};

use fetchloom_cache as _;
#[cfg(test)]
use fetchloom_faults as _;
use serde as _;
#[cfg(test)]
use tempfile as _;
use toml as _;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, Parser};
use fetchloom_archive as _;
use fetchloom_engine::durability::DurabilityTier;
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::store::Store as _;
use fetchloom_engine::threads::ThreadBudget;
use fetchloom_engine::verification::VerificationPolicy;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use fetchloom_sources as _;
#[cfg(test)]
use flate2 as _;

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
        Command::Verify { target } => {
            run_verify(target, resolved, parsed.global.json, observer, sequence)
        }
        Command::Get {
            references,
            transfer,
        } => run_get(references, transfer, parsed, resolved, observer, sequence),
        Command::Plan {
            references,
            transfer,
        } => run_plan(references, transfer, parsed, resolved, observer, sequence),
        Command::Apply { plan, transfer } => {
            run_apply(plan, transfer, parsed, resolved, observer, sequence)
        }
        Command::Repair {
            reference,
            transfer,
        } => run_repair(reference, transfer, parsed, resolved, observer, sequence),
        Command::Cache { command } => run_cache(
            parsed,
            resolved,
            command,
            parsed.global.json,
            parsed.global.yes,
        ),
    }
}

/// Refetches the damaged ranges of a cached object.
///
/// The digest comes from the lock when one pins it and from what the cache last
/// resolved the reference to otherwise.
fn run_repair(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let json = parsed.global.json;
    if let Err(error) = run::allowed_offline(reference, resolved.offline.value) {
        return report(&error, json);
    }
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return report(&error, json),
    };
    let Ok(processor) = Processor::new(thread_budget(parsed)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let work = Arc::new(WorkCounter::new());
    let held = match cache::require(&root, Arc::clone(&work), Arc::new(processor)) {
        Ok(held) => held,
        Err(refused) => return report(&refused, json),
    };

    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return report(&error, json),
    };
    let pinned = match locked::pinned(&lock_path, &run::remote_name(reference), false) {
        Ok(found) => found.and_then(|dataset| {
            dataset
                .artifacts
                .values()
                .next()
                .map(|artifact| artifact.digest)
        }),
        Err(error) => return report(&error, json),
    };
    let digest = match fetchloom_cli::repair::digest_for(&held, reference, pinned) {
        Ok(digest) => digest,
        Err(error) => return report(&error, json),
    };

    let outcome = fetchloom_cli::repair::Repair {
        cache: &held,
        location: reference,
        work: &work,
        observer,
        sequence,
    }
    .run(digest);
    if let Err(error) = outcome.as_ref() {
        observer.emit(&Event::new(
            sequence,
            EventPayload::Failure {
                error: error.clone(),
            },
        ));
    }
    fetchloom_cli::repair::report(&outcome, json)
}

/// Returns the check a run applies to a cache hit and to a destination entry.
///
/// One policy governs both sides: whether a recorded fingerprint may stand in
/// for reading the bytes.
fn verification_of(transfer: &surface::TransferFlags) -> VerificationPolicy {
    match transfer.verify {
        Some(surface::VerifyChoice::Always) => VerificationPolicy::Always,
        Some(surface::VerifyChoice::Fingerprint) | None => VerificationPolicy::Fingerprint,
        Some(surface::VerifyChoice::Never) => VerificationPolicy::Never,
    }
}

fn run_cache(
    parsed: &CommandLine,
    resolved: &settings::Settings,
    command: &surface::CacheCommand,
    json: bool,
    yes: bool,
) -> ExitCode {
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return report(&error, json),
    };
    let Ok(processor) = Processor::new(thread_budget(parsed)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    cache::run(&root, command, Arc::new(processor), json, yes)
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

fn run_verify(
    target: &str,
    resolved: &settings::Settings,
    json: bool,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let path = match run::local_path(target).and_then(|path| run::resolve_path(&path)) {
        Ok(path) => path,
        Err(error) => return report(&error, json),
    };
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return report(&error, json),
    };
    let work = Arc::new(WorkCounter::new());
    let Ok(processor) = Processor::new(ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    )) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let held = match open_cache(
        &surface::TransferFlags::default(),
        &root,
        DurabilityTier::Normal,
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
        Some(Err(error)) => return report(&error, json),
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
                        "fetch {} again, because it now holds {tree} where the run that wrote it                          reported {recorded}",
                        path.display()
                    ),
                );
                observer.emit(&Event::new(
                    sequence,
                    EventPayload::Failure {
                        error: error.clone(),
                    },
                ));
                return report(&error, json);
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
    let remote = run::is_remote(&references[0]);
    let (source, named) = match resolve_places(&references[0], transfer, resolved) {
        Ok(places) => places,
        Err(error) => {
            observer.emit(&Event::new(
                sequence,
                EventPayload::Failure {
                    error: error.clone(),
                },
            ));
            return report(&error, json);
        }
    };

    let (manifest, is_dataset) = match resolve_manifest(&references[0], &source, remote) {
        Ok(found) => found,
        Err(error) => return report(&error, json),
    };
    let dataset = manifest.name.clone();
    let destination = match destination_for(named, &dataset) {
        Ok(destination) => destination,
        Err(error) => return report(&error, json),
    };

    let durability = durability_of(transfer);
    let Ok(processor) = Processor::new(thread_budget(parsed)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let processor = Arc::new(processor);
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return report(&error, json),
    };
    let work = Arc::new(WorkCounter::new());
    let platform = NativePlatform::new(Arc::clone(&work));
    let held = match open_cache(
        transfer, &root, durability, &work, &processor, observer, sequence,
    ) {
        Ok(held) => held,
        Err(refused) => return report(&refused, json),
    };

    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return report(&error, json),
    };
    let selection = selection_of(transfer);
    let pinned = match held_to_lock(&lock_path, &dataset, &manifest, &selection, transfer.locked) {
        Ok(pinned) => pinned,
        Err(error) => return report(&error, json),
    };

    let digester = std::cell::RefCell::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: processor.as_ref(),
        digester: &digester,
        platform: &platform,
        durability,
        cache: held.as_deref(),
        work: &work,
        extract: !transfer.no_extract,
        verify: verification_of(transfer),
    };
    let produced = resolve_and_publish(
        &with,
        &Request {
            reference: &references[0],
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
            verify: verification_of(transfer),
            json,
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
///
/// A reference that named a manifest resolves every artifact the manifest
/// names into one destination. Every other reference resolves one object, which
/// is the same shape with one artifact in it.
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
    /// What a cache hit and a destination entry are checked against.
    verify: VerificationPolicy,
    /// Whether the result is machine readable.
    json: bool,
}

/// Records what a run resolved and reports what it did.
///
/// The lock is written whatever the outcome. The receipt is written only when
/// something was materialized.
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
    if let Err(error) = settled {
        return report(&error, into.json);
    }
    match &produced.outcome {
        Ok(result) => finish_get(
            result,
            into.cache,
            into.manifest,
            &produced.resolved,
            into.verify,
            into.json,
        ),
        Err(error) => report(error, into.json),
    }
}

/// Reports what a run would do, moving no bytes.
///
/// The plan goes to stdout, in the canonical form by default and as JSON under
/// `--json`. Both parse back into the same plan.
fn run_plan(
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
    let (source, named) = match resolve_places(&references[0], transfer, resolved) {
        Ok(places) => places,
        Err(error) => return report(&error, json),
    };
    let dataset = run::dataset_name(&references[0], &source);
    let destination = match named {
        Some(named) => named,
        None => match run::resolve_path(&PathBuf::from(".").join(&dataset)) {
            Ok(destination) => destination,
            Err(error) => return report(&error, json),
        },
    };
    let lock_path = match lock_path_of(transfer) {
        Ok(path) => path,
        Err(error) => return report(&error, json),
    };
    let pinned = match locked::pinned(&lock_path, &dataset, true) {
        Ok(pinned) => pinned,
        Err(error) => return report(&error, json),
    };
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return report(&error, json),
    };
    let work = Arc::new(WorkCounter::new());
    let Ok(processor) = Processor::new(thread_budget(parsed)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let held = match open_cache(
        transfer,
        &root,
        DurabilityTier::Normal,
        &work,
        &Arc::new(processor),
        observer,
        sequence,
    ) {
        Ok(held) => held,
        Err(refused) => return report(&refused, json),
    };
    let plan = match planning::build(
        pinned.as_ref(),
        &dataset,
        &references[0],
        &destination,
        held.as_deref(),
    ) {
        Ok(plan) => plan,
        Err(error) => return report(&error, json),
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
///
/// The digests the plan records are re-resolved: an object the cache holds is
/// materialized from it, and one it does not is fetched under the plan's own
/// digest.
fn run_apply(
    plan_path: &std::path::Path,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let json = parsed.global.json;
    let plan = match planning::read(plan_path, &fetchloom_engine::limits::Limits::default()) {
        Ok(plan) => plan,
        Err(error) => return report(&error, json),
    };
    let artifact = match planning::only_artifact(&plan) {
        Ok(artifact) => artifact.clone(),
        Err(error) => return report(&error, json),
    };
    let destination =
        match run::resolve_path(transfer.output.as_deref().unwrap_or(&plan.destination)) {
            Ok(destination) => destination,
            Err(error) => return report(&error, json),
        };
    let durability = durability_of(transfer);
    let Ok(processor) = Processor::new(thread_budget(parsed)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let processor = Arc::new(processor);
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => return report(&error, json),
    };
    let work = Arc::new(WorkCounter::new());
    let platform = NativePlatform::new(Arc::clone(&work));
    let held = match open_cache(
        transfer, &root, durability, &work, &processor, observer, sequence,
    ) {
        Ok(held) => held,
        Err(refused) => return report(&refused, json),
    };
    let digester = std::cell::RefCell::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: processor.as_ref(),
        digester: &digester,
        platform: &platform,
        durability,
        cache: held.as_deref(),
        work: &work,
        extract: !transfer.no_extract,
        verify: verification_of(transfer),
    };
    let selection = fetchloom_engine::selection::Selection {
        include: artifact.select.clone(),
        exclude: Vec::new(),
        layout: artifact.layout,
    };
    let cached = held
        .as_deref()
        .is_some_and(|cache| cache.contains(artifact.digest).unwrap_or(false));
    let produced = if cached {
        run::materialize_cached(
            &with,
            artifact.digest,
            artifact.size,
            &plan.dataset,
            &destination,
            &selection,
            transfer.force,
            transfer.adopt,
            &artifact.source,
            observer,
            sequence,
        )
    } else if let Err(error) =
        run::allowed_offline(artifact.source.as_str(), resolved.offline.value)
    {
        return report(&error, json);
    } else {
        run::materialize_remote(
            &with,
            artifact.source.as_str(),
            &destination,
            &selection,
            transfer.force,
            transfer.adopt,
            Some(artifact.digest),
            observer,
            sequence,
        )
    };
    let result = match produced {
        Ok(result) => result,
        Err(error) => return report(&error, json),
    };
    let manifest = run::synthesized_manifest(&plan.dataset, artifact.source.as_str());
    let resolved = run::resolved_object(&result, &selection);
    finish_get(
        &result,
        held.as_deref(),
        &manifest,
        &resolved,
        verification_of(transfer),
        json,
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
///
/// A local path carrying a manifest extension is a manifest, as the reference
/// grammar states. Everything else resolves from a manifest synthesized to
/// describe it.
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
///
/// A destination the user named is used as it was named. One nobody named is
/// the dataset's own name under the working directory.
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
///
/// A locked run hands the transfer the digest the lock pins.
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
        observer,
        sequence,
    )
}

/// Returns the source a reference names and the destination it materializes to.
///
/// Both are resolved against the working directory once, where the user named
/// them, so no filesystem call ever receives a path it cannot open.
fn resolve_places(
    reference: &str,
    transfer: &surface::TransferFlags,
    resolved: &settings::Settings,
) -> Result<(PathBuf, Option<PathBuf>), fetchloom_engine::error::Error> {
    run::allowed_offline(reference, resolved.offline.value)?;
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
///
/// Everything compared here is compared before a byte moves.
fn held_to_lock(
    lock_path: &std::path::Path,
    dataset: &str,
    manifest: &fetchloom_engine::manifest::Manifest,
    selection: &fetchloom_engine::selection::Selection,
    is_locked: bool,
) -> Result<Option<fetchloom_engine::lock::LockedDataset>, fetchloom_engine::error::Error> {
    let pinned = locked::pinned(lock_path, dataset, is_locked)?;
    if is_locked && let Some(pinned) = pinned.as_ref() {
        pinned.check_request(manifest.digest()?, manifest.release.as_deref(), selection)?;
    }
    Ok(pinned)
}

/// Records what a run produced and reports it.
///
/// The receipt is written before the result is printed.
fn finish_get(
    result: &run::RunResult,
    cache: Option<&fetchloom_cache::Cache<NativePlatform>>,
    manifest: &fetchloom_engine::manifest::Manifest,
    artifacts: &[run::ResolvedArtifact],
    verify: VerificationPolicy,
    json: bool,
) -> ExitCode {
    if let Some(cache) = cache
        && let Err(error) = run::write_receipt(cache, manifest, artifacts, result, verify)
    {
        return report(&error, json);
    }
    if json {
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

fn durability_of(transfer: &surface::TransferFlags) -> DurabilityTier {
    match transfer.durability {
        Some(surface::DurabilityChoice::Strict) => DurabilityTier::Strict,
        Some(surface::DurabilityChoice::Normal) | None => DurabilityTier::Normal,
        Some(surface::DurabilityChoice::Fast) => DurabilityTier::Fast,
    }
}

fn thread_budget(parsed: &CommandLine) -> ThreadBudget {
    ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        parsed
            .global
            .threads
            .and_then(|value| usize::try_from(value).ok())
            .and_then(std::num::NonZeroUsize::new),
    )
}

fn open_cache(
    transfer: &surface::TransferFlags,
    root: &std::path::Path,
    durability: DurabilityTier,
    work: &Arc<WorkCounter>,
    processor: &Arc<Processor>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Option<Box<fetchloom_cache::Cache<NativePlatform>>>, Box<fetchloom_engine::error::Error>>
{
    let policy = verification_of(transfer);
    let opened = if transfer.no_cache {
        cache::Opened::Degraded {
            reason: "this run asked for no cache".to_owned(),
        }
    } else {
        cache::open(
            root,
            durability,
            policy,
            Arc::clone(work),
            Arc::clone(processor),
        )
    };
    match opened {
        cache::Opened::Ready(held) => Ok(Some(held)),
        cache::Opened::Refused(refused) => {
            observer.emit(&Event::new(
                sequence,
                EventPayload::Failure {
                    error: (*refused).clone(),
                },
            ));
            Err(refused)
        }
        cache::Opened::Degraded { reason } => {
            if !transfer.no_cache {
                cache::report_degrade(observer, sequence, root, &reason);
            }
            Ok(None)
        }
    }
}
