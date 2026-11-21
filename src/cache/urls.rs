//! url records, each a line appended to one of 16 files in `urls/` by the first hex digit of its
//! url's SHA-256, with a 12-byte entry in an index file beside it saying where the line starts. a
//! link costs some 380 bytes of line and 12 of entry, not a file and a disk block of its own. a run
//! reads a shard's entries on its first lookup there, and from then on only the lines it asks for.
//! a line is a check of its json and the json; a line cut short by a run that died mid-write, or
//! that fails its check, is passed over.

use std::collections::HashMap;
use std::fs::File;
#[cfg(test)]
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::folder::Folder;
use super::sharing::Sharing;
use super::{UrlRecord, recorded_url_matches};
use crate::digest;

pub(super) const SHARDS: usize = 16;

/// an entry's bytes: its key, the first 8 bytes of its url's SHA-256, then where its line starts.
const ENTRY: usize = 12;

/// a line's check, the first 8 bytes of its json's SHA-256 in hex, and the space after it.
const CHECK: usize = 16 + 1;

/// how much of a line one read asks for; a longer one takes more.
const WINDOW: usize = 1024;

/// the longest line a read takes, thousands of times what a link's record holds: a longer one, as
/// a co-user of a shared cache could leave with no line end, is passed over.
const LINE_MOST: usize = 1 << 20;

/// the most of a shard's lines a read takes: the 4 GiB its index reaches, and the line past them.
const LOG_MOST: u64 = (1 << 32) + LINE_MOST as u64;

/// the most of a shard's index a read takes, more entries than 4 GiB of the shortest lines need.
const INDEX_MOST: u64 = 1 << 30;

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
    /// where the latest line of `key` this run knows of starts.
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
    /// `folder` opens the shard's folder.
    fn read_on(&mut self, folder: &dyn Fn() -> io::Result<Folder>, shard: usize) -> io::Result<()> {
        if self.file.is_none() {
            match folder().and_then(|urls| urls.read_file(&names(shard).1)) {
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
        if whole > INDEX_MOST {
            return Err(io::ErrorKind::FileTooLarge.into());
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
    /// what the group the cache is shared with gets of the files written in `urls/`.
    sharing: Sharing,
    /// the folder below the cache's root the shards' files are in.
    below: PathBuf,
    /// for the shards of one user's own, that user, whose folder no one else may enter.
    #[cfg(unix)]
    user: Option<u32>,
}

impl Default for Shards {
    fn default() -> Shards {
        Shards::shared(Sharing::default())
    }
}

impl std::fmt::Debug for Shards {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Shards")
    }
}

impl Shards {
    /// shards in `urls/` whose files give the group the cache is shared with what `sharing` says.
    pub(crate) fn shared(sharing: Sharing) -> Shards {
        Shards {
            indexes: (0..SHARDS).map(|_| Mutex::default()).collect(),
            sharing,
            below: PathBuf::from("urls"),
            #[cfg(unix)]
            user: None,
        }
    }

    /// the shards of the records the user `user` wrote in a cache others may write, in a folder
    /// of `urls/` only that user may enter, so no one else could have put a record there.
    #[cfg(unix)]
    pub(crate) fn own(user: u32) -> Shards {
        Shards {
            below: Path::new("urls").join(format!("user-{user}")),
            user: Some(user),
            ..Shards::default()
        }
    }

    /// the shards' folder below the cache's `root`, reached without following a link, and with
    /// `make` made where it is missing. a folder of one user's own that is not theirs alone is
    /// `NotFound`, as [`Folder::private`] says.
    fn folder(&self, root: &Path, make: bool) -> io::Result<Folder> {
        #[cfg(unix)]
        if let Some(user) = self.user {
            return Folder::private(root, &self.below, user, make);
        }
        if make {
            Folder::open(root, &self.below, self.sharing)
        } else {
            Folder::existing(root, &self.below, Sharing::default())
        }
    }

    /// where the shards' files are below the cache's `root`, for messages.
    pub(crate) fn path(&self, root: &Path) -> PathBuf {
        root.join(&self.below)
    }

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
                index.read_on(&|| self.folder(root, false), shard).ok()?;
                index.latest(key)?
            };
            if index.log.is_none() {
                let urls = self.folder(root, false).ok()?;
                index.log = Some(Arc::new(urls.read_file(&names(shard).0).ok()?));
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

    /// appends `record` to its shard, then its entry to the shard's index, holding the shard's
    /// lock, which a second writer waits for.
    pub(crate) fn save(&self, root: &Path, record: &UrlRecord) -> io::Result<()> {
        self.save_with(root, record, &File::lock)
    }

    /// [`Shards::save`], taking the shard's lock with `lock`. where the filesystem cannot lock, the
    /// save goes on without it, as runs sharing the cache do: two saves at once may then lose a
    /// record, but a record names its link, so neither is ever taken for the other.
    fn save_with(
        &self,
        root: &Path,
        record: &UrlRecord,
        lock: &dyn Fn(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        let (shard, key) = place(&record.url);
        let json = serde_json::to_vec(record).map_err(io::Error::other)?;
        let (records, entries) = names(shard);
        // windows locks keep other handles off what they cover and refuse one opened to append.
        // none of these files is opened through a link, nor through a link put where `urls/` was,
        // since a line holds text a server chose
        let urls = self.folder(root, true)?;
        let held = urls.open_or_make(&format!("{shard:x}.lock"))?;
        match lock(&held) {
            Err(err) if !crate::volume::gives_no_locks(&err) => return Err(err),
            _ => {}
        }
        let mut log = urls.open_or_make(&records)?;
        // a shard holds 4 GiB; past that its links go unrecorded until gc writes it again
        let start = u32::try_from(log.seek(SeekFrom::End(0))?).map_err(io::Error::other)?;
        let mut line = Vec::with_capacity(CHECK + json.len() + 1);
        line.extend_from_slice(&check(&json));
        line.push(b' ');
        line.extend_from_slice(&json);
        line.push(b'\n');
        log.write_all(&line)?;
        let mut entry = [0; ENTRY];
        entry[..8].copy_from_slice(&key.to_le_bytes());
        entry[8..].copy_from_slice(&start.to_le_bytes());
        let mut index = urls.open_or_make(&entries)?;
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
    /// calls it holding the cache alone, so no run writes meanwhile. one user's own shards take
    /// their new files in their own folder: one made in `scratch`, which others may enter, could
    /// be held open there by someone and written once it had its name.
    pub(crate) fn rewrite(
        &self,
        root: &Path,
        scratch: &Path,
        shard: usize,
        keep: &dyn Fn(&UrlRecord) -> bool,
    ) -> io::Result<()> {
        let (records, entries) = names(shard);
        let Some(kept) = self.kept(root, shard, keep)? else {
            return Ok(());
        };
        *self.indexes[shard]
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Index::default();
        let urls = self.folder(root, false)?;
        if kept.lines.is_empty() {
            for name in [records, entries] {
                match urls.remove(&name) {
                    Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
                    _ => {}
                }
            }
            Ok(())
        } else {
            #[cfg(unix)]
            let own = self.user.is_some();
            #[cfg(not(unix))]
            let own = false;
            let tmp;
            let scratch = if own {
                &urls
            } else {
                let below = scratch.strip_prefix(root).map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "scratch outside the cache")
                })?;
                tmp = Folder::existing(root, below, self.sharing)?;
                &tmp
            };
            // the lines first: an index never names lines its file does not hold
            urls.replace(&records, &kept.lines, scratch)?;
            urls.replace(&entries, &kept.index, scratch)
        }
    }

    /// the files of shard `shard` a [`Shards::rewrite`] would remove, as a pretend gc lists them:
    /// both, when it would keep no record there.
    pub(crate) fn emptied(
        &self,
        root: &Path,
        shard: usize,
        keep: &dyn Fn(&UrlRecord) -> bool,
    ) -> io::Result<Vec<PathBuf>> {
        let (records, entries) = names(shard);
        let folder = self.path(root);
        Ok(match self.kept(root, shard, keep)? {
            Some(kept) if kept.lines.is_empty() => vec![folder.join(records), folder.join(entries)],
            _ => Vec::new(),
        })
    }

    /// shard `shard` with only the latest good record of each key `keep` takes; none when that is
    /// what it holds already, or the shards' folder is not there. lines without their index, or an
    /// index without its lines, name no record.
    fn kept(
        &self,
        root: &Path,
        shard: usize,
        keep: &dyn Fn(&UrlRecord) -> bool,
    ) -> io::Result<Option<Kept>> {
        let (records, entries) = names(shard);
        let urls = match self.folder(root, false) {
            Ok(urls) => urls,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let read = |name: &str, most| urls.read(name, most);
        let (lines, index) = match (read(&records, LOG_MOST), read(&entries, INDEX_MOST)) {
            (Ok(lines), Ok(index)) => (lines, index),
            (Err(err), _) | (_, Err(err)) if err.kind() != io::ErrorKind::NotFound => {
                return Err(err);
            }
            (Err(_), Err(_)) => return Ok(None),
            _ => {
                return Ok(Some(Kept {
                    lines: Vec::new(),
                    index: Vec::new(),
                }));
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
        let mut kept = Kept {
            lines: Vec::new(),
            index: Vec::new(),
        };
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
                kept.index.extend_from_slice(&key.to_le_bytes());
                let start = u32::try_from(kept.lines.len()).map_err(io::Error::other)?;
                kept.index.extend_from_slice(&start.to_le_bytes());
                kept.lines.extend_from_slice(line);
                kept.lines.push(b'\n');
            }
        }
        Ok((kept.lines != lines || kept.index != index).then_some(kept))
    }
}

/// what a shard holds once only the latest good record of each key `keep` takes is left.
struct Kept {
    lines: Vec<u8>,
    index: Vec<u8>,
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

/// an entry's key and where its line starts.
fn parse(entry: &[u8; ENTRY]) -> (u64, u32) {
    let (key, start) = entry.split_at(8);
    (
        u64::from_le_bytes(key.try_into().unwrap_or_default()),
        u32::from_le_bytes(start.try_into().unwrap_or_default()),
    )
}

/// the empty file a shard's writers lock, so that no lock ever covers its lines or index.
#[cfg(all(test, unix))]
fn lock_path(root: &Path, shard: usize) -> PathBuf {
    root.join("urls").join(format!("{shard:x}.lock"))
}

/// a shard's file of lines and its index in `urls/`.
#[cfg(test)]
fn paths(root: &Path, shard: usize) -> (PathBuf, PathBuf) {
    let folder = root.join("urls");
    let (records, entries) = names(shard);
    (folder.join(records), folder.join(entries))
}

/// the names of a shard's file of lines and its index in its folder.
fn names(shard: usize) -> (String, String) {
    (format!("{shard:x}.log"), format!("{shard:x}.idx"))
}

/// the `urls/` folder below the cache's `root`, reached without following a link.
#[cfg(all(test, unix))]
fn urls(root: &Path) -> io::Result<Folder> {
    Folder::existing(root, Path::new("urls"), Sharing::default())
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
        if read == 0 || line.len() + read > LINE_MOST {
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
            page: false,
        }
    }

    fn store() -> tempfile::TempDir {
        let folder = tempfile::tempdir().unwrap();
        fs::create_dir(folder.path().join("urls")).unwrap();
        folder
    }

    /// `count` links whose keys start with `start`, so their records share a shard.
    fn crowded(count: usize, start: &str) -> Vec<String> {
        // a search without end would hang rather than fail were hashing broken
        let urls: Vec<String> = (0..1 << 17)
            .map(|n| format!("https://example.org/{n}"))
            .filter(|url| digest::sha256(url.as_bytes()).hex().starts_with(start))
            .take(count)
            .collect();
        assert_eq!(urls.len(), count, "links whose keys start with {start}");
        urls
    }

    fn rewrite(shards: &Shards, root: &Path, keep: &dyn Fn(&UrlRecord) -> bool) {
        for shard in 0..SHARDS {
            shards.rewrite(root, root, shard, keep).unwrap();
        }
    }

    /// a record's line holds text a server chose, such as an etag, so a shard's file planted as a
    /// link to someone's file, as their shell's startup file, is never written through.
    #[cfg(unix)]
    #[test]
    fn a_save_never_writes_through_a_linked_record_file() {
        let url = "https://example.org/a";
        let (shard, _) = place(url);
        for linked in 0..3 {
            let folder = store();
            let root = folder.path();
            let (records, entries) = paths(root, shard);
            let path = [records, entries, lock_path(root, shard)][linked].clone();
            let outside = root.join("bashrc");
            fs::write(&outside, b"keep these bytes").unwrap();
            std::os::unix::fs::symlink(&outside, &path).unwrap();

            assert!(Shards::default().save(root, &record(url, 1)).is_err());

            assert_eq!(fs::read(&outside).unwrap(), b"keep these bytes", "{path:?}");
        }
    }

    /// `urls/` someone swapped for a link, as to a shell's folder of startup files, is never
    /// written through: the save fails and nothing appears there.
    #[cfg(unix)]
    #[test]
    fn a_save_never_writes_through_urls_swapped_for_a_link() {
        let folder = store();
        let root = folder.path();
        let outside = root.join("bashrc.d");
        fs::create_dir(&outside).unwrap();
        fs::remove_dir(root.join("urls")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("urls")).unwrap();

        let saved = Shards::default().save(root, &record("https://example.org/a", 1));

        assert!(saved.is_err());
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }

    /// a rewrite's new file takes a name no file had, so one planted at it is never written.
    #[cfg(unix)]
    #[test]
    fn a_rewrite_never_writes_through_a_planted_temporary_name() {
        let folder = store();
        let root = folder.path();
        let outside = root.join("bashrc");
        fs::write(&outside, b"keep these bytes").unwrap();
        fastrand::seed(7);
        let planted = root.join(format!("{:016x}.tmp", fastrand::u64(..)));
        std::os::unix::fs::symlink(&outside, &planted).unwrap();
        fastrand::seed(7);

        let scratch = Folder::existing(root, Path::new(""), Sharing::default()).unwrap();
        let replaced = urls(root).unwrap().replace("0.log", b"a line\n", &scratch);

        replaced.unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"keep these bytes");
        assert_eq!(fs::read(root.join("urls/0.log")).unwrap(), b"a line\n");
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

    /// a save goes through while other handles read the shard's lines and index, as other runs'
    /// lookups do. it locks a file of its own that holds nothing: windows locks keep every other
    /// handle off the bytes they cover, and refuse a handle opened only to append.
    #[test]
    fn a_save_leaves_its_shard_readable_while_it_holds_the_lock() {
        let folder = store();
        let root = folder.path();
        let urls = crowded(2, "0");
        let shards = Shards::default();
        shards.save(root, &record(&urls[0], 1)).unwrap();
        let (records, entries) = paths(root, 0);
        let other = Shards::default();
        let read = std::cell::Cell::new(false);
        let lock = |held: &File| {
            held.lock()?;
            assert_eq!(
                held.metadata()?.len(),
                0,
                "the lock is on a file of its own"
            );
            let mut lines = Vec::new();
            File::open(&records)?.read_to_end(&mut lines)?;
            let mut index = Vec::new();
            File::open(&entries)?.read_to_end(&mut index)?;
            assert_eq!((lines.is_empty(), index.len()), (false, ENTRY));
            assert_eq!(other.find(root, &urls[0]), Some(record(&urls[0], 1)));
            read.set(true);
            Ok(())
        };
        shards.save_with(root, &record(&urls[1], 2), &lock).unwrap();
        assert!(read.get());
        assert_eq!(other.find(root, &urls[1]), Some(record(&urls[1], 2)));
    }

    /// where the filesystem cannot lock, a save goes on without the lock, as runs sharing the
    /// cache do; any other lock error stops it before it writes.
    #[test]
    fn a_save_goes_on_unlocked_only_where_locking_is_unsupported() {
        let folder = store();
        let root = folder.path();
        let urls = crowded(2, "0");
        let shards = Shards::default();
        let unsupported = |_: &File| Err(io::ErrorKind::Unsupported.into());
        shards
            .save_with(root, &record(&urls[0], 1), &unsupported)
            .unwrap();
        assert_eq!(
            Shards::default().find(root, &urls[0]),
            Some(record(&urls[0], 1))
        );
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::Other] {
            let refused = |_: &File| Err(kind.into());
            let error = shards
                .save_with(root, &record(&urls[1], 2), &refused)
                .unwrap_err();
            assert_eq!(error.kind(), kind);
        }
        assert_eq!(Shards::default().find(root, &urls[1]), None);
        let lines = fs::read_to_string(paths(root, 0).0).unwrap();
        assert_eq!(
            lines.lines().count(),
            1,
            "a refused save writes nothing: {lines}"
        );
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
        assert_eq!(
            shards.emptied(root, 0, &|_| true).unwrap(),
            Vec::<PathBuf>::new()
        );
        assert_eq!(
            shards.emptied(root, 0, &|_| false).unwrap(),
            [records.clone(), entries.clone()],
            "a pretend gc lists what a real one removes"
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
