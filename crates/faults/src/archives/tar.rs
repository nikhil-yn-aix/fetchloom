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

pub const TYPEFLAG_REGULAR: u8 = b'0';
pub const TYPEFLAG_HARDLINK: u8 = b'1';
pub const TYPEFLAG_SYMLINK: u8 = b'2';
pub const TYPEFLAG_CHARDEV: u8 = b'3';
pub const TYPEFLAG_BLOCKDEV: u8 = b'4';
pub const TYPEFLAG_DIRECTORY: u8 = b'5';
pub const TYPEFLAG_FIFO: u8 = b'6';
pub const TYPEFLAG_PAX: u8 = b'x';

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
    #[must_use]
    pub fn new() -> Self {
        Self { raw: [0u8; 500] }
    }

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

    pub fn set_name(&mut self, name: &[u8]) -> &mut Self {
        self.set(NAME_OFFSET, NAME_LEN, name)
    }

    pub fn set_mode(&mut self, mode: u32) -> &mut Self {
        let field = octal_field(u64::from(mode), MODE_LEN);
        self.set(MODE_OFFSET, MODE_LEN, &field)
    }

    pub fn set_mode_bytes(&mut self, bytes: &[u8]) -> &mut Self {
        self.set(MODE_OFFSET, MODE_LEN, bytes)
    }

    pub fn set_uid(&mut self, uid: u32) -> &mut Self {
        let field = octal_field(u64::from(uid), UID_LEN);
        self.set(UID_OFFSET, UID_LEN, &field)
    }

    pub fn set_gid(&mut self, gid: u32) -> &mut Self {
        let field = octal_field(u64::from(gid), GID_LEN);
        self.set(GID_OFFSET, GID_LEN, &field)
    }

    pub fn set_size(&mut self, size: u64) -> &mut Self {
        let field = octal_field(size, SIZE_LEN);
        self.set(SIZE_OFFSET, SIZE_LEN, &field)
    }

    pub fn set_mtime(&mut self, mtime: u64) -> &mut Self {
        let field = octal_field(mtime, MTIME_LEN);
        self.set(MTIME_OFFSET, MTIME_LEN, &field)
    }

    pub fn set_typeflag(&mut self, typeflag: u8) -> &mut Self {
        self.raw[TYPEFLAG_OFFSET] = typeflag;
        self
    }

    pub fn set_linkname(&mut self, linkname: &[u8]) -> &mut Self {
        self.set(LINKNAME_OFFSET, LINKNAME_LEN, linkname)
    }

    pub fn set_magic(&mut self, magic: &[u8]) -> &mut Self {
        self.set(MAGIC_OFFSET, MAGIC_LEN, magic)
    }

    pub fn set_version(&mut self, version: &[u8]) -> &mut Self {
        self.set(VERSION_OFFSET, VERSION_LEN, version)
    }

    pub fn set_uname(&mut self, uname: &[u8]) -> &mut Self {
        self.set(UNAME_OFFSET, UNAME_LEN, uname)
    }

    pub fn set_gname(&mut self, gname: &[u8]) -> &mut Self {
        self.set(GNAME_OFFSET, GNAME_LEN, gname)
    }

    pub fn set_devmajor(&mut self, devmajor: u32) -> &mut Self {
        let field = octal_field(u64::from(devmajor), DEVMAJOR_LEN);
        self.set(DEVMAJOR_OFFSET, DEVMAJOR_LEN, &field)
    }

    pub fn set_devminor(&mut self, devminor: u32) -> &mut Self {
        let field = octal_field(u64::from(devminor), DEVMINOR_LEN);
        self.set(DEVMINOR_OFFSET, DEVMINOR_LEN, &field)
    }

    pub fn set_prefix(&mut self, prefix: &[u8]) -> &mut Self {
        self.set(PREFIX_OFFSET, PREFIX_LEN, prefix)
    }

    #[must_use]
    pub fn to_bytes(&self) -> [u8; 512] {
        let mut block = [0u8; 512];
        block[..500].copy_from_slice(&self.raw);
        let sum = checksum_sum(&block);
        block[CHKSUM_OFFSET..CHKSUM_OFFSET + CHKSUM_LEN].copy_from_slice(&checksum_field(sum));
        block
    }

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

#[must_use]
pub fn pax_block(records: &[Vec<u8>]) -> Vec<u8> {
    records
        .iter()
        .flat_map(|record| record.iter().copied())
        .collect()
}

#[derive(Clone, Debug, Default)]
pub struct TarWriter {
    bytes: Vec<u8>,
}

impl TarWriter {
    #[must_use]
    pub fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    pub fn push(&mut self, header: &TarHeader, data: &[u8]) -> &mut Self {
        self.bytes.extend_from_slice(&header.to_bytes());
        self.push_data(data)
    }

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

    #[must_use]
    pub fn finish(mut self) -> Vec<u8> {
        self.bytes.extend(std::iter::repeat_n(0u8, 1024));
        self.bytes
    }
}
