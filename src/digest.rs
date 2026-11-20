//! SHA-256, a file's identity in the cache and in `data.lock`, MD5, which some sources state
//! and which is checked, never trusted as an identity, and git's blob ids, which a repository's
//! tree states.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use md5::Digest as _;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Sha256([u8; 32]);

/// the MD5 of a file's bytes, as zenodo, figshare and dataverse state it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Md5([u8; 16]);

/// the id git gives a file's bytes in a repository of SHA-1 ids: the SHA-1 of `blob`, the
/// bytes' count in decimal, a zero byte and the bytes, as a github or gitlab tree states it.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct BlobId([u8; 20]);

/// hashes a file's bytes as git does, for a count of bytes known before the first.
pub(crate) struct BlobHasher(sha1::Sha1);

impl Sha256 {
    /// reads 64 hex digits, in either case.
    pub(crate) fn from_hex(text: &str) -> Option<Sha256> {
        from_hex(text).map(Sha256)
    }

    pub(crate) fn hex(&self) -> String {
        hex(&self.0)
    }

    /// the first 12 hex digits, enough to name one hash in a log. two hashes may share them.
    pub(crate) fn short(&self) -> String {
        let mut text = self.hex();
        text.truncate(12);
        text
    }

    /// the digest as raw bytes, for hashes built from other hashes.
    pub(crate) fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// hashes a whole file, reading it once through `block`.
    pub(crate) fn of_file(path: &Path, block: Block) -> io::Result<Sha256> {
        Self::of_reader(File::open(path)?, block)
    }

    /// hashes an opened reader through `block`, so cache checks never reopen a checked path.
    pub(crate) fn of_reader(reader: impl Read + Send, block: Block) -> io::Result<Sha256> {
        Self::of_reader_telling(reader, block, &|_| {})
    }

    /// hashes `reader` as [`Sha256::of_reader`] does, telling `read` the bytes of each block as
    /// it reads them, so a bar moves while a large file is hashed.
    pub(crate) fn of_reader_telling(
        reader: impl Read + Send,
        block: Block,
        read: &dyn Fn(u64),
    ) -> io::Result<Sha256> {
        let ahead = block.reads_ahead();
        Self::of_reader_ahead(reader, block, read, ahead)
    }

    /// [`Sha256::of_reader_telling`], whose reads may go ahead on a second thread when `ahead`.
    fn of_reader_ahead(
        mut reader: impl Read + Send,
        mut block: Block,
        read: &dyn Fn(u64),
        ahead: bool,
    ) -> io::Result<Sha256> {
        let _hashing = Hashing::begin();
        let mut hasher = Hasher::default();
        if ahead {
            taken_ahead(block, &mut |buffer| reader.read(buffer), &mut |bytes| {
                hasher.update(bytes);
                read(bytes.len() as u64);
                std::ops::ControlFlow::<()>::Continue(())
            })?;
            return Ok(hasher.finish().0);
        }
        let buffer = block.get();
        loop {
            let bytes = reader.read(buffer)?;
            if bytes == 0 {
                return Ok(hasher.finish().0);
            }
            hasher.update(&buffer[..bytes]);
            read(bytes as u64);
        }
    }
}

/// the most one read of a file being hashed takes.
pub(crate) const BLOCK: usize = 1 << 20;

/// the boundary a [`Block`] starts at: a page on most systems.
pub(crate) const PAGE: usize = 4096;

/// a buffer a file is read into, a block at a time, to be hashed. it starts at a page.
pub(crate) struct Block {
    bytes: Vec<u8>,
    /// where the buffer starts in `bytes`: the first page boundary in it.
    start: usize,
    len: usize,
    /// the most bytes the read it was sized for takes.
    most: u64,
}

impl Block {
    /// a buffer for reading at most `most` bytes: no larger than that, nor than [`BLOCK`]. a
    /// file's `most` is its size and one byte, which shows a file that grew while it was read.
    pub(crate) fn sized(most: u64) -> Block {
        // zeroing a whole block for each small file cost more than hashing it
        let len = usize::try_from(most).map_or(BLOCK, |most| most.clamp(1, BLOCK));
        // a kernel copies into a buffer 16 bytes past a page, where glibc's allocator puts one,
        // several times slower on some cpus
        let bytes = vec![0; len + PAGE - 1];
        let start = bytes.as_ptr().align_offset(PAGE).min(PAGE - 1);
        Block {
            bytes,
            start,
            len,
            most,
        }
    }

    pub(crate) fn get(&mut self) -> &mut [u8] {
        &mut self.bytes[self.start..self.start + self.len]
    }

    /// whether the read this block was sized for goes ahead on a second thread.
    pub(crate) fn reads_ahead(&self) -> bool {
        reads_ahead(self.most)
    }
}

/// the least a hash reads for its reads to go ahead on a second thread: below it, starting the
/// thread costs more than the overlap of reading and hashing saves.
pub(crate) const AHEAD_FROM: u64 = 4 << 20;

/// the buffers a read ahead turns through, the caller's own and one more: one being hashed while
/// the other is read.
const AHEAD_BUFFERS: usize = 2;

/// the most reading threads hashes run at once: one, which starts only beside no other hash. tests
/// in one process share the count, so theirs is high enough that none is refused for another's.
const AHEAD_READERS: usize = if cfg!(test) { 256 } else { 1 };

/// the reading threads hashes run now, at most [`AHEAD_READERS`].
static READERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// one of a count of reading threads, given back when dropped.
struct Reader<'a>(&'a std::sync::atomic::AtomicUsize);

impl Reader<'_> {
    /// one more of `readers`, while fewer than `most` are taken.
    fn take(readers: &std::sync::atomic::AtomicUsize, most: usize) -> Option<Reader<'_>> {
        readers
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |taken| {
                (taken < most).then_some(taken + 1)
            })
            .ok()
            .map(|_| Reader(readers))
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// whether this run's hashes of large files read ahead, set once as a command starts, before it
/// hashes anything.
static AHEAD: AtomicBool = AtomicBool::new(false);

/// reads each large file or range a hash takes on a second thread, a block ahead of the hash, once
/// it is the only hash in progress, for the rest of the run.
pub(crate) fn read_ahead() {
    AHEAD.store(true, Ordering::Relaxed);
}

/// whether a hash of `most` bytes may read ahead on a second thread.
pub(crate) fn reads_ahead(most: u64) -> bool {
    most >= AHEAD_FROM && AHEAD.load(Ordering::Relaxed)
}

/// the hashes that count themselves while reads ahead are on: those in progress, and those batches
/// hold that no worker has begun.
struct Counts {
    hashing: std::sync::atomic::AtomicUsize,
    queued: std::sync::atomic::AtomicUsize,
}

impl Counts {
    const fn new() -> Counts {
        Counts {
            hashing: std::sync::atomic::AtomicUsize::new(0),
            queued: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// whether the hash asking is the only one in progress, with none queued.
    fn alone(&self) -> bool {
        self.hashing.load(Ordering::Relaxed) == 1 && self.queued.load(Ordering::Relaxed) == 0
    }
}

/// this run's counts of hashes.
static COUNTS: Counts = Counts::new();

/// a hash in progress, counted while reads ahead are on, until dropped: a large hash reads ahead
/// only while no other is counted.
pub(crate) struct Hashing<'c>(Option<&'c Counts>);

impl Hashing<'static> {
    pub(crate) fn begin() -> Hashing<'static> {
        Hashing::begin_in(&COUNTS, AHEAD.load(Ordering::Relaxed))
    }
}

impl<'c> Hashing<'c> {
    fn begin_in(counts: &'c Counts, on: bool) -> Hashing<'c> {
        if !on {
            return Hashing(None);
        }
        counts.hashing.fetch_add(1, Ordering::Relaxed);
        if READERS.load(Ordering::Relaxed) > 0 {
            tracing::debug!("a hash began beside a reading thread");
        }
        Hashing(Some(counts))
    }
}

impl Drop for Hashing<'_> {
    fn drop(&mut self) {
        if let Some(counts) = self.0 {
            counts.hashing.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// the hashes of a batch that its workers have not begun, counted while reads ahead are on, until
/// dropped: no large hash reads ahead while any is queued.
pub(crate) struct Queued<'c> {
    counts: Option<&'c Counts>,
    left: std::sync::atomic::AtomicUsize,
}

impl Queued<'static> {
    /// a batch of `count` hashes, none begun.
    pub(crate) fn of(count: usize) -> Queued<'static> {
        Queued::of_in(&COUNTS, AHEAD.load(Ordering::Relaxed), count)
    }
}

impl<'c> Queued<'c> {
    fn of_in(counts: &'c Counts, on: bool, count: usize) -> Queued<'c> {
        let count = if on { count } else { 0 };
        if on {
            counts.queued.fetch_add(count, Ordering::Relaxed);
        }
        Queued {
            counts: on.then_some(counts),
            left: std::sync::atomic::AtomicUsize::new(count),
        }
    }

    /// one of the batch's hashes begun.
    pub(crate) fn begun(&self) {
        if let Some(counts) = self.counts
            && self
                .left
                .try_update(Ordering::Relaxed, Ordering::Relaxed, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            counts.queued.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl Drop for Queued<'_> {
    fn drop(&mut self) {
        if let Some(counts) = self.counts {
            counts
                .queued
                .fetch_sub(*self.left.get_mut(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// whether a test's hashes on its thread count as alone, so a test runs its reads ahead or
    /// in turn whatever other tests hash beside it.
    static ALONE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// a test's say over whether its thread's hashes are alone, until dropped.
#[cfg(test)]
pub(crate) struct AloneInTests;

#[cfg(test)]
impl AloneInTests {
    pub(crate) fn set(alone: bool) -> AloneInTests {
        ALONE.set(Some(alone));
        AloneInTests
    }
}

#[cfg(test)]
impl Drop for AloneInTests {
    fn drop(&mut self) {
        ALONE.set(None);
    }
}

/// whether the hash asking is the only one in progress, with none queued.
fn alone() -> bool {
    #[cfg(test)]
    if let Some(alone) = ALONE.get() {
        return alone;
    }
    COUNTS.alone()
}

/// whether a read ahead was told in the debug log yet, once a run.
static TOLD_AHEAD: std::sync::Once = std::sync::Once::new();

/// when a hash may hand its reads to a second thread: while `alone` says so and one of `readers`
/// is free, of `most`.
struct Admission<'a> {
    alone: &'a dyn Fn() -> bool,
    readers: &'a std::sync::atomic::AtomicUsize,
    most: usize,
}

/// hands `take` the blocks `fill` reads into `block`, in order, until `fill` reads nothing or fails
/// or `take` breaks with a value, as one loop of reading and hashing would. the reads go in turn on
/// this thread until this hash is the only one in progress, with none queued, and the reading
/// thread is free; then the rest go a block ahead on that thread. so a lone large file gets the
/// thread after its first block, and the large files of a batch never, until the last runs alone.
/// a thread once started reads to the end, whatever hashes begin after it. one the system refuses
/// leaves the rest in turn.
pub(crate) fn taken_ahead<B>(
    block: Block,
    fill: &mut (dyn FnMut(&mut [u8]) -> io::Result<usize> + Send),
    take: &mut dyn FnMut(&[u8]) -> std::ops::ControlFlow<B>,
) -> io::Result<Option<B>> {
    let admission = Admission {
        alone: &alone,
        readers: &READERS,
        most: AHEAD_READERS,
    };
    taken_ahead_when(block, fill, take, &admission)
}

fn taken_ahead_when<B>(
    mut block: Block,
    fill: &mut (dyn FnMut(&mut [u8]) -> io::Result<usize> + Send),
    take: &mut dyn FnMut(&[u8]) -> std::ops::ControlFlow<B>,
    admission: &Admission<'_>,
) -> io::Result<Option<B>> {
    let mut refused = false;
    loop {
        if !refused
            && (admission.alone)()
            && let Some(_reader) = Reader::take(admission.readers, admission.most)
        {
            match read_ahead_with(block, fill, take) {
                Ok(taken) => return taken,
                Err(back) => {
                    block = back;
                    refused = true;
                }
            }
        }
        match fill(block.get()) {
            Err(err) => return Err(err),
            Ok(0) => return Ok(None),
            Ok(read) => {
                if let std::ops::ControlFlow::Break(value) = take(&block.get()[..read]) {
                    return Ok(Some(value));
                }
            }
        }
    }
}

fn read_ahead_with<B>(
    first: Block,
    fill: &mut (dyn FnMut(&mut [u8]) -> io::Result<usize> + Send),
    take: &mut dyn FnMut(&[u8]) -> std::ops::ControlFlow<B>,
) -> Result<io::Result<Option<B>>, Block> {
    let (to_hash, read_blocks) = std::sync::mpsc::sync_channel::<(Block, io::Result<usize>)>(1);
    let (to_reader, spares) = std::sync::mpsc::sync_channel::<Block>(AHEAD_BUFFERS);
    std::thread::scope(|scope| {
        let reader = crate::threads::scoped(scope, move || {
            while let Ok(mut block) = spares.recv() {
                let read = fill(block.get());
                let last = !matches!(read, Ok(1..));
                if to_hash.send((block, read)).is_err() || last {
                    break;
                }
            }
        });
        if !reader.is_threaded() {
            return Err(first);
        }
        TOLD_AHEAD.call_once(|| tracing::debug!("a hash reads its file a block ahead"));
        let most = first.most;
        let _ = to_reader.send(first);
        for _ in 1..AHEAD_BUFFERS {
            let _ = to_reader.send(Block::sized(most));
        }
        let taken = loop {
            // a reader gone without its last block panicked, which its join raises
            let Ok((mut block, read)) = read_blocks.recv() else {
                break Ok(None);
            };
            match read {
                Err(err) => break Err(err),
                Ok(0) => break Ok(None),
                Ok(bytes) => {
                    if let std::ops::ControlFlow::Break(value) = take(&block.get()[..bytes]) {
                        break Ok(Some(value));
                    }
                    let _ = to_reader.send(block);
                }
            }
        };
        // closing both ends wakes a reader waiting for a buffer or for room in the queue
        drop((to_reader, read_blocks));
        reader.join();
        Ok(taken)
    })
}

impl Md5 {
    /// reads 32 hex digits, in either case.
    pub(crate) fn from_hex(text: &str) -> Option<Md5> {
        from_hex(text).map(Md5)
    }

    pub(crate) fn hex(&self) -> String {
        hex(&self.0)
    }
}

/// an MD5 taken as bytes stream past, for bytes a record says have one.
#[derive(Default)]
pub(crate) struct Md5Hasher(md5::Md5);

impl Md5Hasher {
    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub(crate) fn finish(self) -> Md5 {
        Md5(self.0.finalize().into())
    }
}

impl BlobId {
    /// reads 40 hex digits, in either case.
    pub(crate) fn from_hex(text: &str) -> Option<BlobId> {
        from_hex(text).map(BlobId)
    }

    pub(crate) fn hex(&self) -> String {
        hex(&self.0)
    }

    /// the id of `bytes`.
    pub(crate) fn of_bytes(bytes: &[u8]) -> BlobId {
        let mut hasher = BlobHasher::new(bytes.len() as u64);
        hasher.update(bytes);
        hasher.finish()
    }

    /// the id of the pointer git-lfs puts in a repository for a file of `size` bytes hashing to
    /// `sha256`, which it keeps apart.
    pub(crate) fn of_lfs_pointer(sha256: &Sha256, size: u64) -> BlobId {
        let pointer = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {size}\n",
            sha256.hex()
        );
        Self::of_bytes(pointer.as_bytes())
    }
}

impl BlobHasher {
    /// a hasher for a file of `size` bytes; bytes of another count give another id.
    pub(crate) fn new(size: u64) -> BlobHasher {
        let mut hasher = sha1::Sha1::default();
        sha1::Digest::update(&mut hasher, format!("blob {size}\0").as_bytes());
        BlobHasher(hasher)
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        sha1::Digest::update(&mut self.0, bytes);
    }

    pub(crate) fn finish(self) -> BlobId {
        BlobId(sha1::Digest::finalize(self.0).into())
    }
}

fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let bytes = text.as_bytes();
    if bytes.len() != 2 * N {
        return None;
    }
    let mut out = [0u8; N];
    for (index, [high, low]) in bytes.as_chunks::<2>().0.iter().enumerate() {
        let high = char::from(*high).to_digit(16)?;
        let low = char::from(*low).to_digit(16)?;
        out[index] = u8::try_from(high * 16 + low).ok()?;
    }
    Some(out)
}

/// lowercase hex digits for `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

macro_rules! text_forms {
    ($type:ident, $name:literal, $digits:literal) => {
        impl Serialize for $type {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.hex())
            }
        }

        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<$type, D::Error> {
                let text = String::deserialize(deserializer)?;
                $type::from_hex(&text).ok_or_else(|| {
                    D::Error::custom(concat!("an ", $name, " is ", $digits, " hex digits"))
                })
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.hex())
            }
        }

        impl fmt::Debug for $type {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!($name, ":{}"), self.hex())
            }
        }
    };
}

text_forms!(Sha256, "SHA-256", "64");
text_forms!(Md5, "MD5", "32");
impl fmt::Display for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

impl fmt::Debug for BlobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "git blob {}", self.hex())
    }
}

/// hashes computed while bytes stream past: always SHA-256, and MD5 when one will be checked.
/// SHA-256 goes through ring, which finds the processor's SHA instructions on every system;
/// sha2 finds them through cpufeatures, which cannot see them on arm64 windows.
#[derive(Clone)]
pub(crate) struct Hasher {
    sha256: ring::digest::Context,
    md5: Option<md5::Md5>,
}

impl Default for Hasher {
    fn default() -> Hasher {
        Hasher {
            sha256: ring::digest::Context::new(&ring::digest::SHA256),
            md5: None,
        }
    }
}

impl Hasher {
    pub(crate) fn with_md5() -> Hasher {
        Hasher {
            md5: Some(md5::Md5::default()),
            ..Hasher::default()
        }
    }

    /// separates the optional MD5 state, including any prefix already hashed.
    pub(crate) fn take_md5(&mut self) -> Option<md5::Md5> {
        self.md5.take()
    }

    /// puts back the MD5 state once its lane has joined the SHA-256 prefix.
    pub(crate) fn restore_md5(&mut self, state: md5::Md5) {
        self.md5 = Some(state);
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.sha256.update(bytes);
        if let Some(md5) = &mut self.md5 {
            md5.update(bytes);
        }
    }

    pub(crate) fn finish(self) -> (Sha256, Option<Md5>) {
        let sha256 = finished(self.sha256);
        (sha256, self.md5.map(|md5| Md5(md5.finalize().into())))
    }

    /// the SHA-256 of the bytes so far. the hasher goes on taking more.
    pub(crate) fn sha256_so_far(&self) -> Sha256 {
        finished(self.sha256.clone())
    }
}

fn finished(context: ring::digest::Context) -> Sha256 {
    let mut digest = [0; 32];
    digest.copy_from_slice(context.finish().as_ref());
    Sha256(digest)
}

pub(crate) fn sha256(bytes: &[u8]) -> Sha256 {
    let mut hasher = Hasher::default();
    hasher.update(bytes);
    hasher.finish().0
}

/// the SHA-256 of the `held` bytes `file` holds with their line ends made the other kind, as git
/// converts a whole file, when that leaves `wanted` bytes: each CRLF made LF where `held` is more,
/// in a file with no LF alone, as git's checkout writes one, else each LF made CRLF, in a file with
/// no CRLF, as git keeps one. none where the two are one count, so no line end differs, and none
/// where the file's line ends are not such ones or their count is not the difference: a first read
/// counts them, which costs no hashing, so only a file that passes is read again and hashed.
pub(crate) fn with_line_ends_made(
    mut file: impl Read + io::Seek,
    held: u64,
    wanted: u64,
) -> io::Result<Option<Sha256>> {
    let to_lf = match held.cmp(&wanted) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => return Ok(None),
    };
    // each line end made the other kind takes a byte off or adds one
    if !line_ends_fit(&mut file, to_lf, held.abs_diff(wanted))? {
        return Ok(None);
    }
    file.rewind()?;
    let (mut hasher, mut made) = (Hasher::default(), 0_u64);
    let mut block = Block::sized(BLOCK as u64);
    let mut converted = Vec::new();
    // a CR waits for the next byte, which tells whether it ends a line
    let mut cr = false;
    loop {
        let buffer = block.get();
        let read = file.read(buffer)?;
        if read == 0 {
            break;
        }
        converted.clear();
        for &byte in &buffer[..read] {
            if !to_lf {
                if byte == b'\n' {
                    converted.push(b'\r');
                }
                converted.push(byte);
            } else if byte == b'\r' {
                if std::mem::replace(&mut cr, true) {
                    converted.push(b'\r');
                }
            } else {
                if std::mem::take(&mut cr) && byte != b'\n' {
                    converted.push(b'\r');
                }
                converted.push(byte);
            }
        }
        hasher.update(&converted);
        made += converted.len() as u64;
    }
    if cr {
        hasher.update(b"\r");
        made += 1;
    }
    Ok((made == wanted).then(|| hasher.finish().0))
}

/// whether `reader` holds just `ends` line ends, each a CRLF where `crlf`, else each an LF with no
/// CR before it, and none of the other kind, as git leaves a whole file it converted.
fn line_ends_fit(reader: &mut impl Read, crlf: bool, ends: u64) -> io::Result<bool> {
    let mut block = Block::sized(BLOCK as u64);
    let (mut count, mut last) = (0_u64, 0_u8);
    loop {
        let buffer = block.get();
        let read = reader.read(buffer)?;
        let Some(&end) = buffer[..read].last() else {
            return Ok(count == ends);
        };
        for (at, &byte) in buffer[..read].iter().enumerate() {
            if byte == b'\n' {
                let before = at.checked_sub(1).map_or(last, |before| buffer[before]);
                if (before == b'\r') != crlf {
                    return Ok(false);
                }
                count += 1;
            }
        }
        if count > ends {
            return Ok(false);
        }
        last = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a byte at a time, so every CR falls at the end of a read, read again from the start once
    /// rewound.
    struct Trickle<'a>(&'a [u8], usize);

    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some(byte) = self.0.get(self.1) else {
                return Ok(0);
            };
            buffer[0] = *byte;
            self.1 += 1;
            Ok(1)
        }
    }

    impl io::Seek for Trickle<'_> {
        fn seek(&mut self, to: io::SeekFrom) -> io::Result<u64> {
            assert_eq!(to, io::SeekFrom::Start(0), "only a rewind");
            self.1 = 0;
            Ok(0)
        }
    }

    /// a file git's checkout made CRLF hashes as its LF bytes, and one git kept as LF as its CRLF
    /// bytes, a lone CR staying, however the reads fall; sizes that match, a line end of the kind
    /// git leaves none of, or more line ends than the sizes allow give none.
    #[test]
    fn line_ends_are_made_the_other_kind_as_git_converts_them() {
        let crlf = b"a\r\nb\rc\r\r\nd\r\n\r";
        let lf = b"a\nb\rc\nd\n\r";
        for (held, wanted, made) in [
            (&crlf[..], 10, Some(sha256(b"a\nb\rc\r\nd\n\r"))),
            (&lf[..], 12, Some(sha256(b"a\r\nb\rc\r\nd\r\n\r"))),
            (b"a\r\nb\n", 4, None),
            (b"a\nb\r\n", 7, None),
            (b"a\r\nb\r\n", 5, None),
            (b"a\nb\n", 4, None),
        ] {
            let size = held.len() as u64;
            for (name, read) in [
                (
                    "at once",
                    with_line_ends_made(io::Cursor::new(held), size, wanted),
                ),
                (
                    "a byte at a time",
                    with_line_ends_made(Trickle(held, 0), size, wanted),
                ),
            ] {
                let read = read.expect("a slice reads");
                assert_eq!(read, made, "{name}: {:?}", String::from_utf8_lossy(held));
            }
        }
    }

    #[test]
    fn match_the_published_test_vectors() {
        let mut hasher = Hasher::with_md5();
        hasher.update(b"abc");
        let (sha256, md5) = hasher.finish();
        assert_eq!(
            sha256.hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            md5.map(|md5| md5.hex()).as_deref(),
            Some("900150983cd24fb0d6963f7d28e17f72")
        );
        assert_eq!(
            Hasher::default().finish().1,
            None,
            "md5 costs nothing unless asked for"
        );
    }

    /// the published digests, and sha2's digest of bytes fed in pieces that straddle SHA-256's
    /// 64-byte blocks, and the digest so far partway through.
    #[test]
    fn hashes_to_the_published_sha256_and_sha2s() {
        let million: Vec<u8> = vec![b'a'; 1_000_000];
        let published: [(&[u8], &str); 4] = [
            (
                b"",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc",
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq",
                "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
            ),
            (
                &million,
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            ),
        ];
        for (bytes, digest) in published {
            let mut hasher = Hasher::default();
            hasher.update(bytes);
            assert_eq!(hasher.finish().0.hex(), digest);
        }
        let bytes: Vec<u8> = (0..=250).cycle().take((1 << 20) + 3).collect();
        for size in [55, 56, 63, 64, 65, 127, 128, 129, bytes.len()] {
            let mut hasher = Hasher::default();
            let mut fed = 0;
            for piece in [1, 6, 57, 63, 64, 65, 4096].into_iter().cycle() {
                let end = (fed + piece).min(size);
                hasher.update(&bytes[fed..end]);
                fed = end;
                if fed == size {
                    break;
                }
            }
            let sha2: [u8; 32] = <sha2::Sha256 as sha2::Digest>::digest(&bytes[..size]).into();
            assert_eq!(hasher.finish().0.bytes(), &sha2, "{size}");
        }
        let mut hasher = Hasher::default();
        hasher.update(b"ab");
        assert_eq!(
            hasher.sha256_so_far().hex(),
            "fb8e20fc2e4c3f248c60c39bd652f3c1347298bb977b8b4d5903b85055620603"
        );
        hasher.update(b"c");
        assert_eq!(hasher.finish().0.hex(), published[1].1);
    }

    #[test]
    fn hashing_tells_the_bytes_of_each_block_it_reads() {
        let block = 1024 * 1024;
        let bytes = vec![7; 2 * block + 3];
        let told = std::cell::RefCell::new(Vec::new());
        let most = bytes.len() as u64 + 1;
        let digest = Sha256::of_reader_telling(bytes.as_slice(), Block::sized(most), &|read| {
            told.borrow_mut().push(read);
        })
        .unwrap();
        assert_eq!(digest, sha256(&bytes));
        let block = block as u64;
        assert_eq!(told.into_inner(), [block, block, 3]);
    }

    /// ids `git hash-object` gives: a file, an empty one, and the pointer git-lfs keeps for the
    /// file's bytes, which only their SHA-256 and size make.
    #[test]
    fn blob_ids_match_the_ids_git_gives() {
        let rows = b"site,count\na,1\nb,2\n";
        for (bytes, id) in [
            (&b"hello\n"[..], "ce013625030ba8dba906f756967f9e9ca394464a"),
            (b"", "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"),
            (rows, "ab6cbfe20b8e805f6b37252a0cbdc2f1c5dc2cdf"),
        ] {
            assert_eq!(BlobId::of_bytes(bytes).hex(), id);
        }
        assert_eq!(
            BlobId::of_lfs_pointer(&sha256(rows), rows.len() as u64).hex(),
            "ae226b637685f1fbb6726d2f2c0ed20550b87c04"
        );
        let mut split = BlobHasher::new(rows.len() as u64);
        split.update(&rows[..4]);
        split.update(&rows[4..]);
        assert_eq!(split.finish(), BlobId::of_bytes(rows));
    }

    #[test]
    fn hex_round_trips_and_ignores_case() {
        let digest = sha256(b"fetchloom");
        assert_eq!(Sha256::from_hex(&digest.hex()), Some(digest));
        assert_eq!(Sha256::from_hex(&digest.hex().to_uppercase()), Some(digest));
        let md5 = Md5::from_hex("900150983CD24FB0D6963F7D28E17F72").unwrap();
        assert_eq!(md5.hex(), "900150983cd24fb0d6963f7d28e17f72");
    }

    #[test]
    fn rejects_anything_but_the_right_number_of_hex_digits() {
        assert_eq!(Sha256::from_hex("abc"), None);
        assert_eq!(Sha256::from_hex(&"g".repeat(64)), None);
        assert_eq!(Md5::from_hex(&"a".repeat(64)), None);
        assert_eq!(Sha256::from_hex(&"é".repeat(32)), None);
    }

    /// a file's buffer holds its size and one byte, at least one byte and at most a block.
    #[test]
    fn a_block_holds_the_file_and_one_byte_up_to_a_mebibyte() {
        for (most, len) in [
            (0, 1),
            (1, 1),
            (4097, 4097),
            (BLOCK as u64, BLOCK),
            (BLOCK as u64 + 1, BLOCK),
            (u64::MAX, BLOCK),
        ] {
            assert_eq!(Block::sized(most).get().len(), len, "{most}");
        }
    }

    /// every block starts at a page, whatever its size.
    #[test]
    fn a_block_starts_at_a_page() {
        for most in [1, 4097, u64::MAX] {
            assert_eq!(Block::sized(most).get().as_ptr().addr() % PAGE, 0, "{most}");
        }
    }

    /// a buffer smaller than the bytes, or larger, hashes them the same as one call over all.
    #[test]
    fn every_block_size_hashes_a_reader_the_same() {
        let bytes: Vec<u8> = (0..=250).cycle().take(3 * BLOCK + 5).collect();
        for size in [0, 1, 4096, BLOCK - 1, BLOCK, BLOCK + 1, 3 * BLOCK + 5] {
            let bytes = &bytes[..size];
            for block in [Block::sized(size as u64 + 1), Block::sized(1)] {
                assert_eq!(
                    Sha256::of_reader(bytes, block).unwrap(),
                    sha256(bytes),
                    "{size}"
                );
            }
        }
    }

    /// a reader that hands out at most the next of `sizes`, in turn, so blocks end in odd places,
    /// and fails once `fail_at` bytes are read.
    struct Uneven<'a> {
        bytes: &'a [u8],
        at: usize,
        sizes: std::iter::Cycle<std::slice::Iter<'a, usize>>,
        fail_at: Option<usize>,
    }

    impl<'a> Uneven<'a> {
        fn new(bytes: &'a [u8], sizes: &'a [usize], fail_at: Option<usize>) -> Uneven<'a> {
            Uneven {
                bytes,
                at: 0,
                sizes: sizes.iter().cycle(),
                fail_at,
            }
        }
    }

    impl Read for Uneven<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.fail_at.is_some_and(|fail_at| self.at >= fail_at) {
                return Err(io::Error::other("the disk went away"));
            }
            let size = (*self.sizes.next().unwrap())
                .min(buffer.len())
                .min(self.bytes.len() - self.at);
            buffer[..size].copy_from_slice(&self.bytes[self.at..self.at + size]);
            self.at += size;
            Ok(size)
        }
    }

    /// reading ahead on a second thread makes the reads reading in turn makes, so it hashes the same
    /// bytes and tells them in the same blocks, whatever the size and wherever the reads end.
    #[test]
    fn a_hash_read_ahead_takes_the_blocks_reading_in_turn_takes() {
        let _alone = AloneInTests::set(true);
        let large = usize::try_from(AHEAD_FROM).unwrap();
        let bytes: Vec<u8> = (0..=250).cycle().take(large + 3 * BLOCK + 7).collect();
        for size in [
            0,
            1,
            BLOCK - 1,
            BLOCK,
            BLOCK + 1,
            large - 1,
            large,
            bytes.len(),
        ] {
            for sizes in [&[BLOCK][..], &[1, 4096, 65_537, BLOCK, 7]] {
                let mut seen = Vec::new();
                for ahead in [false, true] {
                    let told = std::cell::RefCell::new(Vec::new());
                    let digest = Sha256::of_reader_ahead(
                        Uneven::new(&bytes[..size], sizes, None),
                        Block::sized(size as u64 + 1),
                        &|read| told.borrow_mut().push(read),
                        ahead,
                    )
                    .unwrap();
                    assert_eq!(digest, sha256(&bytes[..size]), "{size} {ahead}");
                    seen.push(told.into_inner());
                }
                assert_eq!(seen[0], seen[1], "{size}");
            }
        }
    }

    /// a read that fails mid-file fails the hash read ahead too, once the blocks read before it
    /// are taken, as reading in turn takes them.
    #[test]
    fn a_hash_read_ahead_fails_where_reading_in_turn_fails() {
        let _alone = AloneInTests::set(true);
        let bytes = vec![9_u8; 6 * BLOCK];
        for ahead in [false, true] {
            let told = std::cell::Cell::new(0);
            let failed = Sha256::of_reader_ahead(
                Uneven::new(&bytes, &[BLOCK], Some(5 * BLOCK)),
                Block::sized(bytes.len() as u64 + 1),
                &|read| told.set(told.get() + read),
                ahead,
            )
            .unwrap_err();
            assert_eq!(failed.to_string(), "the disk went away", "{ahead}");
            assert_eq!(told.get(), 5 * BLOCK as u64, "{ahead}");
        }
    }

    /// a reading thread that cannot start leaves every read to the hash's own thread, which asks for
    /// no other thread for the rest of the hash.
    #[test]
    fn a_read_ahead_that_cannot_start_reads_in_turn() {
        let _alone = AloneInTests::set(true);
        let bytes = vec![3_u8; 5 * BLOCK];
        let _refused = crate::threads::refuse_after(0);
        let here = std::thread::current().id();
        let mut fills = Vec::new();
        let mut input = bytes.as_slice();
        let taken = taken_ahead(
            Block::sized(bytes.len() as u64 + 1),
            &mut |buffer| {
                fills.push(std::thread::current().id());
                input.read(buffer)
            },
            &mut |_| std::ops::ControlFlow::<()>::Continue(()),
        );
        assert_eq!(taken.unwrap(), None);
        assert_eq!(fills, [here; 6]);
        let digest = Sha256::of_reader_ahead(
            bytes.as_slice(),
            Block::sized(bytes.len() as u64 + 1),
            &|_| {},
            true,
        )
        .unwrap();
        assert_eq!(digest, sha256(&bytes));
    }

    /// a taker that stops ends the reading thread, which read no more than its buffers hold past
    /// the block taken.
    #[test]
    fn a_read_ahead_stops_reading_once_its_taker_stops() {
        let _alone = AloneInTests::set(true);
        let reads = std::sync::atomic::AtomicUsize::new(0);
        let taken = taken_ahead(
            Block::sized(4096),
            &mut |buffer| {
                reads.fetch_add(1, Ordering::Relaxed);
                buffer.fill(1);
                Ok(buffer.len())
            },
            &mut |block| std::ops::ControlFlow::Break(block.len()),
        );
        assert_eq!(taken.unwrap(), Some(4096));
        assert!(reads.load(Ordering::Relaxed) <= AHEAD_BUFFERS);
    }

    /// a hash reads in turn while it is not alone and hands the rest of its reads to a second
    /// thread at the first block where it is, the bytes taken whole and in order; once handed, the
    /// thread reads to the end though the hash stops being alone, as when another begins beside it,
    /// and its place is free again after.
    #[test]
    fn a_hash_hands_its_reads_over_partway_and_keeps_them_handed() {
        let bytes: Vec<u8> = (0..=250).cycle().take(6 * BLOCK + 3).collect();
        let here = std::thread::current().id();
        // the reads made before the hash is alone, and those after which it stops being alone
        for (from, until) in [(2, None), (0, Some(2)), (3, Some(4))] {
            let readers = std::sync::atomic::AtomicUsize::new(0);
            let made = std::sync::atomic::AtomicUsize::new(0);
            let alone = || {
                let made = made.load(Ordering::Relaxed);
                made >= from && until.is_none_or(|until| made < until)
            };
            let admission = Admission {
                alone: &alone,
                readers: &readers,
                most: 1,
            };
            let mut input = bytes.as_slice();
            let mut fills = Vec::new();
            let mut taken = Vec::new();
            let ended = taken_ahead_when(
                Block::sized(bytes.len() as u64 + 1),
                &mut |buffer| {
                    fills.push(std::thread::current().id());
                    let read = input.read(buffer);
                    made.fetch_add(1, Ordering::Relaxed);
                    read
                },
                &mut |block| {
                    taken.extend_from_slice(block);
                    std::ops::ControlFlow::<()>::Continue(())
                },
                &admission,
            );
            assert_eq!(ended.unwrap(), None);
            assert_eq!(taken, bytes, "{from} {until:?}");
            assert_eq!(fills.len(), 8, "{from} {until:?}");
            assert!(
                fills[..from].iter().all(|id| *id == here),
                "{from} {until:?}"
            );
            assert!(
                fills[from..].iter().all(|id| *id != here),
                "{from} {until:?}"
            );
            assert_eq!(readers.load(Ordering::Relaxed), 0);
        }
    }

    /// the reading thread's place is given back however a read ahead ends: at the end, at a failed
    /// read, and at a taker that stops, so the next large hash may take it.
    #[test]
    fn a_read_ahead_gives_its_place_back_however_it_ends() {
        let bytes = vec![5_u8; 4 * BLOCK];
        let readers = std::sync::atomic::AtomicUsize::new(0);
        let admission = Admission {
            alone: &|| true,
            readers: &readers,
            most: 1,
        };
        for end in ["whole", "failed", "stopped"] {
            let mut input = Uneven::new(&bytes, &[BLOCK], (end == "failed").then_some(2 * BLOCK));
            let ended = taken_ahead_when(
                Block::sized(bytes.len() as u64 + 1),
                &mut |buffer| input.read(buffer),
                &mut |block| {
                    if end == "stopped" {
                        std::ops::ControlFlow::Break(block.len())
                    } else {
                        std::ops::ControlFlow::Continue(())
                    }
                },
                &admission,
            );
            match end {
                "whole" => assert_eq!(ended.unwrap(), None),
                "failed" => assert_eq!(ended.unwrap_err().to_string(), "the disk went away"),
                _ => assert_eq!(ended.unwrap(), Some(BLOCK)),
            }
            assert_eq!(readers.load(Ordering::Relaxed), 0, "{end}");
        }
    }

    /// a hash is alone while it is the only one counted in progress and no batch holds one not
    /// begun; a batch's hashes, once begun or let go, count no more, and nothing counts while
    /// reads ahead are off.
    #[test]
    fn a_hash_is_alone_while_no_other_is_in_progress_or_queued() {
        let counts = Counts::new();
        let first = Hashing::begin_in(&counts, true);
        assert!(counts.alone());
        let second = Hashing::begin_in(&counts, true);
        assert!(!counts.alone());
        drop(second);
        assert!(counts.alone());
        let batch = Queued::of_in(&counts, true, 2);
        assert!(!counts.alone());
        batch.begun();
        assert!(!counts.alone());
        batch.begun();
        batch.begun();
        assert!(counts.alone());
        drop(batch);
        let unbegun = Queued::of_in(&counts, true, 3);
        unbegun.begun();
        assert!(!counts.alone());
        drop(unbegun);
        assert!(counts.alone());
        drop(first);
        assert!(!counts.alone());
        let off = (
            Hashing::begin_in(&counts, false),
            Queued::of_in(&counts, false, 5),
        );
        off.1.begun();
        assert_eq!(
            (
                counts.hashing.load(Ordering::Relaxed),
                counts.queued.load(Ordering::Relaxed)
            ),
            (0, 0)
        );
    }

    /// no more reading threads run at once than the count allows; one that ends frees its place.
    #[test]
    fn reading_threads_stay_within_their_count() {
        let readers = std::sync::atomic::AtomicUsize::new(0);
        let first = Reader::take(&readers, 2).unwrap();
        let second = Reader::take(&readers, 2).unwrap();
        assert!(Reader::take(&readers, 2).is_none());
        drop(first);
        let third = Reader::take(&readers, 2);
        assert!(third.is_some());
        drop((second, third));
        assert_eq!(readers.load(Ordering::Relaxed), 0);
    }
}
