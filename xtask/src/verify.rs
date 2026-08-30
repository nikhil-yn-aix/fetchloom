//! The verification matrix, and everything it cannot reach.
//!
//! One command runs what used to run on six runners: the host natively, Linux
//! inside a privileged container against real loopback filesystems, the same
//! image under emulation behind a flag, and a compile for the one Apple target
//! whose toolchain this machine has. Every target it does not execute is named
//! in its own output, on every run.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// The target linted on the host with the whole workspace.
///
/// The other targets moved to the lanes that have a C compiler for them, because
/// the cryptography the client performs its handshake with is C and this machine
/// cross-compiles no C at all.
const LINT_TARGETS: [&str; 1] = ["x86_64-pc-windows-msvc"];

/// The crates the Apple compile check covers.
///
/// Everything except the two that link the client, because the cryptography it
/// performs its handshake with is C and this machine has no Apple software
/// development kit to compile C against.
const APPLE_CRATES: [&str; 4] = [
    "fetchloom-engine",
    "fetchloom-platform",
    "fetchloom-cache",
    "fetchloom-faults",
];

/// The Apple target this machine compiles and can never run.
const APPLE_TARGET: &str = "aarch64-apple-darwin";

/// The Linux targets the container lane builds and runs.
const LINUX_TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"];

/// The Linux targets the emulated lane builds and runs.
const ARM_TARGETS: [&str; 2] = ["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"];

/// What one step of the matrix did.
struct Step {
    /// What the step is called in the report.
    name: String,
    /// Whether the command succeeded.
    passed: bool,
    /// Whether the step declined to run at all, which is neither a pass nor a
    /// failure and is never counted as either.
    skipped: bool,
    /// How long the command took.
    took: Duration,
}

/// One thing the matrix wanted and did not get.
struct Degrade {
    /// What was wanted.
    requested: String,
    /// What happened instead.
    used: String,
    /// Why.
    reason: String,
}

/// Everything one run of the matrix produced.
struct Report {
    /// Every step, in the order it ran.
    steps: Vec<Step>,
    /// Every fallback, in the order it happened.
    degrades: Vec<Degrade>,
}

impl Report {
    fn new() -> Self {
        Self {
            steps: Vec::new(),
            degrades: Vec::new(),
        }
    }

    fn degrade(&mut self, requested: &str, used: &str, reason: &str) {
        println!("degrade requested={requested} used={used} reason={reason}");
        self.degrades.push(Degrade {
            requested: requested.to_owned(),
            used: used.to_owned(),
            reason: reason.to_owned(),
        });
    }

    fn step(&mut self, name: &str, mut command: Command) -> bool {
        self.step_here(name, || {
            command.status().is_ok_and(|status| status.success())
        })
    }

    fn step_here(&mut self, name: &str, work: impl FnOnce() -> bool) -> bool {
        println!("--- {name}");
        let started = Instant::now();
        let passed = work();
        let took = started.elapsed();
        println!(
            "{} {name} in {:.1} s",
            if passed { "pass" } else { "FAIL" },
            took.as_secs_f64()
        );
        self.steps.push(Step {
            name: name.to_owned(),
            passed,
            skipped: false,
            took,
        });
        passed
    }

    /// Records a step whose work already ran, with the time it actually took.
    ///
    /// `step_here` times the closure it is given, which is nothing when the
    /// work happened before the call and would report a lane that ran for
    /// twenty seconds as taking none.
    fn already_ran(&mut self, name: &str, passed: bool, took: Duration) {
        println!("--- {name}");
        println!(
            "{} {name} in {:.1} s",
            if passed { "pass" } else { "FAIL" },
            took.as_secs_f64()
        );
        self.steps.push(Step {
            name: name.to_owned(),
            passed,
            skipped: false,
            took,
        });
    }

    /// Records a step that declined to run, with the reason it declined.
    ///
    /// A skipped step is reported as skipped and counted as neither a pass nor
    /// a failure, because a lane that passes when it ran nothing is worse than
    /// no lane at all.
    fn skipped(&mut self, name: &str, took: Duration, reason: &str) {
        println!("skip {name} in {:.1} s: {reason}", took.as_secs_f64());
        self.steps.push(Step {
            name: name.to_owned(),
            passed: false,
            skipped: true,
            took,
        });
    }

    fn failed(&self) -> bool {
        self.steps.iter().any(|step| !step.passed && !step.skipped)
    }
}

/// Runs the verification matrix.
///
/// Takes the workspace root and the arguments after the task name. Returns
/// success only when every step that ran passed. Recognizes `--install-hook`,
/// which writes the pre-push hook and runs nothing, `--fast`, which runs the
/// subset the hook runs, and `--arm`, which adds the emulated lane.
pub fn run(workspace: &Path, arguments: &[String]) -> bool {
    if arguments
        .iter()
        .any(|argument| argument == "--install-hook")
    {
        return install_hook(workspace);
    }
    let fast = arguments.iter().any(|argument| argument == "--fast");
    let arm = arguments.iter().any(|argument| argument == "--arm");

    let mut report = Report::new();
    native(workspace, &mut report, fast);
    if !fast {
        container(workspace, &mut report, "amd64", &LINUX_TARGETS, "linux");
        if arm {
            container(workspace, &mut report, "arm64", &ARM_TARGETS, "linux arm");
        } else {
            report.degrade(
                "the aarch64 Linux pair built and run",
                "nothing",
                "the emulated lane runs only behind --arm, because it is slow",
            );
        }
        apple(workspace, &mut report);
    }
    summary(&report);
    !report.failed()
}

fn native(workspace: &Path, report: &mut Report, fast: bool) {
    report.step(
        "format",
        cargo(workspace, &["fmt", "--all", "--", "--check"]),
    );
    for target in LINT_TARGETS {
        let mut lint = cargo(
            workspace,
            &["clippy", "--workspace", "--all-targets", "--target", target],
        );
        if target == APPLE_TARGET {
            lint.env("CARGO_FEATURE_NO_NEON", "1");
        }
        report.step(&format!("lint {target}"), lint);
    }
    report.step(
        "build",
        cargo(workspace, &["build", "--workspace", "--exclude", "xtask"]),
    );

    let volumes = host_volumes(workspace, report);
    let mut test = cargo(workspace, &["test", "--workspace", "--exclude", "xtask"]);
    test.env("FETCHLOOM_VERIFY", "1");
    for (name, value) in &volumes {
        test.env(name, value);
    }
    if !volumes.is_empty() {
        test.env("FETCHLOOM_VERIFY_VOLUMES", "1");
    }
    report.step("test x86_64-pc-windows-msvc", test);

    report.step_here("comments", || {
        crate::check_comments(workspace) == std::process::ExitCode::SUCCESS
    });
    if !fast {
        let started = Instant::now();
        match crate::network::run(workspace, None) {
            crate::network::Outcome::Skipped(reason) => {
                report.skipped("network", started.elapsed(), &reason);
                report.degrade(
                    "real archives fetched from real servers",
                    "nothing",
                    &format!("no host could be reached: {reason}"),
                );
            }
            crate::network::Outcome::Passed => {
                report.already_ran("network", true, started.elapsed());
            }
            crate::network::Outcome::Failed(reason) => {
                println!("{reason}");
                report.already_ran("network", false, started.elapsed());
            }
        }
        report.step_here("benchmark", || {
            crate::run_bench(workspace, &["--compare".to_owned()], true)
                == std::process::ExitCode::SUCCESS
        });
    }
}

fn host_volumes(workspace: &Path, report: &mut Report) -> BTreeMap<String, String> {
    if !cfg!(windows) {
        return BTreeMap::new();
    }
    let script = workspace.join("verify").join("volumes-windows.ps1");
    let file = workspace.join("target").join("volumes.env");
    let built = Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            &script.display().to_string(),
            &file.display().to_string(),
        ])
        .status()
        .is_ok_and(|status| status.success());
    if !built {
        report.degrade(
            "ReFS, a small volume and a case-sensitive directory on this host",
            "only the volume the workspace is on",
            "the Windows volume script needs an elevated shell, and Hyper-V for the virtual disks, so block cloning and the small-volume and case-sensitive rows are unproven here",
        );
        return BTreeMap::new();
    }
    read_env(&file)
}

fn read_env(file: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn container(workspace: &Path, report: &mut Report, arch: &str, targets: &[&str], lane: &str) {
    let channel = channel(workspace);
    let image = format!("fetchloom-verify:{arch}");
    let platform = format!("linux/{arch}");
    let mut build = Command::new("docker");
    build.args([
        "build",
        "--platform",
        &platform,
        "--build-arg",
        &format!("RUST_CHANNEL={channel}"),
        "--build-arg",
        &format!("RUST_TARGETS={}", targets.join(" ")),
        "--tag",
        &image,
        &workspace.join("verify").display().to_string(),
    ]);
    if !report.step(&format!("{lane} image"), build) {
        report.degrade(
            &format!("{lane} built and run"),
            "nothing",
            "the container image did not build, so no Linux target ran",
        );
        return;
    }

    let mut run = Command::new("docker");
    run.args([
        "run",
        "--rm",
        "--privileged",
        "--platform",
        &platform,
        "--volume",
        &format!("{}:/workspace", workspace.display()),
        "--volume",
        &format!("fetchloom-target-{arch}:/target"),
        &image,
        "bash",
        "/workspace/verify/linux.sh",
    ]);
    run.args(targets);
    report.step(&format!("{lane} suite"), run);
}

fn apple(workspace: &Path, report: &mut Report) {
    let mut arguments = vec!["check", "--all-targets", "--target", APPLE_TARGET];
    for crate_name in APPLE_CRATES {
        arguments.push("--package");
        arguments.push(crate_name);
    }
    let mut check = cargo(workspace, &arguments);
    check.env("CARGO_FEATURE_NO_NEON", "1");
    report.step(&format!("compile {APPLE_TARGET}"), check);
    println!("macOS is compiled and never executed");
    report.degrade(
        "the vector implementation of the content digest compiled for Apple silicon",
        "the portable implementation, so the compile check runs at all",
        "the vector implementation is C, and this machine has no Apple software development kit for its headers",
    );
    report.degrade(
        "every crate compiled for Apple silicon",
        "every crate except the source adapter and the binary",
        "both link the cryptography the client performs its handshake with, which is C, and this machine has no Apple software development kit to compile it against",
    );
    report.degrade(
        "the Linux targets linted on the host as well as in the container",
        "the container lane only",
        "the same C cryptography cannot be cross-compiled from this machine either, so the host lints its own target and the container lints Linux",
    );
    report.degrade(
        "aarch64-pc-windows-msvc compiled and run",
        "nothing",
        "this machine is not a Windows on ARM machine",
    );
}

fn summary(report: &Report) {
    println!();
    println!("--- verify");
    for step in &report.steps {
        let verdict = if step.skipped {
            "skip"
        } else if step.passed {
            "pass"
        } else {
            "FAIL"
        };
        println!("{verdict} {} {:.1} s", step.name, step.took.as_secs_f64());
    }
    for degrade in &report.degrades {
        println!(
            "degrade requested={} used={} reason={}",
            degrade.requested, degrade.used, degrade.reason
        );
    }
    println!("macOS is compiled and never executed");
    let skipped = report.steps.iter().filter(|step| step.skipped).count();
    let failed = report
        .steps
        .iter()
        .filter(|step| !step.passed && !step.skipped)
        .count();
    println!(
        "{} of {} steps passed, {skipped} skipped, {} degradations",
        report.steps.len() - failed - skipped,
        report.steps.len() - skipped,
        report.degrades.len()
    );
}

fn cargo(workspace: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned()));
    command.current_dir(workspace);
    command.args(arguments);
    command
}

fn channel(workspace: &Path) -> String {
    let path = workspace.join("rust-toolchain.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return "stable".to_owned();
    };
    text.lines()
        .find_map(|line| line.trim().strip_prefix("channel"))
        .and_then(|rest| rest.split('"').nth(1))
        .unwrap_or("stable")
        .to_owned()
}

fn install_hook(workspace: &Path) -> bool {
    let directory = workspace.join(".git").join("hooks");
    if std::fs::create_dir_all(&directory).is_err() {
        eprintln!("could not create {}", directory.display());
        return false;
    }
    let path = directory.join("pre-push");
    let body = "#!/bin/sh\nexec cargo xtask verify --fast\n";
    if std::fs::write(&path, body).is_err() {
        eprintln!("could not write {}", path.display());
        return false;
    }
    make_executable(&path);
    println!("{}", path.display());
    true
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
}

#[cfg(windows)]
fn make_executable(path: &Path) {
    let _ = path;
}
