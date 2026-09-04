//! The verification matrix, and everything it cannot reach.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

const LINT_TARGETS: [&str; 2] = ["x86_64-pc-windows-msvc", "x86_64-unknown-linux-musl"];

const COMPILE_ONLY_TARGETS: [&str; 1] = ["aarch64-pc-windows-msvc"];

const LINUX_TARGETS: [&str; 2] = ["x86_64-unknown-linux-musl", "x86_64-unknown-linux-gnu"];

const ARM_TARGETS: [&str; 2] = ["aarch64-unknown-linux-musl", "aarch64-unknown-linux-gnu"];

struct Step {
    name: String,
    passed: bool,
    skipped: bool,
    took: Duration,
    declined: String,
}

struct Degrade {
    requested: String,
    used: String,
    reason: String,
}

struct Report {
    steps: Vec<Step>,
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
            declined: String::new(),
        });
        passed
    }

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
            declined: String::new(),
        });
    }

    fn skipped(&mut self, name: &str, took: Duration, reason: &str) {
        println!("skip {name} in {:.1} s: {reason}", took.as_secs_f64());
        self.steps.push(Step {
            name: name.to_owned(),
            passed: false,
            skipped: true,
            took,
            declined: reason.to_owned(),
        });
    }

    fn failed(&self) -> bool {
        self.steps.iter().any(|step| !step.passed && !step.skipped)
    }
}

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
    if fast {
        for lane in ["network", "benchmark", "linux suite", "linux arm suite"] {
            report.skipped(
                lane,
                Duration::ZERO,
                "the push gate runs only what is fast enough to run on every push. cargo xtask verify runs this",
            );
        }
    } else {
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
        unreachable(&mut report);
    }
    summary(&report);
    !report.failed()
}

fn native(workspace: &Path, report: &mut Report, fast: bool) {
    report.step(
        "format",
        cargo(workspace, &["fmt", "--all", "--", "--check"]),
    );
    dependencies(workspace, report);
    for target in LINT_TARGETS {
        let lint = cargo(
            workspace,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--target",
                target,
                "--",
                "-D",
                "warnings",
            ],
        );
        report.step(&format!("lint {target}"), lint);
    }
    report.step(
        "build",
        cargo(workspace, &["build", "--workspace", "--exclude", "xtask"]),
    );
    for target in COMPILE_ONLY_TARGETS {
        let check = cargo(
            workspace,
            &["check", "--workspace", "--all-targets", "--target", target],
        );
        report.step(&format!("compile {target}"), check);
    }

    if fast {
        report.skipped(
            "test x86_64-pc-windows-msvc",
            Duration::ZERO,
            "the suite takes about ten minutes here, and a gate nobody waits for is a gate that gets bypassed. cargo xtask verify runs it",
        );
        report.skipped(
            &format!("msrv {}", rust_version(workspace)),
            Duration::ZERO,
            "building the workspace a second time at its rust-version costs as much as building it once. cargo xtask verify runs it",
        );
    } else {
        let volumes = host_volumes(workspace, report);
        let mut test = cargo(workspace, &["test", "--workspace"]);
        test.env("FETCHLOOM_VERIFY", "1");
        for (name, value) in &volumes {
            test.env(name, value);
        }
        if !volumes.is_empty() {
            test.env("FETCHLOOM_VERIFY_VOLUMES", "1");
        }
        report.step("test x86_64-pc-windows-msvc", test);

        msrv(workspace, report);
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

fn dependencies(workspace: &Path, report: &mut Report) {
    let started = Instant::now();
    if !answers(
        Command::new(cargo_program())
            .current_dir(workspace)
            .args(["deny", "--version"]),
    ) {
        report.skipped(
            "dependencies",
            started.elapsed(),
            "cargo-deny is not installed, so nothing checked the graph against the allow list, the advisory database, the licences, or the registries",
        );
        report.degrade(
            "every crate in the graph checked against the allow list, the advisories, the licences and the registries",
            "nothing",
            "cargo-deny is not installed on this machine",
        );
        return;
    }
    report.step("dependencies", cargo(workspace, &["deny", "check"]));
}

fn msrv(workspace: &Path, report: &mut Report) {
    let version = rust_version(workspace);
    let name = format!("msrv {version}");
    let started = Instant::now();
    if !answers(&mut at_version(
        workspace,
        &version,
        &["cargo", "--version"],
    )) {
        report.skipped(
            &name,
            started.elapsed(),
            &format!(
                "the {version} toolchain is not installed, so the rust-version this workspace states was not built. Install it with rustup toolchain install {version}"
            ),
        );
        report.degrade(
            "the workspace built at the rust-version it states",
            "nothing",
            &format!("the {version} toolchain is not installed on this machine"),
        );
        return;
    }
    let check = at_version(
        workspace,
        &version,
        &["cargo", "check", "--workspace", "--all-targets"],
    );
    report.step(&name, check);
}

fn at_version(workspace: &Path, version: &str, arguments: &[&str]) -> Command {
    let mut command = Command::new("rustup");
    command.current_dir(workspace);
    command.arg("run").arg(version).args(arguments);
    for inherited in ["CARGO", "RUSTC", "RUSTDOC", "RUSTUP_TOOLCHAIN"] {
        command.env_remove(inherited);
    }
    command
}

fn answers(command: &mut Command) -> bool {
    command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn rust_version(workspace: &Path) -> String {
    let path = workspace.join("Cargo.toml");
    let Ok(text) = std::fs::read_to_string(path) else {
        return "stable".to_owned();
    };
    text.lines()
        .find_map(|line| line.trim().strip_prefix("rust-version"))
        .and_then(|rest| rest.split('"').nth(1))
        .unwrap_or("stable")
        .to_owned()
}

fn cargo_program() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

fn host_volumes(workspace: &Path, report: &mut Report) -> BTreeMap<String, String> {
    if !cfg!(windows) {
        return BTreeMap::new();
    }
    let script = workspace
        .join("xtask")
        .join("verify")
        .join("volumes-windows.ps1");
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
        &workspace.join("xtask").join("verify").display().to_string(),
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
        "/workspace/xtask/verify/linux.sh",
    ]);
    run.args(targets);
    if !report.step(&format!("{lane} suite"), run) {
        return;
    }
    offline(workspace, report, arch, &image, &platform, lane);
}

fn offline(
    workspace: &Path,
    report: &mut Report,
    arch: &str,
    image: &str,
    platform: &str,
    lane: &str,
) {
    let mounts = [
        format!("{}:/workspace", workspace.display()),
        format!("fetchloom-target-{arch}:/target"),
        format!("fetchloom-offline-{arch}:/offline"),
    ];
    let mut prepare = Command::new("docker");
    prepare.args(["run", "--rm", "--privileged", "--platform", platform]);
    for mount in &mounts {
        prepare.args(["--volume", mount]);
    }
    prepare.args([
        image,
        "bash",
        "/workspace/xtask/verify/offline.sh",
        "prepare",
    ]);
    if !report.step(&format!("{lane} offline prepare"), prepare) {
        report.degrade(
            "a plan and a bundle prepared from a real host",
            "nothing",
            "the connected half of the offline lane did not run",
        );
        return;
    }

    let mut apply = Command::new("docker");
    apply.args([
        "run",
        "--rm",
        "--privileged",
        "--network",
        "none",
        "--platform",
        platform,
    ]);
    for mount in &mounts {
        apply.args(["--volume", mount]);
    }
    apply.args([image, "bash", "/workspace/xtask/verify/offline.sh", "apply"]);
    report.step(&format!("{lane} offline apply"), apply);
}

fn unreachable(report: &mut Report) {
    report.degrade(
        "aarch64-pc-windows-msvc compiled and run",
        "compiled only",
        "this machine is not a Windows on ARM machine, and compiling is not running",
    );
    report.degrade(
        "arm64ec-pc-windows-msvc compiled",
        "nothing",
        "the cryptography provider refuses the arm64ec architecture, and the emulation ABI it exists for is for mixing with x64 code rather than for a standalone binary",
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
    for step in report.steps.iter().filter(|step| step.skipped) {
        println!(
            "NOT VERIFIED {}: {}. This run proves nothing about what that step covers, and it is \n             not counted above.",
            step.name, step.declined
        );
    }
}

fn cargo(workspace: &Path, arguments: &[&str]) -> Command {
    let mut command = Command::new(cargo_program());
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

const HOOK: &str = "#!/bin/sh\nexec cargo xtask verify --fast\n";

fn install_hook(workspace: &Path) -> bool {
    let directory = workspace.join(".git").join("hooks");
    if std::fs::create_dir_all(&directory).is_err() {
        eprintln!("could not create {}", directory.display());
        return false;
    }
    let path = directory.join("pre-push");
    match std::fs::read_to_string(&path) {
        Ok(found) if found == HOOK => {
            println!("{} is already this hook", path.display());
            return true;
        }
        Ok(_) => {
            eprintln!(
                "{} exists and is not the hook this installs, so nothing was written. Read it, then either delete it and run this again, or add the line `exec cargo xtask verify --fast` to it yourself.",
                path.display()
            );
            return false;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            eprintln!("could not read {}: {error}", path.display());
            return false;
        }
    }
    if std::fs::write(&path, HOOK).is_err() {
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

#[cfg(test)]
mod tests {
    #![expect(
        clippy::unwrap_used,
        reason = "test setup, where a failure to build the input is the assertion"
    )]

    use super::{HOOK, install_hook};

    #[test]
    fn installing_into_a_repository_with_no_hook_writes_the_hook() {
        let scratch = tempfile::TempDir::new().unwrap();
        assert!(install_hook(scratch.path()));
        let written = std::fs::read_to_string(scratch.path().join(".git/hooks/pre-push")).unwrap();
        assert_eq!(written, HOOK);
    }

    #[test]
    fn installing_twice_leaves_one_hook_and_reports_success() {
        let scratch = tempfile::TempDir::new().unwrap();
        assert!(install_hook(scratch.path()));
        assert!(install_hook(scratch.path()));
        let written = std::fs::read_to_string(scratch.path().join(".git/hooks/pre-push")).unwrap();
        assert_eq!(written, HOOK);
    }

    #[test]
    fn installing_over_somebody_elses_hook_refuses_and_keeps_theirs() {
        let scratch = tempfile::TempDir::new().unwrap();
        let path = scratch.path().join(".git/hooks/pre-push");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let theirs = "#!/bin/sh\nexec ./their-own-gate\n";
        std::fs::write(&path, theirs).unwrap();

        assert!(!install_hook(scratch.path()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), theirs);
    }
}
