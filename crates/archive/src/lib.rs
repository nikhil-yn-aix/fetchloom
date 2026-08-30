//! Enumeration and bounded reading of the archive formats phase 3 ships.
//!
//! This crate does not write to a filesystem and does not know about
//! staging, destinations, or selection. It only lists members and hands out
//! their bytes.

mod bare;
mod bomb;
mod extract;
mod path;
mod reader;
mod recognize;
mod shared;
mod tar_reader;
mod zip_reader;

pub use extract::extract;
pub use reader::ArchiveReader;
pub use recognize::{format_from_extension, recognize};

#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use fetchloom_platform as _;
#[cfg(test)]
use tempfile as _;
