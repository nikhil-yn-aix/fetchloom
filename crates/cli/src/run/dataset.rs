//! Materializing every artifact a manifest names, and the receipt that records it.

use super::adapters::{host_of, is_served};
use super::archive::{extract_into, open_archive, recognized_format};
use super::context::{Materialization, RecordedArtifact, RunResult};
use super::local::{Settlement, settle};
use super::object::place_object;
use super::paths::{
    containing_directory, entry_path_str, executable_paths, object_name, remote_name,
    resolve_source_path, staging_beside,
};
use super::remote::transfer_object;
use crate::materialize;
use fetchloom_cache::Cache;
use fetchloom_cache::ingest::Ingested;
use fetchloom_engine::canonical;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::event::{Event, EventPayload, Sequence, Span};
use fetchloom_engine::flights::Flights;
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::outcome::RunStatus;
use fetchloom_engine::receipt::{Receipt, RecordedFingerprint};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::Selection;
use fetchloom_engine::tree::{EntryPath, Mode, TreeEntry};
use fetchloom_engine::trust::{ArtifactKey, RunId, TrustClass, Witness, classify};
use fetchloom_platform::NativePlatform;
use std::path::Path;

/// Returns the manifest a run that was given no manifest resolved from.
#[must_use]
pub fn synthesized_manifest(
    adapters: &Adapters,
    dataset: &str,
    reference: &str,
) -> fetchloom_engine::manifest::Manifest {
    let sources = if is_served(adapters, reference) {
        vec![reference.to_owned()]
    } else {
        Vec::new()
    };
    fetchloom_engine::manifest::Manifest {
        name: dataset.to_owned(),
        release: None,
        artifacts: vec![fetchloom_engine::manifest::Artifact {
            id: dataset.to_owned(),
            sources,
            size: None,
            digest: None,
            media_type: None,
            archive: None,
            select: Vec::new(),
            layout: fetchloom_engine::selection::Layout::Keep,
        }],
        license: None,
    }
}

/// Writes the receipt that records what a run materialized.
///
/// # Errors
///
/// Fails when the manifest digest cannot be taken and when the receipt cannot
/// be written.
pub fn write_receipt(
    cache: &Cache<NativePlatform>,
    manifest: &fetchloom_engine::manifest::Manifest,
    artifacts: &[ResolvedArtifact],
    result: &RunResult,
    verify: fetchloom_engine::verification::VerificationPolicy,
    accepted_terms: Option<fetchloom_engine::license::Acceptance>,
) -> Result<TrustClass, Error> {
    let manifest_digest = manifest.digest()?;
    let run = run_identity(cache);
    let mut recorded = std::collections::BTreeMap::new();
    let mut weakest = result.trust;
    for artifact in artifacts {
        let key = ArtifactKey::of(manifest_digest, &artifact.id);
        if let Some(origin) = artifact.observed.as_deref() {
            cache.record_witness(
                &key,
                Witness {
                    digest: artifact.digest,
                    machine: cache.token().machine.clone(),
                    origin: origin.to_owned(),
                    run: run.clone(),
                    observed_at: fetchloom_engine::timestamp::Timestamp::now(),
                },
            )?;
        }
        let witnesses = cache.witnesses(&key)?;
        let class = if verify == fetchloom_engine::verification::VerificationPolicy::Never {
            TrustClass::Unverified
        } else {
            classify(artifact.prior, artifact.digest, &witnesses)
        };
        weakest = weakest.max(class);
        recorded.insert(
            artifact.id.clone(),
            fetchloom_engine::receipt::ReceiptArtifact {
                digest: artifact.digest,
                source_used: artifact.source.clone(),
                source_reason: artifact.reason.clone(),
                trust: class,
            },
        );
    }
    cache.write_receipt(&Receipt {
        dataset: result.dataset.clone(),
        manifest: manifest_digest,
        artifacts: recorded,
        tree: Some(result.tree),
        executable: result.executable.clone(),
        fingerprints: fingerprints_of(cache, &result.destination),
        destination: result.destination.clone(),
        accepted_terms,
        fetchloom: env!("CARGO_PKG_VERSION").to_owned(),
        completed_at: fetchloom_engine::timestamp::Timestamp::now(),
    })?;
    Ok(weakest)
}

/// Returns what names this run.
pub(super) fn run_identity(cache: &Cache<NativePlatform>) -> RunId {
    let token = cache.token();
    RunId::new(format!(
        "{}:{}:{}",
        token.boot.as_str(),
        token.pid,
        token.start
    ))
}

/// Returns the fingerprint every file of a destination carries right now.
pub(super) fn fingerprints_of(
    cache: &Cache<NativePlatform>,
    destination: &Path,
) -> std::collections::BTreeMap<String, RecordedFingerprint> {
    let mut found = std::collections::BTreeMap::new();
    let Ok(walked) = materialize::walk(destination) else {
        return found;
    };
    for file in &walked.files {
        let full = walked.root.join(&file.relative);
        if let Ok(fingerprint) = cache.platform().fingerprint(&full) {
            found.insert(
                file.entry.as_str().to_owned(),
                RecordedFingerprint::new(fingerprint),
            );
        }
    }
    found
}

/// Returns the name a reference's dataset is recorded under.
#[must_use]
pub fn dataset_name(adapters: &Adapters, reference: &str, source: &Path) -> String {
    if is_served(adapters, reference) {
        return remote_name(reference);
    }
    source.file_name().map_or_else(
        || "dataset".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What is known about where an artifact's bytes came from and what was known
/// about them before the run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Provenance {
    /// The digest supplied before the run, when one was.
    pub prior: Option<ContentDigest>,
    /// The origin that served the bytes, present only when this run moved them.
    pub observed: Option<String>,
}

/// One artifact a manifest named, as this run resolved it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedArtifact {
    /// The name the manifest gave it.
    pub id: String,
    /// The digest its bytes hash to.
    pub digest: ContentDigest,
    /// The interop digest of the same bytes.
    pub interop: fetchloom_engine::digest::InteropDigest,
    /// The length of the object in bytes.
    pub size: u64,
    /// The source that served it, redacted as it was recorded.
    pub source: SafeUrl,
    /// The name the object is known by, which its format is read from.
    pub name: String,
    /// The members the artifact contributes.
    pub selection: Selection,
    /// The format the manifest declared, when it declared one.
    pub declared: Option<ArchiveFormat>,
    /// The digest that was supplied before the run, when one was.
    pub prior: Option<ContentDigest>,
    /// The origin that served the bytes, present only when this run transferred
    /// them in full and verified them as they arrived.
    pub observed: Option<String>,
    /// Why the source that served it was taken over the alternatives, when a
    /// run selected one.
    pub reason: Option<String>,
}

/// What a run against a manifest produced.
pub struct DatasetRun {
    /// Every artifact that resolved, in manifest order, whether or not the run
    /// went on to publish anything.
    pub resolved: Vec<ResolvedArtifact>,
    /// What the run did, or what stopped it.
    pub outcome: Result<RunResult, Error>,
}

/// Reads the manifest a local reference names, when it names one.
///
/// # Errors
///
/// Fails with `manifest.invalid` when the path carries a manifest extension and
/// does not hold a manifest.
#[must_use]
pub fn manifest_at(source: &Path) -> Option<Result<fetchloom_engine::manifest::Manifest, Error>> {
    let syntax = fetchloom_engine::document::Syntax::of_path(source)?;
    if !source.is_file() {
        return None;
    }
    let read = std::fs::read(source).map_err(|reason| {
        Error::new(
            ErrorKind::ManifestInvalid,
            format!("make {} readable: {reason}", source.display()),
        )
    });
    Some(read.and_then(|bytes| {
        fetchloom_engine::manifest::Manifest::parse(&bytes, syntax, &Limits::default())
    }))
}

/// Materializes every artifact a manifest names into one destination.
#[expect(
    clippy::too_many_arguments,
    reason = "the manifest, the lock, and the reconcile flags each name a contract behavior of their own"
)]
pub fn materialize_manifest(
    with: &Materialization<'_>,
    manifest: &fetchloom_engine::manifest::Manifest,
    base: &Path,
    destination: &Path,
    force: bool,
    adopt: bool,
    pinned: Option<&fetchloom_engine::lock::LockedDataset>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> DatasetRun {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let resolving = Span::start();
    emit(EventPayload::ResolveStart);

    let flights = Flights::new(with.tuning.ceilings, |host: &str| {
        with.tuning.controller(host, with.cache)
    });
    let hosts: Vec<String> = manifest
        .artifacts
        .iter()
        .map(|artifact| serving_host(with.adapters, artifact))
        .collect();
    let produced = flights.each(&manifest.artifacts, &hosts, &|artifact| {
        resolve_artifact(with, &flights, artifact, base, pinned, observer, sequence)
    });
    let resolved = produced.outputs;
    if let Some(error) = produced.failure {
        return DatasetRun {
            resolved,
            outcome: Err(error.with_dataset(manifest.name.clone())),
        };
    }
    for entry in with.adapters.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    emit(EventPayload::ResolveEnd {
        duration_ms: resolving.elapsed_ms(),
    });
    emit(EventPayload::PlanReady);

    let outcome = publish_dataset(
        with,
        &resolved,
        &manifest.name,
        destination,
        force,
        adopt,
        &emit,
    );
    DatasetRun { resolved, outcome }
}

/// Returns the host an artifact's first source names, and nothing for an
/// artifact no host serves.
pub(super) fn serving_host(
    adapters: &Adapters,
    artifact: &fetchloom_engine::manifest::Artifact,
) -> String {
    artifact
        .sources
        .first()
        .filter(|first| is_served(adapters, first))
        .map(|first| host_of(first))
        .unwrap_or_default()
}

/// Puts one artifact's bytes in the cache and reports what they are.
pub(super) fn resolve_artifact(
    with: &Materialization<'_>,
    flights: &Flights<'_>,
    artifact: &fetchloom_engine::manifest::Artifact,
    base: &Path,
    pinned: Option<&fetchloom_engine::lock::LockedDataset>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<ResolvedArtifact, Error> {
    let Some(first) = artifact.sources.first() else {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a source for {}, because an artifact with none resolves to nothing",
                artifact.id
            ),
        )
        .with_artifact(artifact.id.clone()));
    };
    let expected = artifact
        .digest
        .and_then(|claims| claims.blake3)
        .or_else(|| {
            pinned
                .and_then(|entry| entry.artifacts.get(&artifact.id))
                .map(|locked| locked.digest)
        });

    let selection = Selection {
        include: artifact.select.clone(),
        exclude: Vec::new(),
        layout: artifact.layout,
    };
    let name = object_name(first);
    let declared = artifact.archive.as_ref().map(|spec| spec.format);

    if let Some((source, _)) = with.adapters.serving(first) {
        let moved = transfer_object(
            with,
            source,
            flights,
            &artifact.sources,
            expected,
            observer,
            sequence,
        )?;
        return Ok(ResolvedArtifact {
            id: artifact.id.clone(),
            digest: moved.digest,
            interop: moved.interop,
            size: moved.size,
            source: moved
                .chosen
                .as_ref()
                .map_or_else(|| SafeUrl::new(first), |taken| taken.source.clone()),
            name,
            selection,
            declared,
            prior: expected,
            observed: moved.observed,
            reason: moved.chosen.map(|taken| taken.reason),
        });
    }

    let ingested = ingest_artifact(with, artifact, base, first, expected, observer, sequence)?;
    Ok(ResolvedArtifact {
        id: artifact.id.clone(),
        digest: ingested.digest,
        interop: ingested.interop,
        size: ingested.size,
        source: SafeUrl::new(&resolve_source_path(base, first).to_string_lossy()),
        name,
        selection,
        declared,
        prior: expected,
        observed: None,
        reason: None,
    })
}

/// Puts the bytes an artifact names on this machine into the cache.
pub(super) fn ingest_artifact(
    with: &Materialization<'_>,
    artifact: &fetchloom_engine::manifest::Artifact,
    base: &Path,
    first: &str,
    expected: Option<ContentDigest>,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<Ingested, Error> {
    let emit = |payload: EventPayload| observer.emit(&Event::new(sequence, payload));
    let path = resolve_source_path(base, first);
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because a run resolves an artifact through a store and neither the cache nor a scratch store beside the destination could be opened",
        )
        .with_artifact(artifact.id.clone()));
    };
    if !path.exists() {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!("check that {} names a path that exists", path.display()),
        )
        .with_artifact(artifact.id.clone()));
    }
    let ingested = cache
        .ingest(&path)
        .map_err(|reason| reason.with_artifact(artifact.id.clone()))?;
    if let Some(expected) = expected
        && expected != ingested.digest
    {
        return Err(Error::new(
            ErrorKind::IntegrityMismatch,
            format!(
                "correct the manifest or replace the bytes, because {} hashes to {} where {} was \
                 stated",
                path.display(),
                ingested.digest,
                expected
            ),
        )
        .with_artifact(artifact.id.clone()));
    }
    if ingested.was_present {
        emit(EventPayload::CacheHit {
            digest: ingested.digest,
        });
    } else {
        emit(EventPayload::CacheMiss {
            digest: ingested.digest,
        });
    }
    Ok(ingested)
}

/// Returns what the cache already holds for a reference, which is what a
/// conditional request is built from.
pub(super) fn prior_from(
    cache: &Cache<NativePlatform>,
) -> impl Fn(&str) -> Option<fetchloom_engine::transfer::Prior> + '_ {
    move |location: &str| {
        let found = cache.resolution(location).ok().flatten()?;
        Some(fetchloom_engine::transfer::Prior {
            digest: found.digest,
            validator: fetchloom_engine::seam::source::Validator {
                etag: found.etag,
                last_modified: found.last_modified,
            },
        })
    }
}

/// Records what a reference resolved to and the validator that came with it.
pub(super) fn remember(
    cache: &Cache<NativePlatform>,
    location: &str,
    transferred: &fetchloom_engine::transfer::Transferred,
) -> Result<(), Error> {
    if !transferred.validator.can_be_asked_with() {
        return Ok(());
    }
    cache.record_resolution(
        location,
        &fetchloom_cache::resolution::Resolution {
            digest: transferred.digest,
            etag: transferred.validator.etag.clone(),
            last_modified: transferred.validator.last_modified.clone(),
        },
    )
}

/// Publishes every resolved artifact into one destination.
pub(super) fn publish_dataset(
    with: &Materialization<'_>,
    resolved: &[ResolvedArtifact],
    dataset: &str,
    destination: &Path,
    force: bool,
    adopt: bool,
    emit: &dyn Fn(EventPayload),
) -> Result<RunResult, Error> {
    let bytes = resolved.iter().map(|artifact| artifact.size).sum();
    let build = || -> Result<RunResult, Error> {
        let entries = build_dataset_staging(with, resolved, destination, emit)?;
        emit(EventPayload::PublishCommit);
        Ok(RunResult {
            status: RunStatus::Materialized,
            dataset: dataset.to_owned(),
            tree: canonical::tree_digest(&entries),
            destination: destination.to_path_buf(),
            entries: entries.len() as u64,
            bytes,
            work: with.work.taken(),
            trust: provisional_trust(with, None),
            executable: executable_paths(&entries),
            artifact: None,
        })
    };
    if !destination.exists() {
        return build();
    }

    let mut expected = Vec::new();
    for artifact in resolved {
        expected.extend(dataset_entries(with, artifact, emit)?);
    }
    let expected = with_ancestor_directories(expected)?;
    settle(
        with,
        destination,
        &Settlement {
            resolved: &expected,
            force,
            adopt,
            dataset,
            artifact: None,
        },
        emit,
        &build,
        &|_| build().map(|_| ()),
    )
}

/// Returns where under the destination a plain artifact lands, which is its
/// identifier and never the way one of its sources happens to spell it.
///
/// # Errors
///
/// Fails with `archive.unsafe_path` when the identifier is not a path the
/// destination can hold, by exactly the rules an archive member passes.
pub(super) fn placement_of(artifact: &ResolvedArtifact, limits: &Limits) -> Result<String, Error> {
    fetchloom_archive::validate_member_path(artifact.id.as_bytes(), limits.nesting_depth)
}

/// Returns the entries with one directory entry for every ancestor a file
/// entry needs, because a tree records every directory it holds.
pub(super) fn with_ancestor_directories(entries: Vec<TreeEntry>) -> Result<Vec<TreeEntry>, Error> {
    let mut held: std::collections::BTreeSet<String> = entries
        .iter()
        .map(|entry| entry_path_str(entry).to_owned())
        .collect();
    let mut wanted = Vec::new();
    for entry in &entries {
        let path = entry_path_str(entry);
        let mut parts: Vec<&str> = path.split('/').collect();
        parts.pop();
        let mut prefix = String::new();
        for part in parts {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            if held.insert(prefix.clone()) {
                wanted.push(prefix.clone());
            }
        }
    }
    let mut all = entries;
    for path in wanted {
        all.push(TreeEntry::Directory {
            path: EntryPath::new(&path)
                .map_err(|reason| Error::new(ErrorKind::ReferenceUnresolved, reason.to_string()))?,
        });
    }
    Ok(all)
}

/// Returns the entries one resolved artifact contributes, writing nothing.
pub(super) fn dataset_entries(
    with: &Materialization<'_>,
    artifact: &ResolvedArtifact,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let Some(format) = recognized_format(with, artifact.digest, &artifact.name, artifact.declared)?
    else {
        let placement = placement_of(artifact, with.policy.limits())?;
        return Ok(vec![TreeEntry::File {
            path: EntryPath::new(&placement)
                .map_err(|reason| Error::new(ErrorKind::ReferenceUnresolved, reason.to_string()))?,
            size: artifact.size,
            mode: Mode::ReadWrite,
            content: artifact.digest,
        }]);
    };
    let mut reader = open_archive(with, artifact.digest, format, &artifact.name)?;
    let result = fetchloom_archive::resolve(&mut reader, &artifact.selection, Limits::default());
    for entry in reader.take_degradations() {
        emit(EventPayload::Degrade {
            requested: entry.requested,
            used: entry.used,
            reason: entry.reason,
        });
    }
    result
}

/// Builds every resolved artifact into one staging directory and publishes it.
pub(super) fn build_dataset_staging(
    with: &Materialization<'_>,
    resolved: &[ResolvedArtifact],
    destination: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let staging = staging_beside(destination);
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .map_err(|reason| filesystem_failure(Surface::Destination, &staging, &reason))?;
    }
    with.platform.create_directories(&staging)?;

    let built = fill_dataset_staging(with, resolved, &staging, emit);
    let entries = match built {
        Ok(entries) => entries,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
    };
    {
        let parent = containing_directory(destination);
        with.platform.create_directories(&parent)?;
    }
    if let Err(error) = with
        .platform
        .publish_directory(&staging, destination, with.durability)
    {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    Ok(entries)
}

pub(super) fn fill_dataset_staging(
    with: &Materialization<'_>,
    resolved: &[ResolvedArtifact],
    staging: &Path,
    emit: &dyn Fn(EventPayload),
) -> Result<Vec<TreeEntry>, Error> {
    let mut entries = Vec::new();
    for artifact in resolved {
        if let Some(format) =
            recognized_format(with, artifact.digest, &artifact.name, artifact.declared)?
        {
            entries.extend(extract_into(
                with,
                artifact.digest,
                format,
                &artifact.name,
                staging,
                &artifact.selection,
                emit,
            )?);
        } else {
            let placement = placement_of(artifact, with.policy.limits())?;
            let at = staging.join(&placement);
            if at.exists() {
                return Err(Error::new(
                    ErrorKind::ArchiveCollision,
                    format!("rename one of them, because two artifacts both land on {placement}"),
                ));
            }
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent).map_err(|reason| {
                    Error::new(
                        ErrorKind::DestinationUnrepresentable,
                        format!("make {} writable: {reason}", parent.display()),
                    )
                })?;
            }
            place_object(with, artifact.digest, &at)?;
            entries.push(TreeEntry::File {
                path: EntryPath::new(&placement).map_err(|reason| {
                    Error::new(ErrorKind::ReferenceUnresolved, reason.to_string())
                })?,
                size: artifact.size,
                mode: Mode::ReadWrite,
                content: artifact.digest,
            });
        }
    }
    with_ancestor_directories(entries)
}

/// Returns what a run against a single object resolved, in the form the lock
/// and the receipt record it.
#[must_use]
pub fn resolved_object(result: &RunResult, selection: &Selection) -> Vec<ResolvedArtifact> {
    let Some(artifact) = &result.artifact else {
        return Vec::new();
    };
    let Some(interop) = artifact.interop else {
        return Vec::new();
    };
    vec![ResolvedArtifact {
        id: artifact.id.clone(),
        digest: artifact.digest,
        interop,
        size: artifact.size,
        source: artifact.source.clone(),
        name: artifact.id.clone(),
        selection: selection.clone(),
        declared: None,
        prior: artifact.prior,
        observed: artifact.observed.clone(),
        reason: None,
    }]
}

/// Returns the trust class a run may claim before its own witness is recorded.
pub(super) fn provisional_trust(
    with: &Materialization<'_>,
    artifact: Option<&RecordedArtifact>,
) -> TrustClass {
    if with.verify == fetchloom_engine::verification::VerificationPolicy::Never {
        return TrustClass::Unverified;
    }
    match artifact {
        Some(artifact) if artifact.prior == Some(artifact.digest) => TrustClass::Verified,
        _ => TrustClass::Tofu,
    }
}
