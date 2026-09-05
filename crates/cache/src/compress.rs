//! The seekable frame format cached objects are written in, and the probe that
//! decides whether a given object is worth writing that way at all.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use fetchloom_engine::compression::{
    CompressionChoice, PROBE_HEAD_BYTES, PROBE_RATIO, Stored, Transform, shuffle, unshuffle,
};
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::limits::COMPRESSION_FRAME_BYTES;

const MAGIC: [u8; 4] = *b"FLZ1";

const TAIL: usize = 8 + 8 + 1 + 1 + 4 + 4;

/// What the probe found, and what it decided from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub stored: Stored,
    pub plain_ratio: f64,
    pub shuffled_ratio: f64,
    pub probed_bytes: u64,
}

impl Decision {
    #[must_use]
    pub fn reason(&self) -> String {
        match self.stored {
            Stored::Raw => format!(
                "its first {} bytes compressed at {:.3} plain and {:.3} byte-shuffled, and neither clears the {PROBE_RATIO:.2} a compressed object has to reach to be worth the processor time",
                self.probed_bytes, self.plain_ratio, self.shuffled_ratio
            ),
            Stored::Zstd { level, transform } => format!(
                "its first {} bytes compressed at {:.3} plain and {:.3} byte-shuffled, so it is stored zstd:{level} {}",
                self.probed_bytes,
                self.plain_ratio,
                self.shuffled_ratio,
                transform.label()
            ),
        }
    }
}

fn compressed_length(bytes: &[u8], level: i32) -> Result<usize, Error> {
    zstd::bulk::compress(bytes, level)
        .map(|held| held.len())
        .map_err(|reason| codec_failure(&reason))
}

fn codec_failure(reason: &std::io::Error) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!("run cache clear, because the compressor refused the bytes it was given: {reason}"),
    )
}

#[expect(
    clippy::cast_precision_loss,
    reason = "both lengths are of a probe sample bounded by PROBE_HEAD_BYTES, far below what a f64 mantissa holds exactly"
)]
fn ratio_of(plain: usize, compressed: usize) -> f64 {
    if compressed == 0 {
        return 1.0;
    }
    plain as f64 / compressed as f64
}

/// Decides how one object is stored, by compressing its head rather than by
/// reading its name.
///
/// # Errors
/// `cache.corrupt` when the compressor refuses the head it is given.
pub fn decide(head: &[u8], choice: CompressionChoice) -> Result<Decision, Error> {
    let level = match choice {
        CompressionChoice::None => {
            return Ok(Decision {
                stored: Stored::Raw,
                plain_ratio: 1.0,
                shuffled_ratio: 1.0,
                probed_bytes: 0,
            });
        }
        CompressionChoice::Auto => fetchloom_engine::compression::MIN_LEVEL,
        CompressionChoice::Zstd(level) => level,
    };
    let sample = &head[..head.len().min(PROBE_HEAD_BYTES)];
    let plain_ratio = ratio_of(sample.len(), compressed_length(sample, level)?);
    let shuffled_ratio = ratio_of(sample.len(), compressed_length(&shuffle(sample), level)?);
    let transform = if shuffled_ratio > plain_ratio {
        Transform::Shuffle
    } else {
        Transform::None
    };
    let stored = if matches!(choice, CompressionChoice::Auto)
        && plain_ratio.max(shuffled_ratio) < PROBE_RATIO
    {
        Stored::Raw
    } else {
        Stored::Zstd { level, transform }
    };
    Ok(Decision {
        stored,
        plain_ratio,
        shuffled_ratio,
        probed_bytes: sample.len() as u64,
    })
}

#[must_use]
fn transform_byte(transform: Transform) -> u8 {
    match transform {
        Transform::None => 0,
        Transform::Shuffle => 1,
    }
}

fn transform_of(byte: u8, at: &Path) -> Result<Transform, Error> {
    match byte {
        0 => Ok(Transform::None),
        1 => Ok(Transform::Shuffle),
        other => Err(malformed(at, &format!("a byte transform numbered {other}"))),
    }
}

fn malformed(at: &Path, what: &str) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "run cache repair, because {} states {what} this build does not read",
            at.display()
        ),
    )
}

/// Reads an object from `source` and writes it to `into` as frames of exactly
/// [`COMPRESSION_FRAME_BYTES`] of input each, followed by the table of their
/// compressed lengths, so a later read decompresses only the frames covering
/// the range it asked for.
///
/// # Errors
/// `cache.corrupt` when the bytes cannot be read or written, and
/// `resource.disk` when the volume is full.
pub fn write_frames(
    source: &mut impl Read,
    into: &mut impl Write,
    at: &Path,
    level: i32,
    transform: Transform,
) -> Result<u64, Error> {
    let frame = usize::try_from(COMPRESSION_FRAME_BYTES).unwrap_or(1 << 20);
    let mut buffer = vec![0u8; frame];
    let mut lengths: Vec<u32> = Vec::new();
    let mut plain_total = 0u64;
    let mut written = 0u64;
    loop {
        let mut filled = 0;
        while filled < frame {
            let taken = source
                .read(&mut buffer[filled..])
                .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;
            if taken == 0 {
                break;
            }
            filled += taken;
        }
        if filled == 0 {
            break;
        }
        let staged;
        let feed = match transform {
            Transform::None => &buffer[..filled],
            Transform::Shuffle => {
                staged = shuffle(&buffer[..filled]);
                staged.as_slice()
            }
        };
        let compressed =
            zstd::bulk::compress(feed, level).map_err(|reason| codec_failure(&reason))?;
        into.write_all(&compressed)
            .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;
        lengths.push(
            u32::try_from(compressed.len())
                .map_err(|_| malformed(at, "a frame longer than a four byte length describes"))?,
        );
        written += compressed.len() as u64;
        plain_total += filled as u64;
        if filled < frame {
            break;
        }
    }

    let count = u32::try_from(lengths.len())
        .map_err(|_| malformed(at, "more frames than a four byte count describes"))?;
    let mut footer = Vec::with_capacity(lengths.len() * 4 + TAIL);
    for length in &lengths {
        footer.extend_from_slice(&length.to_le_bytes());
    }
    footer.extend_from_slice(&plain_total.to_le_bytes());
    footer.extend_from_slice(&COMPRESSION_FRAME_BYTES.to_le_bytes());
    footer.push(transform_byte(transform));
    footer.push(u8::try_from(level).unwrap_or_default());
    footer.extend_from_slice(&count.to_le_bytes());
    footer.extend_from_slice(&MAGIC);
    into.write_all(&footer)
        .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;
    Ok(written + footer.len() as u64)
}

/// One compressed object, read by range.
#[derive(Debug)]
pub struct Frames {
    file: std::fs::File,
    base: u64,
    starts: Vec<u64>,
    lengths: Vec<u32>,
    frame_bytes: u64,
    transform: Transform,
    plain_length: u64,
    held: Option<(usize, Vec<u8>)>,
}

impl Frames {
    /// # Errors
    /// `cache.corrupt` when the file carries no frame table this build reads,
    /// or one describing more bytes than the file holds.
    pub fn open(file: std::fs::File, at: &Path) -> Result<Self, Error> {
        let span = file
            .metadata()
            .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?
            .len();
        Self::open_at(file, 0, span, at)
    }

    /// The same, for a compressed object written inside a larger file, such as
    /// one entry of a pack.
    ///
    /// # Errors
    /// The kinds [`Frames::open`] gives.
    pub fn open_at(
        mut file: std::fs::File,
        base: u64,
        span: u64,
        at: &Path,
    ) -> Result<Self, Error> {
        if span < TAIL as u64 {
            return Err(malformed(at, "fewer bytes than a frame table"));
        }
        let on_disk = span;
        let mut tail = [0u8; TAIL];
        file.seek(SeekFrom::Start(base + on_disk - TAIL as u64))
            .and_then(|_| file.read_exact(&mut tail))
            .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;
        if tail[TAIL - 4..] != MAGIC {
            return Err(malformed(at, "no frame table"));
        }
        let plain_length = u64::from_le_bytes(number(&tail[0..8]));
        let frame_bytes = u64::from_le_bytes(number(&tail[8..16]));
        if frame_bytes == 0 {
            return Err(malformed(at, "a frame size of zero"));
        }
        let transform = transform_of(tail[16], at)?;
        let count = u32::from_le_bytes(number(&tail[18..22])) as usize;

        let table_bytes = count * 4;
        let table_start = on_disk
            .checked_sub(TAIL as u64 + table_bytes as u64)
            .ok_or_else(|| malformed(at, "a frame table longer than the file"))?;
        let mut table = vec![0u8; table_bytes];
        file.seek(SeekFrom::Start(base + table_start))
            .and_then(|_| file.read_exact(&mut table))
            .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;

        let mut lengths = Vec::with_capacity(count);
        let mut starts = Vec::with_capacity(count);
        let mut offset = 0u64;
        for slot in table.as_chunks::<4>().0 {
            let length = u32::from_le_bytes(*slot);
            starts.push(offset);
            lengths.push(length);
            offset += u64::from(length);
        }
        if offset != table_start {
            return Err(malformed(
                at,
                "a frame table whose lengths do not reach the table itself",
            ));
        }
        Ok(Self {
            file,
            base,
            starts,
            lengths,
            frame_bytes,
            transform,
            plain_length,
            held: None,
        })
    }

    #[must_use]
    pub fn plain_length(&self) -> u64 {
        self.plain_length
    }

    fn frame(&mut self, index: usize, at: &Path) -> Result<&[u8], Error> {
        if self.held.as_ref().is_none_or(|(held, _)| *held != index) {
            let mut packed = vec![0u8; self.lengths[index] as usize];
            self.file
                .seek(SeekFrom::Start(self.base + self.starts[index]))
                .and_then(|_| self.file.read_exact(&mut packed))
                .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;
            let wanted = usize::try_from(
                self.plain_length
                    .saturating_sub(index as u64 * self.frame_bytes)
                    .min(self.frame_bytes),
            )
            .unwrap_or(0);
            let plain =
                zstd::bulk::decompress(&packed, wanted).map_err(|reason| codec_failure(&reason))?;
            let plain = match self.transform {
                Transform::None => plain,
                Transform::Shuffle => unshuffle(&plain),
            };
            if plain.len() != wanted {
                return Err(malformed(at, "a frame that decompresses to another length"));
            }
            self.held = Some((index, plain));
        }
        Ok(self
            .held
            .as_ref()
            .map_or(&[][..], |(_, bytes)| bytes.as_slice()))
    }

    /// Fills `into` from the object's uncompressed bytes at `offset`, reading
    /// only the frames that cover it.
    ///
    /// # Errors
    /// `cache.corrupt` when a frame covering the offset cannot be read or does
    /// not decompress to the length the table states.
    pub fn read_at(&mut self, offset: u64, into: &mut [u8], at: &Path) -> Result<usize, Error> {
        if offset >= self.plain_length || into.is_empty() {
            return Ok(0);
        }
        let index = usize::try_from(offset / self.frame_bytes).unwrap_or(usize::MAX);
        if index >= self.lengths.len() {
            return Ok(0);
        }
        let within = usize::try_from(offset % self.frame_bytes).unwrap_or(0);
        let frame = self.frame(index, at)?;
        let taken = frame.len().saturating_sub(within).min(into.len());
        into[..taken].copy_from_slice(&frame[within..within + taken]);
        Ok(taken)
    }
}

fn number<const N: usize>(from: &[u8]) -> [u8; N] {
    let mut into = [0u8; N];
    into.copy_from_slice(&from[..N]);
    into
}
