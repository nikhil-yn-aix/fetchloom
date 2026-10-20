//! Where a download's bytes go. The caller owns the place; a download only writes into it.

use std::io;

use fl_model::partial::PartialRecord;

/// A destination for one download's bytes, with room for the record that lets a later run
/// continue it.
///
/// Every call comes from one blocking thread, in order.
pub trait Sink: Send + 'static {
    /// The record an earlier attempt saved. Bytes up to its count are kept; anything after
    /// them is not trusted.
    fn record(&self) -> Option<PartialRecord>;

    /// Makes room for the whole file once its length is known, so a full disk fails first.
    ///
    /// # Errors
    ///
    /// Returns the destination's error.
    fn allocate(&mut self, length: u64) -> io::Result<()>;

    /// Writes `bytes` at `offset`.
    ///
    /// # Errors
    ///
    /// Returns the destination's error.
    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()>;

    /// Reads into `buffer` from `offset` and returns how many bytes were read, zero at the end.
    ///
    /// # Errors
    ///
    /// Returns the destination's error.
    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize>;

    /// Drops every byte held and the record, to start again from zero.
    ///
    /// # Errors
    ///
    /// Returns the destination's error.
    fn reset(&mut self) -> io::Result<()>;

    /// Saves `record` beside the bytes, replacing the one before.
    ///
    /// # Errors
    ///
    /// Returns the destination's error.
    fn save(&mut self, record: &PartialRecord) -> io::Result<()>;
}
