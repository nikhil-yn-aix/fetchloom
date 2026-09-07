//! Byte-exact tar and zip writers, and the named corpus of hostile and benign
//! archives built from them for the archive-reader adversarial suite.

mod corpus;
mod deflate;
mod tar;
mod zip;

pub use corpus::{Container, Corpus, CorpusEntry, Expectation};
pub use tar::{
    TYPEFLAG_BLOCKDEV, TYPEFLAG_DIRECTORY, TYPEFLAG_HARDLINK, TYPEFLAG_PAX, TYPEFLAG_REGULAR,
    TYPEFLAG_SYMLINK, TarHeader, TarWriter, pax_block, pax_record,
};
pub use zip::{
    METHOD_DEFLATE, METHOD_STORE, Zip64End, ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter,
    crc32,
};
