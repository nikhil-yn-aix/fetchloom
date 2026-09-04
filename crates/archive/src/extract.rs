//! Bounded extraction of an archive into an empty staging directory.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fetchloom_engine::capability::VolumeCapabilities;
use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::hashing::hash_bytes;
use fetchloom_engine::seam::archive::{Archive, ArchiveMember, MemberKind};
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::selection::{Applied, AppliedMember, Candidate, Selection};
use fetchloom_engine::tree::{EntryPath, TreeEntry};
use fetchloom_engine::work::WorkCounter;

use crate::bomb::BombGuard;
pub(crate) use fetchloom_engine::limits::STREAM_BUFFER_BYTES as BUFFER_LEN;

struct Created {
    path: PathBuf,
    member: String,
}

pub fn extract<A, P>(
    archive: &mut A,
    selection: &Selection,
    staging: &Path,
    guard: BombGuard,
    platform: &P,
    work: &WorkCounter,
) -> Result<Vec<TreeEntry>, Error>
where
    A: Archive,
    P: Platform,
{
    let mut created: Vec<Created> = Vec::new();
    let result = run(
        archive,
        selection,
        staging,
        guard,
        platform,
        work,
        &mut created,
    );
    if result.is_err() {
        cleanup(staging);
    }
    result
}

pub(crate) fn canonical_member_path(member: &ArchiveMember) -> String {
    let raw = if member.kind == MemberKind::Directory {
        member.path.trim_end_matches('/')
    } else {
        member.path.as_str()
    };
    raw.split('/')
        .filter(|component| !component.is_empty() && *component != ".")
        .collect::<Vec<&str>>()
        .join("/")
}

fn cleanup(staging: &Path) {
    let Ok(entries) = std::fs::read_dir(staging) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() && !path.is_symlink() {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

pub(crate) fn select_members(
    members: &[ArchiveMember],
    selection: &Selection,
) -> Result<Applied, Error> {
    let mut canonical: Vec<(String, bool)> = Vec::with_capacity(members.len());
    let mut named: Vec<usize> = Vec::with_capacity(members.len());
    for (index, member) in members.iter().enumerate() {
        let path = canonical_member_path(member);
        if path.is_empty() {
            continue;
        }
        canonical.push((path, member.kind == MemberKind::Directory));
        named.push(index);
    }
    let candidates: Vec<Candidate<'_>> = canonical
        .iter()
        .map(|(path, directory)| Candidate {
            path,
            directory: *directory,
        })
        .collect();
    let mut applied = selection.apply(&candidates)?;
    for member in &mut applied.members {
        member.index = named[member.index];
    }
    Ok(applied)
}

pub(crate) struct Plan<'a> {
    pub(crate) directories: Vec<EntryPath>,
    pub(crate) files: Vec<&'a AppliedMember>,
    pub(crate) symlinks: Vec<&'a AppliedMember>,
}

pub(crate) fn build_plan<'a>(
    members: &[ArchiveMember],
    applied: &'a Applied,
) -> Result<Plan<'a>, Error> {
    let mut directory_set: std::collections::BTreeSet<String> = applied
        .directories
        .iter()
        .map(|path| path.as_str().to_owned())
        .collect();
    let mut files: Vec<&AppliedMember> = Vec::new();
    let mut symlinks: Vec<&AppliedMember> = Vec::new();
    for applied_member in &applied.members {
        let member = &members[applied_member.index];
        match member.kind {
            MemberKind::Directory => {
                directory_set.insert(applied_member.path.as_str().to_owned());
            }
            MemberKind::File | MemberKind::HardLink => files.push(applied_member),
            MemberKind::Symlink => symlinks.push(applied_member),
            MemberKind::Other => {
                return Err(Error::new(
                    ErrorKind::ArchiveUnsupported,
                    format!(
                        "member \"{}\" is a type extraction does not carry",
                        member.path
                    ),
                )
                .with_member(&member.path));
            }
        }
        let full = applied_member.path.as_str();
        for (at, _) in full.match_indices('/') {
            directory_set.insert(full[..at].to_owned());
        }
    }
    let mut directories: Vec<EntryPath> = Vec::with_capacity(directory_set.len());
    for raw in directory_set {
        let path = EntryPath::new(&raw).map_err(|reason| {
            Error::new(
                ErrorKind::ArchiveUnsafePath,
                format!("ancestor directory \"{raw}\" {reason}"),
            )
            .with_member(&raw)
        })?;
        directories.push(path);
    }
    directories.sort_by_key(|path| path.as_str().matches('/').count());
    Ok(Plan {
        directories,
        files,
        symlinks,
    })
}

struct Site<'a, P> {
    staging: &'a Path,
    capabilities: &'a VolumeCapabilities,
    platform: &'a P,
    work: &'a WorkCounter,
}

struct WriteState<'a> {
    guard: &'a mut BombGuard,
    created: &'a mut Vec<Created>,
    entries: &'a mut Vec<TreeEntry>,
}

fn write_directories<P: Platform>(
    plan: &Plan<'_>,
    site: &Site<'_, P>,
    state: &mut WriteState<'_>,
) -> Result<(), Error> {
    for path in &plan.directories {
        state.guard.observe_entry()?;
        let target = check_path(path, site.staging, site.capabilities)?;
        create_directory(site.platform, &target, path.as_str(), state.created)?;
        state
            .entries
            .push(TreeEntry::Directory { path: path.clone() });
    }
    Ok(())
}

fn write_files<A, P>(
    plan: &Plan<'_>,
    members: &[ArchiveMember],
    by_path: &HashMap<&str, usize>,
    archive: &mut A,
    site: &Site<'_, P>,
    buffer: &mut [u8],
    state: &mut WriteState<'_>,
) -> Result<(), Error>
where
    A: Archive,
    P: Platform,
{
    for applied_member in &plan.files {
        state.guard.observe_entry()?;
        let member = &members[applied_member.index];
        let path = &applied_member.path;
        let target = check_path(path, site.staging, site.capabilities)?;
        let source_member = match member.kind {
            MemberKind::HardLink => {
                let raw_target = member.target.as_deref().unwrap_or_default();
                let target_path = String::from_utf8_lossy(raw_target).into_owned();
                let Some(&target_index) = by_path.get(target_path.as_str()) else {
                    return Err(Error::new(
                        ErrorKind::ArchiveLinkEscape,
                        format!(
                            "member \"{}\" hard-links to \"{target_path}\", which this archive does not hold",
                            member.path
                        ),
                    )
                    .with_member(&member.path));
                };
                &members[target_index]
            }
            _ => member,
        };
        let mut body = archive.open(source_member)?;
        let file = create_file(site.platform, &target, member.path.as_str(), state.created)?;
        let written = stream_to_file(&mut body, file, buffer, member.path.as_str())?;
        site.work.wrote_bytes(written.len_bytes);
        state.guard.observe_bytes(written.len_bytes)?;
        state.entries.push(TreeEntry::File {
            path: path.clone(),
            mode: member.mode,
            size: written.len_bytes,
            content: written.digest,
        });
    }
    Ok(())
}

fn write_symlinks<P: Platform>(
    plan: &Plan<'_>,
    members: &[ArchiveMember],
    site: &Site<'_, P>,
    state: &mut WriteState<'_>,
) -> Result<(), Error> {
    for applied_member in &plan.symlinks {
        let member = &members[applied_member.index];
        let path = &applied_member.path;
        state.guard.observe_entry()?;
        let target_bytes = member.target.clone().unwrap_or_default();
        state.guard.observe_bytes(target_bytes.len() as u64)?;
        let target = check_path(path, site.staging, site.capabilities)?;
        create_symlink(
            site.platform,
            &target,
            member.path.as_str(),
            &target_bytes,
            state.created,
        )?;
        state.entries.push(TreeEntry::Symlink {
            path: path.clone(),
            size: target_bytes.len() as u64,
            content: hash_bytes(&target_bytes),
        });
    }
    Ok(())
}

fn run<A, P>(
    archive: &mut A,
    selection: &Selection,
    staging: &Path,
    mut guard: BombGuard,
    platform: &P,
    work: &WorkCounter,
    created: &mut Vec<Created>,
) -> Result<Vec<TreeEntry>, Error>
where
    A: Archive,
    P: Platform,
{
    let members = archive.members()?;
    let applied = select_members(&members, selection)?;
    let by_path: HashMap<&str, usize> = members
        .iter()
        .enumerate()
        .map(|(index, member)| (member.path.as_str(), index))
        .collect();
    let capabilities = platform.volume_capabilities(staging)?;
    let plan = build_plan(&members, &applied)?;
    let site = Site {
        staging,
        capabilities: &capabilities,
        platform,
        work,
    };

    let mut entries: Vec<TreeEntry> = Vec::new();
    let mut buffer = vec![0_u8; BUFFER_LEN];
    let mut state = WriteState {
        guard: &mut guard,
        created,
        entries: &mut entries,
    };

    write_directories(&plan, &site, &mut state)?;
    write_files(
        &plan,
        &members,
        &by_path,
        archive,
        &site,
        &mut buffer,
        &mut state,
    )?;
    write_symlinks(&plan, &members, &site, &mut state)?;

    Ok(entries)
}

fn staged_path(staging: &Path, entry: &EntryPath) -> PathBuf {
    let mut result = staging.to_path_buf();
    for component in entry.as_str().split('/') {
        result.push(component);
    }
    result
}

#[cfg(windows)]
const WINDOWS_RESERVED_BASE_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

#[cfg(windows)]
fn worth_confirming(component: &str) -> bool {
    if component.contains(':') || component.ends_with('.') || component.ends_with(' ') {
        return true;
    }
    let base = component.split('.').next().unwrap_or(component);
    WINDOWS_RESERVED_BASE_NAMES.contains(&base.to_ascii_uppercase().as_str())
}

#[cfg(not(windows))]
fn worth_confirming(_component: &str) -> bool {
    false
}

fn confirm_stored_name(member: &str, full: &Path) -> Result<(), Error> {
    let Some(name) = full.file_name() else {
        return Ok(());
    };
    if !worth_confirming(&name.to_string_lossy()) {
        return Ok(());
    }
    let listed = full.parent().is_some_and(|parent| {
        std::fs::read_dir(parent)
            .is_ok_and(|entries| entries.flatten().any(|found| found.file_name() == name))
    });
    if listed {
        return Ok(());
    }
    Err(Error::new(
        ErrorKind::DestinationUnrepresentable,
        format!(
            "rename member \"{member}\", because this volume accepted the name and stored a different one"
        ),
    )
    .with_member(member))
}

fn check_path(
    entry: &EntryPath,
    staging: &Path,
    capabilities: &VolumeCapabilities,
) -> Result<PathBuf, Error> {
    for component in entry.as_str().split('/') {
        let length = u32::try_from(component.len()).unwrap_or(u32::MAX);
        if length > capabilities.max_component_length {
            return Err(Error::new(
                ErrorKind::ArchiveUnsafePath,
                format!(
                    "member \"{entry}\" has a path component {length} bytes long, past this volume's maximum component length of {} bytes",
                    capabilities.max_component_length
                ),
            )
            .with_member(entry.as_str()));
        }
    }
    let full = staged_path(staging, entry);
    let full_length = u32::try_from(full.as_os_str().len()).unwrap_or(u32::MAX);
    if full_length > capabilities.max_path_length {
        return Err(Error::new(
            ErrorKind::ArchiveUnsafePath,
            format!(
                "member \"{entry}\" stages to a path {full_length} bytes long, past this volume's maximum path length of {} bytes",
                capabilities.max_path_length
            ),
        )
        .with_member(entry.as_str()));
    }
    Ok(full)
}

fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn find_collision_partner(created: &[Created], target: &Path) -> Option<String> {
    let target_text = target.to_string_lossy();
    created.iter().find_map(|entry| {
        let existing_text = entry.path.to_string_lossy();
        if existing_text.eq_ignore_ascii_case(&target_text) && existing_text != target_text {
            Some(entry.member.clone())
        } else {
            None
        }
    })
}

fn reclassify_creation_failure(
    error: Error,
    target: &Path,
    member: &str,
    created: &[Created],
) -> Error {
    if exists(target) {
        let other = find_collision_partner(created, target)
            .unwrap_or_else(|| "another entry already staged at this name".to_owned());
        return Error::new(
            ErrorKind::ArchiveCollision,
            format!(
                "member \"{member}\" and member \"{other}\" both claim one name on this volume"
            ),
        )
        .with_member(member);
    }
    error
}

fn create_directory<P: Platform>(
    platform: &P,
    target: &Path,
    member: &str,
    created: &mut Vec<Created>,
) -> Result<(), Error> {
    match platform.create_directory_exclusive(target) {
        Ok(()) => {
            created.push(Created {
                path: target.to_path_buf(),
                member: member.to_owned(),
            });
            confirm_stored_name(member, target)
        }
        Err(error) => Err(reclassify_creation_failure(error, target, member, created)),
    }
}

fn create_file<P: Platform>(
    platform: &P,
    target: &Path,
    member: &str,
    created: &mut Vec<Created>,
) -> Result<File, Error> {
    match platform.create_file_exclusive(target) {
        Ok(file) => {
            created.push(Created {
                path: target.to_path_buf(),
                member: member.to_owned(),
            });
            confirm_stored_name(member, target)?;
            Ok(file)
        }
        Err(error) => Err(reclassify_creation_failure(error, target, member, created)),
    }
}

fn create_symlink<P: Platform>(
    platform: &P,
    target: &Path,
    member: &str,
    link_target: &[u8],
    created: &mut Vec<Created>,
) -> Result<(), Error> {
    match platform.create_symlink(link_target, target) {
        Ok(()) => {
            created.push(Created {
                path: target.to_path_buf(),
                member: member.to_owned(),
            });
            Ok(())
        }
        Err(error) => Err(reclassify_creation_failure(error, target, member, created)),
    }
}

struct StreamResult {
    len_bytes: u64,
    digest: ContentDigest,
}

pub(crate) fn io_failure(member: &str, error: &std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::StorageFull {
        return Error::new(
            ErrorKind::ResourceDisk,
            format!("free space on the volume before extracting \"{member}\": {error}"),
        );
    }
    Error::new(
        ErrorKind::ArchiveUnsupported,
        format!("member \"{member}\" could not be streamed to staging: {error}"),
    )
    .with_member(member)
}

fn stream_to_file(
    body: &mut dyn Read,
    mut file: File,
    buffer: &mut [u8],
    member_text: &str,
) -> Result<StreamResult, Error> {
    let mut hasher = blake3::Hasher::new();
    let mut total: u64 = 0;
    loop {
        let read = body
            .read(buffer)
            .map_err(|error| io_failure(member_text, &error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|error| io_failure(member_text, &error))?;
        total += read as u64;
    }
    Ok(StreamResult {
        len_bytes: total,
        digest: ContentDigest::from_bytes(*hasher.finalize().as_bytes()),
    })
}
