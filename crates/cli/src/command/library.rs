//! `where` and `library`: the path identity decides, and what the library holds.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::Sequence;
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::work::WorkCounter;
use serde::Serialize;

use crate::command::{Requested, get_policy, open_request};
use crate::settings::ProcessEnvironment;
use crate::surface::{CommandLine, LibraryCommand};
use crate::{Reporter, run, settings, surface};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Located {
    pub(crate) dataset: String,
    pub(crate) path: PathBuf,
    pub(crate) held: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Holding {
    pub(crate) entries: Vec<Located>,
    pub(crate) bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Removed {
    pub(crate) path: PathBuf,
    pub(crate) bytes: u64,
}

/// # Errors
/// Whatever resolving the reference gives, and `manifest.invalid` when the
/// manifest it resolved to cannot be rendered canonically.
pub fn path_for(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<(String, PathBuf), Error> {
    let work = Arc::new(WorkCounter::new());
    let limits = settings::limits_for(resolved);
    let adapters = run::adapters_for(&work, &limits);
    let environment = ProcessEnvironment;
    let policy = get_policy(transfer, parsed, resolved, &environment, observer, sequence);
    let Requested { manifest, .. } = open_request(
        reference, transfer, &adapters, resolved, &policy, &limits, &work, observer, sequence, true,
    )?;
    let selection = crate::command::get::selection_of(transfer);
    let root = run::resolve_path(&resolved.library_dir.value)?;
    let path = fetchloom_engine::library::entry_path(
        &root,
        &manifest.name,
        manifest.digest()?,
        manifest.release.as_deref(),
        &selection,
    );
    Ok((manifest.name.clone(), path))
}

#[must_use]
pub fn run_where(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let (dataset, path) = match path_for(reference, transfer, parsed, resolved, observer, sequence)
    {
        Ok(found) => found,
        Err(error) => return reporter.report(&error),
    };
    let located = Located {
        dataset,
        held: path.exists(),
        path,
    };
    if parsed.global.json {
        return crate::command::write_json(&located);
    }
    println!("{}", located.path.display());
    ExitCode::Success
}

fn bytes_under(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(here) = stack.pop() {
        let Ok(reading) = std::fs::read_dir(&here) else {
            continue;
        };
        for entry in reading.flatten() {
            let Ok(found) = entry.metadata() else {
                continue;
            };
            if found.is_dir() {
                stack.push(entry.path());
            } else {
                total += found.len();
            }
        }
    }
    total
}

fn held_by(root: &Path) -> Holding {
    let mut entries = Vec::new();
    let mut bytes = 0;
    let Ok(names) = std::fs::read_dir(root) else {
        return Holding { entries, bytes };
    };
    for named in names.flatten() {
        if !named.path().is_dir() {
            continue;
        }
        let dataset = named.file_name().to_string_lossy().into_owned();
        let Ok(versions) = std::fs::read_dir(named.path()) else {
            continue;
        };
        for version in versions.flatten() {
            let path = version.path();
            if !path.is_dir() {
                continue;
            }
            bytes += bytes_under(&path);
            entries.push(Located {
                dataset: dataset.clone(),
                path,
                held: true,
            });
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Holding { entries, bytes }
}

#[must_use]
pub fn run_library(
    command: &LibraryCommand,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let root = match run::resolve_path(&resolved.library_dir.value) {
        Ok(root) => root,
        Err(error) => return reporter.report(&error),
    };
    match command {
        LibraryCommand::Ls => {
            let holding = held_by(&root);
            if parsed.global.json {
                return crate::command::write_json(&holding);
            }
            for entry in &holding.entries {
                println!("{}  {}", entry.dataset, entry.path.display());
            }
            println!("{} entries  {} bytes", holding.entries.len(), holding.bytes);
            ExitCode::Success
        }
        LibraryCommand::Rm { target } => match remove_entry(&root, target, resolved) {
            Ok(removed) => {
                if parsed.global.json {
                    return crate::command::write_json(&removed);
                }
                println!(
                    "removed  {}  {} bytes",
                    removed.path.display(),
                    removed.bytes
                );
                ExitCode::Success
            }
            Err(error) => reporter.report(&error),
        },
    }
}

fn remove_entry(
    root: &Path,
    target: &str,
    resolved: &settings::Settings,
) -> Result<Removed, Error> {
    let path = crate::project::without_navigation(&run::resolve_path(Path::new(target))?);
    let root = crate::project::without_navigation(root);
    if !path.starts_with(&root) || path == root {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name an entry inside {}, because library rm removes what the library holds and \
                 nothing else",
                root.display()
            ),
        ));
    }
    if !path.is_dir() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        ));
    }
    let bytes = bytes_under(&path);
    std::fs::remove_dir_all(&path).map_err(|reason| {
        fetchloom_engine::error::filesystem_failure(
            fetchloom_engine::error::Surface::Destination,
            &path,
            &reason,
        )
    })?;
    forget_record(resolved, &path)?;
    Ok(Removed { path, bytes })
}

fn forget_record(resolved: &settings::Settings, destination: &Path) -> Result<(), Error> {
    let root = run::resolve_path(&resolved.cache_dir.value)?;
    let work = Arc::new(WorkCounter::new());
    let Ok(processor) =
        fetchloom_engine::pool::Processor::new(crate::command::thread_budget(resolved))
    else {
        return Ok(());
    };
    let held = crate::cache::open(
        &root,
        fetchloom_engine::durability::DurabilityTier::Normal,
        fetchloom_engine::verification::VerificationPolicy::Fingerprint,
        fetchloom_engine::seam::policy::IoMode::Auto,
        fetchloom_engine::compression::CompressionChoice::Auto,
        work,
        Arc::new(processor),
    );
    match held {
        crate::cache::Opened::Ready(held) => held.forget_receipt(destination),
        crate::cache::Opened::Degraded { .. } | crate::cache::Opened::Refused(_) => Ok(()),
    }
}
