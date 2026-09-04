//! Streaming bytes from a body into the store, under the bandwidth ceiling and
//! the controller a stall answers.

use std::io::{Read, Write};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use crate::digest::ContentDigest;
use crate::error::{Error, ErrorKind};
use crate::seam::source::SourceMetadata;
use crate::tuning::{Answer, Controller, Meter, WriteRate};

use super::Pause;
use super::retry::body_failure;

pub(super) enum Asked<B> {
    Silence,
    Unchanged(ContentDigest),
    Changed(SourceMetadata, B),
}

pub(super) enum Either<B> {
    Fetched(B),
    Revalidated(B),
}

impl<B: Read> Read for Either<B> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Fetched(body) | Self::Revalidated(body) => body.read(buffer),
        }
    }
}

pub(super) struct Backpressure<'a, P> {
    pub(super) meter: Option<&'a Meter>,
    pub(super) pause: &'a P,
    pub(super) controller: &'a Mutex<Controller>,
}

pub(super) fn copy(
    mut body: impl Read,
    writer: &mut impl Write,
    location: &str,
    moved: &mut u64,
    buffer: &mut [u8],
    backpressure: &Backpressure<'_, impl Pause>,
) -> Result<(), Error> {
    let Backpressure {
        meter,
        pause,
        controller,
    } = backpressure;
    let mut rate = WriteRate::default();
    loop {
        if crate::cancel::requested() {
            return Ok(());
        }
        let filled = body
            .read(buffer)
            .map_err(|reason| body_failure(location, &reason))?;
        if filled == 0 {
            return Ok(());
        }
        let accepting = Instant::now();
        writer.write_all(&buffer[..filled]).map_err(|reason| {
            Error::new(
                ErrorKind::CacheCorrupt,
                format!("make room in the cache, because the bytes could not be written: {reason}"),
            )
        })?;
        if rate.observed(filled as u64, accepting.elapsed()) {
            controller
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .answered(Answer::Faltered);
        }
        *moved += filled as u64;
        if let Some(meter) = meter {
            let owed = meter.moved(filled as u64);
            if !owed.is_zero() {
                pause.sleep(owed);
            }
        }
    }
}
