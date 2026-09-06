//! The seekable frame format cached objects are written in, and the probe that
//! decides whether a given object is worth writing that way at all.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use fetchloom_engine::compression::{
    CompressionChoice, PROBE_HEAD_BYTES, PROBE_RATIO, PROBE_STRIDES, Stored, dictionary_label,
    shuffle, stride_label, unshuffle,
};
use fetchloom_engine::error::{Error, ErrorKind, Surface, filesystem_failure};
use fetchloom_engine::limits::COMPRESSION_FRAME_BYTES;

const MAGIC: [u8; 4] = *b"FLZ1";

const TAIL: usize = 8 + 8 + 4 + 1 + 1 + 4 + 4;

/// What the probe found, and what it decided from it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Decision {
    pub stored: Stored,
    pub ratios: [f64; PROBE_STRIDES.len()],
    pub probed_bytes: u64,
}

impl Decision {
    #[must_use]
    pub fn best_ratio(&self) -> f64 {
        self.ratios.iter().copied().fold(f64::MIN, f64::max)
    }

    #[must_use]
    pub fn measurements(&self) -> String {
        PROBE_STRIDES
            .iter()
            .zip(self.ratios)
            .map(|(stride, ratio)| format!("{stride}:{ratio:.3}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[must_use]
    pub fn reason(&self) -> String {
        match self.stored {
            Stored::Raw => format!(
                "its first {} bytes measured {} by stride, and the best of those does not clear the {PROBE_RATIO:.2} a compressed object has to reach to be worth the processor time",
                self.probed_bytes,
                self.measurements()
            ),
            Stored::Zstd {
                level,
                stride,
                dictionary,
            } => format!(
                "its first {} bytes measured {} by stride, so it is stored zstd:{level} {} {}",
                self.probed_bytes,
                self.measurements(),
                stride_label(stride),
                dictionary_label(dictionary)
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

/// Decides how one object is stored, by compressing its head at each stride
/// rather than by reading its name.
///
/// # Errors
/// `cache.corrupt` when the compressor refuses the head it is given.
pub fn decide(head: &[u8], choice: CompressionChoice) -> Result<Decision, Error> {
    let level = match choice {
        CompressionChoice::None => {
            return Ok(Decision {
                stored: Stored::Raw,
                ratios: [1.0; PROBE_STRIDES.len()],
                probed_bytes: 0,
            });
        }
        CompressionChoice::Auto => fetchloom_engine::compression::MIN_LEVEL,
        CompressionChoice::Zstd(level) => level,
    };
    let sample = &head[..head.len().min(PROBE_HEAD_BYTES)];
    let mut ratios = [0.0; PROBE_STRIDES.len()];
    for (slot, stride) in ratios.iter_mut().zip(PROBE_STRIDES) {
        *slot = if stride < 2 {
            ratio_of(sample.len(), compressed_length(sample, level)?)
        } else {
            ratio_of(
                sample.len(),
                compressed_length(&shuffle(sample, stride), level)?,
            )
        };
    }
    let mut best = 0;
    for index in 1..ratios.len() {
        if ratios[index] > ratios[best] {
            best = index;
        }
    }
    let stride = if PROBE_STRIDES[best] < 2 {
        0
    } else {
        PROBE_STRIDES[best]
    };
    let stored = if matches!(choice, CompressionChoice::Auto) && ratios[best] < PROBE_RATIO {
        Stored::Raw
    } else {
        Stored::Zstd {
            level,
            stride,
            dictionary: 0,
        }
    };
    Ok(Decision {
        stored,
        ratios,
        probed_bytes: sample.len() as u64,
    })
}

fn stride_of(byte: u8, at: &Path) -> Result<u8, Error> {
    match byte {
        0 | 2 | 4 | 8 => Ok(byte),
        other => Err(malformed(at, &format!("a byte shuffle stride of {other}"))),
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
/// `dictionary` is the trained dictionary every frame is compressed against, or
/// `None`. Its identifier is the one zstd wrote into its header, which zstd
/// also writes into every frame and checks on the way back.
///
/// # Errors
/// `cache.corrupt` when the bytes cannot be read or written, and
/// `resource.disk` when the volume is full.
pub fn write_frames(
    source: &mut impl Read,
    into: &mut impl Write,
    at: &Path,
    level: i32,
    stride: u8,
    dictionary: Option<&[u8]>,
) -> Result<u64, Error> {
    let frame = usize::try_from(COMPRESSION_FRAME_BYTES).unwrap_or(1 << 20);
    let mut buffer = vec![0u8; frame];
    let mut lengths: Vec<u32> = Vec::new();
    let mut plain_total = 0u64;
    let mut written = 0u64;
    let mut compressor = match dictionary {
        Some(bytes) => Some(
            zstd::bulk::Compressor::with_dictionary(level, bytes)
                .map_err(|reason| codec_failure(&reason))?,
        ),
        None => None,
    };
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
        let feed = if stride < 2 {
            &buffer[..filled]
        } else {
            staged = shuffle(&buffer[..filled], stride);
            staged.as_slice()
        };
        let compressed = match compressor.as_mut() {
            Some(held) => held.compress(feed),
            None => zstd::bulk::compress(feed, level),
        }
        .map_err(|reason| codec_failure(&reason))?;
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
    footer.extend_from_slice(&identifier_of(dictionary).to_le_bytes());
    footer.push(stride);
    footer.push(u8::try_from(level).unwrap_or_default());
    footer.extend_from_slice(&count.to_le_bytes());
    footer.extend_from_slice(&MAGIC);
    into.write_all(&footer)
        .map_err(|reason| filesystem_failure(Surface::Cache, at, &reason))?;
    Ok(written + footer.len() as u64)
}

/// The identifier zstd wrote into a trained dictionary's own header, which it
/// also writes into every frame compressed against it. Zero for no dictionary.
#[must_use]
pub fn identifier_of(dictionary: Option<&[u8]>) -> u32 {
    match dictionary {
        Some(bytes) if bytes.len() >= 8 => u32::from_le_bytes(number(&bytes[4..8])),
        _ => 0,
    }
}

/// One compressed object, read by range.
pub struct Frames {
    file: std::fs::File,
    base: u64,
    starts: Vec<u64>,
    lengths: Vec<u32>,
    frame_bytes: u64,
    stride: u8,
    dictionary: u32,
    decompressor: Option<zstd::bulk::Decompressor<'static>>,
    plain_length: u64,
    held: Option<(usize, Vec<u8>)>,
}

impl std::fmt::Debug for Frames {
    fn fmt(&self, into: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        into.debug_struct("Frames")
            .field("frames", &self.lengths.len())
            .field("frame_bytes", &self.frame_bytes)
            .field("stride", &self.stride)
            .field("dictionary", &self.dictionary)
            .field("plain_length", &self.plain_length)
            .finish_non_exhaustive()
    }
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
        Self::open_at(file, 0, span, at, None)
    }

    /// The same, for a compressed object written inside a larger file, such as
    /// one entry of a pack, and against the dictionary that pack holds.
    ///
    /// # Errors
    /// The kinds [`Frames::open`] gives, and `cache.corrupt` when the frames
    /// state a dictionary the caller did not supply.
    pub fn open_at(
        mut file: std::fs::File,
        base: u64,
        span: u64,
        at: &Path,
        dictionary: Option<&[u8]>,
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
        let wanted = u32::from_le_bytes(number(&tail[16..20]));
        let stride = stride_of(tail[20], at)?;
        let count = u32::from_le_bytes(number(&tail[22..26])) as usize;

        let decompressor = decompressor_for(wanted, dictionary, at)?;

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
            stride,
            dictionary: wanted,
            decompressor,
            plain_length,
            held: None,
        })
    }

    #[must_use]
    pub fn plain_length(&self) -> u64 {
        self.plain_length
    }

    #[must_use]
    pub fn dictionary(&self) -> u32 {
        self.dictionary
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
            let plain = match self.decompressor.as_mut() {
                Some(held) => held.decompress(&packed, wanted),
                None => zstd::bulk::decompress(&packed, wanted),
            }
            .map_err(|reason| codec_failure(&reason))?;
            let plain = if self.stride < 2 {
                plain
            } else {
                unshuffle(&plain, self.stride)
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

fn decompressor_for(
    wanted: u32,
    dictionary: Option<&[u8]>,
    at: &Path,
) -> Result<Option<zstd::bulk::Decompressor<'static>>, Error> {
    if wanted == 0 {
        return Ok(None);
    }
    let Some(bytes) = dictionary else {
        return Err(missing_dictionary(at, wanted));
    };
    zstd::bulk::Decompressor::with_dictionary(bytes)
        .map(Some)
        .map_err(|reason| codec_failure(&reason))
}

fn missing_dictionary(at: &Path, wanted: u32) -> Error {
    Error::new(
        ErrorKind::CacheCorrupt,
        format!(
            "run cache repair {}, because every object in that pack was compressed against dictionary {wanted} and the pack no longer states it, so none of them can be read",
            at.display()
        ),
    )
}

fn number<const N: usize>(from: &[u8]) -> [u8; N] {
    let mut into = [0u8; N];
    into.copy_from_slice(&from[..N]);
    into
}
