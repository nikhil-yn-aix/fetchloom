//! The `init` command: walking a source and writing the manifest it implies.

use crate::command::explain::tuning_for;
use crate::command::{open_cache, thread_budget};
use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::terminal::Streams;
use crate::{Reporter, policy, run, settings, surface};
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::event::Sequence;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

#[must_use]
pub fn run_init(
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

    let Ok(processor) = Processor::new(thread_budget(resolved)) else {
        eprintln!("the processor pool could not be built");
        return ExitCode::Resource;
    };
    let processor = Arc::new(processor);
    let inferred = if run::is_served(&adapters, reference) {
        infer_over_the_network(
            reference, &adapters, &policy, &work, &processor, resolved, &limits, observer,
            sequence, &reporter,
        )
    } else {
        match run::local_path(reference) {
            Ok(path) if path.is_dir() => Ok(crate::inference::from_directory(
                &path, &processor, &limits, None,
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
    match write_manifest(output, &rendered, force) {
        Ok(()) => ExitCode::Success,
        Err(error) => reporter.report(&error),
    }
}

/// # Errors
/// `destination.modified` when the file exists and `--force` was not given,
/// and `destination.unrepresentable` when it cannot be written.
pub(crate) fn write_manifest(
    output: Option<&Path>,
    rendered: &str,
    force: bool,
) -> Result<(), fetchloom_engine::error::Error> {
    let Some(path) = output else {
        let mut stdout = std::io::stdout().lock();
        let _ = write!(stdout, "{rendered}");
        let _ = stdout.flush();
        return Ok(());
    };
    if path.exists() && !force {
        return Err(fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::DestinationModified,
            format!(
                "give --force to replace {}, because a manifest is edited after it is generated",
                path.display()
            ),
        ));
    }
    std::fs::write(path, rendered.as_bytes()).map_err(|reason| {
        fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::DestinationUnrepresentable,
            format!("make {} writable: {reason}", path.display()),
        )
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "inference needs the adapters, the policy, the counter, the pool, the settings, the limits, and both observers, each of which names one piece of what it does"
)]
pub(crate) fn infer_over_the_network(
    reference: &str,
    adapters: &Adapters,
    policy: &policy::CommandLinePolicy<'_>,
    work: &Arc<WorkCounter>,
    processor: &Arc<Processor>,
    resolved: &settings::Settings,
    limits: &fetchloom_engine::limits::Limits,
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
    let read: Vec<crate::inference::Observed> = placements
        .into_iter()
        .map(|(path, content, size)| crate::inference::Observed {
            interop: held
                .as_deref()
                .and_then(|cache| cache.recorded_interop(content).ok().flatten()),
            path,
            content,
            size,
        })
        .collect();
    Ok(crate::inference::from_observed(reference, &read, limits))
}
