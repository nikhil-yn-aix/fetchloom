//! url records, each a line appended to one of 16 files in `urls/` by the first hex digit of its
//! url's SHA-256, with a 12-byte entry in an index file beside it saying where the line starts. a
//! link costs some 380 bytes of line and 12 of entry, not a file and a disk block of its own. a run
//! reads a shard's entries on its first lookup there, and from then on only the lines it asks for.
//! a line is a check of its json and the json; a line cut short by a run that died mid-write, or
//! that fails its check, is passed over.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::{UrlRecord, recorded_url_matches};
use crate::digest;

pub(super) const SHARDS: usize = 16;

/// an entry's bytes: its key, the first 8 bytes of its url's SHA-256, then where its line starts.
const ENTRY: usize = 12;

/// a line's check, the first 8 bytes of its json's SHA-256 in hex, and the space after it.
const CHECK: usize = 16 + 1;

/// how much of a line one read asks for; a longer one takes more.
const WINDOW: usize = 1024;

/// one shard as this run read it.
#[derive(Default)]
struct Index {
    /// how many bytes of the index file were read.
    read: u64,
    /// the first read's entries, each its key's last 4 bytes and its line's start, grouped by the
    /// key's bits after the shard's and in file order within a group, so a key's last is latest.
    entries: Vec<(u32, u32)>,
    /// where each group starts in `entries`, then where the last one ends.
    groups: Vec<u32>,
    bits: u32,
    /// the start of each key's latest line written or read since the first read.
    recent: HashMap<u64, u32>,
    /// the shard's lines, then its index, as this run opened them.
    log: Option<Arc<File>>,
    file: Option<File>,
}

impl Index {
    fn latest(&self, key: u64) -> Option<u32> {
        if let Some(start) = self.recent.get(&key) {
            return Some(*start);
        }
        let group = group(key, self.bits);
        let (from, to) = (*self.groups.get(group)?, *self.groups.get(group + 1)?);
        self.entries[from as usize..to as usize]
            .iter()
            .rev()
            .find(|(each, _)| *each == tag(key))
            .map(|(_, start)| *start)
    }

    /// reads the entries past those read before: a first read grouped, a later one as recent.
    fn read_on(&mut self, path: &Path) -> io::Result<()> {
        if self.file.is_none() {
            match File::open(path) {
                Ok(file) => self.file = Some(file),
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(err),
            }
        }
        let Some(mut file) = self.file.as_ref() else {
            return Ok(());
        };
        let whole = file.metadata()?.len() / ENTRY as u64 * ENTRY as u64;
        if whole <= self.read {
            return Ok(());
        }
        file.seek(SeekFrom::Start(self.read))?;
        let mut bytes = vec![0; usize::try_from(whole - self.read).map_err(io::Error::other)?];
        file.read_exact(&mut bytes)?;
        if self.read == 0 {
            self.group(&bytes);
        } else {
            for entry in bytes.as_chunks::<ENTRY>().0 {
                let (key, start) = parse(entry);
                self.recent.insert(key, start);
            }
        }
        self.read = whole;
        Ok(())
    }

    /// sorts the entries `bytes` holds into groups of some four, counting each group's first.
    fn group(&mut self, bytes: &[u8]) {
        let count = bytes.len() / ENTRY;
        self.bits = (count / 4).bit_width();
        let mut groups = vec![0_u32; (1 << self.bits) + 1];
        for entry in bytes.as_chunks::<ENTRY>().0 {
            groups[group(parse(entry).0, self.bits) + 1] += 1;
        }
        for at in 1..groups.len() {
            groups[at] += groups[at - 1];
        }
        let mut next = groups.clone();
        let mut entries = vec![(0, 0); count];
        for entry in bytes.as_chunks::<ENTRY>().0 {
            let (key, start) = parse(entry);
            let slot = &mut next[group(key, self.bits)];
            entries[*slot as usize] = (tag(key), start);
            *slot += 1;
        }
        self.entries = entries;
        self.groups = groups;
    }
}

/// the shards as this run read them, each on its first lookup, then kept up to date by this run's
/// own writes and by reading on when a lookup misses.
pub(crate) struct Shards {
    indexes: Vec<Mutex<Index>>,
}

impl Default for Shards {
    fn default() -> Shards {
        Shards {
            indexes: (0..SHARDS).map(|_| Mutex::default()).collect(),
        }
    }
}

impl std::fmt::Debug for Shards {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Shards")
    }
}

impl Shards {
    /// the record kept for `url` in the cache at `root`.
    pub(crate) fn find(&self, root: &Path, url: &str) -> Option<UrlRecord> {
        let (shard, key) = place(url);
        let (log, start) = {
            let mut index = self.indexes[shard]
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let start = if let Some(start) = index.latest(key) {
                start
            } else {
                index.read_on(&paths(root, shard).1).ok()?;
                index.latest(key)?
            };
            if index.log.is_none() {
                index.log = Some(Arc::new(File::open(paths(root, shard).0).ok()?));
            }
            (Arc::clone(index.log.as_ref()?), start)
        };
        let line = line_at(&log, u64::from(start))?;
        let mut record: UrlRecord = serde_json::from_slice(checked(&line)?).ok()?;
        if !recorded_url_matches(&record.url, url) {
            return None;
        }
        url.clone_into(&mut record.url);
        Some(record)
    }

    /// appends `record` to its shard, then its entry to the shard's index, under the shard's lock,
    /// which a second writer waits for.
    pub(crate) fn save(&self, root: &Path, record: &UrlRecord) -> io::Result<()> {
        let (shard, key) = place(&record.url);
        let json = serde_json::to_vec(record).map_err(io::Error::other)?;
        let (records, entries) = paths(root, shard);
        let mut log = OpenOptions::new().append(true).create(true).open(records)?;
        log.lock()?;
        // a shard holds 4 GiB; past that its links go unrecorded until gc writes it again
        let start = u32::try_from(log.metadata()?.len()).map_err(io::Error::other)?;
        let mut line = Vec::with_capacity(CHECK + json.len() + 1);
        line.extend_from_slice(&check(&json));
        line.push(b' ');
        line.extend_from_slice(&json);
        line.push(b'\n');
        log.write_all(&line)?;
        let mut entry = [0; ENTRY];
        entry[..8].copy_from_slice(&key.to_le_bytes());
        entry[8..].copy_from_slice(&start.to_le_bytes());
        let mut index = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(entries)?;
        // an entry a dead run left unfinished is written over, so each starts on its boundary
        let whole = index.metadata()?.len() / ENTRY as u64 * ENTRY as u64;
        index.seek(SeekFrom::Start(whole))?;
        index.write_all(&entry)?;
        // still under the lock, so this run's writes of one link reach `recent` in file order
        self.indexes[shard]
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .recent
            .insert(key, start);
        Ok(())
    }

    /// writes shard `shard` again with only the latest good record of each key `keep` takes,
    /// through `scratch`, unless it holds no other line already; a shard left with none goes. gc
    /// calls it holding the cache alone, so no run writes meanwhile.
    pub(crate) fn rewrite(
        &self,
        root: &Path,
        scratch: &Path,
        shard: usize,
        keep: &dyn Fn(&UrlRecord) -> bool,
    ) -> io::Result<()> {
        let (records, entries) = paths(root, shard);
        let (lines, index) = match (fs::read(&records), fs::read(&entries)) {
            (Ok(lines), Ok(index)) => (lines, index),
            (Err(err), _) | (_, Err(err)) if err.kind() != io::ErrorKind::NotFound => {
                return Err(err);
            }
            // lines without their index, or an index without its lines, name no record
            _ => {
                *self.indexes[shard]
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner) = Index::default();
                remove(&records)?;
                return remove(&entries);
            }
        };
        let mut latest: HashMap<u64, u32> = HashMap::new();
        let mut order = Vec::new();
        for entry in index.as_chunks::<ENTRY>().0 {
            let (key, start) = parse(entry);
            if latest.insert(key, start).is_none() {
                order.push(key);
            }
        }
        let mut kept = Vec::new();
        let mut kept_index = Vec::new();
        for key in order {
            let Some(line) = lines
                .get(latest[&key] as usize..)
                .and_then(|rest| Some(&rest[..rest.iter().position(|&byte| byte == b'\n')?]))
            else {
                continue;
            };
            let held = checked(line)
                .and_then(|json| serde_json::from_slice::<UrlRecord>(json).ok())
                .is_some_and(|record| keep(&record));
            if held {
                kept_index.extend_from_slice(&key.to_le_bytes());
                let start = u32::try_from(kept.len()).map_err(io::Error::other)?;
                kept_index.extend_from_slice(&start.to_le_bytes());
                kept.extend_from_slice(line);
                kept.push(b'\n');
            }
        }
        if kept == lines && kept_index == index {
            return Ok(());
        }
        *self.indexes[shard]
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Index::default();
        if kept.is_empty() {
            remove(&records)?;
            remove(&entries)
        } else {
            // the lines first: an index never names lines its file does not hold
            replace(scratch, &records, &kept)?;
            replace(scratch, &entries, &kept_index)
        }
    }
}

/// the shard of `url` and its key: the first 8 bytes of the url's SHA-256, whose first 4 bits
/// are the shard.
fn place(url: &str) -> (usize, u64) {
    let hex = digest::sha256(url.as_bytes()).hex();
    let key = u64::from_str_radix(&hex[..16], 16).unwrap_or_default();
    ((key >> 60) as usize, key)
}

/// the group of `key` among `1 << bits`, by its bits after the shard's.
fn group(key: u64, bits: u32) -> usize {
    if bits == 0 {
        0
    } else {
        usize::try_from((key << 4) >> (64 - bits)).unwrap_or_default()
    }
}

/// the last 4 bytes of `key`, which tell it from the others in its group.
fn tag(key: u64) -> u32 {
    u32::try_from(key & u64::from(u32::MAX)).unwrap_or_default()
}

fn parse(entry: &[u8; ENTRY]) -> (u64, u32) {
    let (key, start) = entry.split_at(8);
    (
        u64::from_le_bytes(key.try_into().unwrap_or_default()),
        u32::from_le_bytes(start.try_into().unwrap_or_default()),
    )
}

/// a shard's file of lines and its index.
fn paths(root: &Path, shard: usize) -> (PathBuf, PathBuf) {
    let folder = root.join("urls");
    (
        folder.join(format!("{shard:x}.log")),
        folder.join(format!("{shard:x}.idx")),
    )
}

/// the line starting at `start` in `file`, without its newline; none for one cut short.
fn line_at(file: &File, start: u64) -> Option<Vec<u8>> {
    let mut line = Vec::new();
    let mut window = [0; WINDOW];
    loop {
        let read = read_at(file, start + line.len() as u64, &mut window).ok()?;
        let got = &window[..read];
        if let Some(end) = got.iter().position(|&byte| byte == b'\n') {
            line.extend_from_slice(&got[..end]);
            return Some(line);
        }
        if read == 0 {
            return None;
        }
        line.extend_from_slice(got);
    }
}

#[cfg(unix)]
fn read_at(file: &File, at: u64, buffer: &mut [u8]) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buffer, at)
}

#[cfg(windows)]
fn read_at(file: &File, at: u64, buffer: &mut [u8]) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buffer, at)
}

/// the first 8 bytes of the SHA-256 of a line's json, in hex.
fn check(json: &[u8]) -> [u8; 16] {
    let hex = digest::sha256(json).hex();
    let mut out = [0; 16];
    out.copy_from_slice(&hex.as_bytes()[..16]);
    out
}

/// a line's json, when its check holds.
fn checked(line: &[u8]) -> Option<&[u8]> {
    let json = line.get(CHECK..)?;
    (line[CHECK - 1] == b' ' && check(json) == line[..CHECK - 1]).then_some(json)
}

fn replace(scratch: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = scratch.join(format!("{:016x}.urls", fastrand::u64(..)));
    fs::write(&temporary, bytes)?;
    fs::rename(&temporary, path).inspect_err(|_| {
        let _ = fs::remove_file(&temporary);
    })
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// files the latest line of `url` again under the key of `other`, as copying a record from one
/// link to another would.
#[cfg(test)]
pub(super) fn copy_record(root: &Path, url: &str, other: &str) {
    let (shard, key) = place(url);
    let (records, entries) = paths(root, shard);
    let index = fs::read(entries).unwrap();
    let (_, start) = index
        .as_chunks::<ENTRY>()
        .0
        .iter()
        .map(parse)
        .rev()
        .find(|(each, _)| *each == key)
        .unwrap();
    let line = line_at(&File::open(records).unwrap(), u64::from(start)).unwrap();
    let (shard, key) = place(other);
    let (records, entries) = paths(root, shard);
    let mut log = OpenOptions::new()
        .append(true)
        .create(true)
        .open(records)
        .unwrap();
    let start = u32::try_from(log.metadata().unwrap().len()).unwrap();
    log.write_all(&[&line[..], b"\n"].concat()).unwrap();
    let mut index = OpenOptions::new()
        .append(true)
        .create(true)
        .open(entries)
        .unwrap();
    index
        .write_all(&[&key.to_le_bytes()[..], &start.to_le_bytes()].concat())
        .unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Validators;

    fn record(url: &str, version: u64) -> UrlRecord {
        UrlRecord {
            url: url.to_owned(),
            sha256: digest::sha256(&version.to_le_bytes()),
            size: version,
            name: None,
            validators: Validators {
                etag: Some(format!("\"v{version}\"")),
                ..Validators::default()
            },
        }
    }

    fn store() -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        fs::create_dir(folder.path().join("urls")).unwrap();
        folder
    }

    /// `count` links whose keys start with `start`, so their records share a shard.
    fn crowded(count: usize, start: &str) -> Vec<String> {
        (0..)
            .map(|n| format!("https://example.org/{n}"))
            .filter(|url| digest::sha256(url.as_bytes()).hex().starts_with(start))
            .take(count)
            .collect()
    }

    fn rewrite(shards: &Shards, root: &Path, keep: &dyn Fn(&UrlRecord) -> bool) {
        for shard in 0..SHARDS {
            shards.rewrite(root, root, shard, keep).unwrap();
        }
    }

    /// the latest record of a link is the one found, by this run and by the next.
    #[test]
    fn the_latest_record_of_a_link_is_found() {
        let folder = store();
        let root = folder.path();
        let shards = Shards::default();
        let (a, b, c) = (
            "https://example.org/a",
            "https://example.org/b",
            "https://example.org/c",
        );
        shards.save(root, &record(a, 1)).unwrap();
        shards.save(root, &record(b, 2)).unwrap();
        assert_eq!(shards.find(root, a), Some(record(a, 1)));
        shards.save(root, &record(a, 3)).unwrap();
        assert_eq!(shards.find(root, a), Some(record(a, 3)));
        let next = Shards::default();
        assert_eq!(next.find(root, a), Some(record(a, 3)));
        assert_eq!(next.find(root, b), Some(record(b, 2)));
        assert_eq!(next.find(root, c), None);
        // what another run appends later is found on a miss
        shards.save(root, &record(c, 4)).unwrap();
        assert_eq!(next.find(root, c), Some(record(c, 4)));
    }

    /// among many links of one shard, each first read finds every link's latest record, and a
    /// link whose key differs in its last bytes alone is not taken for another.
    #[test]
    fn a_full_shard_finds_each_latest_record() {
        let folder = store();
        let root = folder.path();
        let urls = crowded(300, "0");
        let shards = Shards::default();
        for (version, url) in urls.iter().enumerate() {
            shards.save(root, &record(url, version as u64)).unwrap();
        }
        for url in urls.iter().step_by(3) {
            shards.save(root, &record(url, 1000)).unwrap();
        }
        let next = Shards::default();
        for (version, url) in urls.iter().enumerate() {
            let latest = if version % 3 == 0 {
                1000
            } else {
                version as u64
            };
            assert_eq!(next.find(root, url), Some(record(url, latest)), "{url}");
        }
        assert_eq!(next.find(root, "https://example.org/none"), None);
    }

    /// a line or an entry a dead run left unfinished, or a line whose check fails, is passed
    /// over, and the next record starts where it should.
    #[test]
    fn a_torn_or_damaged_record_is_passed_over() {
        let folder = store();
        let root = folder.path();
        let urls = crowded(2, "0");
        Shards::default().save(root, &record(&urls[0], 1)).unwrap();
        let (records, entries) = paths(root, 0);
        let mut torn = OpenOptions::new().append(true).open(&records).unwrap();
        torn.write_all(b"0123456789abcdef {\"url\"").unwrap();
        let mut torn = OpenOptions::new().append(true).open(&entries).unwrap();
        torn.write_all(&[7; 9]).unwrap();
        drop(torn);
        assert_eq!(
            Shards::default().find(root, &urls[0]),
            Some(record(&urls[0], 1))
        );
        Shards::default().save(root, &record(&urls[1], 2)).unwrap();
        assert_eq!(fs::metadata(&entries).unwrap().len(), 2 * ENTRY as u64);
        let next = Shards::default();
        assert_eq!(next.find(root, &urls[0]), Some(record(&urls[0], 1)));
        assert_eq!(next.find(root, &urls[1]), Some(record(&urls[1], 2)));
        let mut damaged = fs::read(&records).unwrap();
        let at = damaged.len() - 3;
        damaged[at] ^= 1;
        fs::write(&records, damaged).unwrap();
        assert_eq!(Shards::default().find(root, &urls[1]), None);
    }

    /// two runs writing one shard at once lose no record.
    #[test]
    fn two_writers_lose_no_record() {
        let folder = store();
        let root = folder.path();
        let urls = crowded(2000, "0");
        std::thread::scope(|scope| {
            for half in urls.chunks(1000) {
                scope.spawn(move || {
                    let shards = Shards::default();
                    for (version, url) in half.iter().enumerate() {
                        shards.save(root, &record(url, version as u64)).unwrap();
                    }
                });
            }
        });
        let shards = Shards::default();
        for half in urls.chunks(1000) {
            for (version, url) in half.iter().enumerate() {
                assert_eq!(
                    shards.find(root, url),
                    Some(record(url, version as u64)),
                    "{url}"
                );
            }
        }
    }

    /// a rewrite keeps the latest good record of each link it is asked to keep, and nothing else;
    /// a shard left with none goes.
    #[test]
    fn a_rewrite_keeps_only_the_latest_records_asked_for() {
        let folder = store();
        let root = folder.path();
        let urls = crowded(3, "0");
        let shards = Shards::default();
        shards.save(root, &record(&urls[0], 1)).unwrap();
        shards.save(root, &record(&urls[1], 2)).unwrap();
        shards.save(root, &record(&urls[0], 3)).unwrap();
        shards.save(root, &record(&urls[2], 4)).unwrap();
        let (records, entries) = paths(root, 0);
        let mut torn = OpenOptions::new().append(true).open(&records).unwrap();
        torn.write_all(b"0123").unwrap();
        drop(torn);
        rewrite(&shards, root, &|record| record.size != 2);
        let text = fs::read_to_string(&records).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert_eq!(fs::metadata(&entries).unwrap().len(), 2 * ENTRY as u64);
        assert_eq!(shards.find(root, &urls[0]), Some(record(&urls[0], 3)));
        assert_eq!(shards.find(root, &urls[1]), None);
        assert_eq!(shards.find(root, &urls[2]), Some(record(&urls[2], 4)));
        let before = fs::read(&records).unwrap();
        rewrite(&shards, root, &|_| true);
        assert_eq!(
            fs::read(&records).unwrap(),
            before,
            "a clean shard stays as it is"
        );
        rewrite(&shards, root, &|_| false);
        assert!(!records.exists() && !entries.exists());
        assert_eq!(
            fs::read_dir(root).unwrap().count(),
            1,
            "nothing left in the scratch folder"
        );
    }
}
