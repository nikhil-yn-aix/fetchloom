//! Every check and measurement that must run identically on all three
//! platforms.

mod bench;
mod comments;

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
        "check-comments" => check_comments(&workspace),
        "bench" => run_bench(&workspace, &rest),
        "completions" => generate_completions(&workspace, &rest),
        _ => {
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

const USAGE: &str = "\
usage:
  cargo xtask check-comments
  cargo xtask bench [--save-baseline] [--compare] [--iterations <n>]
  cargo xtask completions <shell> <directory>";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn check_comments(workspace: &Path) -> ExitCode {
    let mut findings = Vec::new();
    let mut files = Vec::new();
    collect(workspace, &mut files);
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            eprintln!("could not read {}", file.display());
            return ExitCode::from(1);
        };
        match file.extension().and_then(std::ffi::OsStr::to_str) {
            Some("rs") => findings.extend(comments::check_rust(&file, &text)),
            Some("md") => findings.extend(comments::check_markdown(&file, &text)),
            _ => {}
        }
    }
    if findings.is_empty() {
        println!("check-comments: clean");
        return ExitCode::SUCCESS;
    }
    for finding in &findings {
        println!("{finding}");
    }
    println!("check-comments: {} findings", findings.len());
    ExitCode::from(1)
}

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == "target" || name == ".git" {
            continue;
        }
        if path.is_dir() {
            collect(&path, files);
        } else {
            files.push(path);
        }
    }
}

fn run_bench(workspace: &Path, arguments: &[String]) -> ExitCode {
    let save = arguments
        .iter()
        .any(|argument| argument == "--save-baseline");
    let compare = arguments.iter().any(|argument| argument == "--compare");
    let iterations = argument_value(arguments, "--iterations")
        .and_then(|value| value.parse().ok())
        .unwrap_or(9);

    let binary = match bench::build_binary(workspace) {
        Ok(binary) => binary,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    let regime = match bench::run_no_op(&binary, iterations) {
        Ok(regime) => regime,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
    };
    let mut current = bench::Baseline {
        target: target_triple(),
        regimes: vec![regime],
    };
    for regime in &current.regimes {
        for metric in &regime.metrics {
            println!(
                "{} {} {} {}",
                regime.regime, metric.name, metric.value, metric.unit
            );
        }
    }

    let path = baseline_path(workspace, &current.target);
    if save {
        if !continuous_integration() {
            current.regimes.iter_mut().for_each(|regime| {
                regime
                    .metrics
                    .retain(|metric| metric.kind == bench::MetricKind::Deterministic);
            });
            println!(
                "timing metrics were not recorded, because a timing baseline is only valid from the runner it was measured on"
            );
        }
        if let Err(error) = bench::save(&current, &path) {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
        println!("baseline written to {}", path.display());
    }
    if compare {
        if !path.exists() {
            if let Err(error) = bench::save(&current, &path) {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
            println!(
                "no baseline existed for this target, so this run was recorded as one at {}",
                path.display()
            );
            return ExitCode::SUCCESS;
        }
        let baseline = match bench::load(&path) {
            Ok(baseline) => baseline,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
        };
        let gate_timing = continuous_integration();
        if let Err(error) = bench::compare(&baseline, &current, gate_timing) {
            eprintln!("{error}");
            return ExitCode::from(1);
        }
        if gate_timing {
            println!("no regime regressed by more than five percent");
        } else {
            println!(
                "no deterministic metric regressed by more than five percent; timing metrics gate only on continuous integration"
            );
        }
    }
    ExitCode::SUCCESS
}

fn baseline_path(workspace: &Path, target: &str) -> PathBuf {
    workspace.join("benchmarks").join(format!("{target}.json"))
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

fn argument_value<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
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

fn continuous_integration() -> bool {
    std::env::var_os("CI").is_some()
}
