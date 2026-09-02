//! The source that reads a file this machine already holds.

use std::io::{Read, Seek, SeekFrom, Take};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use fetchloom_engine::credential::Credential;
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::redact::SafeUrl;
use fetchloom_engine::reference::Host;
use fetchloom_engine::seam::source::{
    ByteRange, Cost, ListingEntry, Revalidated, Served, Source, SourceIdentity, SourceMetadata,
    Validator,
};
use fetchloom_engine::work::WorkCounter;

/// The bytes of one span of a local file.
#[derive(Debug)]
pub struct FileBody {
    reader: Take<std::fs::File>,
}

impl Read for FileBody {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buffer)
    }
}

/// A source that is a path on this machine.
#[derive(Debug)]
pub struct FileSource {
    work: Arc<WorkCounter>,
}

impl FileSource {
    /// Builds a source over the local filesystem.
    #[must_use]
    pub fn new(work: Arc<WorkCounter>) -> Self {
        Self { work }
    }

    /// Reports whether a reference names a path this source can serve.
    #[must_use]
    pub fn serves(location: &str) -> bool {
        !location.contains("://") && Path::new(location).is_file()
    }

    fn open(&self, location: &str) -> Result<(std::fs::File, u64), Error> {
        let path = PathBuf::from(location);
        let handle = std::fs::File::open(&path).map_err(|reason| read_failure(&path, &reason))?;
        let size = handle
            .metadata()
            .map_err(|reason| read_failure(&path, &reason))?
            .len();
        self.work.touched_file();
        Ok((handle, size))
    }

    fn describe(location: &str, size: u64) -> SourceMetadata {
        SourceMetadata {
            location: SafeUrl::new(location),
            host: Host::new("localhost".to_owned()),
            size: Some(size),
            content: None,
            interop: None,
            identity: SourceIdentity::None,
            last_modified: None,
            supports_ranges: true,
            time_to_first_byte: Duration::ZERO,
            retry_after: None,
            cost: Cost {
                egress_charged: Some(false),
                requester_pays: Some(false),
            },
        }
    }
}

impl Source for FileSource {
    type Body = FileBody;

    fn probe(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<SourceMetadata, Error> {
        let (_, size) = self.open(location)?;
        Ok(Self::describe(location, size))
    }

    fn fetch(
        &self,
        location: &str,
        range: Option<ByteRange>,
        _credential: Option<&Credential>,
    ) -> Result<Served<Self::Body>, Error> {
        let (mut handle, size) = self.open(location)?;
        let span = range.unwrap_or(ByteRange {
            start: 0,
            end: size,
        });
        handle
            .seek(SeekFrom::Start(span.start))
            .map_err(|reason| read_failure(Path::new(location), &reason))?;
        Ok(Served {
            metadata: Self::describe(location, size),
            body: FileBody {
                reader: handle.take(span.end.saturating_sub(span.start)),
            },
        })
    }

    fn revalidate(
        &self,
        location: &str,
        _validator: &Validator,
        credential: Option<&Credential>,
    ) -> Result<Revalidated<Self::Body>, Error> {
        Ok(Revalidated::Changed(Box::new(
            self.fetch(location, None, credential)?,
        )))
    }

    fn list(
        &self,
        location: &str,
        _credential: Option<&Credential>,
    ) -> Result<Vec<ListingEntry>, Error> {
        Err(Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a file, because {location} is a path and a path is listed by walking it rather than by asking a source for an index"
            ),
        ))
    }
}

fn read_failure(path: &Path, reason: &std::io::Error) -> Error {
    if reason.kind() == std::io::ErrorKind::NotFound {
        return Error::new(
            ErrorKind::ReferenceUnresolved,
            format!(
                "name a path that exists, because nothing is at {}",
                path.display()
            ),
        );
    }
    filesystem_failure(Surface::Destination, path, reason)
}
