//! A byte-exact zip writer, where every local, central and end-of-directory
//! record is placed by hand so a fixture can be malformed on purpose.

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;

pub const METHOD_STORE: u16 = 0;
pub const METHOD_DEFLATE: u16 = 8;

#[derive(Clone, Debug)]
pub struct ZipLocalHeader {
    pub version_needed: u16,
    pub flags: u16,
    pub method: u16,
    pub mod_time: u16,
    pub mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    pub name: Vec<u8>,
    pub extra: Vec<u8>,
}

impl ZipLocalHeader {
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

#[derive(Clone, Debug)]
pub struct ZipCentralHeader {
    version_made_by: u16,
    pub version_needed: u16,
    pub flags: u16,
    pub method: u16,
    pub mod_time: u16,
    pub mod_date: u16,
    pub crc32: u32,
    pub compressed_size: u32,
    pub uncompressed_size: u32,
    disk_start: u16,
    internal_attrs: u16,
    pub external_attrs: u32,
    local_header_offset: u32,
    pub name: Vec<u8>,
    pub extra: Vec<u8>,
    pub comment: Vec<u8>,
}

impl ZipCentralHeader {
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

#[derive(Clone, Debug)]
pub struct ZipMember {
    pub local: ZipLocalHeader,
    pub central: ZipCentralHeader,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct ZipWriter {
    bytes: Vec<u8>,
    centrals: Vec<ZipCentralHeader>,
}

impl ZipWriter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            centrals: Vec::new(),
        }
    }

    #[must_use]
    pub fn offset(&self) -> u32 {
        u32::try_from(self.bytes.len()).unwrap_or(u32::MAX)
    }

    pub fn push(&mut self, member: ZipMember) -> &mut Self {
        self.bytes.extend_from_slice(&member.local.to_bytes());
        self.bytes.extend_from_slice(&member.data);
        self.centrals.push(member.central);
        self
    }

    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.finish_inner(None)
    }

    #[must_use]
    pub fn finish_zip64(self, end: Zip64End) -> Vec<u8> {
        self.finish_inner(Some(end))
    }

    fn finish_inner(mut self, zip64: Option<Zip64End>) -> Vec<u8> {
        let cd_offset = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX);
        for central in &self.centrals {
            self.bytes.extend_from_slice(&central.to_bytes());
        }
        let cd_size = u32::try_from(self.bytes.len()).unwrap_or(u32::MAX) - cd_offset;
        let count = self.centrals.len();
        let count16 = u16::try_from(count).unwrap_or(u16::MAX);

        if let Some(end) = zip64 {
            let zip64_eocd_offset = self.bytes.len() as u64;
            self.bytes
                .extend_from_slice(&ZIP64_EOCD_SIGNATURE.to_le_bytes());
            self.bytes
                .extend_from_slice(&end.record_size.unwrap_or(44).to_le_bytes());
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

        let stated_count = if zip64.is_some_and(|end| end.count_sentinel) {
            u16::MAX
        } else {
            count16
        };
        let stated_offset = if zip64.is_some_and(|end| end.offset_sentinel) {
            u32::MAX
        } else {
            cd_offset
        };
        self.bytes.extend_from_slice(&EOCD_SIGNATURE.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes.extend_from_slice(&stated_count.to_le_bytes());
        self.bytes.extend_from_slice(&stated_count.to_le_bytes());
        self.bytes.extend_from_slice(&cd_size.to_le_bytes());
        self.bytes.extend_from_slice(&stated_offset.to_le_bytes());
        self.bytes.extend_from_slice(&0u16.to_le_bytes());
        self.bytes
    }
}

/// What the classic end record states where a zip64 end record sits behind it. A
/// writer states a sentinel only where the true value does not fit; a fixture
/// states one where it does, so a reader that trusts the classic field alone is
/// caught by a small archive rather than a four gigabyte one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Zip64End {
    pub count_sentinel: bool,
    pub offset_sentinel: bool,
    pub record_size: Option<u64>,
}

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
