#![cfg_attr(
    windows,
    expect(
        unsafe_code,
        reason = "the peak resident set of a finished process is readable only through a Win32 call"
    )
)]

//! The regime harness: run the real binary, record numbers, gate on them.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use fetchloom_engine::work::Work;
use fetchloom_faults::{Latency, Reply, Script, TestServer};
use serde::{Deserialize, Serialize};

pub const REGRESSION_GATE: f64 = 0.05;

/// How far a bounded metric may rise above its own target.s baseline. Peak
/// memory holds within 2.8 percent across runs on one runner and differs 44
/// percent between targets, and a fall is never a regression, so the band is
/// wider than a counter.s and one sided.
pub const BOUND_GATE: f64 = 0.10;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    Deterministic,
    Bounded,
    Timing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub kind: MetricKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alternative {
    pub tool: String,
    pub does: String,
    pub wall_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeResult {
    pub regime: String,
    pub iterations: u32,
    pub metrics: Vec<Metric>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative: Option<Alternative>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub target: String,
    pub regimes: Vec<RegimeResult>,
}

#[derive(Debug)]
pub enum BenchError {
    Process(io::Error),
    MissingBinary(PathBuf),
    NonZeroExit(i32),
    Baseline(io::Error),
    MalformedBaseline(serde_json::Error),
    RegimeMissing(String),
    MetricAdded {
        regime: String,
        metric: String,
    },
    MalformedResult(serde_json::Error),
    ControllerInert(String),
    NotIdle(String),
    NoPeak,
    SlowDisk(String),
    Moved {
        regime: String,
        metric: String,
        baseline: f64,
        current: f64,
    },
    MetricMissing {
        regime: String,
        metric: String,
    },
    /// Every metric that moved, rather than the first one found. A gate that
    /// names one failure when there are three is a gate that gets read as one
    /// failure.
    Diverged(Vec<BenchError>),
}

impl std::fmt::Display for BenchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Process(error) => write!(f, "could not run the binary: {error}"),
            Self::MissingBinary(path) => write!(f, "no binary at {}", path.display()),
            Self::NonZeroExit(code) => write!(f, "the binary exited {code}"),
            Self::Baseline(error) => write!(f, "could not read or write the baseline: {error}"),
            Self::MalformedBaseline(error) => write!(f, "the baseline is malformed: {error}"),
            Self::RegimeMissing(regime) => write!(f, "regime {regime} is not in both runs"),
            Self::MetricAdded { regime, metric } => write!(
                f,
                "{regime} carries {metric}, which the baseline does not, so record a baseline before gating on it"
            ),
            Self::ControllerInert(reason) => {
                write!(f, "the many-hosts regime decided nothing: {reason}")
            }
            Self::SlowDisk(reason) => write!(f, "the slow-disk regime could not run: {reason}"),
            Self::NoPeak => write!(
                f,
                "the operating system did not report a run's peak resident set, so this run measures nothing about memory"
            ),
            Self::NotIdle(status) => write!(
                f,
                "the no-op regime reported {status} rather than unchanged, so it measured a run that did something"
            ),
            Self::MalformedResult(error) => {
                write!(f, "the run's result could not be read: {error}")
            }
            Self::Moved {
                regime,
                metric,
                baseline,
                current,
            } => write!(f, "{regime} {metric} moved from {baseline} to {current}"),
            Self::MetricMissing { regime, metric } => write!(
                f,
                "{regime} stopped producing {metric}, which the baseline carries"
            ),
            Self::Diverged(found) => {
                writeln!(f, "{} deterministic metrics moved:", found.len())?;
                for one in found {
                    writeln!(f, "  {one}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for BenchError {}

pub fn build_binary(workspace: &Path) -> Result<PathBuf, BenchError> {
    let status = Command::new(cargo())
        .current_dir(workspace)
        .args(["build", "--release", "--bin", "fetchloom"])
        .status()
        .map_err(BenchError::Process)?;
    if !status.success() {
        return Err(BenchError::NonZeroExit(status.code().unwrap_or(-1)));
    }
    let binary = workspace
        .join("target")
        .join("release")
        .join(binary_name("fetchloom"));
    if binary.exists() {
        Ok(binary)
    } else {
        Err(BenchError::MissingBinary(binary))
    }
}

pub fn run_no_op(binary: &Path, iterations: u32) -> Result<RegimeResult, BenchError> {
    let scratch = scratch_directory(NO_OP_ROUND)?;
    let source = scratch.join("idle.bin");
    let destination = scratch.join("out");
    let cache = scratch.join("cache");
    fs::write(&source, non_repeating_bytes(0, NO_OP_BYTES)).map_err(BenchError::Process)?;
    measure_get(binary, &source, &destination, &cache)?;

    let mut samples = Vec::with_capacity(iterations as usize);
    let mut work = Work::default();
    let mut peak = 0;
    for round in 0..=iterations {
        let run = locked_get(binary, &source, &destination, &cache)?;
        if run.outcome.status != "unchanged" {
            return Err(BenchError::NotIdle(run.outcome.status));
        }
        if round > 0 {
            samples.push(run.wall_ms);
            work = run.outcome.work;
            peak = peak.max(run.peak_bytes);
        }
    }
    samples.sort_by(f64::total_cmp);
    let median = samples[samples.len() / 2];
    let size = fs::metadata(binary).map_err(BenchError::Process)?.len();

    Ok(RegimeResult {
        regime: "no-op".to_owned(),
        iterations,
        metrics: vec![
            Metric {
                name: "startup".to_owned(),
                value: median,
                unit: "ms".to_owned(),
                kind: MetricKind::Timing,
            },
            Metric {
                name: "binary-size".to_owned(),
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a binary size below two to the fifty-third bytes is exact"
                )]
                value: size as f64,
                unit: "bytes".to_owned(),
                kind: MetricKind::Deterministic,
            },
            peak_metric(peak),
        ]
        .into_iter()
        .chain(work_metrics(&work))
        .collect(),
        alternative: None,
    })
}

const NO_OP_ROUND: u32 = 900;

const NO_OP_BYTES: usize = 1024;

fn locked_get(
    binary: &Path,
    source: &Path,
    destination: &Path,
    cache: &Path,
) -> Result<Measured, BenchError> {
    let mut command = Command::new(binary);
    command
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(cache.join("fetchloom.lock"))
        .arg("--deterministic-io")
        .arg("--locked")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stderr(std::process::Stdio::null());
    measure(command)
}

pub fn save(baseline: &Baseline, path: &Path) -> Result<(), BenchError> {
    let text = serde_json::to_string_pretty(baseline).map_err(BenchError::MalformedBaseline)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(BenchError::Baseline)?;
    }
    fs::write(path, text + "\n").map_err(BenchError::Baseline)
}

pub fn load(path: &Path) -> Result<Baseline, BenchError> {
    let text = fs::read_to_string(path).map_err(BenchError::Baseline)?;
    serde_json::from_str(&text).map_err(BenchError::MalformedBaseline)
}

pub fn compare(baseline: &Baseline, current: &Baseline) -> Result<(), BenchError> {
    let mut found = Vec::new();
    for regime in &current.regimes {
        let Some(recorded) = baseline
            .regimes
            .iter()
            .find(|candidate| candidate.regime == regime.regime)
        else {
            found.push(BenchError::RegimeMissing(regime.regime.clone()));
            continue;
        };
        for metric in regime
            .metrics
            .iter()
            .filter(|metric| metric.kind != MetricKind::Timing)
        {
            let Some(previous) = recorded
                .metrics
                .iter()
                .find(|candidate| candidate.name == metric.name)
            else {
                found.push(BenchError::MetricAdded {
                    regime: regime.regime.clone(),
                    metric: metric.name.clone(),
                });
                continue;
            };
            let moved = match metric.kind {
                MetricKind::Bounded => {
                    metric.value - previous.value > previous.value.abs() * BOUND_GATE
                }
                _ => (metric.value - previous.value).abs() > previous.value.abs() * REGRESSION_GATE,
            };
            if moved {
                found.push(BenchError::Moved {
                    regime: regime.regime.clone(),
                    metric: metric.name.clone(),
                    baseline: previous.value,
                    current: metric.value,
                });
            }
        }
        for previous in recorded
            .metrics
            .iter()
            .filter(|metric| metric.kind != MetricKind::Timing)
        {
            if !regime
                .metrics
                .iter()
                .any(|candidate| candidate.name == previous.name)
            {
                found.push(BenchError::MetricMissing {
                    regime: regime.regime.clone(),
                    metric: previous.name.clone(),
                });
            }
        }
    }
    if found.is_empty() {
        return Ok(());
    }
    Err(BenchError::Diverged(found))
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn binary_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

pub fn scanner_lane(directory: &Path) -> Result<String, Box<fetchloom_engine::error::Error>> {
    use fetchloom_engine::capability::Scanner;
    use fetchloom_engine::seam::platform::Platform;

    fs::create_dir_all(directory).map_err(|reason| {
        Box::new(fetchloom_engine::error::Error::new(
            fetchloom_engine::error::ErrorKind::ResourceDisk,
            format!("{}: {reason}", directory.display()),
        ))
    })?;
    let platform = fetchloom_platform::NativePlatform::new(std::sync::Arc::new(
        fetchloom_engine::work::WorkCounter::new(),
    ));
    let capabilities = platform.volume_capabilities(directory).map_err(Box::new)?;
    Ok(match capabilities.scanner {
        Scanner::Present { name, cost_ratio } => format!(
            "measured with an on-access scanner enabled: {name}, small writes cost {cost_ratio:.2} times one large write"
        ),
        Scanner::Absent => {
            "measured with no on-access scanner inspecting writes on this volume".to_owned()
        }
        Scanner::Unknown { cost_ratio } => format!(
            "measured on a volume this platform cannot enumerate, where small writes cost {cost_ratio:.2} times one large write, so whether a scanner is present is unknown"
        ),
    })
}

const SMALL_FILES: usize = 1024;

const SMALL_FILE_BYTES: usize = 1024;

const LARGE_FILE_BYTES: usize = 256 * 1024 * 1024;

pub fn run_shapes(binary: &Path, iterations: u32) -> Result<Vec<RegimeResult>, BenchError> {
    let mut small_times = Vec::with_capacity(iterations as usize);
    let mut large_times = Vec::with_capacity(iterations as usize);
    let mut small_work = Work::default();
    let mut large_work = Work::default();
    let mut small_copies = Vec::with_capacity(iterations as usize);
    let mut large_copies = Vec::with_capacity(iterations as usize);
    let mut small_peak = 0;
    let mut large_peak = 0;

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;

        let many = scratch.join("many");
        fs::create_dir_all(&many).map_err(BenchError::Process)?;
        for index in 0..SMALL_FILES {
            let bytes = non_repeating_bytes(index, SMALL_FILE_BYTES);
            fs::write(many.join(format!("file-{index}.bin")), bytes)
                .map_err(BenchError::Process)?;
        }
        let run = measure_get(
            binary,
            &many,
            &scratch.join("many-out"),
            &scratch.join("cache-many"),
        )?;
        small_times.push(run.wall_ms);
        small_work = run.outcome.work;
        small_peak = small_peak.max(run.peak_bytes);
        small_copies.push(copy_tree(&many, &scratch.join("many-copied"))?);

        let one = scratch.join("one");
        fs::create_dir_all(&one).map_err(BenchError::Process)?;
        let bytes = non_repeating_bytes(3, LARGE_FILE_BYTES);
        fs::write(one.join("large.bin"), bytes).map_err(BenchError::Process)?;
        let run = measure_get(
            binary,
            &one,
            &scratch.join("one-out"),
            &scratch.join("cache-one"),
        )?;
        large_times.push(run.wall_ms);
        large_work = run.outcome.work;
        large_peak = large_peak.max(run.peak_bytes);
        large_copies.push(copy_tree(&one, &scratch.join("one-copied"))?);

        let _ = fs::remove_dir_all(&scratch);
    }

    small_times.sort_by(f64::total_cmp);
    large_times.sort_by(f64::total_cmp);

    Ok(vec![
        RegimeResult {
            regime: "many-small-files".to_owned(),
            iterations,
            metrics: vec![Metric {
                name: "wall".to_owned(),
                value: small_times[small_times.len() / 2],
                unit: "ms".to_owned(),
                kind: MetricKind::Timing,
            }]
            .into_iter()
            .chain(work_metrics(&small_work))
            .chain(std::iter::once(peak_metric(small_peak)))
            .collect(),
            alternative: Some(Alternative {
                tool: COPY_TOOL.to_owned(),
                does: "copies the same 1024 files, hashing nothing and verifying nothing"
                    .to_owned(),
                wall_ms: median(small_copies),
            }),
        },
        RegimeResult {
            regime: "one-large-file".to_owned(),
            iterations,
            metrics: vec![Metric {
                name: "wall".to_owned(),
                value: large_times[large_times.len() / 2],
                unit: "ms".to_owned(),
                kind: MetricKind::Timing,
            }]
            .into_iter()
            .chain(work_metrics(&large_work))
            .chain(std::iter::once(peak_metric(large_peak)))
            .collect(),
            alternative: Some(Alternative {
                tool: COPY_TOOL.to_owned(),
                does: "copies the same 256 MiB file, hashing nothing and verifying nothing"
                    .to_owned(),
                wall_ms: median(large_copies),
            }),
        },
    ])
}

const CORPUS_FILES: usize = 64;

const CORPUS_FILE_BYTES: usize = 256 * 1024;

pub fn run_cache(binary: &Path, iterations: u32) -> Result<Vec<RegimeResult>, BenchError> {
    let mut cold_times = Vec::with_capacity(iterations as usize);
    let mut warm_times = Vec::with_capacity(iterations as usize);
    let mut cold_growth = 0u64;
    let mut warm_growth = 0u64;
    let mut cold_work = Work::default();
    let mut warm_work = Work::default();
    let mut cold_peak = 0;
    let mut warm_peak = 0;
    let mut copies = Vec::with_capacity(iterations as usize);

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;
        let source = scratch.join("source");
        let cache = scratch.join("cache");
        write_corpus(&source)?;

        let cold = measure_get(binary, &source, &scratch.join("cold"), &cache)?;
        let after_cold = stored_bytes(&cache);
        let warm = measure_get(binary, &source, &scratch.join("warm"), &cache)?;
        let after_warm = stored_bytes(&cache);

        cold_times.push(cold.wall_ms);
        warm_times.push(warm.wall_ms);
        cold_growth = after_cold;
        warm_growth = after_warm - after_cold;
        cold_work = cold.outcome.work;
        warm_work = warm.outcome.work;
        cold_peak = cold_peak.max(cold.peak_bytes);
        warm_peak = warm_peak.max(warm.peak_bytes);
        copies.push(copy_tree(&source, &scratch.join("copied"))?);
        let _ = fs::remove_dir_all(&scratch);
    }

    warm_times.sort_by(f64::total_cmp);

    Ok(vec![
        RegimeResult {
            regime: "cold-cache".to_owned(),
            iterations,
            metrics: vec![
                Metric {
                    name: "wall".to_owned(),
                    value: cold_times[cold_times.len() / 2],
                    unit: "ms".to_owned(),
                    kind: MetricKind::Timing,
                },
                Metric {
                    name: "cache-growth".to_owned(),
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "a corpus below two to the fifty-third bytes is exact"
                    )]
                    value: cold_growth as f64,
                    unit: "bytes".to_owned(),
                    kind: MetricKind::Deterministic,
                },
            ]
            .into_iter()
            .chain(work_metrics(&cold_work))
            .chain(std::iter::once(peak_metric(cold_peak)))
            .collect(),
            alternative: Some(Alternative {
                tool: COPY_TOOL.to_owned(),
                does: "copies the same 64 files, hashing nothing and verifying nothing".to_owned(),
                wall_ms: median(copies.clone()),
            }),
        },
        RegimeResult {
            regime: "warm-cache".to_owned(),
            iterations,
            metrics: vec![
                Metric {
                    name: "wall".to_owned(),
                    value: warm_times[warm_times.len() / 2],
                    unit: "ms".to_owned(),
                    kind: MetricKind::Timing,
                },
                Metric {
                    name: "cache-growth".to_owned(),
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "a corpus below two to the fifty-third bytes is exact"
                    )]
                    value: warm_growth as f64,
                    unit: "bytes".to_owned(),
                    kind: MetricKind::Deterministic,
                },
            ]
            .into_iter()
            .chain(work_metrics(&warm_work))
            .chain(std::iter::once(peak_metric(warm_peak)))
            .collect(),
            alternative: Some(Alternative {
                tool: COPY_TOOL.to_owned(),
                does: "copies the same 64 files again, which is what a tool with no cache must do"
                    .to_owned(),
                wall_ms: median(copies),
            }),
        },
    ])
}

#[must_use]
pub fn warm_beat_cold(regimes: &[RegimeResult]) -> Option<(f64, f64)> {
    let wall = |name: &str| {
        regimes
            .iter()
            .find(|regime| regime.regime == name)?
            .metrics
            .iter()
            .find(|metric| metric.name == "wall")
            .map(|metric| metric.value)
    };
    Some((wall("cold-cache")?, wall("warm-cache")?))
}

fn scratch_directory(round: u32) -> Result<PathBuf, BenchError> {
    let path = std::env::temp_dir().join(format!("fetchloom-bench-{}-{round}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).map_err(BenchError::Process)?;
    Ok(path)
}

fn write_corpus(into: &Path) -> Result<(), BenchError> {
    fs::create_dir_all(into).map_err(BenchError::Process)?;
    for index in 0..CORPUS_FILES {
        let bytes = non_repeating_bytes(index, CORPUS_FILE_BYTES);
        fs::write(into.join(format!("file-{index}.bin")), bytes).map_err(BenchError::Process)?;
    }
    Ok(())
}

/// A corpus no compressor shrinks, because a regime named for one enormous
/// file exists to measure what a run does with bytes rather than what zstd
/// does with a pattern. The shape before this one repeated every 251 bytes, so
/// 256 MiB of it stored as 109,034 and both large regimes measured the
/// compressed publication path with its extra full read.
fn non_repeating_bytes(index: usize, length: usize) -> Vec<u8> {
    let mut state = u64::try_from(index)
        .unwrap_or(0)
        .wrapping_mul(0x9e37_79b9_7f4a_7c15)
        | 1;
    let mut bytes = Vec::with_capacity(length + 8);
    while bytes.len() < length {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        bytes.extend_from_slice(&state.wrapping_mul(0x2545_f491_4f6c_dd1d).to_le_bytes());
    }
    bytes.truncate(length);
    for (at, byte) in index.to_le_bytes().iter().enumerate() {
        if let Some(slot) = bytes.get_mut(at) {
            *slot = *byte;
        }
    }
    bytes
}

const TRANSFER_OBJECT_BYTES: usize = 4 * 1024 * 1024;

const TRANSFER_INTERRUPTIONS: [usize; 2] = [25, 50];

pub fn run_transfer(binary: &Path, iterations: u32) -> Result<Vec<RegimeResult>, BenchError> {
    let object = non_repeating_bytes(1, TRANSFER_OBJECT_BYTES);
    let mut cold_times = Vec::with_capacity(iterations as usize);
    let mut cold_bytes = 0u64;
    let mut cold_work = Work::default();
    let mut cold_peak = 0;
    let mut interrupted_times = Vec::with_capacity(iterations as usize);
    let mut interrupted_bytes = 0u64;
    let mut interrupted_work = Work::default();
    let mut interrupted_peak = 0;
    let mut cold_curls = Vec::with_capacity(iterations as usize);
    let mut interrupted_curls = Vec::with_capacity(iterations as usize);

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;

        let cold_server =
            TestServer::start(Script::serving(object.clone())).map_err(BenchError::Process)?;
        let run = measure_transfer(
            binary,
            &cold_server,
            &scratch.join("cold"),
            &scratch.join("cache-cold"),
        )?;
        cold_curls.push(curl_to(
            &format!("{}/object", cold_server.origin()),
            &scratch.join("cold.curl"),
        )?);
        drop(cold_server);
        cold_times.push(run.wall_ms);
        cold_bytes = run.outcome.bytes;
        cold_work = run.outcome.work;
        cold_peak = cold_peak.max(run.peak_bytes);

        let replies = TRANSFER_INTERRUPTIONS
            .into_iter()
            .map(|percent| Reply::ClosedMidBody {
                after: object.len() * percent / 100,
            })
            .collect();
        let interrupted_server = TestServer::start(
            Script::serving(object.clone())
                .tagged(vec!["\"stable\"".to_owned()])
                .replying(replies),
        )
        .map_err(BenchError::Process)?;
        let run = measure_transfer(
            binary,
            &interrupted_server,
            &scratch.join("interrupted"),
            &scratch.join("cache-interrupted"),
        )?;
        interrupted_curls.push(curl_to(
            &format!("{}/object", interrupted_server.origin()),
            &scratch.join("interrupted.curl"),
        )?);
        drop(interrupted_server);
        interrupted_times.push(run.wall_ms);
        interrupted_bytes = run.outcome.bytes;
        interrupted_work = run.outcome.work;
        interrupted_peak = interrupted_peak.max(run.peak_bytes);

        let _ = fs::remove_dir_all(&scratch);
    }

    Ok(vec![
        transfer_regime(
            "cold-transfer",
            iterations,
            cold_times,
            cold_bytes,
            &cold_work,
            cold_peak,
            Alternative {
                tool: "curl --output".to_owned(),
                does: "downloads the same object from the same server, hashing nothing, verifying nothing and publishing nothing".to_owned(),
                wall_ms: median(cold_curls),
            },
        ),
        transfer_regime(
            "interrupted-transfer",
            iterations,
            interrupted_times,
            interrupted_bytes,
            &interrupted_work,
            interrupted_peak,
            Alternative {
                tool: "curl --output".to_owned(),
                does: "downloads the same object from the same server after its interruptions are spent, so it never resumes and never pays for one".to_owned(),
                wall_ms: median(interrupted_curls),
            },
        ),
    ])
}

fn transfer_regime(
    regime: &str,
    iterations: u32,
    times: Vec<f64>,
    bytes: u64,
    work: &Work,
    peak: u64,
    alternative: Alternative,
) -> RegimeResult {
    #[expect(
        clippy::cast_precision_loss,
        reason = "an object below two to the fifty-third bytes is exact"
    )]
    let materialized = bytes as f64;
    RegimeResult {
        regime: regime.to_owned(),
        iterations,
        metrics: vec![
            Metric {
                name: "wall".to_owned(),
                value: median(times),
                unit: "ms".to_owned(),
                kind: MetricKind::Timing,
            },
            Metric {
                name: "bytes-materialized".to_owned(),
                value: materialized,
                unit: "bytes".to_owned(),
                kind: MetricKind::Deterministic,
            },
        ]
        .into_iter()
        .chain(work_metrics(work))
        .chain(std::iter::once(peak_metric(peak)))
        .collect(),
        alternative: Some(alternative),
    }
}

#[derive(Deserialize)]
struct RunOutcome {
    bytes: u64,
    status: String,
    work: Work,
}

fn work_metrics(work: &Work) -> Vec<Metric> {
    #[expect(
        clippy::cast_precision_loss,
        reason = "a count below two to the fifty-third is exact"
    )]
    let count = |value: u64| value as f64;
    vec![
        Metric {
            name: "bytes-read".to_owned(),
            value: count(work.bytes_read),
            unit: "bytes".to_owned(),
            kind: MetricKind::Deterministic,
        },
        Metric {
            name: "bytes-written".to_owned(),
            value: count(work.bytes_written),
            unit: "bytes".to_owned(),
            kind: MetricKind::Deterministic,
        },
        Metric {
            name: "requests".to_owned(),
            value: count(work.requests),
            unit: "requests".to_owned(),
            kind: MetricKind::Deterministic,
        },
        Metric {
            name: "file-operations".to_owned(),
            value: count(work.file_operations),
            unit: "operations".to_owned(),
            kind: MetricKind::Deterministic,
        },
    ]
}

fn measure_transfer(
    binary: &Path,
    server: &TestServer,
    destination: &Path,
    cache: &Path,
) -> Result<Measured, BenchError> {
    let url = format!("{}/object", server.origin());
    let mut command = Command::new(binary);
    command
        .arg("get")
        .arg(&url)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(cache.join("fetchloom.lock"))
        .arg("--deterministic-io")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache);
    measure(command)
}

fn measure_get(
    binary: &Path,
    source: &Path,
    destination: &Path,
    cache: &Path,
) -> Result<Measured, BenchError> {
    let mut command = Command::new(binary);
    command
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(cache.join("fetchloom.lock"))
        .arg("--deterministic-io")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stderr(std::process::Stdio::null());
    measure(command)
}

fn directory_bytes(directory: &Path) -> u64 {
    fs::read_dir(directory).map_or(0, |entries| {
        entries
            .flatten()
            .filter_map(|entry| entry.metadata().ok())
            .map(|found| found.len())
            .sum()
    })
}

fn stored_bytes(cache: &Path) -> u64 {
    directory_bytes(&cache.join("objects")) + directory_bytes(&cache.join("packs"))
}

fn time_command(program: &str, arguments: &[&std::ffi::OsStr]) -> Result<f64, BenchError> {
    let started = Instant::now();
    let output = Command::new(program)
        .args(arguments)
        .output()
        .map_err(BenchError::Process)?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        return Err(BenchError::NonZeroExit(output.status.code().unwrap_or(-1)));
    }
    Ok(elapsed.as_secs_f64() * 1000.0)
}

fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

fn copy_tree(from: &Path, to: &Path) -> Result<f64, BenchError> {
    #[cfg(windows)]
    {
        let spec = format!(
            "Copy-Item -Recurse -LiteralPath '{}' -Destination '{}'",
            from.display(),
            to.display()
        );
        time_command(
            "powershell",
            &[
                std::ffi::OsStr::new("-NoProfile"),
                std::ffi::OsStr::new("-NonInteractive"),
                std::ffi::OsStr::new("-Command"),
                std::ffi::OsStr::new(&spec),
            ],
        )
    }
    #[cfg(not(windows))]
    {
        time_command(
            "cp",
            &[std::ffi::OsStr::new("-r"), from.as_os_str(), to.as_os_str()],
        )
    }
}

fn curl_to(url: &str, into: &Path) -> Result<f64, BenchError> {
    time_command(
        "curl",
        &[
            std::ffi::OsStr::new("--silent"),
            std::ffi::OsStr::new("--show-error"),
            std::ffi::OsStr::new("--output"),
            into.as_os_str(),
            std::ffi::OsStr::new(url),
        ],
    )
}

#[cfg(windows)]
pub const COPY_TOOL: &str = "powershell Copy-Item -Recurse";

#[cfg(not(windows))]
pub const COPY_TOOL: &str = "cp -r";

#[must_use]
pub fn publish(baseline: &Baseline, lane: &str) -> String {
    let mut page = String::new();
    page.push_str("# Benchmarks\n\n");
    let _ = write!(
        page,
        "Every number here was measured by `cargo xtask bench --publish` on one machine, target `{}`, with the comparison tool run in the same round against the same input.\n\n",
        baseline.target
    );
    page.push_str("Wall time is a property of the machine as much as of the code. It is published and never gated; this machine has measured the same binary at 773, 2364 and 4165 ms on one regime. The deterministic counters are the ones that gate, at five percent.\n\n");
    let _ = write!(page, "Many-small-files lane: {lane}\n\n");
    page.push_str("| Regime | Fetchloom | Alternative | Ratio | What the alternative does |\n");
    page.push_str("|---|---|---|---|---|\n");
    for regime in &baseline.regimes {
        let wall = regime
            .metrics
            .iter()
            .find(|metric| metric.name == "wall" || metric.name == "startup")
            .map(|metric| metric.value);
        let (tool, does, ratio) = match (&regime.alternative, wall) {
            (Some(alternative), Some(wall)) => (
                format!("{} {:.0} ms", alternative.tool, alternative.wall_ms),
                alternative.does.clone(),
                format!("{:.2}x", wall / alternative.wall_ms),
            ),
            _ => (
                "none".to_owned(),
                "no tool does this, so there is nothing to compare against".to_owned(),
                String::new(),
            ),
        };
        let _ = writeln!(
            page,
            "| {} | {} | {tool} | {ratio} | {does} |",
            regime.regime,
            wall.map_or_else(|| "unmeasured".to_owned(), |value| format!("{value:.0} ms")),
        );
    }
    page.push_str("\nA ratio above one is a regime where Fetchloom is slower than the tool beside it. Those rows are the honest ones: Fetchloom hashes every byte twice, writes an outboard tree, publishes through staging and records what it did, and none of the tools it is measured against do any of that. The comparison is published so the cost is visible, not because the tools are doing the same job.\n\n");
    if let Some(cause) = hosts_cause(baseline) {
        let _ = write!(page, "{cause}\n\n");
    }
    page.push_str("## Deterministic counters\n\n");
    page.push_str("| Regime | bytes read | bytes written | requests | file operations |\n");
    page.push_str("|---|---|---|---|---|\n");
    for regime in &baseline.regimes {
        let of = |name: &str| {
            regime
                .metrics
                .iter()
                .find(|metric| metric.name == name)
                .map_or_else(|| "-".to_owned(), |metric| format!("{:.0}", metric.value))
        };
        let _ = writeln!(
            page,
            "| {} | {} | {} | {} | {} |",
            regime.regime,
            of("bytes-read"),
            of("bytes-written"),
            of("requests"),
            of("file-operations")
        );
    }
    page
}

fn hosts_cause(baseline: &Baseline) -> Option<String> {
    let regime = baseline
        .regimes
        .iter()
        .find(|regime| regime.regime == "many-hosts")?;
    let value = |name: &str| {
        regime
            .metrics
            .iter()
            .find(|metric| metric.name == name)
            .map(|metric| metric.value)
    };
    let wall = value("wall")?;
    let requests = value("requests")?;
    let injected = requests * HOST_LATENCY.as_secs_f64() * 1000.0;
    Some(format!(
        "many-hosts carries the largest ratio on this page and most of it is not transfer cost. \
         The regime injects {} ms of latency into every request so that a per-host ceiling has \
         something to hide, and it issues {requests:.0} of them. Those requests are not serial: \
         the run holds several in flight per host, so the injected latency costs a fraction of the \
         {injected:.0} ms it would cost if they were. Most of the {wall:.0} ms measured is the \
         backoff the rate limited host asks for, which this run waits out one request at a time \
         because a host asking to be left alone drives its count back to one. The alternative pays \
         the same injected latency and waits out none of the backoff.",
        HOST_LATENCY.as_millis()
    ))
}

const OBJECTS_PER_HOST: usize = 8;

const HOST_OBJECT_BYTES: usize = 128 * 1024;

const HOST_LATENCY: Duration = Duration::from_millis(100);

#[derive(Debug, Deserialize)]
struct RecordedHost {
    host: String,
    measurement: fetchloom_engine::tuning::HostMeasurement,
}

fn recorded_hosts(cache: &Path) -> Vec<RecordedHost> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(cache.join("meta").join("host")) else {
        return found;
    };
    for entry in entries.flatten() {
        if let Ok(text) = fs::read_to_string(entry.path())
            && let Ok(record) = serde_json::from_str::<RecordedHost>(&text)
        {
            found.push(record);
        }
    }
    found.sort_by(|left, right| left.host.cmp(&right.host));
    found
}

pub fn run_hosts(binary: &Path, iterations: u32) -> Result<Vec<RegimeResult>, BenchError> {
    let mut measured = hosts_regime(binary, iterations, "many-hosts-concurrency", false)?;
    measured.extend(hosts_regime(
        binary,
        iterations,
        "many-hosts-backoff",
        true,
    )?);
    Ok(measured)
}

fn hosts_regime(
    binary: &Path,
    iterations: u32,
    regime: &str,
    rate_limited: bool,
) -> Result<Vec<RegimeResult>, BenchError> {
    let mut times = Vec::with_capacity(iterations as usize);
    let mut work = Work::default();
    let mut peak = 0;
    let mut curls = Vec::with_capacity(iterations as usize);

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;
        let cache = scratch.join("cache");

        let mut servers = Vec::with_capacity(OBJECTS_PER_HOST * 2);
        let mut urls = Vec::with_capacity(OBJECTS_PER_HOST * 2);
        let mut artifacts = String::new();
        for index in 0..OBJECTS_PER_HOST * 2 {
            let body = non_repeating_bytes(index + 1, HOST_OBJECT_BYTES);
            let script = Script::serving(body)
                .tagged(vec![format!("\"object-{index}\"")])
                .delayed(Latency::default().every_request(HOST_LATENCY));
            let script = if rate_limited && index % 2 == 1 {
                script.replying(vec![Reply::Status {
                    code: 429,
                    retry_after: Some("0".to_owned()),
                }])
            } else {
                script
            };
            let loopback = if index % 2 == 0 { "::1" } else { "127.0.0.1" };
            let server = TestServer::start_on(loopback, script).map_err(BenchError::Process)?;
            let origin = server.origin();
            writeln!(
                artifacts,
                "  - id: object-{index}\n    sources: [\"{origin}/object-{index}\"]"
            )
            .map_err(|_| BenchError::MissingBinary(scratch.clone()))?;
            urls.push(format!("{origin}/object-{index}"));
            servers.push(server);
        }

        let manifest = scratch.join("hosts.yaml");
        fs::write(&manifest, format!("name: hosts\nartifacts:\n{artifacts}"))
            .map_err(BenchError::Process)?;

        let mut command = Command::new(binary);
        command
            .arg("get")
            .arg(&manifest)
            .arg("--output")
            .arg(scratch.join("out"))
            .arg("--lock")
            .arg(scratch.join("fetchloom.lock"))
            .arg("--json")
            .env("FETCHLOOM_CACHE_DIR", &cache);
        let run = measure(command)?;
        let hosts = recorded_hosts(&cache);
        decided(&hosts, rate_limited)?;

        let mut fetched = 0.0;
        for (index, url) in urls.iter().enumerate() {
            fetched += curl_to(url, &scratch.join(format!("curl-{index}")))?;
        }
        curls.push(fetched);
        drop(servers);

        times.push(run.wall_ms);
        work = run.outcome.work;
        peak = peak.max(run.peak_bytes);
    }

    times.sort_by(f64::total_cmp);
    Ok(vec![RegimeResult {
        regime: regime.to_owned(),
        iterations,
        metrics: vec![Metric {
            name: "wall".to_owned(),
            value: times[times.len() / 2],
            unit: "ms".to_owned(),
            kind: MetricKind::Timing,
        }]
        .into_iter()
        .chain(work_metrics(&work))
        .chain(std::iter::once(peak_metric(peak)))
        .collect(),
        alternative: Some(Alternative {
            tool: "curl".to_owned(),
            does: "fetches the same sixteen objects from the same two servers one after \
                   another, hashing nothing, verifying nothing, and never backing off when a \
                   host asks it to"
                .to_owned(),
            wall_ms: median(curls),
        }),
    }])
}

fn free_to_decide() -> bool {
    std::env::var("FETCHLOOM_PER_HOST")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .is_none_or(|ceiling| ceiling > 1)
}

fn decided(hosts: &[RecordedHost], rate_limited: bool) -> Result<(), BenchError> {
    if !free_to_decide() {
        return Ok(());
    }
    if hosts.len() != 2 {
        return Err(BenchError::ControllerInert(format!(
            "the run recorded {} hosts rather than the two it was served by, so the measurement \
             is not keyed by host",
            hosts.len()
        )));
    }
    if hosts[0].host == hosts[1].host {
        return Err(BenchError::ControllerInert(format!(
            "both records name {}, so the two hosts were not tracked apart",
            hosts[0].host
        )));
    }
    let cold = fetchloom_engine::tuning::FIRST_PER_HOST;
    if hosts
        .iter()
        .all(|record| record.measurement.concurrency == cold)
    {
        return Err(BenchError::ControllerInert(format!(
            "every host is recorded at {cold}, which is where a host nothing is known about \
             starts, so nothing the controller does was measured"
        )));
    }
    if rate_limited && hosts[0].measurement.concurrency == hosts[1].measurement.concurrency {
        return Err(BenchError::ControllerInert(format!(
            "{} and {} are both recorded at {}, though only one of them was ever rate limited, \n             so either a rate limit on one host moved the other or neither moved at all",
            hosts[0].host, hosts[1].host, hosts[0].measurement.concurrency
        )));
    }
    Ok(())
}

struct Measured {
    wall_ms: f64,
    peak_bytes: u64,
    outcome: RunOutcome,
}

fn measure(mut command: Command) -> Result<Measured, BenchError> {
    use std::io::Read as _;

    let mut child = command
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(BenchError::Process)?;
    let started = Instant::now();
    let watching = peak_watcher(&child);

    let mut body = Vec::new();
    if let Some(mut stream) = child.stdout.take() {
        stream.read_to_end(&mut body).map_err(BenchError::Process)?;
    }
    let status = child.wait().map_err(BenchError::Process)?;
    let elapsed = started.elapsed();
    let peak_bytes = peak_of(&child, &watching)?;
    if !status.success() {
        return Err(BenchError::NonZeroExit(status.code().unwrap_or(-1)));
    }
    let outcome: RunOutcome = serde_json::from_slice(&body).map_err(BenchError::MalformedResult)?;
    Ok(Measured {
        wall_ms: elapsed.as_secs_f64() * 1000.0,
        peak_bytes,
        outcome,
    })
}

#[cfg(windows)]
struct Watcher;

#[cfg(windows)]
fn peak_watcher(_child: &std::process::Child) -> Watcher {
    Watcher
}

#[cfg(windows)]
fn peak_of(child: &std::process::Child, _watching: &Watcher) -> Result<u64, BenchError> {
    use std::os::windows::io::AsRawHandle as _;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: 0,
        PageFaultCount: 0,
        PeakWorkingSetSize: 0,
        WorkingSetSize: 0,
        QuotaPeakPagedPoolUsage: 0,
        QuotaPagedPoolUsage: 0,
        QuotaPeakNonPagedPoolUsage: 0,
        QuotaNonPagedPoolUsage: 0,
        PagefileUsage: 0,
        PeakPagefileUsage: 0,
    };
    let size = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>())
        .map_err(|_| BenchError::NoPeak)?;
    let handle = child.as_raw_handle() as HANDLE;
    // SAFETY: the handle is owned and still open, and the structure is a live local of exactly the size passed.
    let ok = unsafe { GetProcessMemoryInfo(handle, &raw mut counters, size) };
    if ok == 0 {
        return Err(BenchError::NoPeak);
    }
    u64::try_from(counters.PeakWorkingSetSize).map_err(|_| BenchError::NoPeak)
}

#[cfg(not(windows))]
type Watcher = std::sync::Arc<std::sync::atomic::AtomicU64>;

#[cfg(not(windows))]
fn peak_watcher(child: &std::process::Child) -> Watcher {
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let path = PathBuf::from(format!("/proc/{}/status", child.id()));
    let writing = std::sync::Arc::clone(&seen);
    std::thread::spawn(move || {
        while let Ok(text) = fs::read_to_string(&path) {
            if let Some(kilobytes) = high_water(&text) {
                writing.fetch_max(kilobytes * 1024, std::sync::atomic::Ordering::Relaxed);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    });
    seen
}

#[cfg(not(windows))]
fn high_water(status: &str) -> Option<u64> {
    status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

#[cfg(not(windows))]
fn peak_of(_child: &std::process::Child, seen: &Watcher) -> Result<u64, BenchError> {
    match seen.load(std::sync::atomic::Ordering::Relaxed) {
        0 => Err(BenchError::NoPeak),
        bytes => Ok(bytes),
    }
}

fn peak_metric(peak_bytes: u64) -> Metric {
    Metric {
        name: "peak-memory".to_owned(),
        #[expect(
            clippy::cast_precision_loss,
            reason = "a resident set below two to the fifty-third bytes is exact"
        )]
        value: peak_bytes as f64,
        unit: "bytes".to_owned(),
        kind: MetricKind::Bounded,
    }
}

const CONSTRAINED_LATENCY: Duration = Duration::from_millis(250);

const CONSTRAINED_OBJECT_BYTES: usize = 2 * 1024 * 1024;

pub fn run_constrained(binary: &Path, iterations: u32) -> Result<RegimeResult, BenchError> {
    let object = non_repeating_bytes(5, CONSTRAINED_OBJECT_BYTES);
    let mut times = Vec::with_capacity(iterations as usize);
    let mut work = Work::default();
    let mut peak = 0;
    let mut curls = Vec::with_capacity(iterations as usize);

    for round in 0..iterations {
        let scratch = scratch_directory(CONSTRAINED_ROUND + round)?;
        let server = TestServer::start(
            Script::serving(object.clone())
                .tagged(vec!["\"constrained\"".to_owned()])
                .delayed(Latency::default().every_request(CONSTRAINED_LATENCY)),
        )
        .map_err(BenchError::Process)?;

        let run = measure_transfer(
            binary,
            &server,
            &scratch.join("out"),
            &scratch.join("cache"),
        )?;
        curls.push(curl_to(
            &format!("{}/object", server.origin()),
            &scratch.join("curl"),
        )?);
        drop(server);

        times.push(run.wall_ms);
        work = run.outcome.work;
        peak = peak.max(run.peak_bytes);
        let _ = fs::remove_dir_all(&scratch);
    }

    Ok(RegimeResult {
        regime: "constrained-network".to_owned(),
        iterations,
        metrics: vec![Metric {
            name: "wall".to_owned(),
            value: median(times),
            unit: "ms".to_owned(),
            kind: MetricKind::Timing,
        }]
        .into_iter()
        .chain(work_metrics(&work))
        .chain(std::iter::once(peak_metric(peak)))
        .collect(),
        alternative: Some(Alternative {
            tool: "curl --output".to_owned(),
            does: "downloads the same object from the same slow server, hashing nothing and \
                   verifying nothing"
                .to_owned(),
            wall_ms: median(curls),
        }),
    })
}

const CONSTRAINED_ROUND: u32 = 800;

const SLOW_FLUSH: Duration = Duration::from_millis(8);

const SLOW_DISK_OBJECTS: usize = 24;

const SLOW_DISK_OBJECT_BYTES: usize = 64 * 1024;

const SLOW_DISK_ROUND: u32 = 700;

pub fn run_slow_disk(iterations: u32) -> Result<RegimeResult, BenchError> {
    let mut fast = Vec::with_capacity(iterations as usize);
    let mut slow = Vec::with_capacity(iterations as usize);

    for round in 0..iterations {
        let scratch = scratch_directory(SLOW_DISK_ROUND + round)?;
        fast.push(ingest_round(&scratch.join("fast"), Duration::ZERO)?);
        slow.push(ingest_round(&scratch.join("slow"), SLOW_FLUSH)?);
        let _ = fs::remove_dir_all(&scratch);
    }

    Ok(RegimeResult {
        regime: "slow-disk".to_owned(),
        iterations,
        metrics: vec![
            Metric {
                name: "wall".to_owned(),
                value: median(slow),
                unit: "ms".to_owned(),
                kind: MetricKind::Timing,
            },
            Metric {
                name: "objects".to_owned(),
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a count below two to the fifty-third is exact"
                )]
                value: SLOW_DISK_OBJECTS as f64,
                unit: "objects".to_owned(),
                kind: MetricKind::Deterministic,
            },
        ],
        alternative: Some(Alternative {
            tool: "the same ingest on a device that answers at once".to_owned(),
            does: "performs exactly the same work with no wait charged to a flush or a \
                   preallocation"
                .to_owned(),
            wall_ms: median(fast),
        }),
    })
}

fn ingest_round(root: &Path, charged: Duration) -> Result<f64, BenchError> {
    use std::sync::Arc;

    use fetchloom_engine::durability::DurabilityTier;
    use fetchloom_engine::pool::Processor;
    use fetchloom_engine::seam::policy::IoMode;
    use fetchloom_engine::threads::ThreadBudget;
    use fetchloom_engine::verification::VerificationPolicy;
    use fetchloom_engine::work::WorkCounter;
    use fetchloom_faults::{FaultyPlatform, Operation};
    use fetchloom_platform::NativePlatform;

    let sources = root.join("sources");
    fs::create_dir_all(&sources).map_err(BenchError::Process)?;
    let mut written = Vec::with_capacity(SLOW_DISK_OBJECTS);
    for index in 0..SLOW_DISK_OBJECTS {
        let path = sources.join(format!("object-{index}.bin"));
        fs::write(&path, non_repeating_bytes(index, SLOW_DISK_OBJECT_BYTES))
            .map_err(BenchError::Process)?;
        written.push(path);
    }

    let work = Arc::new(WorkCounter::new());
    let budget = ThreadBudget::resolve(
        std::thread::available_parallelism().unwrap_or(std::num::NonZeroUsize::MIN),
        None,
    );
    let processor = Arc::new(
        Processor::new(budget).map_err(|reason| BenchError::SlowDisk(reason.to_string()))?,
    );
    let platform = FaultyPlatform::new(NativePlatform::new(Arc::clone(&work)));
    if !charged.is_zero() {
        platform.faults().delay(Operation::Flush, charged);
        platform.faults().delay(Operation::Preallocate, charged);
    }
    let cache = fetchloom_cache::Cache::open(
        root.join("cache"),
        platform,
        fetchloom_cache::CacheSettings {
            tier: DurabilityTier::Strict,
            policy: VerificationPolicy::Always,
            io: IoMode::Buffered,
            compression: fetchloom_engine::compression::CompressionChoice::Auto,
        },
        work,
        processor,
    )
    .map_err(|reason| BenchError::SlowDisk(reason.to_string()))?;

    let started = Instant::now();
    for path in &written {
        cache
            .ingest(path)
            .map_err(|reason| BenchError::SlowDisk(reason.to_string()))?;
    }
    Ok(started.elapsed().as_secs_f64() * 1000.0)
}

const PACKED_OBJECTS: usize = 2_000;

const PACKED_ROUND: u32 = 600;

pub fn run_packed_index(binary: &Path, iterations: u32) -> Result<RegimeResult, BenchError> {
    let mut smaller = Vec::with_capacity(iterations as usize);
    let mut larger = Vec::with_capacity(iterations as usize);

    for round in 0..iterations {
        let scratch = scratch_directory(PACKED_ROUND + round)?;
        smaller.push(status_over(binary, &scratch.join("small"), PACKED_OBJECTS)?);
        larger.push(status_over(
            binary,
            &scratch.join("large"),
            PACKED_OBJECTS * 2,
        )?);
        let _ = fs::remove_dir_all(&scratch);
    }

    let small = median(smaller);
    let large = median(larger);
    let ratio = if small > 0.0 { large / small } else { 0.0 };

    Ok(RegimeResult {
        regime: "packed-index".to_owned(),
        iterations,
        metrics: vec![
            Metric {
                name: "wall".to_owned(),
                value: large,
                unit: "ms".to_owned(),
                kind: MetricKind::Timing,
            },
            Metric {
                name: "doubling-ratio".to_owned(),
                value: ratio,
                unit: "times".to_owned(),
                kind: MetricKind::Timing,
            },
            Metric {
                name: "objects".to_owned(),
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "a count below two to the fifty-third is exact"
                )]
                value: (PACKED_OBJECTS * 2) as f64,
                unit: "objects".to_owned(),
                kind: MetricKind::Deterministic,
            },
        ],
        alternative: None,
    })
}

fn status_over(binary: &Path, root: &Path, objects: usize) -> Result<f64, BenchError> {
    let source = root.join("source");
    fs::create_dir_all(&source).map_err(BenchError::Process)?;
    for index in 0..objects {
        fs::write(
            source.join(format!("object-{index}.bin")),
            format!("packed object number {index}").as_bytes(),
        )
        .map_err(BenchError::Process)?;
    }
    let cache = root.join("cache");
    measure_get(binary, &source, &root.join("out"), &cache)?;

    let mut samples = Vec::with_capacity(3);
    for _ in 0..3 {
        let started = Instant::now();
        let status = Command::new(binary)
            .arg("cache")
            .arg("status")
            .env("FETCHLOOM_CACHE_DIR", &cache)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(BenchError::Process)?;
        let elapsed = started.elapsed();
        if !status.success() {
            return Err(BenchError::NonZeroExit(status.code().unwrap_or(-1)));
        }
        samples.push(elapsed.as_secs_f64() * 1000.0);
    }
    Ok(median(samples))
}
#[cfg(test)]
mod tests {
    use super::{Baseline, Metric, MetricKind, RegimeResult, compare};

    fn baseline_of(metrics: Vec<Metric>) -> Baseline {
        Baseline {
            target: "test".to_owned(),
            regimes: vec![RegimeResult {
                regime: "cold-cache".to_owned(),
                iterations: 1,
                metrics,
                alternative: None,
            }],
        }
    }

    fn counted(name: &str, value: f64) -> Metric {
        Metric {
            name: name.to_owned(),
            value,
            unit: "bytes".to_owned(),
            kind: MetricKind::Deterministic,
        }
    }

    #[test]
    fn a_metric_that_did_not_move_passes() {
        let recorded = baseline_of(vec![counted("bytes_written", 1000.0)]);
        let current = baseline_of(vec![counted("bytes_written", 1000.0)]);
        assert!(compare(&recorded, &current).is_ok());
    }

    #[test]
    fn a_metric_that_halved_fails() {
        let recorded = baseline_of(vec![counted("bytes_written", 1000.0)]);
        let current = baseline_of(vec![counted("bytes_written", 500.0)]);
        assert!(
            compare(&recorded, &current).is_err(),
            "a deterministic metric that halved passed the gate, and every counter defect this \
             project has found presents as a drop"
        );
    }

    #[test]
    fn a_metric_that_grew_past_the_band_fails() {
        let recorded = baseline_of(vec![counted("bytes_written", 1000.0)]);
        let current = baseline_of(vec![counted("bytes_written", 1060.0)]);
        assert!(compare(&recorded, &current).is_err());
    }

    #[test]
    fn a_metric_the_run_stopped_producing_fails() {
        let recorded = baseline_of(vec![
            counted("bytes_written", 1000.0),
            counted("bytes_read", 2000.0),
        ]);
        let current = baseline_of(vec![counted("bytes_written", 1000.0)]);
        assert!(
            compare(&recorded, &current).is_err(),
            "a run that stopped producing a metric the baseline carries passed the gate"
        );
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "a probe that cannot decide is the assertion"
    )]
    fn the_corpus_every_regime_is_measured_against_is_one_no_compressor_shrinks() {
        let bytes = super::non_repeating_bytes(
            3,
            fetchloom_engine::compression::PROBE_HEAD_BYTES.saturating_mul(2),
        );
        let decision = fetchloom_cache::compress::decide(
            &bytes,
            fetchloom_engine::compression::CompressionChoice::Auto,
        )
        .unwrap();
        assert_eq!(
            decision.stored,
            fetchloom_engine::compression::Stored::Raw,
            "the benchmark corpus compresses, so every regime named for a large object measures \
             the compressed publication path and its extra full read rather than the object: {}",
            decision.reason()
        );
    }

    #[test]
    #[expect(
        clippy::unwrap_used,
        reason = "a baseline that does not parse is the assertion"
    )]
    fn every_committed_baseline_parses_and_holds_only_what_gates() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("benchmarks");
        let mut found = 0;
        for entry in std::fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|kind| kind != "json") {
                continue;
            }
            found += 1;
            let baseline = super::load(&path).unwrap();
            assert_eq!(
                baseline.target,
                path.file_stem().unwrap().to_string_lossy(),
                "{} states a target its name does not",
                path.display()
            );
            for regime in &baseline.regimes {
                for metric in &regime.metrics {
                    assert_ne!(
                        metric.kind,
                        MetricKind::Timing,
                        "{} records {} {}, which is a duration, and a duration recorded on the runner that gates is a number nobody may compare",
                        path.display(),
                        regime.regime,
                        metric.name
                    );
                }
            }
            let unbounded: Vec<&str> = baseline
                .regimes
                .iter()
                .filter(|regime| {
                    !regime
                        .metrics
                        .iter()
                        .any(|metric| metric.name == "peak-memory")
                })
                .map(|regime| regime.regime.as_str())
                .collect();
            assert_eq!(
                unbounded,
                ["packed-index", "slow-disk"],
                "{} states a peak-memory bound for a different set of regimes than the two that measure a store operation rather than a whole run and so report no process peak",
                path.display()
            );
        }
        assert!(
            found > 0,
            "no baseline is committed, so the benchmark lane records one every run and gates nothing"
        );
    }

    #[test]
    fn a_gate_that_finds_three_failures_reports_three() {
        let recorded = baseline_of(vec![
            counted("bytes_written", 1000.0),
            counted("bytes_read", 2000.0),
            counted("file_operations", 40.0),
        ]);
        let current = baseline_of(vec![
            counted("bytes_written", 500.0),
            counted("bytes_read", 4000.0),
            counted("file_operations", 80.0),
        ]);
        let complaint = compare(&recorded, &current)
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        for named in ["bytes_written", "bytes_read", "file_operations"] {
            assert!(
                complaint.contains(named),
                "the gate found three metrics moved and named fewer, so a red step reads as one failure: {complaint}"
            );
        }
    }

    fn bounded(value: f64) -> Metric {
        Metric {
            name: "peak-memory".to_owned(),
            value,
            unit: "bytes".to_owned(),
            kind: MetricKind::Bounded,
        }
    }

    #[test]
    fn a_bound_that_rose_past_its_band_fails() {
        let recorded = baseline_of(vec![bounded(10_000_000.0)]);
        let current = baseline_of(vec![bounded(11_500_000.0)]);
        assert!(
            compare(&recorded, &current).is_err(),
            "peak memory rose 15 percent above the runner's own baseline and the gate passed"
        );
    }

    #[test]
    fn a_bound_that_moved_inside_its_band_passes_and_one_that_fell_always_does() {
        let recorded = baseline_of(vec![bounded(10_000_000.0)]);
        assert!(
            compare(&recorded, &baseline_of(vec![bounded(10_800_000.0)])).is_ok(),
            "a rise of eight percent failed, which is the spread a runner shows between runs"
        );
        assert!(
            compare(&recorded, &baseline_of(vec![bounded(4_000_000.0)])).is_ok(),
            "holding less memory failed the gate, and holding less is never a regression"
        );
    }

    #[test]
    fn a_timing_metric_never_gates() {
        let timing = |value: f64| Metric {
            name: "wall".to_owned(),
            value,
            unit: "ms".to_owned(),
            kind: MetricKind::Timing,
        };
        let recorded = baseline_of(vec![timing(100.0)]);
        let current = baseline_of(vec![timing(4000.0)]);
        assert!(compare(&recorded, &current).is_ok());
    }
}

#[cfg(test)]
mod hosts_tests {
    use super::{RecordedHost, decided};

    fn host(name: &str, concurrency: u32) -> RecordedHost {
        RecordedHost {
            host: name.to_owned(),
            measurement: fetchloom_engine::tuning::HostMeasurement {
                concurrency,
                throughput: 1,
                time_to_first_byte_ms: 1,
                observed_at: fetchloom_engine::timestamp::Timestamp::now(),
            },
        }
    }

    #[test]
    fn two_hosts_treated_alike_decide_the_regime_where_neither_was_rate_limited() {
        let served = [host("::1", 4), host("127.0.0.1", 4)];
        assert!(
            decided(&served, false).is_ok(),
            "two hosts answering the same way had to differ to count, though nothing made them differ"
        );
    }

    #[test]
    fn two_hosts_treated_alike_decide_nothing_where_one_was_rate_limited() {
        let served = [host("::1", 4), host("127.0.0.1", 4)];
        assert!(decided(&served, true).is_err());
    }

    #[test]
    fn a_host_left_where_it_started_decides_nothing() {
        let cold = fetchloom_engine::tuning::FIRST_PER_HOST;
        let served = [host("::1", cold), host("127.0.0.1", cold)];
        assert!(decided(&served, false).is_err());
    }
}
