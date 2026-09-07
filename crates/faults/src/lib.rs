//! Fault injection, and the observer that records what a run emitted.

mod archives;
mod ftp;
mod http;
mod observer;
mod platform;
mod schedule;

pub use archives::{
    Container, Corpus, CorpusEntry, Expectation, METHOD_DEFLATE, METHOD_STORE, TYPEFLAG_BLOCKDEV,
    TYPEFLAG_DIRECTORY, TYPEFLAG_HARDLINK, TYPEFLAG_PAX, TYPEFLAG_REGULAR, TYPEFLAG_SYMLINK,
    TarHeader, TarWriter, Zip64End, ZipCentralHeader, ZipLocalHeader, ZipMember, ZipWriter, crc32,
    pax_block, pax_record,
};
pub use ftp::{FtpScript, FtpTestServer};
pub use http::{Flight, InFlight, IndexFormat, Latency, Received, Reply, Script, TestServer};
pub use observer::RecordingObserver;
pub use platform::FaultyPlatform;
pub use schedule::{Faults, Operation};
