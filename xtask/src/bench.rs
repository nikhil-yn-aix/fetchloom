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

/// The fraction a metric may worsen by before the gate fails.
pub const REGRESSION_GATE: f64 = 0.05;

/// How a metric is gated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// Identical on identical inputs. Gates on every machine.
    Deterministic,
    /// A property of the machine as much as the code. Gates only on a
    /// verification run against a baseline from that same machine.
    Timing,
}

/// One measured number from one regime.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    /// What was measured.
    pub name: String,
    /// The value measured, in the metric's own unit.
    pub value: f64,
    /// The unit the value is in.
    pub unit: String,
    /// How this metric is gated.
    pub kind: MetricKind,
}

/// What the obvious alternative tool costs on the same regime, on the same
/// machine, in the same run. It is context for a published number and is never
/// gated, because the gate is about this build against the last one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Alternative {
    /// The command the comparison ran.
    pub tool: String,
    /// What it does, which is never exactly what Fetchloom does.
    pub does: String,
    /// The median wall time it took, in milliseconds.
    pub wall_ms: f64,
}

/// Everything one regime measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeResult {
    /// The regime that was run.
    pub regime: String,
    /// How many times the regime was run.
    pub iterations: u32,
    /// The numbers it produced.
    pub metrics: Vec<Metric>,
    /// What the obvious alternative cost on the same regime, when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternative: Option<Alternative>,
}

/// A whole harness run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    /// The target the binary was built for.
    pub target: String,
    /// The regimes that were run.
    pub regimes: Vec<RegimeResult>,
}

/// Why the harness could not produce a result.
#[derive(Debug)]
pub enum BenchError {
    /// The binary could not be built or run.
    Process(io::Error),
    /// The build did not produce a binary.
    MissingBinary(PathBuf),
    /// A run of the binary did not exit zero.
    NonZeroExit(i32),
    /// The baseline file could not be read or written.
    Baseline(io::Error),
    /// The baseline file was not the shape the harness writes.
    MalformedBaseline(serde_json::Error),
    /// A regime in the baseline was not run, or the other way round.
    RegimeMissing(String),
    /// A run carries a deterministic metric the baseline does not, which is a
    /// metric that was added rather than a number that moved.
    MetricAdded {
        /// The regime the metric belongs to.
        regime: String,
        /// The metric the baseline does not carry.
        metric: String,
    },
    /// A run's `--json` result could not be parsed.
    MalformedResult(serde_json::Error),
    /// The regime ran but the adaptive controller decided nothing in it.
    ControllerInert(String),
    /// The no-op regime was not idle, so it measured work rather than startup.
    NotIdle(String),
    /// The operating system did not report what a run's resident set peaked at.
    NoPeak,
    /// The slow-disk regime could not open a cache or finish an ingest.
    SlowDisk(String),
    /// Doubling the number of packed objects more than doubled the cost of
    /// counting them.
    PackedIndexCurve {
        /// How much the cost grew for twice the objects.
        ratio: f64,
    },
    /// A metric moved by more than the gate allows, in either direction.
    Moved {
        /// The regime the metric belongs to.
        regime: String,
        /// The metric that moved.
        metric: String,
        /// What the baseline recorded.
        baseline: f64,
        /// What this run recorded.
        current: f64,
    },
    /// The baseline carries a deterministic metric this run did not produce.
    MetricMissing {
        /// The regime the metric belongs to.
        regime: String,
        /// The metric the run did not produce.
        metric: String,
    },
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
            Self::PackedIndexCurve { ratio } => write!(
                f,
                "twice the packed objects cost {ratio} times as much to count, so a lookup is walking what it should index"
            ),
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
        }
    }
}

impl std::error::Error for BenchError {}

/// Builds the binary in release and returns where it landed.
///
/// # Errors
///
/// Fails when the build cannot be started, when it does not succeed, and when
/// it produces no binary.
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

/// Runs the no-op regime and returns what it measured.
///
/// # Errors
///
/// Fails when the binary cannot be run, when a run does not exit zero, and when
/// the binary's own size cannot be read.
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

/// The scratch round the no-op regime keeps its object in.
const NO_OP_ROUND: u32 = 900;

/// How large the object the no-op regime reconciles is, small enough that the
/// regime measures starting up rather than hashing.
const NO_OP_BYTES: usize = 1024;

/// Runs a locked run against a destination that already holds every entry.
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
        .arg("--locked")
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stderr(std::process::Stdio::null());
    measure(command)
}

/// Writes a baseline to a file.
///
/// # Errors
///
/// Fails when the file cannot be written.
pub fn save(baseline: &Baseline, path: &Path) -> Result<(), BenchError> {
    let text = serde_json::to_string_pretty(baseline).map_err(BenchError::MalformedBaseline)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(BenchError::Baseline)?;
    }
    fs::write(path, text + "\n").map_err(BenchError::Baseline)
}

/// Reads a baseline from a file.
///
/// # Errors
///
/// Fails when the file cannot be read and when it is not the shape the harness
/// writes.
pub fn load(path: &Path) -> Result<Baseline, BenchError> {
    let text = fs::read_to_string(path).map_err(BenchError::Baseline)?;
    serde_json::from_str(&text).map_err(BenchError::MalformedBaseline)
}

/// Compares a run against a baseline.
///
/// # Errors
///
/// Returns the regime and metric that regressed, with both numbers.
pub fn compare(baseline: &Baseline, current: &Baseline) -> Result<(), BenchError> {
    for regime in &current.regimes {
        let recorded = baseline
            .regimes
            .iter()
            .find(|candidate| candidate.regime == regime.regime)
            .ok_or_else(|| BenchError::RegimeMissing(regime.regime.clone()))?;
        for metric in &regime.metrics {
            if metric.kind == MetricKind::Timing {
                continue;
            }
            let found = recorded
                .metrics
                .iter()
                .find(|candidate| candidate.name == metric.name);
            let Some(previous) = found else {
                return Err(BenchError::MetricAdded {
                    regime: regime.regime.clone(),
                    metric: metric.name.clone(),
                });
            };
            if (metric.value - previous.value).abs() > previous.value.abs() * REGRESSION_GATE {
                return Err(BenchError::Moved {
                    regime: regime.regime.clone(),
                    metric: metric.name.clone(),
                    baseline: previous.value,
                    current: metric.value,
                });
            }
        }
        for previous in &recorded.metrics {
            if previous.kind == MetricKind::Timing {
                continue;
            }
            if !regime
                .metrics
                .iter()
                .any(|candidate| candidate.name == previous.name)
            {
                return Err(BenchError::MetricMissing {
                    regime: regime.regime.clone(),
                    metric: previous.name.clone(),
                });
            }
        }
    }
    Ok(())
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

/// Reports what is inspecting writes on the volume the regimes run on.
///
/// # Errors
///
/// Fails when the volume cannot be probed.
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

/// How many files the many-small-files regime materializes.
const SMALL_FILES: usize = 1024;

/// How many bytes each of those files holds.
const SMALL_FILE_BYTES: usize = 1024;

/// How many bytes the one-large-file regime materializes.
const LARGE_FILE_BYTES: usize = 256 * 1024 * 1024;

/// Runs the many-small-files and one-large-file regimes.
///
/// # Errors
///
/// Fails when the corpus cannot be written, when a run does not exit zero, and
/// when a run's `--json` result cannot be read.
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

/// How many files the cache regimes materialize.
const CORPUS_FILES: usize = 64;

/// How many bytes each of those files holds.
const CORPUS_FILE_BYTES: usize = 256 * 1024;

/// Runs the cold cache and warm cache regimes and returns what they measured.
///
/// # Errors
///
/// Fails when the corpus cannot be written, when a run does not exit zero, and
/// when the cache cannot be measured.
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

/// Reports whether the warm regime was faster than the cold one.
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

/// Returns bytes no two indices share.
fn non_repeating_bytes(index: usize, length: usize) -> Vec<u8> {
    let seed = u8::try_from(index % 251).unwrap_or(0);
    let mut bytes: Vec<u8> = (0..length)
        .map(|offset| {
            let offset = u8::try_from(offset % 251).unwrap_or(0);
            offset.wrapping_mul(seed).wrapping_add(seed)
        })
        .collect();
    for (at, byte) in index.to_le_bytes().iter().enumerate() {
        if let Some(slot) = bytes.get_mut(at) {
            *slot = *byte;
        }
    }
    bytes
}

/// How many bytes the transfer regimes' object holds.
const TRANSFER_OBJECT_BYTES: usize = 4 * 1024 * 1024;

/// Where the interrupted transfer regime's server closes the connection partway
/// through the body, as percentages of the object, before serving it whole.
const TRANSFER_INTERRUPTIONS: [usize; 2] = [25, 50];

/// Runs the cold transfer and interrupted transfer regimes and returns what
/// they measured.
///
/// # Errors
///
/// Fails when the test server cannot be started, when a run does not exit zero,
/// and when a run's `--json` result cannot be read.
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

/// Builds one transfer regime result from what its rounds measured.
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

/// What a run's `--json` result carries that a benchmark reads.
#[derive(Deserialize)]
struct RunOutcome {
    bytes: u64,
    status: String,
    work: Work,
}

/// Turns the four work counters into the metrics that gate on them.
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

/// Returns how many bytes the cache holds, in whichever placement holds them.
fn stored_bytes(cache: &Path) -> u64 {
    directory_bytes(&cache.join("objects")) + directory_bytes(&cache.join("packs"))
}

/// Runs a comparison command and returns how long it took, in milliseconds.
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

/// Returns the median of what a comparison command took over several rounds.
fn median(mut times: Vec<f64>) -> f64 {
    times.sort_by(f64::total_cmp);
    times[times.len() / 2]
}

/// Copies a tree with the platform's own copy, which is the alternative every
/// local regime is measured against.
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

/// Downloads a URL with curl, which is the alternative the transfer regimes are
/// measured against.
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

/// The copy the published comparison runs, named as the reader would run it.
#[cfg(windows)]
pub const COPY_TOOL: &str = "powershell Copy-Item -Recurse";

/// The copy the published comparison runs, named as the reader would run it.
#[cfg(not(windows))]
pub const COPY_TOOL: &str = "cp -r";

/// Renders the published comparison page from what a run measured, so the page
/// cannot drift from the numbers it reports.
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

/// States what the many-hosts ratio is made of, from the same run that produced
/// it, so the largest loss on the page is published with its cause rather than
/// bare.
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

/// How many objects each host serves in the many-hosts regime.
///
/// The controller starts a host it has measured nothing about at
/// `FIRST_PER_HOST`, adds one after a clean transfer, and halves on a rate
/// limit. Eight is the smallest count that exercises the whole cycle rather
/// than one end of it: two clean transfers to climb from two to the politeness
/// ceiling of four, a third to prove it holds there rather than climbing past
/// it, a rate limit to halve it, and the rest to climb back. Anything smaller
/// records a number that only proves the controller starts somewhere.
const OBJECTS_PER_HOST: usize = 8;

/// How many bytes each of those objects holds.
const HOST_OBJECT_BYTES: usize = 128 * 1024;

/// How long each host waits before answering anything.
///
/// Loopback has no latency, and concurrency exists to hide latency, so without
/// an injected wait no per-host ceiling can pay for itself in this regime.
const HOST_LATENCY: Duration = Duration::from_millis(100);

/// What one host answered, read back from the measurement the run recorded.
#[derive(Debug, Deserialize)]
struct RecordedHost {
    host: String,
    measurement: fetchloom_engine::tuning::HostMeasurement,
}

/// Returns what every host the run measured was recorded as, by host name.
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

/// Runs the many-hosts regime: many objects across two hosts, so the adaptive
/// controller has something to decide and its decision can be read back.
///
/// # Errors
///
/// Fails when a server cannot be bound, when the run does not exit zero, when
/// its `--json` result cannot be read, and when the controller did not decide
/// anything, which is the whole point of the regime.
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

/// Runs one half of the many-hosts pair. Without a rate limit the regime
/// measures what concurrency buys; with one it measures what backoff costs,
/// and mixing them measures neither.
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
        decided(&hosts)?;

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

/// Reports whether anything the run was given left the controller free to
/// decide. A per-host ceiling of one is not an inert controller, it is a
/// controller with one choice, so the regime measures such a run and asserts
/// nothing about adaptation in it.
fn free_to_decide() -> bool {
    std::env::var("FETCHLOOM_PER_HOST")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .is_none_or(|ceiling| ceiling > 1)
}

/// Refuses a run in which the controller decided nothing, because a regime that
/// passes with the controller inert is not a regime.
fn decided(hosts: &[RecordedHost]) -> Result<(), BenchError> {
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
    if hosts[0].measurement.concurrency == hosts[1].measurement.concurrency {
        return Err(BenchError::ControllerInert(format!(
            "{} and {} are both recorded at {}, though only one of them was ever rate limited, \n             so either a rate limit on one host moved the other or neither moved at all",
            hosts[0].host, hosts[1].host, hosts[0].measurement.concurrency
        )));
    }
    Ok(())
}

/// What one measured run of the binary cost.
struct Measured {
    wall_ms: f64,
    peak_bytes: u64,
    outcome: RunOutcome,
}

/// Runs the binary once, timing it and recording the largest resident set the
/// operating system saw it hold.
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
    let peak_bytes = peak_of(&child, watching)?;
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
fn peak_of(child: &std::process::Child, _watching: Watcher) -> Result<u64, BenchError> {
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
    // SAFETY: the child handle is owned and still open, and the structure is a live local of exactly the size passed.
    let ok =
        unsafe { GetProcessMemoryInfo(child.as_raw_handle() as HANDLE, &raw mut counters, size) };
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
fn peak_of(_child: &std::process::Child, seen: Watcher) -> Result<u64, BenchError> {
    match seen.load(std::sync::atomic::Ordering::Relaxed) {
        0 => Err(BenchError::NoPeak),
        bytes => Ok(bytes),
    }
}

/// Turns a peak resident set into the metric that records it.
fn peak_metric(peak_bytes: u64) -> Metric {
    Metric {
        name: "peak-memory".to_owned(),
        #[expect(
            clippy::cast_precision_loss,
            reason = "a resident set below two to the fifty-third bytes is exact"
        )]
        value: peak_bytes as f64,
        unit: "bytes".to_owned(),
        kind: MetricKind::Timing,
    }
}

/// How long the constrained-network regime charges every request, which is what
/// makes a loopback socket behave like a remote one.
const CONSTRAINED_LATENCY: Duration = Duration::from_millis(250);

/// How large the object the constrained-network regime transfers is.
const CONSTRAINED_OBJECT_BYTES: usize = 2 * 1024 * 1024;

/// Runs the constrained-network regime: one object from one host that answers
/// slowly, so a run is bounded by the network rather than by the disk.
///
/// # Errors
///
/// Fails when the server cannot be bound, when the run does not exit zero, and
/// when its `--json` result cannot be read.
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

/// The scratch rounds the constrained-network regime uses.
const CONSTRAINED_ROUND: u32 = 800;

/// How long the slow-disk regime charges each durability flush, which is what
/// a device that answers slowly costs a run that keeps its promises.
const SLOW_FLUSH: Duration = Duration::from_millis(8);

/// How many objects the slow-disk regime ingests.
const SLOW_DISK_OBJECTS: usize = 24;

/// How large each of them is.
const SLOW_DISK_OBJECT_BYTES: usize = 64 * 1024;

/// The scratch rounds the slow-disk regime uses.
const SLOW_DISK_ROUND: u32 = 700;

/// Runs the slow-disk regime: the same ingest, once against a device that
/// answers at once and once against one that charges every flush and every
/// preallocation.
///
/// The regime drives the cache in this process rather than the binary in
/// another one, because a fault schedule reaches the platform it wraps and
/// never a separate process. That is the whole reason a slow disk was named
/// nowhere in the harness until now.
///
/// # Errors
///
/// Fails when the cache cannot be opened and when an ingest fails.
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

/// Ingests the regime's objects into a fresh cache, charging each durability
/// call the given wait, and returns how long it took in milliseconds.
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
        DurabilityTier::Strict,
        VerificationPolicy::Always,
        IoMode::Buffered,
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

/// How many packed objects the smaller half of the packed-index regime holds.
const PACKED_OBJECTS: usize = 2_000;

/// The largest doubling ratio the regime accepts. Quadratic is four and linear
/// is two, so a run above this is a lookup that walks what it should index.
const PACKED_RATIO_CEILING: f64 = 2.0;

/// The scratch rounds the packed-index regime uses.
const PACKED_ROUND: u32 = 600;

/// Runs the packed-index regime: the command whose whole job is to count what
/// a cache holds, against a cache of packed objects and against one of twice
/// as many.
///
/// # Errors
///
/// Fails when a cache cannot be filled, when a run does not exit zero, and
/// when doubling the object count more than doubles the cost, which is the
/// shape a per-lookup rebuild has.
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
    if ratio > PACKED_RATIO_CEILING {
        return Err(BenchError::PackedIndexCurve { ratio });
    }

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

/// Fills a cache with the given number of packed objects and returns how long
/// `cache status` takes over it, in milliseconds.
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
