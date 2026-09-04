//! Enumeration and bounded reading of the archive formats this build reads.

#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use fetchloom_platform as _;
#[cfg(test)]
use tempfile as _;

mod bare;
pub mod bomb;
mod extract;
mod path;
mod reader;
mod recognize;
mod resolve;
mod shared;
mod tar_reader;
mod zip_reader;

pub use extract::extract;
pub use path::validate_member_path;
pub use reader::ArchiveReader;
pub use recognize::{SNIFF_LENGTH, format_from_extension, recognize};
pub use resolve::resolve;
