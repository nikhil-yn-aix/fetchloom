//! The one reader type over a `Read + Seek` source that implements the engine's
//! `Archive` seam.

use std::io::{Read, Seek, SeekFrom};

use fetchloom_engine::degrade::{Degradation, DegradeQueue};
use fetchloom_engine::error::{Error, ErrorKind};
use fetchloom_engine::limits::Limits;
use fetchloom_engine::manifest::ArchiveFormat;
use fetchloom_engine::seam::archive::{Archive, ArchiveMember};

use crate::bare::{self, BareCompression};
use crate::shared::SharedSource;
use crate::tar_reader::{self, TarCompression, TarOffset, TarStream};
use crate::zip_reader::{self, ZipOffset};

enum Offsets {
    Tar(Vec<TarOffset>),
    Zip(Vec<ZipOffset>),
    Bare(u64),
}

#[derive(Clone, Copy)]
enum Located {
    Tar(TarOffset),
    Zip(ZipOffset),
    Bare(u64),
}

struct Listing {
    members: Vec<ArchiveMember>,
    offsets: Offsets,
    at: std::collections::HashMap<String, usize>,
}

pub struct ArchiveReader<R> {
    source: SharedSource<R>,
    format: ArchiveFormat,
    name: String,
    limits: Limits,
    on_disk_bytes: u64,
    cache: Option<Listing>,
    tar_stream: Option<TarStream>,
    degradations: DegradeQueue,
}

impl<R: Read + Seek + 'static> ArchiveReader<R> {
    pub fn new(
        source: R,
        format: ArchiveFormat,
        name: impl Into<String>,
        limits: Limits,
    ) -> Result<Self, Error> {
        let mut source = source;
        let on_disk_bytes = source.seek(SeekFrom::End(0)).map_err(|error| {
            Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("could not measure the archive's length: {error}"),
            )
        })?;
        Ok(Self {
            source: SharedSource::new(source),
            format,
            name: name.into(),
            limits,
            on_disk_bytes,
            cache: None,
            tar_stream: None,
            degradations: DegradeQueue::new(),
        })
    }

    #[must_use]
    pub fn take_degradations(&self) -> Vec<Degradation> {
        self.degradations.take()
    }

    fn tar_compression(&self) -> Option<TarCompression> {
        match self.format {
            ArchiveFormat::Tar => Some(TarCompression::None),
            ArchiveFormat::TarGzip => Some(TarCompression::Gzip),
            ArchiveFormat::TarZstd => Some(TarCompression::Zstd),
            ArchiveFormat::TarXz => Some(TarCompression::Xz),
            ArchiveFormat::TarBzip2 => Some(TarCompression::Bzip2),
            _ => None,
        }
    }

    fn bare_compression(&self) -> Option<BareCompression> {
        match self.format {
            ArchiveFormat::Gzip => Some(BareCompression::Gzip),
            ArchiveFormat::Zstd => Some(BareCompression::Zstd),
            ArchiveFormat::Xz => Some(BareCompression::Xz),
            ArchiveFormat::Bzip2 => Some(BareCompression::Bzip2),
            _ => None,
        }
    }

    fn bare_extension(&self) -> &'static str {
        match self.format {
            ArchiveFormat::Gzip => ".gz",
            ArchiveFormat::Zstd => ".zst",
            ArchiveFormat::Xz => ".xz",
            ArchiveFormat::Bzip2 => ".bz2",
            _ => "",
        }
    }

    fn listing(&mut self) -> Result<&Listing, Error> {
        if self.cache.is_none() {
            let (members, offsets) = self.populate()?;
            let at = members
                .iter()
                .enumerate()
                .map(|(index, member)| (member.path.clone(), index))
                .collect();
            self.cache = Some(Listing {
                members,
                offsets,
                at,
            });
        }
        self.cache.as_ref().ok_or_else(|| {
            Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("archive \"{}\" listed nothing", self.name),
            )
        })
    }

    fn populate(&mut self) -> Result<(Vec<ArchiveMember>, Offsets), Error> {
        if let Some(compression) = self.tar_compression() {
            let (members, offsets) = tar_reader::list_members(
                &self.source,
                compression,
                &self.name,
                self.on_disk_bytes,
                self.limits,
            )?;
            return Ok((members, Offsets::Tar(offsets)));
        }
        if self.format == ArchiveFormat::Zip {
            let (members, offsets) = zip_reader::list_members(
                &self.source,
                &self.name,
                self.on_disk_bytes,
                self.limits,
                &self.degradations,
            )?;
            return Ok((members, Offsets::Zip(offsets)));
        }
        let compression = self.bare_compression().unwrap_or(BareCompression::Gzip);
        let member_name = bare::member_name(&self.name, self.bare_extension());
        let member = bare::list_member(
            &self.source,
            compression,
            &member_name,
            self.on_disk_bytes,
            self.limits,
        )?;
        let size = member.size;
        Ok((vec![member], Offsets::Bare(size)))
    }
}

impl<R: Read + Seek + 'static> Archive for ArchiveReader<R> {
    type Body = Box<dyn Read>;

    fn format(&self) -> ArchiveFormat {
        self.format
    }

    fn members(&mut self) -> Result<Vec<ArchiveMember>, Error> {
        Ok(self.listing()?.members.clone())
    }

    fn open(&mut self, member: &ArchiveMember) -> Result<Self::Body, Error> {
        let listing = self.listing()?;
        let found = listing
            .at
            .get(&member.path)
            .copied()
            .filter(|index| listing.members[*index] == *member);
        let Some(index) = found else {
            return Err(Error::new(
                ErrorKind::ArchiveUnsupported,
                format!("member \"{}\" is not in this archive", member.path),
            ));
        };
        let located = match &listing.offsets {
            Offsets::Tar(entries) => Located::Tar(entries[index]),
            Offsets::Zip(entries) => Located::Zip(entries[index]),
            Offsets::Bare(size) => Located::Bare(*size),
        };
        match located {
            Located::Tar(offset) => {
                let compression = self.tar_compression().unwrap_or(TarCompression::None);
                tar_reader::open_member(
                    &mut self.tar_stream,
                    &self.source,
                    compression,
                    &self.name,
                    offset,
                )
            }
            Located::Zip(offset) => zip_reader::open_member(&self.source, offset),
            Located::Bare(size) => {
                let compression = self.bare_compression().unwrap_or(BareCompression::Gzip);
                let member_name = bare::member_name(&self.name, self.bare_extension());
                bare::open_member(&self.source, compression, &member_name, size)
            }
        }
    }
}
