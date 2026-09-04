//! Every check and measurement that must run identically on all three
//! platforms.

mod bench;
mod network;
mod profile;
mod verify;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn main() -> ExitCode {
    let mut arguments = std::env::args().skip(1);
    let Some(task) = arguments.next() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let rest: Vec<String> = arguments.collect();
    let workspace = workspace_root();

    match task.as_str() {
        "bench" => run_bench(&workspace, &rest, verification_run()),
        "completions" => generate_completions(&workspace, &rest),
        "network" => run_network(&workspace, &rest),
        "profile" => profile::run(&rest),
        "verify" => {
            if verify::run(&workspace, &rest) {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

const USAGE: &str = "\
usage:
  cargo xtask bench [--save-baseline] [--compare] [--publish] [--iterations <n>] [--regime <name>]
  cargo xtask completions <shell> <directory>
  cargo xtask network [path to a built fetchloom]
  cargo xtask profile [--rounds <n>]
  cargo xtask verify [--fast] [--arm] [--install-hook]";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

pub(crate) fn run_bench(workspace: &Path, arguments: &[String], gate_timing: bool) -> ExitCode {
    let save = arguments
        .iter()
        .any(|argument| argument == "--save-baseline");
    let compare = arguments.iter().any(|argument| argument == "--compare");
    let publish = arguments.iter().any(|argument| argument == "--publish");
    let iterations = argument_value(arguments, "--iterations")
        .and_then(|value| value.parse().ok())
        .unwrap_or(9);
    let only = argument_value(arguments, "--regime");
    let wanted = |regime: &str| only.is_none_or(|named| named == regime);

    let binary = match bench::build_binary(workspace) {
        Ok(binary) => binary,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    let regimes = match collect_regimes(&binary, iterations, &wanted) {
        Ok(regimes) => regimes,
        Err(code) => return code,
    };
    regimes_into_baseline(
        regimes,
        workspace,
        &Asked {
            save,
            compare,
            publish,
            timing: if gate_timing {
                Timing::Recorded
            } else {
                Timing::Reported
            },
            scope: if only.is_none() {
                Scope::Whole
            } else {
                Scope::One
            },
        },
    )
}

fn collect_regimes(
    binary: &Path,
    iterations: u32,
    wanted: &dyn Fn(&str) -> bool,
) -> Result<Vec<bench::RegimeResult>, ExitCode> {
    let mut regimes = Vec::new();
    if wanted("no-op") {
        match bench::run_no_op(binary, iterations) {
            Ok(regime) => regimes.push(regime),
            Err(error) => {
                eprintln!("{error}");
                return Err(ExitCode::from(1));
            }
        }
    }
    if wanted("cold-cache") || wanted("warm-cache") {
        regimes.extend(measure_cache(binary, iterations)?);
    }
    if wanted("cold-transfer") || wanted("interrupted-transfer") {
        regimes.extend(measure_transfer(binary, iterations)?);
    }
    if wanted("many-small-files") || wanted("one-large-file") {
        regimes.extend(measure_shapes(binary, iterations)?);
    }
    if wanted("many-hosts-concurrency") || wanted("many-hosts-backoff") {
        regimes.extend(measure_hosts(binary, iterations)?);
    }
    if wanted("constrained-network") {
        match bench::run_constrained(binary, iterations.min(3)) {
            Ok(regime) => regimes.push(regime),
            Err(error) => {
                eprintln!("{error}");
                return Err(ExitCode::from(1));
            }
        }
    }
    if wanted("packed-index") {
        match bench::run_packed_index(binary, iterations.min(3)) {
            Ok(regime) => regimes.push(regime),
            Err(error) => {
                eprintln!("{error}");
                return Err(ExitCode::from(1));
            }
        }
    }
    if wanted("slow-disk") {
        match bench::run_slow_disk(iterations.min(3)) {
            Ok(regime) => regimes.push(regime),
            Err(error) => {
                eprintln!("{error}");
                return Err(ExitCode::from(1));
            }
        }
    }
    regimes.retain(|regime| wanted(&regime.regime));
    Ok(regimes)
}

fn regimes_into_baseline(
    regimes: Vec<bench::RegimeResult>,
    workspace: &Path,
    asked: &Asked,
) -> ExitCode {
    let Asked {
        save,
        compare,
        publish,
        timing,
        scope,
    } = *asked;
    let gate_timing = timing == Timing::Recorded;
    let whole = scope == Scope::Whole;
    let mut current = bench::Baseline {
        target: target_triple(),
        regimes,
    };
    for regime in &current.regimes {
        for metric in &regime.metrics {
            println!(
                "{} {} {} {}",
                regime.regime, metric.name, metric.value, metric.unit
            );
        }
    }
    let lane = match bench::scanner_lane(&std::env::temp_dir().join("fetchloom-bench-lane")) {
        Ok(lane) => lane,
        Err(error) => format!("lane unknown: {}", error.next_action()),
    };
    println!("many-small-files {lane}");
    if publish {
        let page = workspace.join("docs").join("benchmarks.md");
        if let Err(error) = std::fs::write(&page, bench::publish(&current, &lane)) {
            eprintln!("could not write {}: {error}", page.display());
            return ExitCode::from(1);
        }
        println!("published to {}", page.display());
    }

    if !whole {
        println!(
            "one regime was measured, so nothing was recorded or compared, because a baseline states every regime"
        );
        return ExitCode::SUCCESS;
    }
    let path = baseline_path(workspace, &current.target);
    record_and_gate(&mut current, &path, save, compare, gate_timing)
}

fn record_and_gate(
    current: &mut bench::Baseline,
    path: &Path,
    save: bool,
    compare: bool,
    gate_timing: bool,
) -> ExitCode {
    if save {
        if !gate_timing {
            current.regimes.iter_mut().for_each(|regime| {
                regime
                    .metrics
                    .retain(|metric| metric.kind == bench::MetricKind::Deterministic);
            });
            println!(
                "timing metrics were not recorded, because a timing baseline is only valid from the machine it was measured on, under cargo xtask verify"
            );
        }
        if let Err(error) = bench::save(current, path) {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
        println!("baseline written to {}", path.display());
    }
    if compare {
        if !path.exists() {
            if let Err(error) = bench::save(current, path) {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
            println!(
                "no baseline existed for this target, so this run was recorded as one at {}",
                path.display()
            );
            return ExitCode::SUCCESS;
        }
        let baseline = match bench::load(path) {
            Ok(baseline) => baseline,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
        };
        if let Err(error) = bench::compare(&baseline, current) {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
        println!(
            "no deterministic metric moved by more than five percent in either direction, and none the baseline carries went missing; a timing metric is recorded and reported and never gates, because no machine here is quiet enough for a wall clock to mean anything"
        );
    }
    ExitCode::SUCCESS
}

fn baseline_path(workspace: &Path, target: &str) -> PathBuf {
    workspace
        .join("xtask")
        .join("benchmarks")
        .join(format!("{target}.json"))
}

fn generate_completions(workspace: &Path, arguments: &[String]) -> ExitCode {
    let (Some(shell), Some(directory)) = (arguments.first(), arguments.get(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let binary = match bench::build_binary(workspace) {
        Ok(binary) => binary,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    let output = match Command::new(&binary).args(["completions", shell]).output() {
        Ok(output) => output,
        Err(error) => {
            eprintln!("could not run the binary: {error}");
            return ExitCode::from(1);
        }
    };
    if !output.status.success() {
        eprintln!("{}", String::from_utf8_lossy(&output.stderr));
        return ExitCode::from(1);
    }
    let directory = Path::new(directory);
    if let Err(error) = std::fs::create_dir_all(directory) {
        eprintln!("could not create {}: {error}", directory.display());
        return ExitCode::from(1);
    }
    let path = directory.join(format!("fetchloom.{shell}"));
    if let Err(error) = std::fs::write(&path, output.stdout) {
        eprintln!("could not write {}: {error}", path.display());
        return ExitCode::from(1);
    }
    println!("{}", path.display());
    ExitCode::SUCCESS
}

pub(crate) fn argument_value<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    let position = arguments.iter().position(|argument| argument == name)?;
    arguments.get(position + 1).map(String::as_str)
}

fn target_triple() -> String {
    std::env::var("XTASK_TARGET").unwrap_or_else(|_| host_triple())
}

fn host_triple() -> String {
    let output = Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_owned()))
        .arg("-vV")
        .output();
    let Ok(output) = output else {
        return "unknown".to_owned();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .unwrap_or("unknown")
        .to_owned()
}

fn verification_run() -> bool {
    std::env::var_os("FETCHLOOM_VERIFY").is_some()
}

fn measure_cache(binary: &Path, iterations: u32) -> Result<Vec<bench::RegimeResult>, ExitCode> {
    let measured = match bench::run_cache(binary, iterations.min(3)) {
        Ok(regimes) => regimes,
        Err(error) => {
            eprintln!("{error}");
            return Err(ExitCode::from(1));
        }
    };
    if let Some((cold, warm)) = bench::warm_beat_cold(&measured)
        && warm >= cold
    {
        eprintln!(
            "a warm cache took {warm} ms and a cold one took {cold} ms, so reuse saved nothing"
        );
        return Err(ExitCode::from(1));
    }
    Ok(measured)
}

fn measure_transfer(binary: &Path, iterations: u32) -> Result<Vec<bench::RegimeResult>, ExitCode> {
    match bench::run_transfer(binary, iterations.min(3)) {
        Ok(regimes) => Ok(regimes),
        Err(error) => {
            eprintln!("{error}");
            Err(ExitCode::from(1))
        }
    }
}

fn measure_hosts(binary: &Path, iterations: u32) -> Result<Vec<bench::RegimeResult>, ExitCode> {
    match bench::run_hosts(binary, iterations.min(3)) {
        Ok(regimes) => Ok(regimes),
        Err(error) => {
            eprintln!("{error}");
            Err(ExitCode::from(1))
        }
    }
}

fn measure_shapes(binary: &Path, iterations: u32) -> Result<Vec<bench::RegimeResult>, ExitCode> {
    match bench::run_shapes(binary, iterations.min(3)) {
        Ok(regimes) => Ok(regimes),
        Err(error) => {
            eprintln!("{error}");
            Err(ExitCode::from(1))
        }
    }
}

pub(crate) fn run_network(workspace: &Path, arguments: &[String]) -> ExitCode {
    let binary = arguments.first().map(PathBuf::from);
    match network::run(workspace, binary.as_deref()) {
        network::Outcome::Passed => {
            println!("network: every recorded subject matched");
            ExitCode::SUCCESS
        }
        network::Outcome::Skipped(reason) => {
            println!("network: skipped, {reason}");
            ExitCode::SUCCESS
        }
        network::Outcome::Failed(reason) => {
            println!("network: {reason}");
            ExitCode::from(1)
        }
    }
}

struct Asked {
    save: bool,
    compare: bool,
    publish: bool,
    timing: Timing,
    scope: Scope,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Timing {
    Recorded,
    Reported,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Whole,
    One,
}
