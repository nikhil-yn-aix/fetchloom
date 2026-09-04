//! A byte-exact POSIX ustar writer, where every header field is placed at the
//! offset the format states rather than by a library that hides them.

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

    /// Returns a header with the ordinary ustar fields filled in for a member
    /// of the given name and type: mode `0644`, uid and gid `0`, mtime `0`,
    /// magic `ustar`, version `00`, owner `user` and `group`.
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

    /// Sets the uid field from a numeric id, encoded as null-terminated octal.
    pub fn set_uid(&mut self, uid: u32) -> &mut Self {
        let field = octal_field(u64::from(uid), UID_LEN);
        self.set(UID_OFFSET, UID_LEN, &field)
    }

    /// Sets the gid field from a numeric id, encoded as null-terminated octal.
    pub fn set_gid(&mut self, gid: u32) -> &mut Self {
        let field = octal_field(u64::from(gid), GID_LEN);
        self.set(GID_OFFSET, GID_LEN, &field)
    }

    /// Sets the declared size field from a byte count, encoded as
    /// null-terminated octal. This need not match the bytes actually written
    /// after the header.
    pub fn set_size(&mut self, size: u64) -> &mut Self {
        let field = octal_field(size, SIZE_LEN);
        self.set(SIZE_OFFSET, SIZE_LEN, &field)
    }

    /// Sets the mtime field from a Unix timestamp, encoded as null-terminated
    /// octal.
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

    /// Renders this header as a 512-byte block, computing the checksum field
    /// over the rest of the block as the ustar specification requires: the
    /// checksum field itself is treated as eight spaces while the sum is taken.
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

/// Concatenates pax records into the data block of a per-file extended header
/// entry.
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

    /// Appends one entry: the header block followed by its data, padded with
    /// zero bytes to the next 512-byte boundary. The data length need not match
    /// the header's declared size.
    pub fn push(&mut self, header: &TarHeader, data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(&header.to_bytes());
        self.push_data(data)
    }

    /// Appends one entry using an already-rendered 512-byte header block, for
    /// cases that need direct control over the checksum bytes.
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
