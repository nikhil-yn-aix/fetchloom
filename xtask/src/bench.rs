//! The regime harness: run the real binary, record numbers, gate on them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use fetchloom_engine::work::Work;
use fetchloom_faults::{Reply, Script, TestServer};
use serde::{Deserialize, Serialize};

/// The fraction a metric may worsen by before the gate fails.
pub const REGRESSION_GATE: f64 = 0.05;

/// How a metric is gated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricKind {
    /// Identical on identical inputs, so it gates on every machine.
    Deterministic,
    /// A property of the machine as much as the code, so it gates only on
    /// a verification run against a baseline from that same machine.
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

/// Everything one regime measured.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RegimeResult {
    /// The regime that was run.
    pub regime: String,
    /// How many times the regime was run.
    pub iterations: u32,
    /// The numbers it produced.
    pub metrics: Vec<Metric>,
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
    /// A run's `--json` result could not be parsed.
    MalformedResult(serde_json::Error),
    /// A metric worsened by more than the gate allows.
    Regression {
        /// The regime the metric belongs to.
        regime: String,
        /// The metric that worsened.
        metric: String,
        /// What the baseline recorded.
        baseline: f64,
        /// What this run recorded.
        current: f64,
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
            Self::MalformedResult(error) => {
                write!(f, "the run's result could not be read: {error}")
            }
            Self::Regression {
                regime,
                metric,
                baseline,
                current,
            } => write!(
                f,
                "{regime} {metric} regressed from {baseline} to {current}"
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
/// The regime runs the binary on an operation that moves no bytes, so it
/// measures startup and configuration discovery, and it bounds what a
/// dependency costs simply by existing. Reconciliation joins the regime when a
/// locked run against an unchanged destination exists to measure.
///
/// # Errors
///
/// Fails when the binary cannot be run, when a run does not exit zero, and when
/// the binary's own size cannot be read.
pub fn run_no_op(binary: &Path, iterations: u32) -> Result<RegimeResult, BenchError> {
    let mut samples = Vec::with_capacity(iterations as usize);
    for _ in 0..iterations {
        let started = Instant::now();
        let status = Command::new(binary)
            .arg("explain")
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
        ],
    })
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
/// Takes the baseline, the current run, and whether timing metrics gate. Returns
/// nothing when every gated metric is within the gate. Deterministic metrics
/// gate on every machine. Timing metrics gate only when timing gating is on,
/// which is only under `cargo xtask verify` against a baseline recorded on that
/// same machine. Fails on the first gated metric that worsened by more than the
/// gate allows, and on any regime present in one run and not the other.
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
            let previous = match found {
                Some(previous) => previous,
                None if metric.kind == MetricKind::Timing => continue,
                None => return Err(BenchError::RegimeMissing(regime.regime.clone())),
            };
            if metric.value > previous.value * (1.0 + REGRESSION_GATE) {
                return Err(BenchError::Regression {
                    regime: regime.regime.clone(),
                    metric: metric.name.clone(),
                    baseline: previous.value,
                    current: metric.value,
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
/// Takes the directory the regimes write into. Returns the sentence the
/// benchmark prints beside its numbers, so a small-files measurement always
/// says which lane it came from rather than leaving the reader to guess.
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
    let platform = fetchloom_platform::NativePlatform::new();
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
///
/// Enough that per-entry cost dominates the bytes moved, and no more, because
/// this regime runs under `cargo xtask verify` and a scanner-enabled run of it
/// costs about a second per hundred files on the machine that measured it.
const SMALL_FILES: usize = 1024;

/// How many bytes each of those files holds.
const SMALL_FILE_BYTES: usize = 1024;

/// How many bytes the one-large-file regime materializes.
const LARGE_FILE_BYTES: usize = 256 * 1024 * 1024;

/// Runs the many-small-files and one-large-file regimes.
///
/// The two regimes move the same kind of bytes through opposite shapes: one
/// tree of many tiny files against one file of the same total size. The first
/// is where per-entry cost dominates and where an on-access scanner is felt;
/// the second is where the streaming path is felt. Both record the work
/// counters, which are identical on identical inputs, alongside a wall clock
/// that is not.
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

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;

        let many = scratch.join("many");
        fs::create_dir_all(&many).map_err(BenchError::Process)?;
        for index in 0..SMALL_FILES {
            let seed = u8::try_from(index % 251).unwrap_or(1);
            let bytes = non_repeating_bytes(seed, SMALL_FILE_BYTES);
            fs::write(many.join(format!("file-{index}.bin")), bytes)
                .map_err(BenchError::Process)?;
        }
        let (wall, work) = measure_get(
            binary,
            &many,
            &scratch.join("many-out"),
            &scratch.join("cache-many"),
        )?;
        small_times.push(wall);
        small_work = work;

        let one = scratch.join("one");
        fs::create_dir_all(&one).map_err(BenchError::Process)?;
        let bytes = non_repeating_bytes(3, LARGE_FILE_BYTES);
        fs::write(one.join("large.bin"), bytes).map_err(BenchError::Process)?;
        let (wall, work) = measure_get(
            binary,
            &one,
            &scratch.join("one-out"),
            &scratch.join("cache-one"),
        )?;
        large_times.push(wall);
        large_work = work;

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
            .collect(),
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
            .collect(),
        },
    ])
}

/// How many files the cache regimes materialize.
const CORPUS_FILES: usize = 64;

/// How many bytes each of those files holds.
const CORPUS_FILE_BYTES: usize = 256 * 1024;

/// Runs the cold cache and warm cache regimes and returns what they measured.
///
/// A cold run starts with an empty cache and writes every object into it. A warm
/// run uses the cache the cold run filled and writes nothing into it, which is
/// the deterministic difference between the two: the warm run's cache growth is
/// zero. Both runs materialize the same tree, so the timing difference is the
/// work the cache saved.
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

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;
        let source = scratch.join("source");
        let cache = scratch.join("cache");
        write_corpus(&source)?;

        let (cold, cold_run) = measure_get(binary, &source, &scratch.join("cold"), &cache)?;
        let after_cold = directory_bytes(&cache.join("objects"));
        let (warm, warm_run) = measure_get(binary, &source, &scratch.join("warm"), &cache)?;
        let after_warm = directory_bytes(&cache.join("objects"));

        cold_times.push(cold);
        warm_times.push(warm);
        cold_growth = after_cold;
        warm_growth = after_warm - after_cold;
        cold_work = cold_run;
        warm_work = warm_run;
        let _ = fs::remove_dir_all(&scratch);
    }

    cold_times.sort_by(f64::total_cmp);
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
            .collect(),
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
            .collect(),
        },
    ])
}

/// Reports whether the warm regime was faster than the cold one.
///
/// Takes the regimes a run measured. Returns nothing when the two are not both
/// present, which is what a run that measured neither answers.
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
        let seed = u8::try_from(index % 251).unwrap_or(1);
        let bytes = non_repeating_bytes(seed, CORPUS_FILE_BYTES);
        fs::write(into.join(format!("file-{index}.bin")), bytes).map_err(BenchError::Process)?;
    }
    Ok(())
}

fn non_repeating_bytes(seed: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|offset| {
            let offset = u8::try_from(offset % 251).unwrap_or(0);
            offset.wrapping_mul(seed).wrapping_add(seed)
        })
        .collect()
}

/// How many bytes the transfer regimes' object holds.
const TRANSFER_OBJECT_BYTES: usize = 4 * 1024 * 1024;

/// Where the interrupted transfer regime's server closes the connection
/// partway through the body, as percentages of the object, before serving it
/// whole.
///
/// Fewer than the attempt limit allows, because the client may retry a request
/// of its own on a connection it had pooled, which consumes a scripted reply
/// without the transfer having made an attempt.
const TRANSFER_INTERRUPTIONS: [usize; 2] = [25, 50];

/// Runs the cold transfer and interrupted transfer regimes and returns what
/// they measured.
///
/// A cold run fetches a 4 MiB object from a local test server that serves it
/// whole. An interrupted run fetches the same object from a server that
/// closes the connection four times partway through the body, at twenty,
/// forty, sixty and eighty percent, before serving it whole. Both regimes
/// materialize the same object, so the deterministic difference between them
/// is nothing: both must report the same bytes transferred.
///
/// # Errors
///
/// Fails when the test server cannot be started, when a run does not exit
/// zero, and when a run's `--json` result cannot be read.
pub fn run_transfer(binary: &Path, iterations: u32) -> Result<Vec<RegimeResult>, BenchError> {
    let object = non_repeating_bytes(1, TRANSFER_OBJECT_BYTES);
    let mut cold_times = Vec::with_capacity(iterations as usize);
    let mut cold_bytes = 0u64;
    let mut cold_work = Work::default();
    let mut interrupted_times = Vec::with_capacity(iterations as usize);
    let mut interrupted_bytes = 0u64;
    let mut interrupted_work = Work::default();

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;

        let cold_server =
            TestServer::start(Script::serving(object.clone())).map_err(BenchError::Process)?;
        let (wall, bytes, work) = measure_transfer(
            binary,
            &cold_server,
            &scratch.join("cold"),
            &scratch.join("cache-cold"),
        )?;
        drop(cold_server);
        cold_times.push(wall);
        cold_bytes = bytes;
        cold_work = work;

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
        let (wall, bytes, work) = measure_transfer(
            binary,
            &interrupted_server,
            &scratch.join("interrupted"),
            &scratch.join("cache-interrupted"),
        )?;
        drop(interrupted_server);
        interrupted_times.push(wall);
        interrupted_bytes = bytes;
        interrupted_work = work;

        let _ = fs::remove_dir_all(&scratch);
    }

    cold_times.sort_by(f64::total_cmp);
    interrupted_times.sort_by(f64::total_cmp);

    Ok(vec![
        RegimeResult {
            regime: "cold-transfer".to_owned(),
            iterations,
            metrics: vec![
                Metric {
                    name: "wall".to_owned(),
                    value: cold_times[cold_times.len() / 2],
                    unit: "ms".to_owned(),
                    kind: MetricKind::Timing,
                },
                Metric {
                    name: "bytes-materialized".to_owned(),
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "an object below two to the fifty-third bytes is exact"
                    )]
                    value: cold_bytes as f64,
                    unit: "bytes".to_owned(),
                    kind: MetricKind::Deterministic,
                },
            ]
            .into_iter()
            .chain(work_metrics(&cold_work))
            .collect(),
        },
        RegimeResult {
            regime: "interrupted-transfer".to_owned(),
            iterations,
            metrics: vec![
                Metric {
                    name: "wall".to_owned(),
                    value: interrupted_times[interrupted_times.len() / 2],
                    unit: "ms".to_owned(),
                    kind: MetricKind::Timing,
                },
                Metric {
                    name: "bytes-materialized".to_owned(),
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "an object below two to the fifty-third bytes is exact"
                    )]
                    value: interrupted_bytes as f64,
                    unit: "bytes".to_owned(),
                    kind: MetricKind::Deterministic,
                },
            ]
            .into_iter()
            .chain(work_metrics(&interrupted_work))
            .collect(),
        },
    ])
}

/// What a run's `--json` result carries that a benchmark reads.
#[derive(Deserialize)]
struct RunOutcome {
    bytes: u64,
    work: Work,
}

/// Turns the three work counters into the metrics that gate on them.
///
/// Takes what a run reported. Returns one deterministic metric per counter,
/// because each is identical on identical inputs and none is a duration.
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
    ]
}

fn measure_transfer(
    binary: &Path,
    server: &TestServer,
    destination: &Path,
    cache: &Path,
) -> Result<(f64, u64, Work), BenchError> {
    let url = format!("{}/object", server.origin());
    let started = Instant::now();
    let output = Command::new(binary)
        .arg("get")
        .arg(&url)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(cache.join("fetchloom.lock"))
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .output()
        .map_err(BenchError::Process)?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        return Err(BenchError::NonZeroExit(output.status.code().unwrap_or(-1)));
    }
    let outcome: RunOutcome =
        serde_json::from_slice(&output.stdout).map_err(BenchError::MalformedResult)?;
    Ok((elapsed.as_secs_f64() * 1000.0, outcome.bytes, outcome.work))
}

fn measure_get(
    binary: &Path,
    source: &Path,
    destination: &Path,
    cache: &Path,
) -> Result<(f64, Work), BenchError> {
    let started = Instant::now();
    let output = Command::new(binary)
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .arg("--lock")
        .arg(cache.join("fetchloom.lock"))
        .arg("--json")
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(BenchError::Process)?;
    let elapsed = started.elapsed();
    if !output.status.success() {
        return Err(BenchError::NonZeroExit(output.status.code().unwrap_or(-1)));
    }
    let outcome: RunOutcome =
        serde_json::from_slice(&output.stdout).map_err(BenchError::MalformedResult)?;
    Ok((elapsed.as_secs_f64() * 1000.0, outcome.work))
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
