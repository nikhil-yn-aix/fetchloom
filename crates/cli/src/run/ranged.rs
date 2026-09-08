//! Reading an object over ranged requests, so a format with an index at its end
//! costs the index rather than the object.

use std::io::{Read, Seek, SeekFrom};

use fetchloom_engine::credential::Credential;
use fetchloom_engine::seam::source::{ByteRange, Source};

pub(crate) const WINDOW_BYTES: u64 = 1 << 16;

pub(crate) struct RangedReader<S: Source> {
    source: S,
    location: String,
    credential: Option<Credential>,
    length: u64,
    position: u64,
    window: Vec<u8>,
    window_at: u64,
}

impl<S: Source> RangedReader<S> {
    pub(crate) fn new(
        source: S,
        location: &str,
        credential: Option<Credential>,
        length: u64,
    ) -> Self {
        Self {
            source,
            location: location.to_owned(),
            credential,
            length,
            position: 0,
            window: Vec::new(),
            window_at: 0,
        }
    }

    fn held(&self, at: u64) -> Option<&[u8]> {
        let end = self.window_at + self.window.len() as u64;
        if at < self.window_at || at >= end {
            return None;
        }
        let offset = usize::try_from(at - self.window_at).ok()?;
        Some(&self.window[offset..])
    }

    fn fill(&mut self, at: u64, wanted: usize) -> std::io::Result<()> {
        let span = u64::try_from(wanted)
            .unwrap_or(WINDOW_BYTES)
            .max(WINDOW_BYTES);
        let end = at.saturating_add(span).min(self.length);
        if end <= at {
            self.window.clear();
            self.window_at = at;
            return Ok(());
        }
        let served = self
            .source
            .fetch(
                &self.location,
                Some(ByteRange { start: at, end }),
                self.credential.as_ref(),
                None,
            )
            .map_err(std::io::Error::other)?;
        let mut taken = Vec::with_capacity(usize::try_from(end - at).unwrap_or_default());
        let body = served.body;
        body.take(end - at).read_to_end(&mut taken)?;
        self.window = taken;
        self.window_at = at;
        Ok(())
    }
}

impl<S: Source> Read for RangedReader<S> {
    fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
        if self.position >= self.length || into.is_empty() {
            return Ok(0);
        }
        if self.held(self.position).is_none_or(<[u8]>::is_empty) {
            self.fill(self.position, into.len())?;
        }
        let Some(held) = self.held(self.position) else {
            return Ok(0);
        };
        let taken = held.len().min(into.len());
        into[..taken].copy_from_slice(&held[..taken]);
        self.position += taken as u64;
        Ok(taken)
    }
}

impl<S: Source> Seek for RangedReader<S> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let at = match to {
            SeekFrom::Start(at) => i128::from(at),
            SeekFrom::End(offset) => i128::from(self.length) + i128::from(offset),
            SeekFrom::Current(offset) => i128::from(self.position) + i128::from(offset),
        };
        if at < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a seek before the start of the object",
            ));
        }
        self.position = u64::try_from(at).unwrap_or(self.length);
        Ok(self.position)
    }
}
