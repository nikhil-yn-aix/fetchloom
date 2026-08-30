//! Byte-exact tar and zip writers, and the named corpus of hostile and
//! benign archives built from them for the archive-reader adversarial
//! suite.

/// The container format an archive byte string is encoded in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    /// A POSIX ustar tar archive.
    Tar,
    /// A zip archive.
    Zip,
}

/// What a correct archive reader must do with a corpus entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expectation {
    /// The archive must be refused with the named error kind, naming the
    /// given member path.
    Rejected {
        /// The error kind label from `contracts.md`, such as
        /// `archive.unsafe_path`.
        kind: &'static str,
        /// The member path the error must name.
        member: String,
    },
    /// The archive is benign and must extract cleanly.
    Benign,
}

/// One named archive in the corpus, with the bytes it is built from and
/// what a correct reader must do with them.
#[derive(Clone, Debug)]
pub struct CorpusEntry {
    name: &'static str,
    container: Container,
    bytes: Vec<u8>,
    attacks: &'static str,
    expectation: Expectation,
}

impl CorpusEntry {
    /// Returns the name this entry is filed under in the corpus.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Returns which container this entry's bytes are encoded as.
    #[must_use]
    pub fn container(&self) -> Container {
        self.container
    }

    /// Returns the raw archive bytes this entry carries.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the one-line statement of what this entry attacks.
    #[must_use]
    pub fn attacks(&self) -> &'static str {
        self.attacks
    }

    /// Returns what a correct reader must do with this entry.
    #[must_use]
    pub fn expectation(&self) -> &Expectation {
        &self.expectation
    }
}

/// The full set of named archives used by the archive-reader adversarial
/// tests.
#[derive(Clone, Debug)]
pub struct Corpus {
    entries: Vec<CorpusEntry>,
}

impl Corpus {
    /// Builds the corpus. Every call returns the same bytes for the same
    /// name.
    #[must_use]
    pub fn build() -> Self {
        Self {
            entries: corpus_entries(),
        }
    }

    /// Returns every entry in the corpus.
    #[must_use]
    pub fn entries(&self) -> &[CorpusEntry] {
        &self.entries
    }

    /// Returns the entries whose archive must be rejected.
    #[must_use]
    pub fn hostile(&self) -> Vec<&CorpusEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.expectation, Expectation::Rejected { .. }))
            .collect()
    }

    /// Returns the entries whose archive must extract cleanly.
    #[must_use]
    pub fn benign(&self) -> Vec<&CorpusEntry> {
        self.entries
            .iter()
            .filter(|entry| matches!(entry.expectation, Expectation::Benign))
            .collect()
    }
}

const NAME_OFFSET: usize = 0;
const NAME_LEN: usize = 100;
const MODE_OFFSET: usize = 100;
const MODE_LEN: usize = 8;
const UID_OFFSET: usize = 108;
const UID_LEN: usize = 8;
const GID_OFFSET: usize = 116;
const GID_LEN: usize = 8;
const SIZE_OFFSET: usize = 124;
const SIZE_LEN: usize = 12;
const MTIME_OFFSET: usize = 136;
const MTIME_LEN: usize = 12;
const CHKSUM_OFFSET: usize = 148;
const CHKSUM_LEN: usize = 8;
const TYPEFLAG_OFFSET: usize = 156;
const LINKNAME_OFFSET: usize = 157;
const LINKNAME_LEN: usize = 100;
const MAGIC_OFFSET: usize = 257;
const MAGIC_LEN: usize = 6;
const VERSION_OFFSET: usize = 263;
const VERSION_LEN: usize = 2;
const UNAME_OFFSET: usize = 265;
const UNAME_LEN: usize = 32;
const GNAME_OFFSET: usize = 297;
const GNAME_LEN: usize = 32;
const DEVMAJOR_OFFSET: usize = 329;
const DEVMAJOR_LEN: usize = 8;
const DEVMINOR_OFFSET: usize = 337;
const DEVMINOR_LEN: usize = 8;
const PREFIX_OFFSET: usize = 345;
const PREFIX_LEN: usize = 155;

/// The regular file entry type.
pub const TYPEFLAG_REGULAR: u8 = b'0';
/// The hard link entry type.
pub const TYPEFLAG_HARDLINK: u8 = b'1';
/// The symbolic link entry type.
pub const TYPEFLAG_SYMLINK: u8 = b'2';
/// The character device entry type.
pub const TYPEFLAG_CHARDEV: u8 = b'3';
/// The block device entry type.
pub const TYPEFLAG_BLOCKDEV: u8 = b'4';
/// The directory entry type.
pub const TYPEFLAG_DIRECTORY: u8 = b'5';
/// The FIFO entry type.
pub const TYPEFLAG_FIFO: u8 = b'6';
/// The per-file pax extended header entry type.
pub const TYPEFLAG_PAX: u8 = b'x';

/// A ustar header, laid out field by field with no validation applied, so a
/// header a correct reader would refuse can be written on purpose.
#[derive(Clone, Debug)]
pub struct TarHeader {
    raw: [u8; 500],
}

impl Default for TarHeader {
    fn default() -> Self {
        Self::new()
    }
}

impl TarHeader {
    /// Returns a header with every field zeroed.
    #[must_use]
    pub fn new() -> Self {
        Self { raw: [0u8; 500] }
    }

    /// Returns a header with the ordinary ustar fields filled in for a
    /// member of the given name and type: mode `0644`, uid and gid `0`,
    /// mtime `0`, magic `ustar`, version `00`, owner `user` and `group`.
    #[must_use]
    pub fn ustar(name: &[u8], typeflag: u8) -> Self {
        let mut header = Self::new();
        header
            .set_name(name)
            .set_mode(0o644)
            .set_uid(0)
            .set_gid(0)
            .set_size(0)
            .set_mtime(0)
            .set_typeflag(typeflag)
            .set_magic(b"ustar\0")
            .set_version(b"00")
            .set_uname(b"user")
            .set_gname(b"group");
        header
    }

    fn set(&mut self, offset: usize, len: usize, bytes: &[u8]) -> &mut Self {
        let written = bytes.len().min(len);
        self.raw[offset..offset + written].copy_from_slice(&bytes[..written]);
        for slot in &mut self.raw[offset + written..offset + len] {
            *slot = 0;
        }
        self
    }

    /// Sets the member path, truncated to the field width if longer.
    pub fn set_name(&mut self, name: &[u8]) -> &mut Self {
        self.set(NAME_OFFSET, NAME_LEN, name)
    }

    /// Sets the mode field from a numeric mode, encoded as null-terminated
    /// octal.
    pub fn set_mode(&mut self, mode: u32) -> &mut Self {
        let field = octal_field(u64::from(mode), MODE_LEN);
        self.set(MODE_OFFSET, MODE_LEN, &field)
    }

    /// Sets the raw bytes of the mode field, with no formatting applied.
    pub fn set_mode_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.set(MODE_OFFSET, MODE_LEN, bytes)
    }

    /// Sets the uid field from a numeric id, encoded as null-terminated
    /// octal.
    pub fn set_uid(&mut self, uid: u32) -> &mut Self {
        let field = octal_field(u64::from(uid), UID_LEN);
        self.set(UID_OFFSET, UID_LEN, &field)
    }

    /// Sets the gid field from a numeric id, encoded as null-terminated
    /// octal.
    pub fn set_gid(&mut self, gid: u32) -> &mut Self {
        let field = octal_field(u64::from(gid), GID_LEN);
        self.set(GID_OFFSET, GID_LEN, &field)
    }

    /// Sets the declared size field from a byte count, encoded as
    /// null-terminated octal. This need not match the bytes actually
    /// written after the header.
    pub fn set_size(&mut self, size: u64) -> &mut Self {
        let field = octal_field(size, SIZE_LEN);
        self.set(SIZE_OFFSET, SIZE_LEN, &field)
    }

    /// Sets the mtime field from a Unix timestamp, encoded as
    /// null-terminated octal.
    pub fn set_mtime(&mut self, mtime: u64) -> &mut Self {
        let field = octal_field(mtime, MTIME_LEN);
        self.set(MTIME_OFFSET, MTIME_LEN, &field)
    }

    /// Sets the typeflag byte directly.
    pub fn set_typeflag(&mut self, typeflag: u8) -> &mut Self {
        self.raw[TYPEFLAG_OFFSET] = typeflag;
        self
    }

    /// Sets the link target, truncated to the field width if longer.
    pub fn set_linkname(&mut self, linkname: &[u8]) -> &mut Self {
        self.set(LINKNAME_OFFSET, LINKNAME_LEN, linkname)
    }

    /// Sets the magic field directly, with no validation.
    pub fn set_magic(&mut self, magic: &[u8]) -> &mut Self {
        self.set(MAGIC_OFFSET, MAGIC_LEN, magic)
    }

    /// Sets the version field directly, with no validation.
    pub fn set_version(&mut self, version: &[u8]) -> &mut Self {
        self.set(VERSION_OFFSET, VERSION_LEN, version)
    }

    /// Sets the owning user name.
    pub fn set_uname(&mut self, uname: &[u8]) -> &mut Self {
        self.set(UNAME_OFFSET, UNAME_LEN, uname)
    }

    /// Sets the owning group name.
    pub fn set_gname(&mut self, gname: &[u8]) -> &mut Self {
        self.set(GNAME_OFFSET, GNAME_LEN, gname)
    }

    /// Sets the device major number, encoded as null-terminated octal.
    pub fn set_devmajor(&mut self, devmajor: u32) -> &mut Self {
        let field = octal_field(u64::from(devmajor), DEVMAJOR_LEN);
        self.set(DEVMAJOR_OFFSET, DEVMAJOR_LEN, &field)
    }

    /// Sets the device minor number, encoded as null-terminated octal.
    pub fn set_devminor(&mut self, devminor: u32) -> &mut Self {
        let field = octal_field(u64::from(devminor), DEVMINOR_LEN);
        self.set(DEVMINOR_OFFSET, DEVMINOR_LEN, &field)
    }

    /// Sets the ustar path prefix, truncated to the field width if longer.
    pub fn set_prefix(&mut self, prefix: &[u8]) -> &mut Self {
        self.set(PREFIX_OFFSET, PREFIX_LEN, prefix)
    }

    /// Renders this header as a 512-byte block, computing the checksum
    /// field over the rest of the block as the ustar specification
    /// requires: the checksum field itself is treated as eight spaces
    /// while the sum is taken.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; 512] {
        let mut block = [0u8; 512];
        block[..500].copy_from_slice(&self.raw);
        let sum = checksum_sum(&block);
        block[CHKSUM_OFFSET..CHKSUM_OFFSET + CHKSUM_LEN].copy_from_slice(&checksum_field(sum));
        block
    }

    /// Renders this header as a 512-byte block with the given bytes placed
    /// verbatim into the checksum field, bypassing the computed value.
    #[must_use]
    pub fn to_bytes_with_checksum(&self, checksum: [u8; CHKSUM_LEN]) -> [u8; 512] {
        let mut block = [0u8; 512];
        block[..500].copy_from_slice(&self.raw);
        block[CHKSUM_OFFSET..CHKSUM_OFFSET + CHKSUM_LEN].copy_from_slice(&checksum);
        block
    }
}

fn checksum_sum(block: &[u8; 512]) -> u32 {
    let mut sum: u32 = 0;
    for (index, byte) in block.iter().enumerate() {
        if (CHKSUM_OFFSET..CHKSUM_OFFSET + CHKSUM_LEN).contains(&index) {
            sum += 0x20;
        } else {
            sum += u32::from(*byte);
        }
    }
    sum
}

fn checksum_field(sum: u32) -> [u8; CHKSUM_LEN] {
    let digits = format!("{sum:06o}");
    let mut field = [0u8; CHKSUM_LEN];
    field[..6].copy_from_slice(&digits.as_bytes()[..6]);
    field[6] = 0;
    field[7] = b' ';
    field
}

fn octal_field(value: u64, len: usize) -> Vec<u8> {
    let digits = len - 1;
    let rendered = format!("{value:0digits$o}");
    let trimmed = if rendered.len() > digits {
        rendered[rendered.len() - digits..].to_string()
    } else {
        rendered
    };
    let mut field = trimmed.into_bytes();
    field.push(0);
    field
}

/// One record of a pax extended header: `length keyword=value\n`, with the
/// length computed to include its own digits, as the pax specification
/// requires.
#[must_use]
pub fn pax_record(keyword: &str, value: &str) -> Vec<u8> {
    let fixed = keyword.len() + value.len() + 3;
    let mut total = fixed + fixed.to_string().len();
    loop {
        let candidate = fixed + total.to_string().len();
        if candidate == total {
            break;
        }
        total = candidate;
    }
    format!("{total} {keyword}={value}\n").into_bytes()
}

/// Concatenates pax records into the data block of a per-file extended
/// header entry.
#[must_use]
pub fn pax_block(records: &[Vec<u8>]) -> Vec<u8> {
    records
        .iter()
        .flat_map(|record| record.iter().copied())
        .collect()
}

/// Builds a tar archive byte by byte from headers and data supplied in full,
/// with no sanitization of paths, links, or sizes.
#[derive(Clone, Debug, Default)]
pub struct TarWriter {
    bytes: Vec<u8>,
}

impl TarWriter {
    /// Returns an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    /// Appends one entry: the header block followed by its data, padded
    /// with zero bytes to the next 512-byte boundary. The data length need
    /// not match the header's declared size.
    pub fn push(&mut self, header: &TarHeader, data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(&header.to_bytes());
        self.push_data(data)
    }

    /// Appends one entry using an already-rendered 512-byte header block,
    /// for cases that need direct control over the checksum bytes.
    pub fn push_block(&mut self, block: &[u8; 512], data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(block);
        self.push_data(data)
    }

    fn push_data(&mut self, data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(data);
        let remainder = data.len() % 512;
        if remainder != 0 {
            self.bytes.extend(std::iter::repeat_n(0u8, 512 - remainder));
        }
        self
    }

    /// Finishes the archive, appending the two zero-filled end-of-archive
    /// blocks the ustar format requires.
    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.bytes.extend(std::iter::repeat_n(0u8, 1024));
        self.bytes
    }
}

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;

/// The store compression method.
pub const METHOD_STORE: u16 = 0;
/// The deflate compression method.
pub const METHOD_DEFLATE: u16 = 8;

/// A zip local file header, with its own name and size fields independent
/// of any central directory entry for the same member.
#[derive(Clone, Debug)]
pub struct ZipLocalHeader {
    /// The minimum version a reader needs to extract this entry.
    pub version_needed: u16,
    /// The general purpose bit flags.
    pub flags: u16,
    /// The compression method.
    pub method: u16,
    /// The MS-DOS last modified time.
    pub mod_time: u16,
    /// The MS-DOS last modified date.
    pub mod_date: u16,
    /// The CRC-32 of the uncompressed data.
    pub crc32: u32,
    /// The declared compressed size.
    pub compressed_size: u32,
    /// The declared uncompressed size.
    pub uncompressed_size: u32,
    /// The member name.
    pub name: Vec<u8>,
    /// The extra field bytes.
    pub extra: Vec<u8>,
}

impl ZipLocalHeader {
    /// Returns a stored, uncompressed local header for the given name and
    /// data, with the CRC-32 and sizes computed from the data.
    #[must_use]
    pub fn store(name: &[u8], data: &[u8]) -> Self {
        Self {
            version_needed: 20,
            flags: 0,
            method: METHOD_STORE,
            mod_time: 0,
            mod_date: 0,
            crc32: crc32(data),
            compressed_size: u32::try_from(data.len()).unwrap_or(u32::MAX),
            uncompressed_size: u32::try_from(data.len()).unwrap_or(u32::MAX),
            name: name.to_vec(),
            extra: Vec::new(),
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(30 + self.name.len() + self.extra.len());
        out.extend_from_slice(&LOCAL_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&self.version_needed.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.method.to_le_bytes());
        out.extend_from_slice(&self.mod_time.to_le_bytes());
        out.extend_from_slice(&self.mod_date.to_le_bytes());
        out.extend_from_slice(&self.crc32.to_le_bytes());
        out.extend_from_slice(&self.compressed_size.to_le_bytes());
        out.extend_from_slice(&self.uncompressed_size.to_le_bytes());
        out.extend_from_slice(
            &u16::try_from(self.name.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u16::try_from(self.extra.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.name);
        out.extend_from_slice(&self.extra);
        out
    }
}

/// A zip central directory file header, with its own name and size fields
/// independent of the local file header for the same member.
#[derive(Clone, Debug)]
pub struct ZipCentralHeader {
    /// The version the writer claims to have made this entry with.
    pub version_made_by: u16,
    /// The minimum version a reader needs to extract this entry.
    pub version_needed: u16,
    /// The general purpose bit flags.
    pub flags: u16,
    /// The compression method.
    pub method: u16,
    /// The MS-DOS last modified time.
    pub mod_time: u16,
    /// The MS-DOS last modified date.
    pub mod_date: u16,
    /// The CRC-32 of the uncompressed data.
    pub crc32: u32,
    /// The declared compressed size.
    pub compressed_size: u32,
    /// The declared uncompressed size.
    pub uncompressed_size: u32,
    /// The disk number this entry starts on.
    pub disk_start: u16,
    /// The internal file attributes.
    pub internal_attrs: u16,
    /// The external file attributes, carrying the Unix mode in its upper
    /// sixteen bits when a writer chooses to set it there.
    pub external_attrs: u32,
    /// The byte offset of this member's local file header.
    pub local_header_offset: u32,
    /// The member name.
    pub name: Vec<u8>,
    /// The extra field bytes.
    pub extra: Vec<u8>,
    /// The per-entry comment bytes.
    pub comment: Vec<u8>,
}

impl ZipCentralHeader {
    /// Returns a central directory entry matching the given local header,
    /// at the given local header offset.
    #[must_use]
    pub fn from_local(local: &ZipLocalHeader, local_header_offset: u32) -> Self {
        Self {
            version_made_by: 20,
            version_needed: local.version_needed,
            flags: local.flags,
            method: local.method,
            mod_time: local.mod_time,
            mod_date: local.mod_date,
            crc32: local.crc32,
            compressed_size: local.compressed_size,
            uncompressed_size: local.uncompressed_size,
            disk_start: 0,
            internal_attrs: 0,
            external_attrs: 0,
            local_header_offset,
            name: local.name.clone(),
            extra: Vec::new(),
            comment: Vec::new(),
        }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut out =
            Vec::with_capacity(46 + self.name.len() + self.extra.len() + self.comment.len());
        out.extend_from_slice(&CENTRAL_SIGNATURE.to_le_bytes());
        out.extend_from_slice(&self.version_made_by.to_le_bytes());
        out.extend_from_slice(&self.version_needed.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.method.to_le_bytes());
        out.extend_from_slice(&self.mod_time.to_le_bytes());
        out.extend_from_slice(&self.mod_date.to_le_bytes());
        out.extend_from_slice(&self.crc32.to_le_bytes());
        out.extend_from_slice(&self.compressed_size.to_le_bytes());
        out.extend_from_slice(&self.uncompressed_size.to_le_bytes());
        out.extend_from_slice(
            &u16::try_from(self.name.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u16::try_from(self.extra.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(
            &u16::try_from(self.comment.len())
                .unwrap_or(u16::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.disk_start.to_le_bytes());
        out.extend_from_slice(&self.internal_attrs.to_le_bytes());
        out.extend_from_slice(&self.external_attrs.to_le_bytes());
        out.extend_from_slice(&self.local_header_offset.to_le_bytes());
        out.extend_from_slice(&self.name);
        out.extend_from_slice(&self.extra);
        out.extend_from_slice(&self.comment);
        out
    }
}

/// One zip member: a local file header and data pair, and the central
/// directory entry that describes it, held separately so the two can
/// disagree.
#[derive(Clone, Debug)]
pub struct ZipMember {
    /// The local file header written at the member's offset.
    pub local: ZipLocalHeader,
    /// The central directory entry describing this member.
    pub central: ZipCentralHeader,
    /// The bytes written after the local header.
    pub data: Vec<u8>,
}

/// Builds a zip archive byte by byte from local headers, central directory
/// entries, and data supplied in full, with no sanitization of paths,
/// links, or sizes.
#[derive(Clone, Debug, Default)]
pub struct ZipWriter {
    bytes: Vec<u8>,
    centrals: Vec<ZipCentralHeader>,
}

impl ZipWriter {
    /// Returns an empty writer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            centrals: Vec::new(),
        }
    }

    /// Returns the byte offset the next member's local header would be
    /// written at, for building a central directory entry that points at
    /// it correctly.
    #[must_use]
    pub fn offset(&self) -> u32 {
        u32::try_from(self.bytes.len()).unwrap_or(u32::MAX)
    }

    /// Appends one member: its local header, its data, and its central
    /// directory entry, held for the central directory written at
    /// `finish`.
    pub fn push(&mut self, member: ZipMember) -> &mut Self {
        self.bytes.extend_from_slice(&member.local.to_bytes());
        self.bytes.extend_from_slice(&member.data);
        self.centrals.push(member.central);
        self
    }

    /// Finishes the archive: the central directory, then a single
    /// end-of-central-directory record.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.finish_inner(false)
    }

    /// Finishes the archive as `finish` does, additionally writing a
    /// Zip64 end-of-central-directory record and locator before the
    /// ordinary end-of-central-directory record.
    #[must_use]
    pub fn finish_zip64(self) -> Vec<u8> {
        self.finish_inner(true)
    }

    fn finish_inner(mut self, zip64: bool) -> Vec<u8> {
        let cd_offset = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX);
        for central in &self.centrals {
            self.bytes.extend_from_slice(&central.to_bytes());
        }
        let cd_size = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX) - cd_offset;
        let count = self.centrals.len();
        let count16 = u16::try_from(count).unwrap_or(u16::MAX);

        if zip64 {
            let zip64_eocd_offset = self.bytes.len() as u64;
            self.bytes
                .extend_from_slice(&ZIP64_EOCD_SIGNATURE.to_le_bytes());
            self.bytes.extend_from_slice(&44u64.to_le_bytes());
            self.bytes.extend_from_slice(&45u16.to_le_bytes());
            self.bytes.extend_from_slice(&45u16.to_le_bytes());
            self.bytes.extend_from_slice(&0u32.to_le_bytes());
            self.bytes.extend_from_slice(&0u32.to_le_bytes());
            self.bytes.extend_from_slice(&(count as u64).to_le_bytes());
            self.bytes.extend_from_slice(&(count as u64).to_le_bytes());
            self.bytes
                .extend_from_slice(&u64::from(cd_size).to_le_bytes());
            self.bytes
                .extend_from_slice(&u64::from(cd_offset).to_le_bytes());

            self.bytes
                .extend_from_slice(&ZIP64_LOCATOR_SIGNATURE.to_le_bytes());
            self.bytes.extend_from_slice(&0u32.to_le_bytes());
            self.bytes
                .extend_from_slice(&zip64_eocd_offset.to_le_bytes());
            self.bytes.extend_from_slice(&1u32.to_le_bytes());
        }

        self.bytes.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&count16.to_le_bytes());
        self.bytes.extend_from_slice(&count16.to_le_bytes());
        self.bytes.extend_from_slice(&cd_size.to_le_bytes());
        self.bytes.extend_from_slice(&cd_offset.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes
    }
}

/// Computes the CRC-32 (the zip and gzip variant, polynomial `0xEDB88320`)
/// of the given bytes.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

struct BitWriter {
    bytes: Vec<u8>,
    current: u8,
    filled: u8,
}

impl BitWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            current: 0,
            filled: 0,
        }
    }

    fn push_bit(&mut self, bit: u8) {
        self.current |= (bit & 1) << self.filled;
        self.filled += 1;
        if self.filled == 8 {
            self.bytes.push(self.current);
            self.current = 0;
            self.filled = 0;
        }
    }

    fn push_value(&mut self, value: u32, bits: u8) {
        for index in 0..bits {
            self.push_bit(((value >> index) & 1) as u8);
        }
    }

    fn push_code(&mut self, code: u32, bits: u8) {
        for index in (0..bits).rev() {
            self.push_bit(((code >> index) & 1) as u8);
        }
    }

    fn finish(mut self) -> Vec<u8> {
        if self.filled > 0 {
            self.bytes.push(self.current);
        }
        self.bytes
    }
}

fn fixed_literal_code(byte: u8) -> (u32, u8) {
    let value = u32::from(byte);
    if value <= 143 {
        (0x30 + value, 8)
    } else {
        (0x190 + (value - 144), 9)
    }
}

fn fixed_length_symbol_code(symbol: u32) -> (u32, u8) {
    if symbol == 256 {
        (0, 7)
    } else if symbol <= 279 {
        (1 + (symbol - 257), 7)
    } else {
        (0xC0 + (symbol - 280), 8)
    }
}

/// Encodes `count` repeated copies of `byte` as a raw DEFLATE stream (RFC
/// 1951, one fixed-Huffman block), using a length-258 distance-1
/// back-reference for every run of 258 bytes after an initial literal, so a
/// handful of compressed bytes legitimately decompresses to far more than
/// the archive's own on-disk size.
fn deflate_repeated_byte(byte: u8, count: u32) -> Vec<u8> {
    let mut writer = BitWriter::new();
    writer.push_value(1, 1);
    writer.push_value(1, 2);
    if count == 0 {
        let (end_code, end_bits) = fixed_length_symbol_code(256);
        writer.push_code(end_code, end_bits);
        return writer.finish();
    }
    let (literal_code, literal_bits) = fixed_literal_code(byte);
    writer.push_code(literal_code, literal_bits);
    let mut remaining = count - 1;
    while remaining >= 258 {
        let (length_code, length_bits) = fixed_length_symbol_code(285);
        writer.push_code(length_code, length_bits);
        writer.push_code(0, 5);
        remaining -= 258;
    }
    for _ in 0..remaining {
        let (literal_code, literal_bits) = fixed_literal_code(byte);
        writer.push_code(literal_code, literal_bits);
    }
    let (end_code, end_bits) = fixed_length_symbol_code(256);
    writer.push_code(end_code, end_bits);
    writer.finish()
}

fn tar_single(name: &[u8], typeflag: u8, mode: u32, linkname: &[u8], data: &[u8]) -> Vec<u8> {
    let mut header = TarHeader::ustar(name, typeflag);
    header.set_mode(mode).set_size(data.len() as u64);
    if !linkname.is_empty() {
        header.set_linkname(linkname);
    }
    let mut writer = TarWriter::new();
    writer.push(&header, data);
    writer.finish()
}

fn tar_device(name: &[u8], typeflag: u8, devmajor: u32, devminor: u32) -> Vec<u8> {
    let mut header = TarHeader::ustar(name, typeflag);
    header.set_devmajor(devmajor).set_devminor(devminor);
    let mut writer = TarWriter::new();
    writer.push(&header, b"");
    writer.finish()
}

fn rejected(kind: &'static str, member: &str) -> Expectation {
    Expectation::Rejected {
        kind,
        member: member.to_string(),
    }
}

fn entry(
    name: &'static str,
    container: Container,
    bytes: Vec<u8>,
    attacks: &'static str,
    expectation: Expectation,
) -> CorpusEntry {
    CorpusEntry {
        name,
        container,
        bytes,
        attacks,
        expectation,
    }
}

fn tar_pax_entry(
    name: &'static str,
    attacks: &'static str,
    records: &[Vec<u8>],
    real_name: &[u8],
    data: &[u8],
    expectation: Expectation,
) -> CorpusEntry {
    let pax_data = pax_block(records);
    let mut pax_header = TarHeader::ustar(b"pax_header", TYPEFLAG_PAX);
    pax_header.set_size(pax_data.len() as u64);
    let mut file_header = TarHeader::ustar(real_name, TYPEFLAG_REGULAR);
    file_header.set_size(data.len() as u64);
    let mut writer = TarWriter::new();
    writer.push(&pax_header, &pax_data);
    writer.push(&file_header, data);
    entry(name, Container::Tar, writer.finish(), attacks, expectation)
}

#[expect(
    clippy::too_many_lines,
    reason = "one flat list of named corpus entries reads more clearly than a spread of tiny helpers"
)]
fn corpus_entries() -> Vec<CorpusEntry> {
    let mut entries = Vec::new();

    entries.push(entry(
        "tar_absolute_path",
        Container::Tar,
        tar_single(b"/etc/passwd", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path rooted at the filesystem root",
        rejected("archive.unsafe_path", "/etc/passwd"),
    ));

    entries.push(entry(
        "tar_dotdot_leading",
        Container::Tar,
        tar_single(b"../evil.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path starting with a parent-directory component",
        rejected("archive.unsafe_path", "../evil.txt"),
    ));

    entries.push(entry(
        "tar_dotdot_embedded",
        Container::Tar,
        tar_single(b"docs/../../secret.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path with a parent-directory component after the first segment",
        rejected("archive.unsafe_path", "docs/../../secret.txt"),
    ));

    entries.push(entry(
        "tar_backslash_path",
        Container::Tar,
        tar_single(b"dir\\evil.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path using a backslash, which Windows cannot store in a name",
        rejected("archive.unsafe_path", "dir\\evil.txt"),
    ));

    entries.push(entry(
        "tar_windows_drive_path",
        Container::Tar,
        tar_single(b"C:/Windows/evil.dll", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path naming an absolute Windows drive location",
        rejected("archive.unsafe_path", "C:/Windows/evil.dll"),
    ));

    entries.push(entry(
        "tar_symlink_absolute_target",
        Container::Tar,
        tar_single(b"link1", TYPEFLAG_SYMLINK, 0o777, b"/etc/passwd", b""),
        "a symlink whose target is an absolute path",
        rejected("archive.link_escape", "link1"),
    ));

    entries.push(entry(
        "tar_symlink_dotdot_target",
        Container::Tar,
        tar_single(b"link2", TYPEFLAG_SYMLINK, 0o777, b"../../etc/passwd", b""),
        "a symlink whose target climbs out of the destination with parent-directory components",
        rejected("archive.link_escape", "link2"),
    ));

    entries.push(entry(
        "tar_symlink_dotdot_then_write_through",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let link = TarHeader::ustar(b"escape", TYPEFLAG_SYMLINK);
            let mut link = link;
            link.set_linkname(b"..");
            writer.push(&link, b"");
            let mut file = TarHeader::ustar(b"escape/evil.txt", TYPEFLAG_REGULAR);
            file.set_size(1);
            writer.push(&file, b"x");
            writer.finish()
        },
        "a symlink pointing at the parent directory, followed by a member written through it",
        rejected("archive.link_escape", "escape"),
    ));

    entries.push(entry(
        "tar_hardlink_outside",
        Container::Tar,
        tar_single(b"hard1", TYPEFLAG_HARDLINK, 0o644, b"../../etc/passwd", b""),
        "a hard link naming a target outside the destination",
        rejected("archive.link_escape", "hard1"),
    ));

    entries.push(entry(
        "tar_block_device",
        Container::Tar,
        tar_device(b"dev/sda", TYPEFLAG_BLOCKDEV, 8, 0),
        "a block device entry",
        rejected("archive.unsupported", "dev/sda"),
    ));

    entries.push(entry(
        "tar_char_device",
        Container::Tar,
        tar_device(b"dev/tty", TYPEFLAG_CHARDEV, 5, 0),
        "a character device entry",
        rejected("archive.unsupported", "dev/tty"),
    ));

    entries.push(entry(
        "tar_fifo",
        Container::Tar,
        tar_single(b"pipe", TYPEFLAG_FIFO, 0o644, b"", b""),
        "a FIFO entry",
        rejected("archive.unsupported", "pipe"),
    ));

    entries.push(entry(
        "tar_setuid_mode",
        Container::Tar,
        tar_single(b"suid.bin", TYPEFLAG_REGULAR, 0o4755, b"", b"x"),
        "a regular file carrying the setuid bit",
        rejected("archive.unsupported", "suid.bin"),
    ));

    entries.push(entry(
        "tar_setgid_mode",
        Container::Tar,
        tar_single(b"sgid.bin", TYPEFLAG_REGULAR, 0o2755, b"", b"x"),
        "a regular file carrying the setgid bit",
        rejected("archive.unsupported", "sgid.bin"),
    ));

    entries.push(entry(
        "tar_duplicate_member",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut first = TarHeader::ustar(b"dup.txt", TYPEFLAG_REGULAR);
            first.set_size(1);
            writer.push(&first, b"a");
            let mut second = TarHeader::ustar(b"dup.txt", TYPEFLAG_REGULAR);
            second.set_size(1);
            writer.push(&second, b"b");
            writer.finish()
        },
        "the same member path written twice",
        rejected("archive.collision", "dup.txt"),
    ));

    let long_component: String = std::iter::repeat_n('a', 300).collect();
    let long_name = format!("{long_component}.txt");
    entries.push(tar_pax_entry(
        "tar_path_too_long",
        "a single path component longer than any platform's name limit",
        &[pax_record("path", &long_name)],
        b"placeholder.txt",
        b"x",
        rejected("archive.unsafe_path", &long_name),
    ));

    entries.push(entry(
        "tar_path_invalid_utf8",
        Container::Tar,
        tar_single(b"bad\xFF\xFE.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path that is not valid UTF-8",
        rejected("archive.unsafe_path", "bad\u{fffd}\u{fffd}.txt"),
    ));

    entries.push(tar_pax_entry(
        "tar_path_with_nul",
        "a member path containing an embedded NUL byte",
        &[pax_record("path", "bad\u{0}name.txt")],
        b"placeholder.txt",
        b"x",
        rejected("archive.unsafe_path", "bad\u{0}name.txt"),
    ));

    let deep_name = {
        use std::fmt::Write as _;
        let mut path = String::new();
        for index in 0..70 {
            let _ = write!(path, "d{index}/");
        }
        path.push_str("file.txt");
        path
    };
    entries.push(tar_pax_entry(
        "tar_nesting_too_deep",
        "a member path nested more than 64 components deep",
        &[pax_record("path", &deep_name)],
        b"placeholder.txt",
        b"x",
        rejected("archive.unsafe_path", &deep_name),
    ));

    entries.push(entry(
        "tar_case_collision",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut first = TarHeader::ustar(b"A.txt", TYPEFLAG_REGULAR);
            first.set_size(1);
            writer.push(&first, b"a");
            let mut second = TarHeader::ustar(b"a.txt", TYPEFLAG_REGULAR);
            second.set_size(1);
            writer.push(&second, b"b");
            writer.finish()
        },
        "two member paths colliding only under case folding",
        rejected("archive.collision", "a.txt"),
    ));

    entries.push(entry(
        "tar_unicode_normalization_collision",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let precomposed = "caf\u{e9}.txt";
            let decomposed = "cafe\u{301}.txt";
            let mut first = TarHeader::ustar(precomposed.as_bytes(), TYPEFLAG_REGULAR);
            first.set_size(1);
            writer.push(&first, b"a");
            let mut second = TarHeader::ustar(decomposed.as_bytes(), TYPEFLAG_REGULAR);
            second.set_size(1);
            writer.push(&second, b"b");
            writer.finish()
        },
        "two member paths colliding only under Unicode normalization",
        rejected("archive.collision", "cafe\u{301}.txt"),
    ));

    entries.push(entry(
        "tar_windows_reserved_con",
        Container::Tar,
        tar_single(b"CON", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path matching a Windows-reserved device name",
        rejected("destination.unrepresentable", "CON"),
    ));

    entries.push(entry(
        "tar_windows_reserved_aux_txt",
        Container::Tar,
        tar_single(b"aux.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path matching a Windows-reserved device name with an extension",
        rejected("destination.unrepresentable", "aux.txt"),
    ));

    entries.push(entry(
        "tar_windows_reserved_trailing_dot",
        Container::Tar,
        tar_single(b"trailing.", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path ending in a dot, which Windows cannot store",
        rejected("destination.unrepresentable", "trailing."),
    ));

    entries.push(entry(
        "tar_windows_reserved_trailing_space",
        Container::Tar,
        tar_single(b"trailing ", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path ending in a space, which Windows cannot store",
        rejected("destination.unrepresentable", "trailing "),
    ));

    entries.push(entry(
        "tar_windows_reserved_colon",
        Container::Tar,
        tar_single(b"weird:name.txt", TYPEFLAG_REGULAR, 0o644, b"", b"x"),
        "a member path containing a colon, which Windows cannot store",
        rejected("destination.unrepresentable", "weird:name.txt"),
    ));

    entries.push(tar_pax_entry(
        "tar_pax_extended_attribute",
        "a pax extended header carrying an extended attribute",
        &[pax_record("SCHILY.xattr.user.comment", "hello")],
        b"xattr.txt",
        b"x",
        rejected("archive.unsupported", "xattr.txt"),
    ));

    entries.push(tar_pax_entry(
        "tar_pax_ownership",
        "a pax extended header carrying ownership",
        &[pax_record("uid", "1234"), pax_record("gid", "5678")],
        b"owned.txt",
        b"x",
        rejected("archive.unsupported", "owned.txt"),
    ));

    entries.push(entry(
        "zip_absolute_path",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"/etc/passwd", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a member path rooted at the filesystem root",
        rejected("archive.unsafe_path", "/etc/passwd"),
    ));

    entries.push(entry(
        "zip_dotdot_traversal",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"../../evil.txt", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a member path climbing out of the destination with parent-directory components",
        rejected("archive.unsafe_path", "../../evil.txt"),
    ));

    entries.push(entry(
        "zip_name_disagreement",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"safe.txt", data);
            let mut central = ZipCentralHeader::from_local(&local, offset);
            central.name = b"evil.txt".to_vec();
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a local file header whose name disagrees with its central directory entry",
        rejected("archive.unsafe_path", "evil.txt"),
    ));

    entries.push(entry(
        "zip_size_disagreement",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"size_mismatch.txt", data);
            let mut central = ZipCentralHeader::from_local(&local, offset);
            central.compressed_size = 999;
            central.uncompressed_size = 999;
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a local file header whose size disagrees with its central directory entry",
        rejected("archive.unsupported", "size_mismatch.txt"),
    ));

    entries.push(entry(
        "zip_duplicate_name",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let first_data = b"a";
            let first_offset = writer.offset();
            let first_local = ZipLocalHeader::store(b"dup.txt", first_data);
            let first_central = ZipCentralHeader::from_local(&first_local, first_offset);
            writer.push(ZipMember {
                local: first_local,
                central: first_central,
                data: first_data.to_vec(),
            });
            let second_data = b"b";
            let second_offset = writer.offset();
            let second_local = ZipLocalHeader::store(b"dup.txt", second_data);
            let second_central = ZipCentralHeader::from_local(&second_local, second_offset);
            writer.push(ZipMember {
                local: second_local,
                central: second_central,
                data: second_data.to_vec(),
            });
            writer.finish()
        },
        "the same member name written twice",
        rejected("archive.collision", "dup.txt"),
    ));

    entries.push(entry(
        "zip_symlink_escape_unix_mode",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let target = b"../../etc/passwd";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"escape_link", target);
            let mut central = ZipCentralHeader::from_local(&local, offset);
            central.external_attrs = 0o120_777u32 << 16;
            writer.push(ZipMember {
                local,
                central,
                data: target.to_vec(),
            });
            writer.finish()
        },
        "a symlink escaping the destination, expressed only through the Unix mode in the external attributes",
        rejected("archive.link_escape", "escape_link"),
    ));

    entries.push(entry(
        "zip_unsupported_method",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let mut local = ZipLocalHeader::store(b"lzma.txt", data);
            local.method = 14;
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "an entry using a compression method that is neither store nor deflate",
        rejected("archive.unsupported", "lzma.txt"),
    ));

    entries.push(entry(
        "zip_backslash_path",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let first_data = b"x";
            let first_offset = writer.offset();
            let first_local = ZipLocalHeader::store(b"a/b.txt", first_data);
            let first_central = ZipCentralHeader::from_local(&first_local, first_offset);
            writer.push(ZipMember {
                local: first_local,
                central: first_central,
                data: first_data.to_vec(),
            });
            let second_data = b"y";
            let second_offset = writer.offset();
            let second_local = ZipLocalHeader::store(b"dir\\evil.txt", second_data);
            let second_central = ZipCentralHeader::from_local(&second_local, second_offset);
            writer.push(ZipMember {
                local: second_local,
                central: second_central,
                data: second_data.to_vec(),
            });
            writer.finish()
        },
        "a member path using a backslash alongside another member that already uses a forward slash, so the backslash cannot be read as a separator",
        rejected("archive.unsafe_path", "dir\\evil.txt"),
    ));

    entries.push(entry(
        "zip_backslash_separated_normalizes",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"x";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"dir\\evil.txt", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish()
        },
        "a zip whose only member holds a backslash and no member anywhere holds a forward slash, which is the Windows PowerShell Compress-Archive shape and is read as backslash-separated",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_two_files_and_dir",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut dir = TarHeader::ustar(b"docs/", TYPEFLAG_DIRECTORY);
            dir.set_mode(0o755);
            writer.push(&dir, b"");
            let mut readme = TarHeader::ustar(b"docs/readme.txt", TYPEFLAG_REGULAR);
            readme.set_size(5);
            writer.push(&readme, b"hello");
            let mut top = TarHeader::ustar(b"top.txt", TYPEFLAG_REGULAR);
            top.set_size(5);
            writer.push(&top, b"world");
            writer.finish()
        },
        "a small, well formed tar of two files and a directory",
        Expectation::Benign,
    ));

    entries.push(entry(
        "zip_benign_two_files_and_dir",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let dir_data = b"";
            let dir_offset = writer.offset();
            let dir_local = ZipLocalHeader::store(b"docs/", dir_data);
            let dir_central = ZipCentralHeader::from_local(&dir_local, dir_offset);
            writer.push(ZipMember {
                local: dir_local,
                central: dir_central,
                data: dir_data.to_vec(),
            });
            let readme_data = b"hello";
            let readme_offset = writer.offset();
            let readme_local = ZipLocalHeader::store(b"docs/readme.txt", readme_data);
            let readme_central = ZipCentralHeader::from_local(&readme_local, readme_offset);
            writer.push(ZipMember {
                local: readme_local,
                central: readme_central,
                data: readme_data.to_vec(),
            });
            let top_data = b"world";
            let top_offset = writer.offset();
            let top_local = ZipLocalHeader::store(b"top.txt", top_data);
            let top_central = ZipCentralHeader::from_local(&top_local, top_offset);
            writer.push(ZipMember {
                local: top_local,
                central: top_central,
                data: top_data.to_vec(),
            });
            writer.finish()
        },
        "a small, well formed zip of two files and a directory",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_zero_byte_file",
        Container::Tar,
        tar_single(b"empty.bin", TYPEFLAG_REGULAR, 0o644, b"", b""),
        "a tar holding a zero-byte file",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_empty_directory",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut dir = TarHeader::ustar(b"emptydir/", TYPEFLAG_DIRECTORY);
            dir.set_mode(0o755);
            writer.push(&dir, b"");
            writer.finish()
        },
        "a tar holding an empty directory",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_benign_identical_content_twice",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            let mut first = TarHeader::ustar(b"a.bin", TYPEFLAG_REGULAR);
            first.set_size(13);
            writer.push(&first, b"same content!");
            let mut second = TarHeader::ustar(b"b.bin", TYPEFLAG_REGULAR);
            second.set_size(13);
            writer.push(&second, b"same content!");
            writer.finish()
        },
        "a tar holding two members with identical content",
        Expectation::Benign,
    ));

    entries.push(entry(
        "zip_benign_zip64",
        Container::Zip,
        {
            let mut writer = ZipWriter::new();
            let data = b"hi";
            let offset = writer.offset();
            let local = ZipLocalHeader::store(b"small.txt", data);
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: data.to_vec(),
            });
            writer.finish_zip64()
        },
        "a zip using Zip64 end-of-central-directory records",
        Expectation::Benign,
    ));

    entries.push(entry(
        "tar_bomb_entry_count",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            for name in [b"a.txt".as_slice(), b"b.txt", b"c.txt", b"d.txt"] {
                let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
                header.set_size(1);
                writer.push(&header, b"x");
            }
            writer.finish()
        },
        "more entries than an overridden entry limit allows, discovered as they are actually written to staging rather than trusted from a declared count",
        rejected("archive.bomb", "tar_bomb_entry_count"),
    ));

    entries.push(entry(
        "tar_bomb_expanded_bytes",
        Container::Tar,
        {
            let mut writer = TarWriter::new();
            for name in [b"a.bin".as_slice(), b"b.bin", b"c.bin"] {
                let mut header = TarHeader::ustar(name, TYPEFLAG_REGULAR);
                let data = vec![b'x'; 64];
                header.set_size(64);
                writer.push(&header, &data);
            }
            writer.finish()
        },
        "more expanded bytes than an overridden byte limit allows, counted as the bytes are actually written to staging",
        rejected("archive.bomb", "tar_bomb_expanded_bytes"),
    ));

    entries.push(entry(
        "zip_bomb_expansion_ratio",
        Container::Zip,
        {
            let byte = b'A';
            let count: u32 = 1 + 258 * 2000;
            let compressed = deflate_repeated_byte(byte, count);
            let content = vec![byte; count as usize];
            let crc = crc32(&content);
            let mut writer = ZipWriter::new();
            let offset = writer.offset();
            let local = ZipLocalHeader {
                version_needed: 20,
                flags: 0,
                method: METHOD_DEFLATE,
                mod_time: 0,
                mod_date: 0,
                crc32: crc,
                compressed_size: u32::try_from(compressed.len()).unwrap_or(u32::MAX),
                uncompressed_size: count,
                name: b"bomb.bin".to_vec(),
                extra: Vec::new(),
            };
            let central = ZipCentralHeader::from_local(&local, offset);
            writer.push(ZipMember {
                local,
                central,
                data: compressed,
            });
            writer.finish()
        },
        "a legitimately deflate-compressed member that expands far past the archive's own on-disk size",
        rejected("archive.bomb", "zip_bomb_expansion_ratio"),
    ));

    entries
}
