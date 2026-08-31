//! Carrying objects between machines that do not trust each other.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use fetchloom_engine::digest::ContentDigest;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::hashing;
use fetchloom_engine::seam::platform::Platform;
use fetchloom_engine::seam::store::Store;

use crate::Cache;
use crate::layout::{digest_of, name_of};

/// The size of one tar block.
const BLOCK: usize = 512;

/// How many bytes move between a bundle and the cache at a time.
const BUFFER: usize = 1 << 20;

/// What one export or import did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleReport {
    /// The objects the bundle holds.
    pub objects: u64,
    /// The bytes those objects hold.
    pub bytes: u64,
    /// The objects the cache already had, which import left alone.
    pub already_held: u64,
}

impl<P: Platform> Cache<P> {
    /// Writes every object the cache holds into a bundle.
    ///
    /// # Errors
    ///
    /// Fails when an object cannot be read and when the bundle cannot be
    /// written.
    pub fn export(&self, to: &Path) -> Result<BundleReport, Error> {
        let mut digests = self.list()?;
        digests.sort_by(|left, right| left.bytes().cmp(right.bytes()));

        let mut writing = std::fs::File::create(to)
            .map_err(|reason| filesystem_failure(Surface::Cache, to, &reason))?;
        let mut buffer = vec![0u8; BUFFER];
        let mut report = BundleReport {
            objects: 0,
            bytes: 0,
            already_held: 0,
        };
        for digest in digests {
            let mut reading = self.read(digest)?;
            let length = reading.length();
            write_all(&mut writing, &header(&name_of(digest), length), to)?;
            let mut moved = 0u64;
            loop {
                let filled = reading.read(&mut buffer).map_err(|reason| {
                    filesystem_failure(Surface::Cache, &self.layout().objects(), &reason)
                })?;
                if filled == 0 {
                    break;
                }
                write_all(&mut writing, &buffer[..filled], to)?;
                moved += filled as u64;
                self.work().read_bytes(filled as u64);
            }
            if moved != length {
                return Err(Error::new(
                    ErrorKind::IntegrityTruncated,
                    format!(
                        "run cache verify, because {digest} is {moved} bytes where it was {length}"
                    ),
                ));
            }
            write_all(&mut writing, &padding(length), to)?;
            report.objects += 1;
            report.bytes += length;
        }
        write_all(&mut writing, &[0u8; BLOCK * 2], to)?;
        writing
            .flush()
            .map_err(|reason| filesystem_failure(Surface::Cache, to, &reason))?;
        Ok(report)
    }

    /// Reads a bundle into the cache, publishing nothing until all of it checks
    /// out.
    ///
    /// # Errors
    ///
    /// Fails with `archive.unsafe_path` for a member named like a path, with
    /// `cache.corrupt` for a member named anything else, with
    /// `integrity.mismatch` for bytes that do not hash to their name, and with
    /// whatever the tar reader found for a bundle that is not one.
    pub fn import(&self, from: &Path, format: BundleReader) -> Result<BundleReport, Error> {
        let mut staged: Vec<Staged> = Vec::new();
        let outcome = self.stage_bundle(from, format, &mut staged);
        if outcome.is_err() {
            for held in &staged {
                let _ = std::fs::remove_file(&held.scratch);
            }
        }
        let mut report = outcome?;
        for held in staged {
            if self.contains(held.digests.content)? {
                let _ = std::fs::remove_file(&held.scratch);
                report.already_held += 1;
                continue;
            }
            self.publish_imported(&held.scratch, &held.digests)?;
        }
        Ok(report)
    }

    fn stage_bundle(
        &self,
        from: &Path,
        mut format: BundleReader,
        staged: &mut Vec<Staged>,
    ) -> Result<BundleReport, Error> {
        let mut report = BundleReport {
            objects: 0,
            bytes: 0,
            already_held: 0,
        };
        let mut index = 0u64;
        while let Some(member) = format.next_member(from)? {
            let claimed = claimed_digest(&member.name)?;
            let scratch = self
                .layout()
                .partial()
                .join(format!("{}-{index}.import", self.token().pid));
            index += 1;
            let digests = self.stage_member(&mut format, &member, &scratch)?;
            let digest = digests.content;
            staged.push(Staged { digests, scratch });
            if digest != claimed {
                return Err(Error::new(
                    ErrorKind::IntegrityMismatch,
                    format!(
                        "get the bundle again from a source you trust, because a member named \
                         {} holds bytes that hash to {digest}",
                        member.name
                    ),
                ));
            }
            report.objects += 1;
            report.bytes += member.size;
        }
        Ok(report)
    }

    fn stage_member(
        &self,
        format: &mut BundleReader,
        member: &BundleMember,
        scratch: &Path,
    ) -> Result<hashing::Digests, Error> {
        let _ = std::fs::remove_file(scratch);
        let mut writing = self.platform().create_file_exclusive(scratch)?;
        let mut pair = hashing::Pair::new();
        let mut buffer = vec![0u8; BUFFER];
        let mut left = member.size;
        while left > 0 {
            let want = usize::try_from(left.min(BUFFER as u64)).unwrap_or(BUFFER);
            let filled = format.read_body(&mut buffer[..want])?;
            if filled == 0 {
                return Err(Error::new(
                    ErrorKind::IntegrityTruncated,
                    format!(
                        "get the bundle again, because it ends inside the member named {}",
                        member.name
                    ),
                ));
            }
            writing
                .write_all(&buffer[..filled])
                .map_err(|reason| filesystem_failure(Surface::Cache, scratch, &reason))?;
            pair.update(self.processor(), &buffer[..filled]);
            left -= filled as u64;
            self.work().read_bytes(filled as u64);
            self.work().wrote_bytes(filled as u64);
        }
        format.finish_body(member.size)?;
        self.platform().flush(&writing, self.tier())?;
        drop(writing);
        Ok(pair.finish())
    }

    fn publish_imported(&self, scratch: &Path, digests: &hashing::Digests) -> Result<(), Error> {
        self.publish_object(scratch, digests)?;
        self.finish_publication(digests)
    }
}

/// One member of a bundle, written and hashed but not yet published.
struct Staged {
    /// What the member's own bytes produced.
    digests: hashing::Digests,
    /// Where those bytes are waiting.
    scratch: PathBuf,
}

/// One member a bundle holds.
struct BundleMember {
    /// The name the member arrived under, which is a claim and not a path.
    name: String,
    /// How many bytes the member holds.
    size: u64,
}

/// Returns the digest a member's name claims.
fn claimed_digest(name: &str) -> Result<ContentDigest, Error> {
    if name.contains(['/', '\\']) || name.contains("..") || name.starts_with('.') {
        return Err(Error::new(
            ErrorKind::ArchiveUnsafePath,
            format!(
                "get the bundle again from a source you trust, because a member named {name} \
                 names a path where a bundle names only digests"
            ),
        ));
    }
    digest_of(name).ok_or_else(|| {
        Error::new(
            ErrorKind::CacheCorrupt,
            format!(
                "get the bundle again from a source you trust, because a member named {name} is \
                 not the digest of anything"
            ),
        )
    })
}

/// Reads the members of a bundle in the order they were written.
pub struct BundleReader {
    file: std::fs::File,
    at: u64,
}

impl BundleReader {
    /// Opens a bundle.
    ///
    /// # Errors
    ///
    /// Fails when the bundle cannot be opened.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let file = std::fs::File::open(path)
            .map_err(|reason| filesystem_failure(Surface::Cache, path, &reason))?;
        Ok(Self { file, at: 0 })
    }

    fn next_member(&mut self, from: &Path) -> Result<Option<BundleMember>, Error> {
        let mut block = [0u8; BLOCK];
        let filled = fill(&mut self.file, &mut block)
            .map_err(|reason| filesystem_failure(Surface::Cache, from, &reason))?;
        if filled == 0 {
            return Err(truncated(from));
        }
        if filled < BLOCK {
            return Err(truncated(from));
        }
        self.at += BLOCK as u64;
        if block.iter().all(|byte| *byte == 0) {
            return Ok(None);
        }
        let name_end = block[..100]
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(100);
        let name = String::from_utf8_lossy(&block[..name_end]).into_owned();
        let size = octal(&block[124..136]).ok_or_else(|| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!(
                    "get the bundle again from a source you trust, because the header of {name} \
                     states no length"
                ),
            )
        })?;
        if !checksum_matches(&block) {
            return Err(Error::new(
                ErrorKind::CacheCorrupt,
                format!(
                    "get the bundle again from a source you trust, because the header of {name} \
                     does not check out"
                ),
            ));
        }
        Ok(Some(BundleMember { name, size }))
    }

    fn read_body(&mut self, into: &mut [u8]) -> Result<usize, Error> {
        let filled = self.file.read(into).map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("the bundle could not be read: {reason}"),
            )
        })?;
        self.at += filled as u64;
        Ok(filled)
    }

    fn finish_body(&mut self, size: u64) -> Result<(), Error> {
        let over = usize::try_from(size % BLOCK as u64).unwrap_or(0);
        if over == 0 {
            return Ok(());
        }
        let mut block = [0u8; BLOCK];
        let want = BLOCK - over;
        let filled = fill(&mut self.file, &mut block[..want]).map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("the bundle could not be read: {reason}"),
            )
        })?;
        if filled < want {
            return Err(Error::new(
                ErrorKind::IntegrityTruncated,
                "get the bundle again, because it ends before its last member does",
            ));
        }
        self.at += filled as u64;
        Ok(())
    }
}

fn truncated(from: &Path) -> Error {
    Error::new(
        ErrorKind::IntegrityTruncated,
        format!(
            "get {} again, because it ends before the bundle does",
            from.display()
        ),
    )
}

fn fill(reader: &mut impl Read, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        let read = reader.read(&mut buffer[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

fn octal(field: &[u8]) -> Option<u64> {
    let text = std::str::from_utf8(field).ok()?;
    let trimmed = text.trim_matches(|byte| byte == ' ' || byte == '\0');
    if trimmed.is_empty() {
        return Some(0);
    }
    u64::from_str_radix(trimmed, 8).ok()
}

fn checksum_matches(block: &[u8; BLOCK]) -> bool {
    let Some(stated) = octal(&block[148..156]) else {
        return false;
    };
    let mut sum = 0u64;
    for (index, byte) in block.iter().enumerate() {
        sum += if (148..156).contains(&index) {
            u64::from(b' ')
        } else {
            u64::from(*byte)
        };
    }
    sum == stated
}

fn header(name: &str, size: u64) -> [u8; BLOCK] {
    let mut block = [0u8; BLOCK];
    let name = name.as_bytes();
    block[..name.len()].copy_from_slice(name);
    write_octal(&mut block[100..108], 0o444, 7);
    write_octal(&mut block[108..116], 0, 7);
    write_octal(&mut block[116..124], 0, 7);
    write_octal(&mut block[124..136], size, 11);
    write_octal(&mut block[136..148], 0, 11);
    block[156] = b'0';
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    for slot in &mut block[148..156] {
        *slot = b' ';
    }
    let mut sum = 0u64;
    for byte in &block {
        sum += u64::from(*byte);
    }
    write_octal(&mut block[148..154], sum, 6);
    block[154] = 0;
    block[155] = b' ';
    block
}

fn write_octal(field: &mut [u8], value: u64, digits: usize) {
    let text = format!("{value:0digits$o}");
    let bytes = text.as_bytes();
    let taken = bytes.len().min(field.len());
    field[..taken].copy_from_slice(&bytes[..taken]);
    if taken < field.len() {
        field[taken] = 0;
    }
}

fn padding(size: u64) -> Vec<u8> {
    let over = usize::try_from(size % BLOCK as u64).unwrap_or(0);
    if over == 0 {
        Vec::new()
    } else {
        vec![0u8; BLOCK - over]
    }
}

fn write_all(writing: &mut std::fs::File, bytes: &[u8], to: &Path) -> Result<(), Error> {
    writing
        .write_all(bytes)
        .map_err(|reason| filesystem_failure(Surface::Cache, to, &reason))
}
