//! The verification matrix, and everything it cannot reach.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Machine {
    Anywhere,
    Windows,
    Linux,
}

struct Lane {
    name: &'static str,
    machine: Machine,
    arch: &'static str,
    packages: &'static [&'static str],
    tools: &'static [&'static str],
    msrv: bool,
}

const VOLUME_PACKAGES: &[&str] = &[
    "bindfs",
    "btrfs-progs",
    "dosfstools",
    "e2fsprogs",
    "fuse3",
    "xfsprogs",
];

const LANES: [Lane; 8] = [
    Lane {
        name: "checks",
        machine: Machine::Anywhere,
        arch: "",
        packages: &[],
        tools: &["cargo-deny"],
        msrv: false,
    },
    Lane {
        name: "windows",
        machine: Machine::Windows,
        arch: "x86_64",
        packages: &[],
        tools: &[],
        msrv: true,
    },
    Lane {
        name: "windows-arm",
        machine: Machine::Windows,
        arch: "aarch64",
        packages: &[],
        tools: &[],
        msrv: false,
    },
    Lane {
        name: "linux",
        machine: Machine::Linux,
        arch: "x86_64",
        packages: VOLUME_PACKAGES,
        tools: &[],
        msrv: true,
    },
    Lane {
        name: "linux-arm",
        machine: Machine::Linux,
        arch: "aarch64",
        packages: VOLUME_PACKAGES,
        tools: &[],
        msrv: false,
    },
    Lane {
        name: "network",
        machine: Machine::Anywhere,
        arch: "",
        packages: &[],
        tools: &[],
        msrv: false,
    },
    Lane {
        name: "offline",
        machine: Machine::Linux,
        arch: "",
        packages: &[],
        tools: &[],
        msrv: false,
    },
    Lane {
        name: "benchmark",
        machine: Machine::Anywhere,
        arch: "",
        packages: &[],
        tools: &[],
        msrv: false,
    },
];

impl Lane {
    fn here(&self) -> bool {
        let machine = match self.machine {
            Machine::Anywhere => true,
            Machine::Windows => cfg!(windows),
            Machine::Linux => cfg!(target_os = "linux"),
        };
        machine && (self.arch.is_empty() || self.arch == std::env::consts::ARCH)
    }

    fn wants(&self) -> String {
        let machine = match self.machine {
            Machine::Anywhere => "any machine",
            Machine::Windows => "windows",
            Machine::Linux => "linux",
        };
        if self.arch.is_empty() {
            machine.to_owned()
        } else {
            format!("{machine} {}", self.arch)
        }
    }

    fn elsewhere(&self) -> String {
        format!(
            "this lane needs {}, and this machine is {} {}. The verify workflow runs it on a runner that is",
            self.wants(),
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }

    fn targets(&self) -> Vec<String> {
        let arch = if self.arch.is_empty() {
            std::env::consts::ARCH
        } else {
            self.arch
        };
        if cfg!(windows) {
            vec![format!("{arch}-pc-windows-msvc")]
        } else {
            vec![
                format!("{arch}-unknown-linux-gnu"),
                format!("{arch}-unknown-linux-musl"),
            ]
        }
    }

    fn builds(&self) -> bool {
        !matches!(self.name, "checks" | "benchmark")
    }

    fn packages(&self) -> Vec<&'static str> {
        let mut packages = self.packages.to_vec();
        if cfg!(target_os = "linux") && self.builds() {
            packages.push("musl-tools");
        }
        packages.sort_unstable();
        packages.dedup();
        packages
    }
}

fn lane(name: &str) -> Option<&'static Lane> {
    LANES.iter().find(|lane| lane.name == name)
}

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
    let named = crate::argument_value(arguments, "--lane");
    let asked = match named.map(|name| (name, lane(name))) {
        Some((name, None)) => {
            eprintln!(
                "no lane is named {name}. The lanes are {}",
                LANES
                    .iter()
                    .map(|lane| lane.name)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return false;
        }
        Some((_, found)) => found,
        None => None,
    };
    if arguments.iter().any(|argument| argument == "--provision") {
        return provision(workspace, asked);
    }

    let mut report = Report::new();
    if let Some(lane) = asked {
        if !lane.here() {
            eprintln!(
                "{} was asked for by name and {}",
                lane.name,
                lane.elsewhere()
            );
            return false;
        }
        work(lane, workspace, &mut report);
    } else if arguments.iter().any(|argument| argument == "--fast") {
        push_gate(workspace, &mut report);
    } else {
        for lane in &LANES {
            if lane.here() {
                work(lane, workspace, &mut report);
            } else {
                report.skipped(lane.name, Duration::ZERO, &lane.elsewhere());
            }
        }
        report.degrade(
            "arm64ec-pc-windows-msvc compiled",
            "nothing",
            "the cryptography provider refuses the arm64ec architecture, and the emulation ABI it exists for is for mixing with x64 code rather than for a standalone binary",
        );
    }
    summary(&report);
    !report.failed()
}

fn native() -> Option<&'static Lane> {
    LANES
        .iter()
        .find(|lane| lane.machine != Machine::Anywhere && lane.here())
}

fn push_gate(workspace: &Path, report: &mut Report) {
    let native = native();
    format(workspace, report);
    dependencies(workspace, report);
    if let Some(lane) = native {
        for target in lane.targets() {
            lint(workspace, report, &target);
            build(workspace, report, &target);
        }
    }
    for lane in &LANES {
        if lane.name == "checks" || native.is_some_and(|native| native.name == lane.name) {
            continue;
        }
        report.skipped(
            lane.name,
            Duration::ZERO,
            "the push gate runs only what is fast enough to run on every push. cargo xtask verify runs what this machine can prove, and the workflows run every lane on a runner that is native to it",
        );
    }
    if let Some(lane) = native {
        report.skipped(
            &format!("test {}", lane.targets().join(" and ")),
            Duration::ZERO,
            "the suite takes about twenty minutes here, and a gate nobody waits for is a gate that gets bypassed. cargo xtask verify runs it",
        );
        report.skipped(
            &format!("msrv {}", rust_version(workspace)),
            Duration::ZERO,
            "building the workspace a second time at its rust-version costs as much as building it once. cargo xtask verify runs it",
        );
    }
}

fn work(lane: &Lane, workspace: &Path, report: &mut Report) {
    match lane.name {
        "checks" => {
            format(workspace, report);
            dependencies(workspace, report);
        }
        "network" => network(workspace, lane, report),
        "offline" => offline(workspace, report),
        "benchmark" => {
            report.step_here("benchmark", || {
                crate::run_bench(workspace, &["--compare".to_owned()], true)
                    == std::process::ExitCode::SUCCESS
            });
        }
        _ => platform(lane, workspace, report),
    }
}

fn platform(lane: &Lane, workspace: &Path, report: &mut Report) {
    let volumes = volumes(workspace, report);
    for target in lane.targets() {
        lint(workspace, report, &target);
        build(workspace, report, &target);
        let mut test = cargo(workspace, &["test", "--workspace", "--target", &target]);
        test.env("FETCHLOOM_VERIFY", "1");
        for (name, value) in &volumes {
            test.env(name, value);
        }
        if !volumes.is_empty() {
            test.env("FETCHLOOM_VERIFY_VOLUMES", "1");
        }
        report.step(&format!("test {target}"), test);
    }
    if lane.msrv {
        msrv(workspace, report);
    }
}

fn format(workspace: &Path, report: &mut Report) {
    report.step(
        "format",
        cargo(workspace, &["fmt", "--all", "--", "--check"]),
    );
}

fn lint(workspace: &Path, report: &mut Report, target: &str) {
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

fn build(workspace: &Path, report: &mut Report, target: &str) -> bool {
    let build = cargo(
        workspace,
        &[
            "build",
            "--workspace",
            "--exclude",
            "xtask",
            "--target",
            target,
        ],
    );
    report.step(&format!("build {target}"), build)
}

fn built(workspace: &Path, target: &str) -> std::path::PathBuf {
    workspace
        .join("target")
        .join(target)
        .join("debug")
        .join(if cfg!(windows) {
            "fetchloom.exe"
        } else {
            "fetchloom"
        })
}

fn network(workspace: &Path, lane: &Lane, report: &mut Report) {
    for target in lane.targets() {
        if !build(workspace, report, &target) {
            continue;
        }
        let name = format!("network {target}");
        let started = Instant::now();
        match crate::network::run(workspace, Some(&built(workspace, &target))) {
            crate::network::Outcome::Skipped(reason) => {
                report.skipped(&name, started.elapsed(), &reason);
                report.degrade(
                    "real archives fetched from real servers",
                    "nothing",
                    &format!("no host could be reached: {reason}"),
                );
            }
            crate::network::Outcome::Passed => {
                report.already_ran(&name, true, started.elapsed());
            }
            crate::network::Outcome::Failed(reason) => {
                println!("{reason}");
                report.already_ran(&name, false, started.elapsed());
            }
        }
    }
}

fn offline(workspace: &Path, report: &mut Report) {
    let target = format!("{}-unknown-linux-musl", std::env::consts::ARCH);
    if !build(workspace, report, &target) {
        return;
    }
    let script = workspace
        .join("xtask")
        .join("verify")
        .join("offline.sh")
        .display()
        .to_string();
    let here = workspace
        .join("target")
        .join("offline")
        .display()
        .to_string();
    let binary = built(workspace, &target).display().to_string();

    let mut prepare = Command::new("bash");
    prepare
        .current_dir(workspace)
        .args([&script, &binary, &here, "prepare"]);
    if !report.step("offline prepare", prepare) {
        report.degrade(
            "a plan and a bundle prepared from a real host",
            "nothing",
            "the connected half of the offline lane did not run",
        );
        return;
    }

    let Some(mut apply) = privileged() else {
        report.skipped(
            "offline apply",
            Duration::ZERO,
            "the apply half runs in a network namespace holding no interface, which needs root, and sudo answered nothing here",
        );
        report.degrade(
            "an apply proven to reach no host",
            "nothing",
            "no network namespace could be entered on this machine",
        );
        return;
    };
    apply
        .current_dir(workspace)
        .args(["unshare", "--net", "bash", &script, &binary, &here, "apply"]);
    report.step("offline apply", apply);
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
            "cargo-deny is not installed, so nothing checked the graph against the allow list, the advisory database, the licences, or the registries. Install it with cargo xtask verify --lane checks --provision",
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
                "the {version} toolchain is not installed, so the rust-version this workspace states was not built. Install it with cargo xtask verify --lane {} --provision",
                native().map_or("checks", |lane| lane.name)
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

fn privileged() -> Option<Command> {
    if !answers(Command::new("sudo").args(["-n", "true"])) {
        return None;
    }
    let mut command = Command::new("sudo");
    command.arg("-n");
    Some(command)
}

fn volumes(workspace: &Path, report: &mut Report) -> BTreeMap<String, String> {
    let file = workspace.join("target").join("volumes.env");
    let script = |name: &str| {
        workspace
            .join("xtask")
            .join("verify")
            .join(name)
            .display()
            .to_string()
    };
    let built = if cfg!(windows) {
        Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                &script("volumes-windows.ps1"),
                &file.display().to_string(),
            ])
            .status()
            .is_ok_and(|status| status.success())
    } else {
        privileged().is_some_and(|mut command| {
            command
                .args([
                    "bash",
                    &script("volumes-linux.sh"),
                    &file.display().to_string(),
                ])
                .status()
                .is_ok_and(|status| status.success())
        })
    };
    if !built {
        let _ = std::fs::remove_file(&file);
        if cfg!(windows) {
            report.degrade(
                "ReFS, a small volume and a case-sensitive directory on this host",
                "only the volume the workspace is on",
                "the Windows volume script needs an elevated shell, and Hyper-V for the virtual disks, so block cloning and the small-volume and case-sensitive rows are unproven here",
            );
        } else {
            report.degrade(
                "btrfs, xfs, vfat, a small volume, a read-only volume, a second volume and a FUSE mount",
                "only the volume the workspace is on",
                "the Linux volume script attaches loop devices and mounts filesystems, which needs root, and sudo answered nothing here",
            );
        }
        return BTreeMap::new();
    }
    read_env(&file)
}

fn read_env(file: &Path) -> BTreeMap<String, String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return BTreeMap::new();
    };
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.trim().to_owned()))
        .collect()
}

fn provision(workspace: &Path, asked: Option<&'static Lane>) -> bool {
    let wanted: Vec<&Lane> = match asked {
        Some(lane) => vec![lane],
        None => LANES.iter().filter(|lane| lane.here()).collect(),
    };
    let mut targets: Vec<String> = Vec::new();
    let mut packages: Vec<&str> = Vec::new();
    let mut tools: Vec<&str> = Vec::new();
    let mut msrv = false;
    for lane in &wanted {
        if lane.builds() {
            targets.extend(lane.targets());
        }
        packages.extend(lane.packages());
        tools.extend(lane.tools);
        msrv |= lane.msrv;
    }
    targets.sort_unstable();
    targets.dedup();
    packages.sort_unstable();
    packages.dedup();
    tools.sort_unstable();
    tools.dedup();

    let mut ready = true;
    if !targets.is_empty() {
        println!("targets {}", targets.join(" "));
        ready &= Command::new("rustup")
            .current_dir(workspace)
            .args(["target", "add"])
            .args(&targets)
            .status()
            .is_ok_and(|status| status.success());
    }
    if msrv {
        let version = rust_version(workspace);
        println!("toolchain {version}");
        ready &= Command::new("rustup")
            .current_dir(workspace)
            .args(["toolchain", "install", &version, "--profile", "minimal"])
            .env_remove("RUSTUP_TOOLCHAIN")
            .status()
            .is_ok_and(|status| status.success());
    }
    if !packages.is_empty() {
        println!("packages {}", packages.join(" "));
        let Some(mut update) = privileged() else {
            eprintln!("system packages need root, and sudo answered nothing here");
            return false;
        };
        ready &= update
            .args(["apt-get", "update", "--quiet"])
            .status()
            .is_ok_and(|status| status.success());
        let Some(mut install) = privileged() else {
            return false;
        };
        ready &= install
            .args(["apt-get", "install", "--yes", "--no-install-recommends"])
            .args(&packages)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .status()
            .is_ok_and(|status| status.success());
    }
    for tool in tools {
        println!("tool {tool}");
        ready &= Command::new(cargo_program())
            .current_dir(workspace)
            .args(["install", "--locked", tool])
            .status()
            .is_ok_and(|status| status.success());
    }
    ready
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

    use super::{HOOK, LANES, Machine, install_hook, lane};

    fn lanes_the_workflows_name() -> Vec<String> {
        let workflows = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".github")
            .join("workflows");
        let mut named = Vec::new();
        for entry in std::fs::read_dir(&workflows).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            for line in text.lines() {
                if let Some(rest) = line.trim().strip_prefix("- lane: ") {
                    named.push(rest.trim().to_owned());
                }
            }
        }
        named
    }

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

    #[test]
    fn a_byte_order_mark_does_not_rename_the_first_volume_variable() {
        let scratch = tempfile::TempDir::new().unwrap();
        let path = scratch.path().join("volumes.env");
        std::fs::write(
            &path,
            "\u{feff}FETCHLOOM_TEST_CLONE_VOLUMES=E:\\\nFETCHLOOM_TEST_SMALL_VOLUMES=F:\\\n",
        )
        .unwrap();
        let read = super::read_env(&path);
        assert_eq!(
            read.get("FETCHLOOM_TEST_CLONE_VOLUMES").map(String::as_str),
            Some("E:\\"),
            "the mark Windows PowerShell writes at the head of a utf8 file became part of the name"
        );
    }

    #[test]
    fn every_lane_a_workflow_names_is_a_lane_this_file_defines() {
        let named = lanes_the_workflows_name();
        assert!(!named.is_empty(), "no workflow names a lane");
        for name in &named {
            assert!(lane(name).is_some(), "no lane is named {name}");
        }
    }

    #[test]
    fn every_lane_but_the_benchmark_runs_somewhere_in_ci() {
        let named = lanes_the_workflows_name();
        for lane in &LANES {
            if lane.name == "benchmark" {
                continue;
            }
            assert!(
                named.iter().any(|name| name == lane.name),
                "{} is a lane no workflow runs",
                lane.name
            );
        }
    }

    #[test]
    fn at_most_one_platform_lane_claims_this_machine() {
        let here: Vec<&str> = LANES
            .iter()
            .filter(|lane| lane.machine != Machine::Anywhere && lane.here())
            .map(|lane| lane.name)
            .collect();
        assert!(here.len() <= 1, "{here:?} all claim this machine");
    }
}
