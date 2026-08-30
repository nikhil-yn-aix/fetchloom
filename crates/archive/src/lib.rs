//! Enumeration and bounded reading of the archive formats this build reads.

mod bare;
mod bomb;
mod extract;
mod path;
mod reader;
mod recognize;
mod resolve;
mod shared;
mod tar_reader;
mod zip_reader;

pub use extract::extract;
pub use reader::ArchiveReader;
pub use recognize::{format_from_extension, recognize};
pub use resolve::resolve;

#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use fetchloom_platform as _;
#[cfg(test)]
use tempfile as _;
