//! tar archives, plain or compressed, read in one pass from start to end.

use std::cell::Cell;
use std::io::{self, BufRead, Read, Seek, SeekFrom};

use ::tar::{Archive, Entry, EntryType};
use bzip2::bufread::BzDecoder;
use flate2::bufread::GzDecoder;
use liblzma::bufread::XzDecoder;
use liblzma::stream::{CONCATENATED, MtStreamBuilder, Stream};

use super::guard::{Guard, Meter};
use super::input::Input;
use super::names::Named;
use super::{Format, Keep, Limits, Unpacking, damaged, refused, refused_member};
use crate::error::Error;
use crate::units;

/// the most the tar crate may read on its own for one member: headers, long names and pax
/// records. it holds long names and pax records in memory whole.
const HEADERS_CAP: u64 = 16 << 20;

/// the most memory an xz stream may ask for to be decompressed. `xz -9e` streams ask for 65 MiB,
/// while a header of a few bytes may ask for 4 GiB before any output.
pub(super) const XZ_MEMORY: u64 = 256 << 20;

/// the largest zstd window, as a power of two: half of `memory`, which the decoder's buffers share,
/// at least libzstd's default 128 MiB, which `zstd --long` uses, and at most `--long=31`'s 2 GiB.
fn zstd_window_log(memory: u64) -> u32 {
    (memory / 2).max(1).ilog2().clamp(27, 31)
}

/// the most of an xz file's end read for its index: 16 bytes or so a block, so a million blocks.
const XZ_INDEX_MOST: u64 = 16 << 20;

/// the stream a tar sits in, decompressed. gzip, xz and bzip2 streams may be several joined end
/// to end, as pigz, bgzip and pbzip2 write them. with `xz_threads`, an xz stream that
/// [`xz_threads`] found alone in its file decodes on that many threads in their memory, and goes
/// on with one decoder from where they were should liblzma fail to start one.
pub(super) fn decompressed<'r>(
    format: Format,
    compressed: impl BufRead + 'r,
    memory: u64,
    xz_threads: Option<Threads<'r>>,
) -> io::Result<Box<dyn Read + 'r>> {
    Ok(match format {
        Format::TarGz => Box::new(Streams::<GzDecoder<_>>::new(compressed)),
        Format::TarXz => {
            let limit = memory.max(XZ_MEMORY);
            match xz_threads {
                // one stream alone, so the decoder needs no CONCATENATED, which it cannot take
                Some(threads) => {
                    tracing::debug!(
                        threads = threads.count,
                        "the xz stream decodes on several threads"
                    );
                    let stream = MtStreamBuilder::new()
                        .threads(threads.count)
                        .memlimit_threading(threads.memory.min(limit))
                        .memlimit_stop(limit)
                        .decoder()?;
                    Box::new(HandOver {
                        decoder: Decoder::Threaded(Box::new(XzDecoder::new_stream(
                            compressed, stream,
                        ))),
                        threads,
                        given: 0,
                        limit,
                    })
                }
                None => Box::new(XzDecoder::new_stream(
                    compressed,
                    Stream::new_auto_decoder(limit, CONCATENATED)?,
                )),
            }
        }
        Format::TarZst => {
            let mut decoder = zstd::stream::read::Decoder::with_buffer(compressed)?;
            decoder.window_log_max(zstd_window_log(memory))?;
            Box::new(decoder)
        }
        Format::TarBz2 => Box::new(Streams::<BzDecoder<_>>::new(compressed)),
        Format::Tar | Format::Zip | Format::SevenZ => Box::new(compressed),
    })
}

/// an xz stream's threads and the most memory they take, as [`xz_threads`] plans them, with the
/// same stream read again from its start and the run's stop, for one decoder to go on from where
/// they were should liblzma fail to start one.
pub(super) struct Threads<'r> {
    pub(super) count: u32,
    pub(super) memory: u64,
    pub(super) again: Box<dyn FnMut() -> io::Result<Box<dyn BufRead + 'r>> + 'r>,
    pub(super) stopped: Box<dyn Fn() -> bool + 'r>,
}

impl<'r> Threads<'r> {
    /// `count` threads in `memory` for the stream of `input`, which `guard` counts and stops.
    pub(super) fn of(
        (count, memory): (u32, u64),
        input: &'r Input,
        guard: &Guard<'r>,
    ) -> Threads<'r> {
        let meter = guard.meter();
        Threads {
            count,
            memory,
            // bytes read again count only up to the archive's size, as a seek's do
            again: Box::new(move || {
                let inner = input.reopen(meter)?;
                Ok(Box::new(Counted { inner, meter }) as Box<dyn BufRead + 'r>)
            }),
            stopped: Box::new(guard.stopping()),
        }
    }
}

/// liblzma's threaded decoder, which hands over to one decoder when liblzma fails to start a
/// thread, as when the system allows no more, or to take memory for one: it says either as memory
/// it cannot allocate. the threaded decoder goes first, with its threads and their buffers; then
/// one decoder reads the stream again from its start and passes over the bytes the threads gave,
/// so the reader takes each byte once. the hand-over retries what is interrupted, and a read that
/// fails leaves it where it was.
struct HandOver<'r> {
    decoder: Decoder<'r>,
    /// how one decoder reads the stream again, and the run's stop while it does.
    threads: Threads<'r>,
    /// the bytes given so far.
    given: u64,
    /// the memory the one decoder may take.
    limit: u64,
}

/// which decoder a [`HandOver`] reads with: liblzma's threads, or one decoder after them.
enum Decoder<'r> {
    /// liblzma's threads decode.
    Threaded(Box<dyn Read + 'r>),
    /// they failed and went, and one decoder is still to read the stream again.
    Failed,
    /// one decoder, with the bytes the threads gave that it has still to pass over.
    One(Box<dyn Read + 'r>, u64),
}

impl Read for HandOver<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            let decoder = match &mut self.decoder {
                Decoder::Failed => {
                    let compressed =
                        uninterrupted(&*self.threads.stopped, &mut *self.threads.again)?;
                    let stream = Stream::new_auto_decoder(self.limit, CONCATENATED)?;
                    let one = Box::new(XzDecoder::new_stream(compressed, stream));
                    self.decoder = Decoder::One(one, self.given);
                    continue;
                }
                Decoder::One(decoder, left) if *left > 0 => {
                    pass_over(decoder, left, &*self.threads.stopped)?;
                    continue;
                }
                Decoder::Threaded(decoder) | Decoder::One(decoder, _) => decoder,
            };
            match decoder.read(buffer) {
                Ok(read) => {
                    self.given += read as u64;
                    return Ok(read);
                }
                Err(err) if matches!(self.decoder, Decoder::Threaded(_)) && lacks_memory(&err) => {
                    tracing::debug!(%err, given = self.given, "liblzma's threads failed, so one decoder goes on from where they were");
                    self.decoder = Decoder::Failed;
                }
                Err(err) => return Err(err),
            }
        }
    }
}

/// has `decoder` pass over `left` of its bytes, counting them down, so a read that fails leaves
/// it where it was.
fn pass_over(decoder: &mut dyn Read, left: &mut u64, stopped: &dyn Fn() -> bool) -> io::Result<()> {
    let mut scratch = vec![0; 64 << 10];
    while *left > 0 {
        let most = usize::try_from(*left).map_or(scratch.len(), |left| left.min(scratch.len()));
        match uninterrupted(stopped, || decoder.read(&mut scratch[..most]))? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            read => *left -= read as u64,
        }
    }
    Ok(())
}

/// `attempt`, tried again while it is interrupted, unless the run asked to stop.
fn uninterrupted<T>(
    stopped: &dyn Fn() -> bool,
    mut attempt: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    loop {
        if stopped() {
            return Err(io::Error::other(Stopped));
        }
        match attempt() {
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            done => return done,
        }
    }
}

/// whether `err` is liblzma's word that it could not allocate memory, which its threaded decoder
/// also says when it cannot start a thread.
pub(super) fn lacks_memory(err: &(dyn std::error::Error + 'static)) -> bool {
    err.downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .and_then(|inner| inner.downcast_ref::<liblzma::stream::Error>())
        .is_some_and(|inner| *inner == liblzma::stream::Error::Mem)
}

/// the run asked to stop while one decoder went over what liblzma's threads gave.
#[derive(Debug)]
pub(super) struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("stopped while unpacking the archive")
    }
}

impl std::error::Error for Stopped {}

/// whether `err`, a decoder's error, is [`Stopped`].
pub(super) fn stopped(err: &(dyn std::error::Error + 'static)) -> bool {
    err.downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .is_some_and(<dyn std::error::Error + Send + Sync>::is::<Stopped>)
}

/// the threads the xz stream in `input` decodes on and the most memory they take, as [`xz_plan`]
/// plans them from the blocks the stream's index lists: none, so one decoder takes it, unless
/// `limits` grants more than one thread and the input is one whole file that is exactly one xz
/// stream whose index lists more than one block.
pub(super) fn xz_threads(input: &Input, format: Format, limits: &Limits) -> Option<(u32, u64)> {
    if format != Format::TarXz || limits.xz_threads < 2 {
        return None;
    }
    let mut file = input.whole_file()?.ok()?;
    let (blocks, largest) = xz_blocks(&mut file).ok()??;
    xz_plan(limits, blocks, largest)
}

/// the threads an xz stream of `blocks` blocks, the largest `largest` bytes decoded, decodes on
/// within `limits`, and the most memory liblzma may take for them, the archive's share of the
/// run's budget: as many as the run grants, the blocks hold and that share holds about three of
/// the largest block for, each thread holding one block's input, its output and its dictionary.
/// none below two, so one decoder takes it.
pub(super) fn xz_plan(limits: &Limits, blocks: u64, largest: u64) -> Option<(u32, u64)> {
    let memory = limits.xz_threading;
    let fit = memory / largest.saturating_mul(3).max(1);
    let threads = u64::from(limits.xz_threads).min(fit).min(blocks);
    (threads >= 2).then(|| (u32::try_from(threads).unwrap_or(u32::MAX), memory))
}

/// the blocks of the xz stream that `file` holds whole, with the largest one's decoded size, as
/// the index at its end lists them; none when the file is not exactly one xz stream, as streams
/// joined end to end, one with padding after it or a legacy .lzma file are not. the decoder still
/// checks the index, so a damaged one only picks the way it decodes.
fn xz_blocks(file: &mut (impl Read + Seek)) -> io::Result<Option<(u64, u64)>> {
    const MAGIC: [u8; 6] = [0xfd, b'7', b'z', b'X', b'Z', 0];
    let size = file.seek(SeekFrom::End(0))?;
    // a stream header and footer of 12 bytes each, around an index of 8 at least
    if size < 32 {
        return Ok(None);
    }
    let mut header = [0; 6];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut header)?;
    let mut footer = [0; 12];
    file.seek(SeekFrom::Start(size - 12))?;
    file.read_exact(&mut footer)?;
    if header != MAGIC || footer[10..] != *b"YZ" {
        return Ok(None);
    }
    let backward = (u64::from(u32::from_le_bytes([
        footer[4], footer[5], footer[6], footer[7],
    ])) + 1)
        * 4;
    if backward > XZ_INDEX_MOST || backward + 24 > size {
        return Ok(None);
    }
    let mut index = vec![0; usize::try_from(backward).unwrap_or(0)];
    file.seek(SeekFrom::Start(size - 12 - backward))?;
    file.read_exact(&mut index)?;
    let mut at = 1;
    let count = match index.first() {
        Some(0) => varint(&index, &mut at),
        _ => None,
    };
    let Some(count) = count else {
        return Ok(None);
    };
    let (mut blocks, mut largest) = (0_u64, 0_u64);
    for _ in 0..count {
        let (Some(unpadded), Some(decoded)) = (varint(&index, &mut at), varint(&index, &mut at))
        else {
            return Ok(None);
        };
        blocks = blocks.saturating_add(unpadded.div_ceil(4).saturating_mul(4));
        largest = largest.max(decoded);
    }
    let stream = blocks.saturating_add(backward).saturating_add(24);
    Ok((stream == size).then_some((count, largest)))
}

/// the xz multibyte integer at `at` in `bytes`, of up to nine bytes of seven bits each, low first,
/// moving `at` past it.
fn varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0_u64;
    for shift in (0..63).step_by(7) {
        let byte = *bytes.get(*at)?;
        *at += 1;
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// whether `err` is an xz stream refused for asking more than [`XZ_MEMORY`] to be decompressed.
pub(super) fn needs_too_much_memory(err: &(dyn std::error::Error + 'static)) -> bool {
    err.downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .and_then(|inner| inner.downcast_ref::<liblzma::stream::Error>())
        .is_some_and(|inner| *inner == liblzma::stream::Error::MemLimit)
}

/// whether `err` is a zstd frame refused for asking a window larger than its decoder allows.
pub(super) fn needs_too_large_window(err: &(dyn std::error::Error + 'static)) -> bool {
    use zstd::zstd_safe::{get_error_name, zstd_sys::ZSTD_ErrorCode};
    // the zstd crate keeps only libzstd's name for the error, so the name is what tells it apart
    let code = ZSTD_ErrorCode::ZSTD_error_frameParameter_windowTooLarge as usize;
    let window_too_large = get_error_name(code.wrapping_neg());
    err.downcast_ref::<io::Error>()
        .and_then(io::Error::get_ref)
        .is_some_and(|inner| inner.to_string() == window_too_large)
}

/// the error for a stream of what `unpacking` says unpacks that would not decompress: too large a
/// zstd window, else damage.
pub(super) fn unreadable_stream(
    unpacking: Unpacking,
    at: Option<&str>,
    err: &(dyn std::error::Error + 'static),
    memory: u64,
) -> Error {
    if needs_too_large_window(err) {
        let window = 1 << zstd_window_log(memory);
        let noun = unpacking.noun();
        return super::past(
            unpacking,
            format!(
                "the {noun}'s zstd stream needs a window larger than {} to {}",
                units::size(window),
                unpacking.verb()
            ),
            &format!("FETCHLOOM_MEMORY, which gives each {noun} a quarter of it"),
            "memory",
        );
    }
    damaged(unpacking, at, err, memory)
}

/// `input`'s bytes as `meter` counts them while a decompressor reads them.
pub(super) fn counted<'m>(input: &Input, meter: &'m Meter) -> Result<impl BufRead + 'm, Error> {
    Ok(Counted {
        inner: input.open(meter)?,
        meter,
    })
}

pub(super) fn unpack(
    input: &Input,
    format: Format,
    guard: &mut Guard<'_>,
    keep: &mut Keep<'_>,
) -> Result<(), Error> {
    let counted = Counted {
        inner: input.open(guard.meter())?,
        meter: guard.meter(),
    };
    let memory = guard.limits().memory;
    let threads =
        xz_threads(input, format, guard.limits()).map(|plan| Threads::of(plan, input, guard));
    let decoded = decompressed(format, counted, memory, threads)
        .map_err(|err| unreadable_stream(Unpacking::Archive, None, &err, memory))?;
    let headers = Headers::default();
    let mut tar = Archive::new(Capped {
        inner: decoded,
        headers: &headers,
    });
    let entries = tar
        .entries()
        .map_err(|err| unreadable_stream(Unpacking::Archive, None, &err, memory))?;
    for entry in entries {
        let mut entry = entry.map_err(|err| {
            headers
                .refusal()
                .unwrap_or_else(|| unreadable_stream(Unpacking::Archive, None, &err, memory))
        })?;
        guard.skip(headers.read.replace(0))?;
        guard.entry()?;
        let raw = String::from_utf8(entry.path_bytes().into_owned()).map_err(|err| {
            refused(format!(
                "the archive holds \"{}\", a name that is not valid UTF-8",
                String::from_utf8_lossy(err.as_bytes())
            ))
        })?;
        let is_folder = match entry.header().entry_type() {
            EntryType::Regular | EntryType::Continuous => false,
            EntryType::Directory => true,
            // pax global headers carry defaults for the whole archive, such as the commit id
            // git archive writes, and name no file
            EntryType::XGlobalHeader => continue,
            EntryType::Link => {
                headers.in_member.set(true);
                let linked = hardlink(guard, &raw, &mut entry);
                headers.in_member.set(false);
                linked?;
                continue;
            }
            other => return Err(refused_member(&raw, &special(other))),
        };
        if is_sparse(&mut entry) {
            return Err(refused_member(&raw, "it is a sparse file"));
        }
        match guard.name(&raw, is_folder)? {
            Named::File(path) => {
                guard.fits(&path, entry.size())?;
                headers.in_member.set(true);
                let kept = keep(guard, path, entry.size(), &mut entry);
                headers.in_member.set(false);
                kept?;
            }
            Named::System(path) => {
                // read here rather than by the tar crate, so its bytes are no headers
                headers.in_member.set(true);
                let passed = guard.pass(&path, &mut entry);
                headers.in_member.set(false);
                passed?;
                guard.leave_out(path);
            }
            Named::Unstorable => {
                headers.in_member.set(true);
                let passed = guard.pass(&raw, &mut entry);
                headers.in_member.set(false);
                passed?;
            }
            Named::Folder => {}
        }
    }
    guard.skip(headers.read.replace(0))?;
    // a compressed stream's own check, such as gzip's CRC, comes after the tar's end
    if !matches!(format, Format::Tar) {
        let mut rest = tar.into_inner().inner.take(HEADERS_CAP + 1);
        let trailing = io::copy(&mut rest, &mut io::sink())
            .map_err(|err| unreadable_stream(Unpacking::Archive, None, &err, memory))?;
        if trailing > HEADERS_CAP {
            return Err(refused(format!(
                "the archive holds more than {} after its end",
                units::size(HEADERS_CAP)
            )));
        }
        guard.skip(trailing)?;
    }
    Ok(())
}

/// takes the hardlink `raw`, tar's way of writing a file's second name, as that file's bytes. its
/// target must be an earlier file in the archive; any other could reach outside it.
fn hardlink(
    guard: &mut Guard<'_>,
    raw: &str,
    entry: &mut Entry<'_, impl Read>,
) -> Result<(), Error> {
    let target = entry
        .link_name_bytes()
        .map(|target| String::from_utf8_lossy(&target).into_owned())
        .unwrap_or_default();
    // looked up before the link's own name is added, so a link to itself finds nothing
    let earlier = guard.earlier_file(&target);
    match guard.name(raw, false)? {
        Named::System(path) => guard.leave_out(path),
        Named::Unstorable => {}
        Named::File(path) => {
            let Some(earlier) = earlier else {
                return Err(refused_member(
                    raw,
                    &format!(
                        "it is a hardlink to \"{}\", which is no file before it in the archive",
                        target.escape_debug()
                    ),
                ));
            };
            guard.link(path, earlier);
        }
        Named::Folder => return Err(refused_member(raw, "it is a hardlink named as a folder")),
    }
    // a hardlink usually holds no bytes of its own; any it holds are its target's again
    guard.pass(raw, entry)
}

fn special(kind: EntryType) -> String {
    match kind {
        EntryType::Symlink => "it is a symlink".to_owned(),
        EntryType::Link => "it is a hardlink".to_owned(),
        EntryType::Char | EntryType::Block => "it is a device".to_owned(),
        EntryType::Fifo => "it is a fifo".to_owned(),
        EntryType::GNUSparse => "it is a sparse file".to_owned(),
        other => format!(
            "it is a special entry of type '{}'",
            char::from(other.as_byte()).escape_default()
        ),
    }
}

/// whether pax records mark the entry as a GNU sparse file. the tar crate would hand over its
/// sparse map as if it were the file's bytes.
fn is_sparse(entry: &mut Entry<'_, impl Read>) -> bool {
    entry
        .pax_extensions()
        .ok()
        .flatten()
        .is_some_and(|mut records| {
            records.any(|record| {
                record.is_ok_and(|record| record.key_bytes().starts_with(b"GNU.sparse."))
            })
        })
}

/// the compressed stream, counting each byte the decompressor takes.
struct Counted<'m, R> {
    inner: R,
    meter: &'m Meter,
}

impl<R: BufRead> Read for Counted<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.meter.count(read);
        Ok(read)
    }
}

impl<R: BufRead> BufRead for Counted<'_, R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.meter.count(amount);
        self.inner.consume(amount);
    }
}

/// what the tar crate read on its own since the last member.
#[derive(Default)]
struct Headers {
    in_member: Cell<bool>,
    read: Cell<u64>,
}

impl Headers {
    fn refusal(&self) -> Option<Error> {
        (self.read.get() > HEADERS_CAP).then(|| {
            refused(format!(
                "the archive holds a member whose tar headers take more than {}",
                units::size(HEADERS_CAP)
            ))
        })
    }
}

/// the decompressed stream, failing once the tar crate reads past [`HEADERS_CAP`] outside members.
struct Capped<'h, R> {
    inner: R,
    headers: &'h Headers,
}

impl<R: Read> Read for Capped<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        if !self.headers.in_member.get() {
            let total = self.headers.read.get() + read as u64;
            self.headers.read.set(total);
            if total > HEADERS_CAP {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "tar headers too large",
                ));
            }
        }
        Ok(read)
    }
}

/// a decoder of one compressed stream, which gives back what follows the stream once it ends.
trait OneStream: Read + Sized {
    type Input: BufRead;
    /// the format, as an error names it.
    const NAME: &'static str;
    fn start(input: Self::Input) -> Self;
    fn input(&mut self) -> &mut Self::Input;
    fn into_input(self) -> Self::Input;
}

impl<R: BufRead> OneStream for GzDecoder<R> {
    type Input = R;
    const NAME: &'static str = "gzip";
    fn start(input: R) -> Self {
        GzDecoder::new(input)
    }
    fn input(&mut self) -> &mut R {
        self.get_mut()
    }
    fn into_input(self) -> R {
        self.into_inner()
    }
}

impl<R: BufRead> OneStream for BzDecoder<R> {
    type Input = R;
    const NAME: &'static str = "bzip2";
    fn start(input: R) -> Self {
        BzDecoder::new(input)
    }
    fn input(&mut self) -> &mut R {
        self.get_mut()
    }
    fn into_input(self) -> R {
        self.into_inner()
    }
}

/// streams joined end to end, as pigz, bgzip and pbzip2 write them, then perhaps zero bytes up to
/// the end, which gzip and bzip2 ignore too. any other byte after a stream starts another.
struct Streams<D> {
    /// the stream being read, none once the last one ended.
    stream: Option<D>,
}

impl<D: OneStream> Streams<D> {
    fn new(input: D::Input) -> Streams<D> {
        Streams {
            stream: Some(D::start(input)),
        }
    }
}

impl<D: OneStream> Read for Streams<D> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while let Some(stream) = &mut self.stream {
            let read = stream.read(buffer)?;
            if read > 0 || buffer.is_empty() {
                return Ok(read);
            }
            let input = stream.input();
            match input.fill_buf()?.first().copied() {
                None => self.stream = None,
                Some(0) => {
                    zeros_to_end(input, D::NAME)?;
                    self.stream = None;
                }
                Some(_) => {
                    let input = self.stream.take().map(D::into_input);
                    self.stream = input.map(D::start);
                }
            }
        }
        Ok(0)
    }
}

/// reads past zero bytes to the end of `compressed`, failing at any other byte after the `name`
/// stream.
fn zeros_to_end(compressed: &mut impl BufRead, name: &str) -> io::Result<()> {
    loop {
        let bytes = compressed.fill_buf()?;
        if bytes.is_empty() {
            return Ok(());
        }
        if bytes.iter().any(|byte| *byte != 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bytes other than zeros come after its {name} stream"),
            ));
        }
        let zeros = bytes.len();
        compressed.consume(zeros);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::BufRead;

    use ::tar::{Builder, EntryType, Header};

    use super::super::samples::{
        Scratch, compress, pax, raw_entry, tar_of, xz_asking_for, xz_in_blocks, zstd_asking_for,
    };
    use super::super::{Format, Input, Limits, Unpacking};
    use super::{Decoder, HandOver, Read, Threads, XZ_MEMORY, decompressed, xz_blocks, xz_threads};
    use crate::digest;
    use crate::error::Kind;

    const FILES: &[(&str, &[u8])] = &[
        ("data/train.csv", b"x,y\n1,2\n"),
        ("data/empty.txt", b""),
        ("README.md", b"# readme\n"),
    ];

    const COMPRESSED: [Format; 4] = [Format::TarGz, Format::TarXz, Format::TarZst, Format::TarBz2];

    fn paths(scratch: &Scratch, format: Format, tar: &[u8]) -> Vec<String> {
        let unpacked = scratch.unpack(format, tar).unwrap();
        unpacked
            .members
            .into_iter()
            .map(|member| member.path)
            .collect()
    }

    fn built(entries: impl FnOnce(&mut Builder<Vec<u8>>)) -> Vec<u8> {
        let mut tar = Builder::new(Vec::new());
        entries(&mut tar);
        tar.into_inner().unwrap()
    }

    #[test]
    fn every_compression_unpacks() {
        let tar = tar_of(FILES);
        for format in [Format::Tar].into_iter().chain(COMPRESSED) {
            let scratch = Scratch::new();
            let unpacked = scratch.unpack(format, &compress(format, &tar)).unwrap();
            let paths: Vec<_> = unpacked.members.iter().map(|m| m.path.as_str()).collect();
            assert_eq!(
                paths,
                ["README.md", "data/empty.txt", "data/train.csv"],
                "{format:?}"
            );
            scratch.assert_stored(&unpacked.members[1], b"");
            scratch.assert_stored(&unpacked.members[2], b"x,y\n1,2\n");
        }
    }

    #[test]
    fn streams_joined_end_to_end_unpack() {
        let tar = tar_of(FILES);
        let (head, tail) = tar.split_at(1024);
        for format in COMPRESSED {
            let mut joined = compress(format, head);
            joined.extend(compress(format, tail));
            let scratch = Scratch::new();
            let unpacked = scratch.unpack(format, &joined).unwrap();
            let paths: Vec<_> = unpacked
                .members
                .iter()
                .map(|member| member.path.as_str())
                .collect();
            assert_eq!(
                paths,
                ["README.md", "data/empty.txt", "data/train.csv"],
                "{format:?}"
            );
            for member in &unpacked.members {
                let (_, bytes) = FILES.iter().find(|(path, _)| *path == member.path).unwrap();
                assert_eq!(
                    member.size,
                    bytes.len() as u64,
                    "{format:?}: {}",
                    member.path
                );
                scratch.assert_stored(member, bytes);
            }
        }
    }

    #[test]
    fn a_damaged_check_after_the_tar_end_is_caught() {
        let mut packed = compress(Format::TarGz, &tar_of(FILES));
        let crc = packed.len() - 8;
        packed[crc] ^= 0xff;
        let err = Scratch::new().refuse(Format::TarGz, &packed);
        assert!(err.message.contains("damaged"), "{err:?}");
    }

    /// zero bytes after a gzip or bzip2 stream are padding, which gzip and bzip2 ignore, while any
    /// other bytes after it, even after zeros, are damage.
    #[test]
    fn zeros_after_a_gzip_or_bzip2_stream_are_padding() {
        for format in [Format::TarGz, Format::TarBz2] {
            let packed = compress(format, &tar_of(FILES));
            // a few blocks, fewer zeros than a gzip header, and more than one read of the archive
            for zeros in [1024, 1, 9, super::super::BUFFER + 1] {
                let padded = [packed.clone(), vec![0; zeros]].concat();
                assert_eq!(
                    paths(&Scratch::new(), format, &padded),
                    ["README.md", "data/empty.txt", "data/train.csv"],
                    "{format:?}, {zeros} zeros"
                );
            }
            let stream = compress(format, b"");
            let zeros_then_stream = [vec![0; 4], stream].concat();
            for after in [&b"junk"[..], b"\0\0\0\0junk", &zeros_then_stream] {
                let err = Scratch::new().refuse(format, &[&packed[..], after].concat());
                assert!(
                    err.message.starts_with("the archive is damaged"),
                    "{format:?}, {after:?}: {}",
                    err.message
                );
            }
        }
    }

    #[test]
    fn an_empty_tar_unpacks_to_nothing() {
        let unpacked = Scratch::new()
            .unpack(Format::TarGz, &compress(Format::TarGz, &tar_of(&[])))
            .unwrap();
        assert_eq!(unpacked.members, []);
        assert_eq!(unpacked.tree, digest::sha256(b""));
    }

    #[test]
    fn long_names_unpack_from_gnu_and_pax_headers() {
        let long = format!("{}/data.csv", "deep".repeat(40));
        let gnu = tar_of(&[(&long, b"1")]);
        let pax = built(|tar| {
            pax(tar, &[("path", long.as_bytes())]);
            raw_entry(tar, b"short", EntryType::Regular, b"1");
        });
        for tar in [gnu, pax] {
            assert_eq!(
                paths(&Scratch::new(), Format::Tar, &tar),
                std::slice::from_ref(&long)
            );
        }
    }

    #[test]
    fn links_devices_and_fifos_are_refused() {
        let kinds = [
            (EntryType::Symlink, "a symlink"),
            (EntryType::Char, "a device"),
            (EntryType::Block, "a device"),
            (EntryType::Fifo, "a fifo"),
            (EntryType::new(b'V'), "a special entry of type 'V'"),
        ];
        for (kind, reason) in kinds {
            let tar = built(|tar| {
                raw_entry(tar, b"good.txt", EntryType::Regular, b"fine");
                raw_entry(tar, b"special", kind, b"");
            });
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert_eq!(err.kind, Kind::SourceInvalid);
            assert_eq!(
                err.message,
                format!(
                    "the archive holds \"special\", which fetchloom refuses because it is {reason}"
                )
            );
        }
    }

    /// a hardlink entry named `name` that points at `target`, as the archive spells both.
    fn hardlink(tar: &mut Builder<Vec<u8>>, name: &[u8], target: &[u8]) {
        let mut header = Header::new_ustar();
        let fields = header.as_ustar_mut().unwrap();
        fields.name[..name.len()].copy_from_slice(name);
        fields.linkname[..target.len()].copy_from_slice(target);
        header.set_entry_type(EntryType::Link);
        header.set_size(0);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append(&header, &[][..]).unwrap();
    }

    /// a hardlink to an earlier file unpacks to that file's bytes again; a later file, a folder,
    /// the link itself or a path outside the archive is refused, since such a link could reach
    /// outside it.
    #[test]
    fn a_hardlink_to_an_earlier_file_unpacks_as_that_file() {
        let tar = built(|tar| {
            raw_entry(tar, b"h/a.wav", EntryType::Regular, b"RIFF");
            hardlink(tar, b"h/b.wav", b"h/a.wav");
            hardlink(tar, b"h/c.wav", b"./h/b.wav");
        });
        let scratch = Scratch::new();
        let unpacked = scratch.unpack(Format::Tar, &tar).unwrap();
        let members: Vec<(&str, u64)> = unpacked
            .members
            .iter()
            .map(|member| (member.path.as_str(), member.size))
            .collect();
        assert_eq!(members, [("h/a.wav", 4), ("h/b.wav", 4), ("h/c.wav", 4)]);
        for member in &unpacked.members {
            assert_eq!(member.sha256, digest::sha256(b"RIFF"), "{}", member.path);
        }
        assert_eq!(scratch.cache.objects().len(), 1);
        let refused = [
            "h/later.wav",
            "h",
            "/etc/passwd",
            "../a.wav",
            "h/A.wav",
            "h/b.wav",
            "./h/b.wav",
        ];
        for target in refused {
            let tar = built(|tar| {
                raw_entry(tar, b"h/a.wav", EntryType::Regular, b"RIFF");
                hardlink(tar, b"h/b.wav", target.as_bytes());
                raw_entry(tar, b"h/later.wav", EntryType::Regular, b"x");
            });
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert_eq!(
                err.message,
                format!(
                    "the archive holds \"h/b.wav\", which fetchloom refuses because it is a hardlink to \"{target}\", which is no file before it in the archive"
                )
            );
        }
    }

    #[test]
    fn sparse_files_are_refused() {
        let gnu = built(|tar| {
            let mut header = Header::new_gnu();
            header.set_path("disk.img").unwrap();
            header.set_entry_type(EntryType::GNUSparse);
            header.set_size(0);
            header.as_gnu_mut().unwrap().set_real_size(0);
            header.set_cksum();
            tar.append(&header, &[][..]).unwrap();
        });
        let pax = built(|tar| {
            pax(
                tar,
                &[("GNU.sparse.major", b"1"), ("GNU.sparse.name", b"disk.img")],
            );
            raw_entry(
                tar,
                b"GNUSparseFile.0/disk.img",
                EntryType::Regular,
                b"1\n0\n0\n",
            );
        });
        for tar in [gnu, pax] {
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert!(
                err.message.ends_with("it is a sparse file"),
                "{}",
                err.message
            );
        }
    }

    #[test]
    fn one_unsafe_name_refuses_files_already_written() {
        for name in ["../evil.txt", "/etc/passwd", "a/../../b"] {
            let tar = built(|tar| {
                raw_entry(tar, b"good.txt", EntryType::Regular, b"fine");
                raw_entry(tar, name.as_bytes(), EntryType::Regular, b"evil");
            });
            let err = Scratch::new().refuse(Format::Tar, &tar);
            assert!(err.message.contains(name), "{}", err.message);
        }
    }

    #[test]
    fn the_root_entry_and_leading_dot_slashes_are_dropped() {
        let tar = built(|tar| {
            raw_entry(tar, b"./", EntryType::Directory, b"");
            raw_entry(tar, b"./data/", EntryType::Directory, b"");
            raw_entry(tar, b"./data/a.csv", EntryType::Regular, b"1");
            raw_entry(tar, b"old/", EntryType::Regular, b"");
        });
        assert_eq!(paths(&Scratch::new(), Format::Tar, &tar), ["data/a.csv"]);
    }

    #[test]
    fn a_pax_global_header_names_no_file() {
        let tar = built(|tar| {
            let comment = b"52 comment=0123456789abcdef0123456789abcdef01234567\n";
            raw_entry(tar, b"pax_global_header", EntryType::XGlobalHeader, comment);
            raw_entry(tar, b"a.csv", EntryType::Regular, b"1");
        });
        assert_eq!(paths(&Scratch::new(), Format::Tar, &tar), ["a.csv"]);
    }

    #[test]
    fn huge_headers_are_refused_before_they_fill_memory() {
        let tar = built(|tar| {
            pax(tar, &[("comment", &vec![b'a'; 17 << 20])]);
            raw_entry(tar, b"a.csv", EntryType::Regular, b"1");
        });
        let err = Scratch::new().refuse(Format::TarZst, &compress(Format::TarZst, &tar));
        assert!(err.message.contains("tar headers"), "{}", err.message);
    }

    #[test]
    fn data_the_tar_skips_counts_toward_the_ratio() {
        let zeros = vec![0; 1 << 20];
        let tar = built(|tar| {
            for _ in 0..8 {
                raw_entry(tar, b"pax_global_header", EntryType::XGlobalHeader, &zeros);
                raw_entry(tar, b"data/", EntryType::Directory, &zeros);
            }
            raw_entry(tar, b"a.csv", EntryType::Regular, b"1");
        });
        let err = Scratch::new().refuse(Format::TarZst, &compress(Format::TarZst, &tar));
        assert!(err.message.contains("1000 times"), "{}", err.message);
    }

    #[test]
    fn a_zstd_window_takes_half_the_memory_within_libzstd_bounds() {
        let mib = 1 << 20;
        for (memory, log) in [
            (1, 27),
            (256 * mib, 27),
            (512 * mib, 28),
            (1024 * mib, 29),
            (4096 * mib, 31),
            (u64::MAX, 31),
        ] {
            assert_eq!(super::zstd_window_log(memory), log, "{memory}");
        }
    }

    #[test]
    fn a_name_that_is_not_utf8_is_refused() {
        let tar = built(|tar| raw_entry(tar, b"caf\xe9.txt", EntryType::Regular, b"1"));
        let err = Scratch::new().refuse(Format::Tar, &tar);
        assert!(err.message.contains("not valid UTF-8"), "{}", err.message);
    }

    #[test]
    fn a_cut_short_archive_is_damaged() {
        let packed = compress(Format::TarGz, &tar_of(FILES));
        let err = Scratch::new().refuse(Format::TarGz, &packed[..packed.len() / 2]);
        assert!(
            err.message.starts_with("the archive is damaged"),
            "{}",
            err.message
        );
    }

    #[test]
    fn an_xz_stream_asking_for_too_much_memory_is_refused() {
        // a 512 MiB dictionary, twice what an xz stream may ask for
        let packed = xz_asking_for(&compress(Format::TarXz, &tar_of(FILES)), 34);
        let err = Scratch::new().refuse(Format::TarXz, &packed);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive's xz stream needs more than 256.0 MiB of memory to unpack"
        );
        let within = xz_asking_for(&compress(Format::TarXz, &tar_of(FILES)), 18);
        assert_eq!(paths(&Scratch::new(), Format::TarXz, &within).len(), 3);
    }

    /// a tar of some 300 KiB, and that tar in one xz stream of 64 KiB blocks, as `xz -T` writes.
    fn tar_in_blocks() -> (Vec<u8>, Vec<u8>) {
        let big: Vec<u8> = (0..300_000_u32).map(|at| (at * 7 % 251) as u8).collect();
        let tar = tar_of(&[("data/big.bin", &big), ("data/train.csv", b"x,y\n1,2\n")]);
        let blocked = xz_in_blocks(&tar, 64 << 10);
        (tar, blocked)
    }

    /// only an xz stream alone in its file whose index lists several blocks decodes on threads,
    /// as many as the run grants, the stream has blocks and the memory holds three of its largest
    /// blocks for; streams joined end to end, one padded after, one of one block, a legacy .lzma
    /// file and a split archive keep the one decoder.
    #[test]
    fn only_one_xz_stream_of_several_blocks_alone_in_its_file_decodes_on_threads() {
        let scratch = Scratch::new();
        let (tar, blocked) = tar_in_blocks();
        let (blocks, largest) = xz_blocks(&mut std::io::Cursor::new(&blocked))
            .unwrap()
            .unwrap();
        assert!(blocks >= 4, "{blocks}");
        assert_eq!(largest, 64 << 10);
        let granted = |xz_threads, memory| Limits {
            xz_threads,
            xz_threading: memory,
            ..Limits::default()
        };
        let threads = |name: &str, bytes: &[u8], limits: &Limits| {
            let input = Input::file(&scratch.file(name, bytes)).unwrap();
            xz_threads(&input, Format::TarXz, limits).map(|(threads, _)| threads)
        };
        let plenty = granted(4, XZ_MEMORY);
        assert_eq!(threads("plenty", &blocked, &plenty), Some(4));
        assert_eq!(threads("default", &blocked, &Limits::default()), None);
        let all = u32::try_from(blocks).unwrap();
        assert_eq!(threads("all", &blocked, &granted(64, XZ_MEMORY)), Some(all));
        assert_eq!(
            threads("two", &blocked, &granted(4, 6 * (64 << 10))),
            Some(2)
        );
        assert_eq!(threads("one", &blocked, &granted(4, 5 * (64 << 10))), None);
        let one_block = compress(Format::TarXz, &tar);
        let joined = [blocked.as_slice(), &blocked].concat();
        let padded = [blocked.as_slice(), &[0; 4]].concat();
        let lzma = {
            let options = liblzma::stream::LzmaOptions::new_preset(1).unwrap();
            let stream = liblzma::stream::Stream::new_lzma_encoder(&options).unwrap();
            let mut encoder = liblzma::write::XzEncoder::new_stream(Vec::new(), stream);
            std::io::Write::write_all(&mut encoder, &tar).unwrap();
            encoder.finish().unwrap()
        };
        for (name, bytes) in [
            ("one block", &one_block),
            ("joined", &joined),
            ("padded", &padded),
            ("lzma", &lzma),
        ] {
            assert_eq!(threads(name, bytes, &plenty), None, "{name}");
        }
        let input = Input::file(&scratch.file("gz", &blocked)).unwrap();
        assert_eq!(xz_threads(&input, Format::TarGz, &plenty), None);
        let parts = scratch.parts(&blocked, &[blocked.len() / 2]);
        assert_eq!(xz_threads(&parts, Format::TarXz, &plenty), None);
    }

    /// a tar in an xz stream of several blocks decodes to the same bytes on threads as on one
    /// decoder and unpacks to the same files; a tar in two xz streams joined end to end, the last
    /// of several blocks, keeps the one decoder, which reads past the first.
    #[test]
    fn a_tar_in_xz_blocks_unpacks_alike_on_threads() {
        let (tar, blocked) = tar_in_blocks();
        let mut decoded = Vec::new();
        decompressed(
            Format::TarXz,
            blocked.as_slice(),
            XZ_MEMORY,
            Some(four_threads(&blocked)),
        )
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
        assert_eq!(decoded, tar);
        let half = tar.len() / 2;
        let joined = [
            compress(Format::TarXz, &tar[..half]),
            xz_in_blocks(&tar[half..], 64 << 10),
        ]
        .concat();
        let threaded = Limits {
            xz_threads: 4,
            ..Limits::default()
        };
        for (name, packed) in [("blocked", &blocked), ("joined", &joined)] {
            let alone = Scratch::new().unpack(Format::TarXz, packed).unwrap();
            let scratch = Scratch::new();
            let on_threads = scratch
                .unpack_with(Format::TarXz, packed, &threaded, &|_| false)
                .unwrap();
            assert_eq!(on_threads.members, alone.members, "{name}");
            assert_eq!(on_threads.tree, alone.tree, "{name}");
            assert_eq!(on_threads.members.len(), 2, "{name}");
        }
    }

    /// four threads for the stream `bytes`, which reads again as it is, never stopped.
    fn four_threads(bytes: &[u8]) -> Threads<'_> {
        Threads {
            count: 4,
            memory: XZ_MEMORY,
            again: Box::new(move || Ok(Box::new(bytes) as Box<dyn std::io::BufRead + '_>)),
            stopped: Box::new(|| false),
        }
    }

    /// a threaded decoder that gives its bytes, then fails as liblzma's does when it cannot
    /// start a thread, and marks when it goes.
    struct Refused<'b>(&'b [u8], &'b Cell<bool>);

    impl Read for Refused<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() {
                return Err(liblzma::stream::Error::Mem.into());
            }
            let read = self.0.len().min(buffer.len()).min(1000);
            buffer[..read].copy_from_slice(&self.0[..read]);
            self.0 = &self.0[read..];
            Ok(read)
        }
    }

    impl Drop for Refused<'_> {
        fn drop(&mut self) {
            self.1.set(true);
        }
    }

    /// a hand-over from `refused` to one decoder that reads the stream again through `threads`.
    fn handing<'r>(refused: Refused<'r>, threads: Threads<'r>) -> HandOver<'r> {
        HandOver {
            decoder: Decoder::Threaded(Box::new(refused)),
            threads,
            given: 0,
            limit: XZ_MEMORY,
        }
    }

    /// a stream read again that is interrupted the second time it is asked for bytes, if it
    /// `interrupts`.
    struct Interrupting<'b> {
        bytes: &'b [u8],
        asks: u32,
        interrupts: bool,
    }

    impl Read for Interrupting<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.fill_buf()?.read(buffer)?;
            self.consume(read);
            Ok(read)
        }
    }

    impl BufRead for Interrupting<'_> {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            self.asks += 1;
            if self.interrupts && self.asks == 2 {
                return Err(std::io::ErrorKind::Interrupted.into());
            }
            Ok(self.bytes)
        }

        fn consume(&mut self, taken: usize) {
            self.bytes = &self.bytes[taken..];
        }
    }

    /// a threaded decoder that fails for a thread or memory, after it gave part of the stream,
    /// hands over to one decoder, which reads the stream again and gives the rest, each byte once,
    /// wherever it failed. a stop asked while that decoder catches up is the run's stop, and a
    /// decoder short of memory reads as the system's want, never as damage.
    #[test]
    fn a_refused_threaded_decoder_hands_over_where_it_was() {
        let (tar, blocked) = tar_in_blocks();
        let gone = Cell::new(false);
        // a stop asked from the second look, once one decoder passes over bytes
        let refused_after = |given: usize, stopping: bool| {
            let looks = Cell::new(0);
            let stopped = move || {
                looks.set(looks.get() + 1);
                stopping && looks.get() > 1
            };
            handing(
                Refused(&tar[..given], &gone),
                Threads {
                    stopped: Box::new(stopped),
                    ..four_threads(&blocked)
                },
            )
        };
        for given in [0, 1, 1000, 65_537, tar.len() - 1, tar.len()] {
            let mut read = Vec::new();
            refused_after(given, false).read_to_end(&mut read).unwrap();
            assert!(read == tar, "refused after {given} bytes");
        }
        let mut read = Vec::new();
        let stopped = refused_after(65_537, true)
            .read_to_end(&mut read)
            .unwrap_err();
        assert_eq!(read.len(), 65_537);
        let error = super::super::damaged(Unpacking::Archive, None, &stopped, XZ_MEMORY);
        assert_eq!(error.kind, Kind::Interrupted);
        let short: std::io::Error = liblzma::stream::Error::Mem.into();
        let error = super::super::damaged(Unpacking::Archive, None, &short, XZ_MEMORY);
        assert_eq!(error.kind, Kind::LocalIo);
        assert!(!error.message.contains("damaged"), "{}", error.message);
    }

    /// the threaded decoder that failed goes, with its threads and their buffers, before one
    /// decoder reads the stream again, and the hand-over tries again what is interrupted, the
    /// stream's reopening or a read of it while that decoder passes over what the threads gave,
    /// so a reader that takes an interruption as an error still takes each byte once.
    #[test]
    fn a_failed_threaded_decoder_goes_first_and_interruptions_are_tried_again() {
        let (tar, blocked) = tar_in_blocks();
        let mut failures = Vec::new();
        for (case, reopening_interrupted, reading_interrupted) in [
            ("uninterrupted", false, false),
            ("reopening interrupted", true, false),
            ("passing over interrupted", false, true),
        ] {
            let gone = Cell::new(false);
            let gone_first = Cell::new(true);
            let reopenings = Cell::new(0);
            let again = || {
                gone_first.set(gone_first.get() && gone.get());
                reopenings.set(reopenings.get() + 1);
                if reopening_interrupted && reopenings.get() == 1 {
                    return Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
                }
                let stream = Interrupting {
                    bytes: &blocked,
                    asks: 0,
                    interrupts: reading_interrupted,
                };
                Ok(Box::new(stream) as Box<dyn BufRead + '_>)
            };
            let threads = Threads {
                again: Box::new(again),
                ..four_threads(&blocked)
            };
            let mut handover = handing(Refused(&tar[..200_000], &gone), threads);
            let mut read = Vec::new();
            let mut buffer = [0; 8192];
            let outcome = loop {
                match handover.read(&mut buffer) {
                    Ok(0) => break Ok(()),
                    Ok(given) => read.extend_from_slice(&buffer[..given]),
                    Err(err) => break Err(err),
                }
            };
            match outcome {
                Err(err) => failures.push(format!("{case}: {err}")),
                Ok(()) if read != tar => failures.push(format!("{case}: not the tar's bytes")),
                Ok(()) => {}
            }
            if !gone_first.get() {
                failures.push(format!(
                    "{case}: the threaded decoder was there at the reopening"
                ));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// a run's memory budget, a container's limit included, sets the threads an xz stream of
    /// several blocks decodes on and the memory liblzma may take for them: the archive's quarter
    /// of the budget holds three of the largest block a thread. so nine blocks of 24 MiB, as `xz
    /// -T` writes them at its default level, decode on one decoder in a container of 160 MiB or
    /// with 64 MiB named, and on the four threads the run grants where the budget is 4 GiB.
    #[test]
    fn xz_threads_and_their_memory_follow_the_runs_budget() {
        const MIB: u64 = 1 << 20;
        let limits = |memory: u64, named: Option<&str>| {
            let var = |name: &str| {
                (name == "FETCHLOOM_MEMORY")
                    .then(|| named.map(std::ffi::OsString::from))
                    .flatten()
            };
            let machine = crate::machine::Machine {
                processors: 24,
                memory: Some(memory),
                memory_in_cgroup: true,
                ..crate::machine::Machine::default()
            };
            let settings =
                crate::settings::Settings::read(&var, crate::settings::File::default(), machine);
            let mut limits = settings.unwrap().archive_limits();
            // the share of the threads unpack_all grants each archive
            limits.xz_threads = 4;
            limits
        };
        let plan = |limits: &Limits, blocks| super::xz_plan(limits, blocks, 24 * MIB);
        let large = 64 << 30;
        let contained = limits(160 * MIB, None);
        assert_eq!(plan(&contained, 9), None, "a container of 160 MiB");
        assert_eq!(plan(&limits(large, Some("64M")), 9), None, "64 MiB named");
        let roomy = limits(large, None);
        assert_eq!(plan(&roomy, 9), Some((4, 1 << 30)), "a budget of 4 GiB");
        assert_eq!(plan(&roomy, 3), Some((3, 1 << 30)), "a thread a block");
        let two = limits(large, Some("576M"));
        assert_eq!(plan(&two, 9), Some((2, 144 * MIB)), "room for two");
    }

    /// an xz stream of several blocks whose first block asks for too much memory is refused on
    /// threads as it is on one decoder, leaving nothing in the cache.
    #[test]
    fn xz_blocks_asking_for_too_much_memory_are_refused_on_threads() {
        let (_, blocked) = tar_in_blocks();
        // a 512 MiB dictionary, twice what an xz stream may ask for
        let asking = xz_asking_for(&blocked, 34);
        let threaded = Limits {
            xz_threads: 4,
            ..Limits::default()
        };
        let scratch = Scratch::new();
        let err = scratch
            .unpack_with(Format::TarXz, &asking, &threaded, &|_| false)
            .unwrap_err();
        scratch.assert_empty();
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive's xz stream needs more than 256.0 MiB of memory to unpack"
        );
    }

    /// a tar in xz blocks unpacks to the same files when the system refuses every new thread, as
    /// a refused worker leaves its work to the caller elsewhere: liblzma's threaded decoder fails
    /// a block whose thread will not start.
    #[cfg(target_os = "linux")]
    #[test]
    fn xz_blocks_unpack_when_no_thread_starts() {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        const CHILD: &str = "FETCHLOOM_TEST_NO_NEW_THREADS";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "archive::tar::tests::xz_blocks_unpack_when_no_thread_starts",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return;
        }
        if rustix::process::geteuid().is_root() {
            return crate::test_skips::skip("unprivileged", "root ignores the limit on threads");
        }
        let (_, blocked) = tar_in_blocks();
        let alone = Scratch::new().unpack(Format::TarXz, &blocked).unwrap();
        let threaded = Limits {
            xz_threads: 4,
            ..Limits::default()
        };
        let scratch = Scratch::new();
        let maximum = getrlimit(Resource::Nproc).maximum;
        setrlimit(
            Resource::Nproc,
            Rlimit {
                current: Some(0),
                maximum,
            },
        )
        .unwrap();
        assert!(std::thread::Builder::new().spawn(|| {}).is_err());
        let unpacked = scratch
            .unpack_with(Format::TarXz, &blocked, &threaded, &|_| false)
            .unwrap();
        assert_eq!(unpacked.members, alone.members);
        assert_eq!(unpacked.tree, alone.tree);
    }

    #[test]
    fn a_zstd_frame_asking_for_too_much_memory_is_refused() {
        let packed = zstd_asking_for(31, &tar_of(FILES));
        let err = Scratch::new().refuse(Format::TarZst, &packed);
        assert_eq!(err.kind, Kind::SourceInvalid);
        assert_eq!(
            err.message,
            "the archive's zstd stream needs a window larger than 128.0 MiB to unpack"
        );
        let within = zstd_asking_for(20, &tar_of(FILES));
        assert_eq!(paths(&Scratch::new(), Format::TarZst, &within).len(), 3);
    }
}
