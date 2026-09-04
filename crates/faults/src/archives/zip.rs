//! A byte-exact zip writer, where every local, central and end-of-directory
//! record is placed by hand so a fixture can be malformed on purpose.

const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;

/// The store compression method.
pub const METHOD_STORE: u16 = 0;
/// The deflate compression method.
pub const METHOD_DEFLATE: u16 = 8;

/// A zip local file header, with its own name and size fields independent of
/// any central directory entry for the same member.
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
    /// Returns a stored, uncompressed local header for the given name and data,
    /// with the CRC-32 and sizes computed from the data.
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
    /// Returns a central directory entry matching the given local header, at
    /// the given local header offset.
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

/// One zip member: a local file header and data pair, and the central directory
/// entry that describes it, held separately.
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
/// entries, and data supplied in full, with no sanitization of paths, links, or
/// sizes.
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

    /// Returns the byte offset the next member's local header would be written
    /// at, for building a central directory entry that points at it correctly.
    #[must_use]
    pub fn offset(&self) -> u32 {
        u32::try_from(self.bytes.len()).unwrap_or(u32::MAX)
    }

    /// Appends one member: its local header, its data, and its central
    /// directory entry, held for the central directory written at `finish`.
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

    /// Finishes the archive as `finish` does, additionally writing a Zip64
    /// end-of-central-directory record and locator before the ordinary
    /// end-of-central-directory record.
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

/// Computes the CRC-32 (the zip and gzip variant, polynomial `0xEDB88320`) of
/// the given bytes.
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
