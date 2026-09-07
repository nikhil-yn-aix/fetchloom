//! `probe` and `list`: what a source states about an object, and what a
//! container holds, without materializing either.

use std::path::Path;
use std::sync::Arc;

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::erased::Adapters;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::event::{Event, EventPayload, Sequence};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::{ArchiveFormat, Artifact};
use fetchloom_engine::outcome::ExitCode;
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::seam::archive::{Archive, ArchiveMember, MemberKind};
use fetchloom_engine::seam::observer::Observer;
use fetchloom_engine::seam::policy::Policy;
use fetchloom_engine::seam::source::{Serves, Source};
use fetchloom_engine::seam::store::Store;
use fetchloom_engine::trust::TrustClass;
use fetchloom_engine::work::WorkCounter;
use fetchloom_platform::NativePlatform;
use serde::Serialize;

use crate::command::{Opened, Requested, get_policy, open_for, open_request};
use crate::run::ranged::RangedReader;
use crate::settings::ProcessEnvironment;
use crate::surface::CommandLine;
use crate::{Reporter, run, settings, surface};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct StatedDigest {
    pub(crate) algorithm: String,
    pub(crate) value: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Probed {
    pub(crate) artifact: String,
    pub(crate) location: SafeUrl,
    pub(crate) size: Option<u64>,
    pub(crate) digests: Vec<StatedDigest>,
    pub(crate) trust: TrustClass,
    pub(crate) ranges: bool,
    pub(crate) cached: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ProbeResult {
    pub(crate) dataset: String,
    pub(crate) artifacts: Vec<Probed>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Listed {
    pub(crate) path: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
    pub(crate) size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) digest: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct ListResult {
    pub(crate) entries: Vec<Listed>,
    pub(crate) skipped: u64,
}

fn kind_of(member: &ArchiveMember) -> &'static str {
    match member.kind {
        MemberKind::File => "file",
        MemberKind::Directory => "directory",
        MemberKind::Symlink => "symlink",
        MemberKind::HardLink => "hardlink",
        MemberKind::Other => "other",
    }
}

fn location_of(artifact: &Artifact, reference: &str, base: &Path, adapters: &Adapters) -> String {
    let Some(first) = artifact.sources.first() else {
        return reference.to_owned();
    };
    if run::is_served(adapters, first) {
        return first.clone();
    }
    run::resolve_source_path(base, first)
        .to_string_lossy()
        .into_owned()
}

fn stated_digests(artifact: &Artifact) -> Vec<StatedDigest> {
    let mut stated = Vec::new();
    if let Some(claims) = artifact.digest.as_ref() {
        if let Some(content) = claims.blake3 {
            stated.push(StatedDigest {
                algorithm: "blake3".to_owned(),
                value: content.to_string(),
            });
        }
        if let Some(interop) = claims.sha256 {
            stated.push(StatedDigest {
                algorithm: "sha256".to_owned(),
                value: interop.to_string(),
            });
        }
    }
    stated
}

fn cached_digest(
    cache: Option<&fetchloom_cache::Cache<NativePlatform>>,
    location: &str,
    claimed: Option<ContentDigest>,
) -> Result<Option<ContentDigest>, Error> {
    let Some(cache) = cache else {
        return Ok(None);
    };
    if let Some(claimed) = claimed
        && cache.contains(claimed)?
    {
        return Ok(Some(claimed));
    }
    let Some(resolution) = cache.resolution(location)? else {
        return Ok(None);
    };
    if cache.contains(resolution.digest)? {
        Ok(Some(resolution.digest))
    } else {
        Ok(None)
    }
}

#[must_use]
pub fn run_probe(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let work = Arc::new(WorkCounter::new());
    let limits = settings::limits_for(resolved);
    let adapters = run::adapters_for(&work, &limits);
    let environment = ProcessEnvironment;
    let policy = get_policy(transfer, parsed, resolved, &environment, observer, sequence);

    let Requested {
        reference: named,
        source,
        manifest,
        ..
    } = match open_request(
        reference, transfer, &adapters, resolved, &policy, &limits, &work, observer, sequence, true,
    ) {
        Ok(opened) => opened,
        Err(error) => return reporter.report(&error),
    };

    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let Opened {
        held,
        scratch: _scratch,
        ..
    } = match open_for(
        resolved, transfer, &work, &here, &policy, observer, sequence, &reporter,
    ) {
        Ok(opened) => opened,
        Err(code) => return code,
    };

    let base = source
        .parent()
        .map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
    let mut answered = Vec::with_capacity(manifest.artifacts.len());
    for artifact in &manifest.artifacts {
        match probe_one(
            artifact,
            named.as_str(),
            &base,
            &adapters,
            &policy,
            held.as_deref(),
        ) {
            Ok(probed) => answered.push(probed),
            Err(error) => return reporter.report(&error),
        }
    }
    let answer = ProbeResult {
        dataset: manifest.name.clone(),
        artifacts: answered,
    };
    if parsed.global.json {
        return crate::command::write_json(&answer);
    }
    for probed in &answer.artifacts {
        println!(
            "{}  {}  {}  {}  {}  {}",
            probed.artifact,
            probed
                .size
                .map_or_else(|| "?".to_owned(), |size| size.to_string()),
            probed.trust.label(),
            if probed.ranges { "ranges" } else { "whole" },
            if probed.cached { "cached" } else { "absent" },
            probed.location,
        );
        for stated in &probed.digests {
            println!("  {} {}", stated.algorithm, stated.value);
        }
    }
    ExitCode::Success
}

fn probe_one(
    artifact: &Artifact,
    reference: &str,
    base: &Path,
    adapters: &Adapters,
    policy: &dyn Policy,
    cache: Option<&fetchloom_cache::Cache<NativePlatform>>,
) -> Result<Probed, Error> {
    let location = location_of(artifact, reference, base, adapters);
    let claimed = artifact.digest.as_ref().and_then(|claims| claims.blake3);
    let cached = cached_digest(cache, &location, claimed)?;
    let mut digests = stated_digests(artifact);
    let mut size = artifact.size;
    let mut ranges = false;

    if run::is_served(adapters, &location) {
        if policy.offline() {
            fetchloom_engine::network::forbid();
            let Some(held) = cached else {
                return Err(Error::new(
                    ErrorKind::PolicyOffline,
                    format!(
                        "run the command again without --offline to ask {} what it holds",
                        SafeUrl::new(&location)
                    ),
                ));
            };
            size = size.or_else(|| cache.and_then(|cache| cache.size_of(held)));
            if !digests.iter().any(|stated| stated.algorithm == "blake3") {
                digests.push(StatedDigest {
                    algorithm: "blake3".to_owned(),
                    value: held.to_string(),
                });
            }
        } else {
            let credential = run::credential_for(policy, &run::host_of(&location))?;
            let metadata = adapters.probe(&location, credential.as_ref())?;
            size = metadata.size.or(size);
            ranges = metadata.supports_ranges;
            if let Some(content) = metadata.content
                && !digests.iter().any(|stated| stated.algorithm == "blake3")
            {
                digests.push(StatedDigest {
                    algorithm: "blake3".to_owned(),
                    value: content.to_string(),
                });
            }
            if let Some(interop) = metadata.interop
                && !digests.iter().any(|stated| stated.algorithm == "sha256")
            {
                digests.push(StatedDigest {
                    algorithm: "sha256".to_owned(),
                    value: interop.to_string(),
                });
            }
        }
    } else {
        let path = run::local_path(&location)?;
        if let Ok(found) = std::fs::metadata(&path) {
            size = Some(found.len());
        }
        ranges = true;
    }

    let trust = if digests.is_empty() {
        TrustClass::Tofu
    } else {
        TrustClass::Verified
    };
    Ok(Probed {
        artifact: artifact.id.clone(),
        location: SafeUrl::new(&location),
        size,
        digests,
        trust,
        ranges,
        cached: cached.is_some(),
    })
}

#[must_use]
pub fn run_list(
    reference: &str,
    transfer: &surface::TransferFlags,
    parsed: &CommandLine,
    resolved: &settings::Settings,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> ExitCode {
    let reporter = Reporter::new(parsed.global.json, observer, sequence);
    let work = Arc::new(WorkCounter::new());
    let limits = settings::limits_for(resolved);
    let adapters = run::adapters_for(&work, &limits);
    let environment = ProcessEnvironment;
    let policy = get_policy(transfer, parsed, resolved, &environment, observer, sequence);

    let Requested {
        reference: named,
        source,
        manifest,
        ..
    } = match open_request(
        reference, transfer, &adapters, resolved, &policy, &limits, &work, observer, sequence, true,
    ) {
        Ok(opened) => opened,
        Err(error) => return reporter.report(&error),
    };

    let here = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let opened = match open_for(
        resolved, transfer, &work, &here, &policy, observer, sequence, &reporter,
    ) {
        Ok(opened) => opened,
        Err(code) => return code,
    };
    let tuning = crate::command::explain::tuning_for(resolved, &policy);
    let digester = std::sync::Mutex::new(fetchloom_engine::hashing::Digester::new());
    let with = run::Materialization {
        processor: opened.processor(),
        digester: &digester,
        platform: opened.platform(),
        durability: opened.durability(),
        cache: opened.cache(),
        work: &work,
        extract: true,
        verify: policy.verification(),
        tuning: &tuning,
        policy: &policy,
        adapters: &adapters,
    };

    let base = source
        .parent()
        .map_or_else(|| std::path::PathBuf::from("."), Path::to_path_buf);
    let mut entries = Vec::new();
    let mut skipped = 0;
    for artifact in &manifest.artifacts {
        match list_one(
            &with,
            artifact,
            named.as_str(),
            &base,
            &limits,
            observer,
            sequence,
        ) {
            Ok(mut found) => {
                skipped += found.skipped;
                entries.append(&mut found.entries);
            }
            Err(error) => return reporter.report(&error),
        }
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let answer = ListResult { entries, skipped };
    if parsed.global.json {
        return crate::command::write_json(&answer);
    }
    for entry in &answer.entries {
        println!(
            "{:9}  {:>12}  {}{}",
            entry.kind,
            entry
                .size
                .map_or_else(|| "?".to_owned(), |size| size.to_string()),
            entry.path,
            entry
                .digest
                .as_ref()
                .map_or_else(String::new, |digest| format!("  {digest}")),
        );
    }
    ExitCode::Success
}

fn listing_limits(limits: &Limits) -> Limits {
    Limits {
        archive_entries: limits.listing_entries,
        ..*limits
    }
}

fn list_one(
    with: &run::Materialization<'_>,
    artifact: &Artifact,
    reference: &str,
    base: &Path,
    limits: &Limits,
    observer: &dyn Observer,
    sequence: &Sequence,
) -> Result<ListResult, Error> {
    let location = location_of(artifact, reference, base, with.adapters);
    if matches!(
        Source::serves(with.adapters, &location),
        Some(Serves::Container)
    ) {
        let credential = run::credential_for(with.policy, &run::host_of(&location))?;
        let listed = with.adapters.list(&location, credential.as_ref())?;
        return Ok(ListResult {
            entries: listed
                .entries
                .into_iter()
                .map(|entry| Listed {
                    path: entry.path,
                    kind: "file".to_owned(),
                    size: entry.size,
                    digest: entry
                        .content
                        .map(|digest| digest.to_string())
                        .or_else(|| entry.interop.map(|digest| digest.to_string())),
                })
                .collect(),
            skipped: listed.skipped,
        });
    }

    let name = run::remote_name(&location);
    let bounded = listing_limits(limits);
    let declared = artifact
        .archive
        .as_ref()
        .map(|spec| spec.format)
        .or_else(|| fetchloom_archive::format_from_extension(&name));

    if !run::is_served(with.adapters, &location) {
        let path = run::local_path(&location)?;
        if path.is_dir() {
            return walked(&path);
        }
        let file = std::fs::File::open(&path).map_err(|reason| {
            fetchloom_engine::error::filesystem_failure(
                fetchloom_engine::error::Surface::Source,
                &path,
                &reason,
            )
        })?;
        return members_of(file, declared, &name, &bounded);
    }

    let claimed = artifact.digest.as_ref().and_then(|claims| claims.blake3);
    if let Some(held) = cached_digest(with.cache, &location, claimed)?
        && let Some(cache) = with.cache
    {
        return members_of(cache.read(held)?, declared, &name, &bounded);
    }

    let credential = run::credential_for(with.policy, &run::host_of(&location))?;
    let metadata = with.adapters.probe(&location, credential.as_ref())?;
    let indexed_at_the_end = declared == Some(ArchiveFormat::Zip);
    if indexed_at_the_end
        && metadata.supports_ranges
        && let Some(length) = metadata.size
    {
        let reading = RangedReader::new(
            run::adapters_for(with.work, with.policy.limits()),
            &location,
            credential,
            length,
        );
        return members_of(reading, declared, &name, &bounded);
    }

    observer.emit(&Event::new(
        sequence,
        EventPayload::Degrade {
            requested: format!("the index of {}", SafeUrl::new(&location)),
            used: format!(
                "every one of its {} bytes, kept in the cache",
                metadata
                    .size
                    .map_or_else(|| "unstated".to_owned(), |size| size.to_string())
            ),
            reason: if indexed_at_the_end {
                "the source does not serve ranges, so the index at the end of the archive cannot \
                 be read on its own"
                    .to_owned()
            } else {
                "this format states no index, so what it holds is known only by reading all of it"
                    .to_owned()
            },
        },
    ));
    let moved = run::fetch_into_cache(with, &location, observer, sequence)?;
    let Some(cache) = with.cache else {
        return Err(Error::new(
            ErrorKind::CacheCorrupt,
            "make a directory Fetchloom can write to, because listing a format with no index \
             reads all of it through a store",
        ));
    };
    members_of(cache.read(moved.digest)?, declared, &name, &bounded)
}

fn walked(path: &Path) -> Result<ListResult, Error> {
    let found = crate::materialize::walk(path)?;
    let mut entries: Vec<Listed> = found
        .entries
        .iter()
        .map(|entry| Listed {
            path: entry.path().as_str().to_owned(),
            kind: match entry {
                fetchloom_engine::tree::TreeEntry::Directory { .. } => "directory",
                fetchloom_engine::tree::TreeEntry::Symlink { .. } => "symlink",
                fetchloom_engine::tree::TreeEntry::File { .. } => "file",
            }
            .to_owned(),
            size: None,
            digest: None,
        })
        .collect();
    for file in &found.files {
        let size = std::fs::metadata(found.root.join(&file.relative))
            .map(|stated| stated.len())
            .ok();
        entries.push(Listed {
            path: file.entry.as_str().to_owned(),
            kind: "file".to_owned(),
            size,
            digest: None,
        });
    }
    Ok(ListResult {
        entries,
        skipped: 0,
    })
}

fn members_of<R: std::io::Read + std::io::Seek + 'static>(
    source: R,
    declared: Option<ArchiveFormat>,
    name: &str,
    limits: &Limits,
) -> Result<ListResult, Error> {
    let Some(format) = declared else {
        return Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a container this build reads, because {name} states no format this build \
                 can enumerate"
            ),
        ));
    };
    let mut reader = fetchloom_archive::ArchiveReader::new(source, format, name, *limits)?;
    let members = reader.members()?;
    if members.len() as u64 > limits.archive_entries {
        return Err(Error::new(
            ErrorKind::ResourceLimit,
            format!(
                "select a part of {name} instead, because it holds more than the {} entries a \
                 listing carries",
                limits.archive_entries
            ),
        ));
    }
    Ok(ListResult {
        entries: members
            .iter()
            .map(|member| Listed {
                path: member.path.clone(),
                kind: kind_of(member).to_owned(),
                size: Some(member.size),
                digest: None,
            })
            .collect(),
        skipped: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::listing_limits;
    use fetchloom_engine::limits::Limits;

    #[test]
    fn a_listing_is_bounded_by_the_listing_limit_and_not_by_the_archive_limit() {
        let limits = Limits::default();
        assert_ne!(limits.archive_entries, limits.listing_entries);
        assert_eq!(
            listing_limits(&limits).archive_entries,
            limits.listing_entries,
            "an archive enumerated for a listing was bounded by what an extraction is bounded by"
        );
    }
}
