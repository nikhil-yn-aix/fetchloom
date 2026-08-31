//! A cheaply cloned handle onto one seekable source, so a member body can keep
//! reading after the call that opened it returns.

use std::cell::RefCell;
use std::io::{Read, Result, Seek, SeekFrom};
use std::rc::Rc;

/// A source shared between the archive reader and the bodies it hands out.
pub struct SharedSource<R> {
    inner: Rc<RefCell<R>>,
}

impl<R> SharedSource<R> {
    /// Wraps a source in a handle that can be cloned.
    pub fn new(source: R) -> Self {
        Self {
            inner: Rc::new(RefCell::new(source)),
        }
    }
}

impl<R> Clone for SharedSource<R> {
    fn clone(&self) -> Self {
        Self {
            inner: Rc::clone(&self.inner),
        }
    }
}

impl<R: Read> Read for SharedSource<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        self.inner.borrow_mut().read(buf)
    }
}

impl<R: Seek> Seek for SharedSource<R> {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        self.inner.borrow_mut().seek(pos)
    }
}
