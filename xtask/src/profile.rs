//! Where the time in the hashing pipeline goes, measured rather than reasoned
//! about.

#![expect(
    clippy::cast_precision_loss,
    reason = "a rate printed to the nearest megabyte per second does not need every bit of a byte count"
)]

use std::hint::black_box;
use std::time::{Duration, Instant};

use blake3::Hasher;
use fetchloom_engine::pool::Processor;
use fetchloom_engine::threads::ThreadBudget;
use sha2::{Digest as _, Sha256};

/// One shape the pairing can take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// What the engine does now: enter the pool once per chunk.
    InstallPerChunk,
    /// Enter the pool once for the whole stream.
    InstallOnce,
    /// Never enter the pool.
    Inline,
    /// Enter the pool once and hash the content digest across it.
    InstallOnceParallelContent,
}

impl Shape {
    fn label(self) -> &'static str {
        match self {
            Self::InstallPerChunk => "install-per-chunk",
            Self::InstallOnce => "install-once",
            Self::Inline => "inline",
            Self::InstallOnceParallelContent => "install-once-parallel-content",
        }
    }
}

/// How many bytes one chunk of a stream carries.
const CHUNK: usize = 1 << 20;

/// Runs the profile and prints one line per measurement.
///
/// Takes the workspace root, which is unused, and the arguments after the task
/// name. Returns success unless the pool cannot be built.
pub fn run(arguments: &[String]) -> std::process::ExitCode {
    let rounds: u32 = crate::argument_value(arguments, "--rounds")
        .and_then(|value| value.parse().ok())
        .unwrap_or(7);
    let budget = ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    );
    let Ok(processor) = Processor::new(budget) else {
        eprintln!("the processor pool could not be built");
        return std::process::ExitCode::from(1);
    };
    println!("threads {}", budget.threads());

    for (name, length, streams) in [
        ("1KiB", 1024usize, 4096u32),
        ("128KiB", 128 * 1024, 256),
        ("256KiB", 256 * 1024, 128),
        ("512KiB", 512 * 1024, 64),
        ("1MiB", 1 << 20, 64),
        ("4MiB", 4 << 20, 16),
        ("16MiB", 16 << 20, 4),
    ] {
        let bytes = vec![7u8; length];
        for shape in [
            Shape::InstallPerChunk,
            Shape::InstallOnce,
            Shape::Inline,
            Shape::InstallOnceParallelContent,
        ] {
            let mut best = Duration::MAX;
            for _ in 0..rounds {
                let started = Instant::now();
                for _ in 0..streams {
                    black_box(hash(shape, &processor, &bytes));
                }
                best = best.min(started.elapsed());
            }
            let moved = u64::from(streams) * length as u64;
            let rate = moved as f64 / best.as_secs_f64() / 1e6;
            println!(
                "{name} {} {:.3} ms {rate:.0} MB/s",
                shape.label(),
                best.as_secs_f64() * 1e3
            );
        }
    }

    println!("--- the buffer a stream reads through");
    for (name, length) in [("fresh-per-stream", CHUNK), ("sized-to-input", 1024)] {
        let mut best = Duration::MAX;
        for _ in 0..rounds {
            let started = Instant::now();
            for _ in 0..1024 {
                let buffer = vec![0u8; length];
                black_box(buffer.len());
            }
            best = best.min(started.elapsed());
        }
        println!("buffer {name} {:.3} ms", best.as_secs_f64() * 1e3);
    }
    std::process::ExitCode::SUCCESS
}

/// Hashes one stream's worth of bytes in the given shape.
fn hash(shape: Shape, processor: &Processor, bytes: &[u8]) -> [u8; 32] {
    let mut content = Hasher::new();
    let mut interop = Sha256::new();
    match shape {
        Shape::InstallPerChunk => {
            for chunk in bytes.chunks(CHUNK) {
                processor.install(|| {
                    rayon::join(|| content.update(chunk), || interop.update(chunk));
                });
            }
        }
        Shape::InstallOnce => processor.install(|| {
            for chunk in bytes.chunks(CHUNK) {
                rayon::join(|| content.update(chunk), || interop.update(chunk));
            }
        }),
        Shape::Inline => {
            for chunk in bytes.chunks(CHUNK) {
                content.update(chunk);
                interop.update(chunk);
            }
        }
        Shape::InstallOnceParallelContent => processor.install(|| {
            for chunk in bytes.chunks(CHUNK) {
                rayon::join(|| content.update_rayon(chunk), || interop.update(chunk));
            }
        }),
    }
    let _ = interop.finalize();
    *content.finalize().as_bytes()
}
