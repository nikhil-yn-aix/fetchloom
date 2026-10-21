//! Fetching a JSON record into memory.

use std::io;

use fl_model::partial::PartialRecord;
use fl_transfer::client::Client;
use fl_transfer::download::Download;
use fl_transfer::error::TransferError;
use fl_transfer::sink::Sink;
use fl_transfer::url::SafeUrl;
use serde_json::Value;

use crate::source::SourceError;

const LIMIT: usize = 64 << 20;

struct Capture(Vec<u8>);

impl Sink for Capture {
    fn record(&self) -> Option<PartialRecord> {
        None
    }

    fn allocate(&mut self, length: u64) -> io::Result<()> {
        match usize::try_from(length) {
            Ok(length) if length <= LIMIT => Ok(()),
            _ => Err(too_large()),
        }
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| too_large())?;
        let end = start.checked_add(bytes.len()).ok_or_else(too_large)?;
        if end > LIMIT {
            return Err(too_large());
        }
        if self.0.len() < end {
            self.0.resize(end, 0);
        }
        self.0[start..end].copy_from_slice(bytes);
        Ok(())
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        let start = usize::try_from(offset).map_err(|_| too_large())?;
        let held = self.0.get(start..).unwrap_or_default();
        let count = held.len().min(buffer.len());
        buffer[..count].copy_from_slice(&held[..count]);
        Ok(count)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.0.clear();
        Ok(())
    }

    fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
        Ok(())
    }
}

fn too_large() -> io::Error {
    io::Error::other("the record is larger than 64 MiB")
}

pub(crate) async fn fetch(
    client: &Client,
    url: &SafeUrl,
    reference: &str,
) -> Result<Value, SourceError> {
    let download = Download {
        url: url.clone(),
        length: None,
        stated: Vec::new(),
        stable: false,
        typed: false,
    };
    let finished = client
        .download(&download, Capture(Vec::new()))
        .await
        .map_err(|error| match error {
            TransferError::Status {
                status: 404 | 410, ..
            } => SourceError::Unresolved {
                reference: reference.to_owned(),
            },
            other => SourceError::Transfer(other),
        })?;
    serde_json::from_slice(&finished.sink.0).map_err(|error| SourceError::Body {
        url: url.clone(),
        reason: error.to_string(),
    })
}
