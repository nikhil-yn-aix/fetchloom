//! The commands that work against the record a run left beside a destination:
//! `status`, `diff`, `revert` and `promote`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::merge::{Merged, Resolution};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::receipt::Receipt;
use fetchloom_engine::reconcile::{ReconcileOutcome, Reconciled, reconcile};
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::tree::{EntryPath, TreeEntry};
use fetchloom_engine::work::WorkCounter;

use crate::command::explain::tuning_for;
use crate::command::{Opened, get_policy, open_for};
use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::{Reporter, run, settings, surface};

pub(crate) struct Held<'a> {
    pub(crate) opened: Opened,
    pub(crate) work: Arc<WorkCounter>,
    pub(crate) adapters: fetchloom_engine::erased::Adapters,
    pub(crate) policy: crate::policy::CommandLinePolicy<'a>,
    pub(crate) destination: PathBuf,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the target, the flags, the settings, the environment and both observers each name a piece of what these commands open"
)]
pub(crate) fn open<'a>(
    target: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    environment: &'a ProcessEnvironment,
    observer: &'a dyn Observer,
    sequence: &'a Sequence,
    reporter: &Reporter<'_>,
) -> Result<Held<'a>, ExitCode> {
    let destination = match run::local_path(target).and_then(|path| run::resolve_path(&path)) {
        Ok(path) => path,
        Err(error) => return Err(reporter.report(&error)),
    };
    if !destination.exists() {
        return Err(reporter.report(&Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "check that {} names a path that exists",
                destination.display()
            ),
        )));
    }
    let work = Arc::new(WorkCounter::new());
    let limits = settings::limits_for(resolved);
    let adapters = run::adapters_for(&work, &limits);
    let policy = get_policy(transfer, parsed, resolved, environment, observer, sequence);
    let opened = open_for(
        resolved,
        transfer,
        &work,
        &destination,
        &policy,
        observer,
        sequence,
        reporter,
    )?;
    Ok(Held {
        opened,
        work,
        adapters,
        policy,
        destination,
    })
}

pub(crate) fn record_of(
    cache: Option<&fetchloom_cache::Cache<fetchloom_platform::NativePlatform>>,
    destination: &Path,
) -> Result<Receipt, Error> {
    let held = match cache {
        Some(cache) => cache.read_receipt(destination)?,
        None => None,
    };
    held.filter(|receipt| !receipt.entries.is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorKind::ReferenceUnresolved,
                format!(
                    "run get once into {}, because nothing here records what was written there",
                    destination.display()
                ),
            )
        })
}

#[must_use]
pub(crate) fn state_of(outcome: ReconcileOutcome) -> &'static str {
    match outcome {
        ReconcileOutcome::Unchanged => "unchanged",
        ReconcileOutcome::Modified => "modified",
        ReconcileOutcome::Restored => "deleted",
        ReconcileOutcome::Foreign => "added",
    }
}

fn describe(entry: Option<&TreeEntry>) -> String {
    match entry {
        None => "absent".to_owned(),
        Some(TreeEntry::Directory { .. }) => "directory".to_owned(),
        Some(TreeEntry::File { size, content, .. } | TreeEntry::Symlink { size, content, .. }) => {
            format!("{content} {size}")
        }
    }
}

fn found_by<'a>(entries: &'a [TreeEntry], path: &EntryPath) -> Option<&'a TreeEntry> {
    entries.iter().find(|entry| entry.path() == path)
}

pub(crate) struct Compared {
    pub(crate) record: Receipt,
    pub(crate) found: Vec<TreeEntry>,
    pub(crate) outcomes: Vec<Reconciled>,
}

pub(crate) fn compare(held: &Held<'_>, resolved: &settings::Settings) -> Result<Compared, Error> {
    let record = record_of(held.opened.held.as_deref(), &held.destination)?;
    let tuning = tuning_for(resolved, &held.policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let verify = fetchloom_engine::seam::policy::Policy::verification(&held.policy);
    let with = run::Materialization {
        processor: held.opened.processor.as_ref(),
        digester: &digester,
        platform: &held.opened.platform,
        durability: held.opened.durability,
        cache: held.opened.held.as_deref(),
        work: &held.work,
        extract: true,
        verify,
        tuning: &tuning,
        policy: &held.policy,
        adapters: &held.adapters,
    };
    let base = run::Base {
        entries: &record.entries,
        fingerprints: &record.fingerprints,
    };
    let found = run::destination_entries(&with, &held.destination, base)?;
    let found = run::with_resolved_modes(&record.entries, found);
    let outcomes = reconcile(record.resolved_entries(), &found);
    Ok(Compared {
        record,
        found,
        outcomes,
    })
}

pub(crate) fn report(compared: &Compared, json: bool, differences: bool) -> ExitCode {
    if json {
        let body: Vec<serde_json::Value> = compared
            .outcomes
            .iter()
            .filter(|found| found.outcome != ReconcileOutcome::Unchanged)
            .map(|found| {
                let recorded = found_by(compared.record.resolved_entries(), &found.path);
                let holds = found_by(&compared.found, &found.path);
                if differences {
                    serde_json::json!({
                        "path": found.path.as_str(),
                        "state": state_of(found.outcome),
                        "record": recorded,
                        "found": holds,
                    })
                } else {
                    serde_json::json!({
                        "path": found.path.as_str(),
                        "state": state_of(found.outcome),
                    })
                }
            })
            .collect();
        return crate::command::write_json(&serde_json::json!({ "entries": body }));
    }
    for found in &compared.outcomes {
        if found.outcome == ReconcileOutcome::Unchanged {
            continue;
        }
        let state = state_of(found.outcome);
        if differences {
            let recorded = describe(found_by(compared.record.resolved_entries(), &found.path));
            let holds = describe(found_by(&compared.found, &found.path));
            println!("{state:9} {}  {recorded} -> {holds}", found.path.as_str());
        } else {
            println!("{state:9} {}", found.path.as_str());
        }
    }
    ExitCode::Success
}

pub(crate) fn decisions_for_revert(compared: &Compared, named: &[String]) -> Vec<Merged> {
    let wanted: std::collections::HashSet<&str> = named.iter().map(String::as_str).collect();
    compared
        .outcomes
        .iter()
        .map(|found| {
            let takes = wanted.is_empty() || wanted.contains(found.path.as_str());
            let resolution = match found.outcome {
                ReconcileOutcome::Unchanged => Resolution::Unchanged,
                ReconcileOutcome::Modified | ReconcileOutcome::Restored if takes => {
                    Resolution::TakeUpstream
                }
                ReconcileOutcome::Foreign if takes => Resolution::StaysDeleted,
                _ => Resolution::KeepYours,
            };
            Merged {
                path: found.path.clone(),
                resolution,
            }
        })
        .collect()
}

pub(crate) fn unnamed(compared: &Compared, named: &[String]) -> Vec<String> {
    let held: std::collections::HashSet<&str> = compared
        .outcomes
        .iter()
        .map(|found| found.path.as_str())
        .collect();
    named
        .iter()
        .filter(|path| !held.contains(path.as_str()))
        .cloned()
        .collect()
}

pub(crate) fn emit_decisions(decisions: &[Merged], observer: &dyn Observer, sequence: &Sequence) {
    for decision in decisions {
        observer.emit(&Event::new(
            sequence,
            EventPayload::MergeResolutionReached {
                path: decision.path.as_str().to_owned(),
                resolution: decision.resolution,
            },
        ));
    }
}

pub fn run_status(
    target: &str,
    differences: bool,
    verify: Option<surface::VerifyChoice>,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let environment = ProcessEnvironment;
    let transfer = surface::TransferFlags {
        verify,
        ..surface::TransferFlags::default()
    };
    let held = match open(
        target,
        &transfer,
        parsed,
        resolved,
        &environment,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(held) => held,
        Err(code) => return code,
    };
    match compare(&held, resolved) {
        Ok(compared) => report(&compared, parsed.global.json, differences),
        Err(error) => reporter.report(&error),
    }
}

pub fn run_revert(
    target: &str,
    named: &[String],
    verify: Option<surface::VerifyChoice>,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let environment = ProcessEnvironment;
    let transfer = surface::TransferFlags {
        verify,
        ..surface::TransferFlags::default()
    };
    let held = match open(
        target,
        &transfer,
        parsed,
        resolved,
        &environment,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(held) => held,
        Err(code) => return code,
    };
    let compared = match compare(&held, resolved) {
        Ok(compared) => compared,
        Err(error) => return reporter.report(&error),
    };
    let absent = unnamed(&compared, named);
    if !absent.is_empty() {
        return reporter.report(&Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name an entry the record holds, because it states nothing about {}",
                absent.join(", ")
            ),
        ));
    }
    let decisions = decisions_for_revert(&compared, named);
    emit_decisions(&decisions, observer, sequence);
    let restored = decisions
        .iter()
        .filter(|decision| {
            matches!(
                decision.resolution,
                Resolution::TakeUpstream | Resolution::StaysDeleted
            )
        })
        .count() as u64;
    if restored == 0 {
        return finish_revert(&held, 0, parsed.global.json);
    }
    let tuning = tuning_for(resolved, &held.policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let verify = fetchloom_engine::seam::policy::Policy::verification(&held.policy);
    let with = run::Materialization {
        processor: held.opened.processor.as_ref(),
        digester: &digester,
        platform: &held.opened.platform,
        durability: held.opened.durability,
        cache: held.opened.held.as_deref(),
        work: &held.work,
        extract: true,
        verify,
        tuning: &tuning,
        policy: &held.policy,
        adapters: &held.adapters,
    };
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    match run::revert_to_record(
        &with,
        &held.destination,
        &compared.record,
        &compared.found,
        &decisions,
        &emit,
    ) {
        Ok(()) => {
            emit(EventPayload::PublishCommit);
            finish_revert(&held, restored, parsed.global.json)
        }
        Err(error) => reporter.report(&error),
    }
}

fn finish_revert(held: &Held<'_>, restored: u64, json: bool) -> ExitCode {
    if json {
        return crate::command::write_json(&serde_json::json!({
            "restored": restored,
            "path": held.destination,
        }));
    }
    println!("{restored} restored  {}", held.destination.display());
    ExitCode::Success
}

#[expect(
    clippy::too_many_arguments,
    reason = "the target, where the manifest and the lock are written, the overwrite flag, the settings and both observers each name a piece of what promote does"
)]
pub fn run_promote(
    target: &str,
    output: Option<&Path>,
    lock: &Path,
    force: bool,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let environment = ProcessEnvironment;
    let transfer = surface::TransferFlags::default();
    let held = match open(
        target,
        &transfer,
        parsed,
        resolved,
        &environment,
        observer,
        sequence,
        &reporter,
    ) {
        Ok(held) => held,
        Err(code) => return code,
    };
    let record = match record_of(held.opened.held.as_deref(), &held.destination) {
        Ok(record) => record,
        Err(error) => return reporter.report(&error),
    };
    let Some(cache) = held.opened.held.as_deref() else {
        return reporter.report(&Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because promoting a tree keeps its bytes in the cache",
        ));
    };
    let limits = settings::limits_for(resolved);
    let mut manifest = match crate::inference::from_directory(
        &held.destination,
        held.opened.processor.as_ref(),
        &limits,
        Some(cache),
    ) {
        Ok(manifest) => manifest,
        Err(error) => return reporter.report(&error),
    };
    manifest.name.clone_from(&record.dataset);
    manifest.derived_from = Some(fetchloom_engine::manifest::DerivedFrom {
        dataset: record.dataset.clone(),
        manifest: record.manifest,
        release: None,
        tree: record.tree,
    });
    let rendered = match fetchloom_engine::document::render_model(&manifest) {
        Ok(rendered) => rendered,
        Err(error) => return reporter.report(&error),
    };
    if let Err(error) = crate::command::init::write_manifest(output, &rendered, force) {
        return reporter.report(&error);
    }
    let pinned = match promoted_lock(&manifest) {
        Ok(pinned) => pinned,
        Err(error) => return reporter.report(&error),
    };
    let mut existing = match fetchloom_engine::lock::Lock::read(lock, &limits) {
        Ok(existing) => existing,
        Err(error) => return reporter.report(&error),
    };
    existing.datasets.insert(manifest.name.clone(), pinned);
    if let Err(error) = existing.write(lock) {
        return reporter.report(&error);
    }
    if parsed.global.json {
        return crate::command::write_json(&serde_json::json!({
            "dataset": manifest.name,
            "artifacts": manifest.artifacts.len(),
            "lock": lock,
            "derived_from": record.tree,
        }));
    }
    println!(
        "{}  {} artifacts  {}",
        manifest.name,
        manifest.artifacts.len(),
        lock.display()
    );
    ExitCode::Success
}

fn promoted_lock(
    manifest: &fetchloom_engine::manifest::Manifest,
) -> Result<fetchloom_engine::lock::LockedDataset, Error> {
    let mut pinned = std::collections::BTreeMap::new();
    for artifact in &manifest.artifacts {
        let claims = artifact.digest.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::ManifestInvalid,
                format!(
                    "promote states a digest for every artifact, and {} carries none",
                    artifact.id
                ),
            )
        })?;
        let (Some(digest), Some(interop)) = (claims.blake3, claims.sha256) else {
            return Err(Error::new(
                ErrorKind::ManifestInvalid,
                format!(
                    "promote states both digests for every artifact, and {} carries one",
                    artifact.id
                ),
            ));
        };
        pinned.insert(
            artifact.id.clone(),
            fetchloom_engine::lock::LockedArtifact {
                digest,
                interop,
                size: artifact.size.unwrap_or_default(),
                select: artifact.select.clone(),
                layout: artifact.layout,
            },
        );
    }
    Ok(fetchloom_engine::lock::LockedDataset {
        manifest: manifest.digest()?,
        release: manifest.release.clone(),
        artifacts: pinned,
        tree: None,
    })
}
