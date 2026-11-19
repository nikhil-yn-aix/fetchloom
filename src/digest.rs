//! SHA-256, a file's identity in the cache and in `data.lock`, MD5, which some sources state
//! and which is checked, never trusted as an identity, and git's blob ids, which a repository's
//! tree states.

use std::fmt;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

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
    pub(crate) fn of_reader(reader: impl Read, block: Block) -> io::Result<Sha256> {
        Self::of_reader_telling(reader, block, &|_| {})
    }

    /// hashes `reader` as [`Sha256::of_reader`] does, telling `read` the bytes of each block as
    /// it reads them, so a bar moves while a large file is hashed.
    pub(crate) fn of_reader_telling(
        mut reader: impl Read,
        mut block: Block,
        read: &dyn Fn(u64),
    ) -> io::Result<Sha256> {
        let mut hasher = Hasher::default();
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
        Block { bytes, start, len }
    }

    pub(crate) fn get(&mut self) -> &mut [u8] {
        &mut self.bytes[self.start..self.start + self.len]
    }
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
#[derive(Clone, Default)]
pub(crate) struct Hasher {
    sha256: sha2::Sha256,
    md5: Option<md5::Md5>,
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
        sha2::Digest::update(&mut self.sha256, bytes);
        if let Some(md5) = &mut self.md5 {
            md5.update(bytes);
        }
    }

    pub(crate) fn finish(self) -> (Sha256, Option<Md5>) {
        let sha256 = Sha256(sha2::Digest::finalize(self.sha256).into());
        (sha256, self.md5.map(|md5| Md5(md5.finalize().into())))
    }

    /// the SHA-256 of the bytes so far. the hasher goes on taking more.
    pub(crate) fn sha256_so_far(&self) -> Sha256 {
        Sha256(sha2::Digest::finalize(self.sha256.clone()).into())
    }
}

pub(crate) fn sha256(bytes: &[u8]) -> Sha256 {
    let mut hasher = Hasher::default();
    hasher.update(bytes);
    hasher.finish().0
}

/// the SHA-256 and count of the bytes `reader` holds with their line ends made the other kind, as
/// git converts them: each CRLF made LF, then each LF with no CR before it made CRLF.
pub(crate) fn with_other_line_ends(mut reader: impl Read) -> io::Result<[(Sha256, u64); 2]> {
    let (mut lf, mut crlf) = ((Hasher::default(), 0_u64), (Hasher::default(), 0_u64));
    let mut block = Block::sized(BLOCK as u64);
    let (mut to_lf, mut to_crlf) = (Vec::new(), Vec::new());
    // a CR waits for the next byte, which tells whether it ends a line
    let (mut held, mut last) = (false, 0_u8);
    loop {
        let buffer = block.get();
        let read = reader.read(buffer)?;
        if read == 0 {
            break;
        }
        to_lf.clear();
        to_crlf.clear();
        for &byte in &buffer[..read] {
            if std::mem::take(&mut held) && byte != b'\n' {
                to_lf.push(b'\r');
            }
            if byte == b'\r' {
                held = true;
            } else {
                to_lf.push(byte);
            }
            if byte == b'\n' && last != b'\r' {
                to_crlf.push(b'\r');
            }
            to_crlf.push(byte);
            last = byte;
        }
        lf.0.update(&to_lf);
        lf.1 += to_lf.len() as u64;
        crlf.0.update(&to_crlf);
        crlf.1 += to_crlf.len() as u64;
    }
    if held {
        lf.0.update(b"\r");
        lf.1 += 1;
    }
    Ok([(lf.0.finish().0, lf.1), (crlf.0.finish().0, crlf.1)])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a byte at a time, so every CR falls at the end of a read.
    struct Trickle<'a>(&'a [u8]);

    impl Read for Trickle<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let Some((first, rest)) = self.0.split_first() else {
                return Ok(0);
            };
            buffer[0] = *first;
            self.0 = rest;
            Ok(1)
        }
    }

    /// CRLF becomes LF and a lone LF becomes CRLF, either way the line ends git converts, while a
    /// lone CR stays, however the reads fall.
    #[test]
    fn other_line_ends_are_hashed_as_git_converts_them() {
        let held = b"a\r\nb\rc\r\r\nd\n\r";
        let expected = [
            (sha256(b"a\nb\rc\r\nd\n\r"), 10),
            (sha256(b"a\r\nb\rc\r\r\nd\r\n\r"), 13),
        ];
        for (name, read) in [
            ("at once", with_other_line_ends(&held[..])),
            ("a byte at a time", with_other_line_ends(Trickle(held))),
        ] {
            let read = read.expect("a slice reads");
            assert_eq!(read, expected, "{name}");
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
}
