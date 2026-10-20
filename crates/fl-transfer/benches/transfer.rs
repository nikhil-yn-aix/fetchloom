//! Benches for the download path over loopback against the ceiling of hashing alone: one stream
//! with BLAKE3, with MD5 beside it, into a file, and eight streams at once.

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use divan::counter::BytesCount;
use fl_model::digest::{Algorithm, Digest};
use fl_model::partial::PartialRecord;
use fl_transfer::client::{Client, Config};
use fl_transfer::credentials::Chain;
use fl_transfer::download::Download;
use fl_transfer::hash::Hasher;
use fl_transfer::schedule::{Queued, Scheduler};
use fl_transfer::sink::Sink;
use fl_transfer::url::SafeUrl;
use md5::Digest as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::{Builder, Runtime};

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;
const PIECE: usize = 256 * 1024;
const PRE_GENERATED: usize = 64 * 1024 * 1024;
const SEEDS: [u64; 4] = [
    0x9E37_79B9_7F4A_7C15,
    0xD1B5_4A32_D192_ED03,
    0x8CB9_2BA7_2F3D_8DD7,
    0xA076_1D64_78BD_642F,
];

static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static ADDRESS: OnceLock<SocketAddr> = OnceLock::new();
static CLIENT: OnceLock<Client> = OnceLock::new();
static MD5_GIB: OnceLock<Digest> = OnceLock::new();
static MD5_128: OnceLock<Digest> = OnceLock::new();
static CEILING: OnceLock<Vec<u8>> = OnceLock::new();
static FILES: AtomicUsize = AtomicUsize::new(0);

fn main() {
    divan::main();
}

fn step(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

fn fill(state: &mut [u64; 4], buffer: &mut [u8]) {
    for chunk in buffer.as_chunks_mut::<32>().0 {
        for (lane, word) in state.iter_mut().zip(chunk.as_chunks_mut::<8>().0) {
            *word = step(lane).to_le_bytes();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn md5_of_stream(length: u64) -> Digest {
    let mut state = SEEDS;
    let mut buffer = vec![0_u8; PIECE];
    let mut hasher = md5::Md5::new();
    let mut left = length;
    while left > 0 {
        fill(&mut state, &mut buffer);
        let take = usize::try_from(left).map_or(PIECE, |left| left.min(PIECE));
        hasher.update(&buffer[..take]);
        left -= take as u64;
    }
    Digest::from_hex(Algorithm::Md5, &hex(&hasher.finalize())).expect("an md5 digest")
}

fn stated_md5(length: u64) -> Digest {
    let cell = if length == GIB { &MD5_GIB } else { &MD5_128 };
    *cell.get_or_init(|| md5_of_stream(length))
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn runtime() -> &'static Runtime {
    RUNTIME.get_or_init(|| {
        Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("the client runtime")
    })
}

async fn serve(mut tcp: TcpStream) -> io::Result<()> {
    let mut head = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = tcp.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        head.extend_from_slice(&chunk[..read]);
    }
    let text = String::from_utf8_lossy(&head);
    let length: u64 = text
        .split(' ')
        .nth(1)
        .and_then(|target| target.trim_start_matches('/').parse().ok())
        .unwrap_or(0);
    let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
    tcp.write_all(reply.as_bytes()).await?;
    let mut state = SEEDS;
    let mut buffer = vec![0_u8; PIECE];
    let mut left = length;
    while left > 0 {
        fill(&mut state, &mut buffer);
        let take = usize::try_from(left).map_or(PIECE, |left| left.min(PIECE));
        tcp.write_all(&buffer[..take]).await?;
        left -= take as u64;
    }
    tcp.shutdown().await
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn address() -> SocketAddr {
    *ADDRESS.get_or_init(|| {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let Ok(runtime) = Builder::new_multi_thread()
                .worker_threads(4)
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::bind(("127.0.0.1", 0)).await else {
                    return;
                };
                if let Ok(address) = listener.local_addr() {
                    let _ = sender.send(address);
                }
                while let Ok((tcp, _)) = listener.accept().await {
                    tokio::spawn(serve(tcp));
                }
            });
        });
        receiver.recv().expect("the bench server address")
    })
}

#[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
fn client() -> &'static Client {
    CLIENT.get_or_init(|| {
        let config = Config {
            per_host: 8,
            ..Config::default()
        };
        runtime()
            .block_on(async { Client::new(config, Arc::new(Chain::new(Vec::new()))) })
            .expect("the bench client")
    })
}

struct Discard;

impl Sink for Discard {
    fn record(&self) -> Option<PartialRecord> {
        None
    }

    fn allocate(&mut self, _length: u64) -> io::Result<()> {
        Ok(())
    }

    fn write_at(&mut self, _offset: u64, _bytes: &[u8]) -> io::Result<()> {
        Ok(())
    }

    fn read_at(&mut self, _offset: u64, _buffer: &mut [u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn reset(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
        Ok(())
    }
}

struct FileSink {
    file: File,
    path: PathBuf,
}

impl FileSink {
    #[expect(clippy::expect_used, reason = "a bench cannot run without its input")]
    fn create() -> Self {
        let number = FILES.fetch_add(1, Ordering::Relaxed);
        let path =
            PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("transfer-{number}.bin"));
        fs::create_dir_all(env!("CARGO_TARGET_TMPDIR")).expect("the bench directory");
        let file = File::create(&path).expect("the bench file");
        Self { file, path }
    }
}

impl Drop for FileSink {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

impl Sink for FileSink {
    fn record(&self) -> Option<PartialRecord> {
        None
    }

    fn allocate(&mut self, length: u64) -> io::Result<()> {
        self.file.set_len(length)
    }

    fn write_at(&mut self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(bytes)
    }

    fn read_at(&mut self, offset: u64, buffer: &mut [u8]) -> io::Result<usize> {
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read(buffer)
    }

    fn reset(&mut self) -> io::Result<()> {
        self.file.set_len(0)
    }

    fn save(&mut self, _record: &PartialRecord) -> io::Result<()> {
        Ok(())
    }
}

#[expect(
    clippy::expect_used,
    reason = "a bench cannot run when its download fails"
)]
async fn fetch<S: Sink>(length: u64, stated: Vec<Digest>, sink: S) -> S {
    let download = Download {
        url: SafeUrl::parse(&format!("http://{}/{length}", address())).expect("the bench url"),
        length: Some(length),
        stated,
        stable: true,
    };
    let done = client()
        .download(&download, sink)
        .await
        .expect("a finished download");
    assert_eq!(done.bytes, length);
    done.sink
}

fn ceiling() -> &'static [u8] {
    CEILING.get_or_init(|| {
        let mut state = SEEDS;
        let mut buffer = vec![0_u8; PRE_GENERATED];
        fill(&mut state, &mut buffer);
        assert!(!buffer.is_empty(), "the ceiling buffer exists");
        buffer
    })
}

fn hash_gib(stated: &[Digest]) -> Hasher {
    let data = ceiling();
    let mut hasher = Hasher::new(stated);
    for _ in 0..GIB / PRE_GENERATED as u64 {
        for piece in data.chunks(PIECE) {
            hasher.update(piece);
        }
    }
    hasher
}

/// One GiB over loopback, BLAKE3 only, into a sink that discards.
#[divan::bench(sample_count = 5, sample_size = 1)]
fn download_1gib_blake3(bencher: divan::Bencher) {
    client();
    bencher
        .counter(BytesCount::new(GIB))
        .bench_local(|| runtime().block_on(fetch(GIB, Vec::new(), Discard)));
}

/// One GiB over loopback with MD5 stated beside BLAKE3, into a sink that discards.
#[divan::bench(sample_count = 5, sample_size = 1)]
fn download_1gib_blake3_md5(bencher: divan::Bencher) {
    client();
    let stated = stated_md5(GIB);
    bencher
        .counter(BytesCount::new(GIB))
        .bench_local(|| runtime().block_on(fetch(GIB, vec![stated], Discard)));
}

/// One GiB over loopback with MD5 stated, written to a file.
#[divan::bench(sample_count = 5, sample_size = 1)]
fn download_1gib_blake3_md5_file(bencher: divan::Bencher) {
    client();
    let stated = stated_md5(GIB);
    bencher
        .counter(BytesCount::new(GIB))
        .with_inputs(FileSink::create)
        .bench_values(|sink| runtime().block_on(fetch(GIB, vec![stated], sink)));
}

/// Eight 128 MiB downloads at once through the scheduler, MD5 stated, into sinks that discard.
#[divan::bench(sample_count = 5, sample_size = 1)]
fn download_8x128mib_blake3_md5(bencher: divan::Bencher) {
    let stated = stated_md5(128 * MIB);
    let scheduler = Scheduler::new(8, client().hosts());
    bencher.counter(BytesCount::new(GIB)).bench_local(|| {
        let queue: Vec<Queued<Vec<Digest>>> = (0..8)
            .map(|index| Queued {
                group: index.to_string(),
                host: "127.0.0.1".to_owned(),
                job: vec![stated],
            })
            .collect();
        runtime().block_on(scheduler.run(queue, |stated| fetch(128 * MIB, stated, Discard)))
    });
}

/// The ceiling: one GiB hashed in memory with BLAKE3 alone on one thread.
#[divan::bench(sample_count = 5, sample_size = 1)]
fn hash_1gib_blake3(bencher: divan::Bencher) {
    ceiling();
    bencher
        .counter(BytesCount::new(GIB))
        .bench_local(|| hash_gib(&[]).finish());
}

/// The ceiling: one GiB hashed in memory with BLAKE3 and MD5 on one thread.
#[divan::bench(sample_count = 5, sample_size = 1)]
fn hash_1gib_blake3_md5(bencher: divan::Bencher) {
    ceiling();
    let stated = [stated_md5(128 * MIB)];
    bencher
        .counter(BytesCount::new(GIB))
        .bench_local(|| hash_gib(&stated).finish());
}

/// The loopback alone: one GiB read with a plain reqwest client and thrown away.
#[divan::bench(sample_count = 5, sample_size = 1)]
#[expect(clippy::expect_used, reason = "a bench cannot run when its read fails")]
fn read_1gib_loopback(bencher: divan::Bencher) {
    let plain = reqwest::Client::new();
    let url = format!("http://{}/{GIB}", address());
    bencher.counter(BytesCount::new(GIB)).bench_local(|| {
        runtime().block_on(async {
            let mut response = plain.get(&url).send().await.expect("a response");
            let mut read = 0;
            while let Some(chunk) = response.chunk().await.expect("a chunk") {
                read += chunk.len() as u64;
            }
            assert_eq!(read, GIB);
        });
    });
}
