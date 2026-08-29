//! The regime harness: run the real binary, record numbers, gate on them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

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
    /// continuous integration against a baseline from that same runner.
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
/// which is only on continuous integration against a baseline recorded on that
/// same runner. Fails on the first gated metric that worsened by more than the
/// gate allows, and on any regime present in one run and not the other.
///
/// # Errors
///
/// Returns the regime and metric that regressed, with both numbers.
pub fn compare(
    baseline: &Baseline,
    current: &Baseline,
    gate_timing: bool,
) -> Result<(), BenchError> {
    for regime in &current.regimes {
        let recorded = baseline
            .regimes
            .iter()
            .find(|candidate| candidate.regime == regime.regime)
            .ok_or_else(|| BenchError::RegimeMissing(regime.regime.clone()))?;
        for metric in &regime.metrics {
            if metric.kind == MetricKind::Timing && !gate_timing {
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

    for round in 0..iterations {
        let scratch = scratch_directory(round)?;
        let source = scratch.join("source");
        let cache = scratch.join("cache");
        write_corpus(&source)?;

        let cold = measure_get(binary, &source, &scratch.join("cold"), &cache)?;
        let after_cold = directory_bytes(&cache.join("objects"));
        let warm = measure_get(binary, &source, &scratch.join("warm"), &cache)?;
        let after_warm = directory_bytes(&cache.join("objects"));

        cold_times.push(cold);
        warm_times.push(warm);
        cold_growth = after_cold;
        warm_growth = after_warm - after_cold;
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
            ],
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
            ],
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
        let bytes: Vec<u8> = (0..CORPUS_FILE_BYTES)
            .map(|offset| {
                let offset = u8::try_from(offset % 251).unwrap_or(0);
                offset.wrapping_mul(seed).wrapping_add(seed)
            })
            .collect();
        fs::write(into.join(format!("file-{index}.bin")), bytes).map_err(BenchError::Process)?;
    }
    Ok(())
}

fn measure_get(
    binary: &Path,
    source: &Path,
    destination: &Path,
    cache: &Path,
) -> Result<f64, BenchError> {
    let started = Instant::now();
    let status = Command::new(binary)
        .arg("get")
        .arg(source)
        .arg("--output")
        .arg(destination)
        .env("FETCHLOOM_CACHE_DIR", cache)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(BenchError::Process)?;
    let elapsed = started.elapsed();
    if !status.success() {
        return Err(BenchError::NonZeroExit(status.code().unwrap_or(-1)));
    }
    Ok(elapsed.as_secs_f64() * 1000.0)
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
