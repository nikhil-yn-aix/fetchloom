//! The verification matrix, and everything it cannot reach.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Tier {
    Unit,
    Integration,
    System,
}

impl Tier {
    fn named(name: &str) -> Option<Self> {
        match name {
            "unit" => Some(Self::Unit),
            "integration" => Some(Self::Integration),
            "system" => Some(Self::System),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Unit => "unit",
            Self::Integration => "integration",
            Self::System => "system",
        }
    }
}

struct Suite {
    package: &'static str,
    target: &'static str,
    tier: Tier,
}

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

const SUITES: [Suite; 64] = [
    Suite {
        package: "fetchloom-archive",
        target: "bomb",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "corpus",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "extract",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-archive",
        target: "formats",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "fuzz",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "passes",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "resolve",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "separator",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "streamed_zip",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-archive",
        target: "zip64",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-cache",
        target: "compaction",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-cache",
        target: "compression",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-cache",
        target: "concurrency",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cache",
        target: "storage",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-cache",
        target: "store",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-cache",
        target: "volumes",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cache",
        target: "interruption",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cache",
        target: "witness",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-engine",
        target: "candidate",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "contracts",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "credential",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "damage",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-engine",
        target: "digest_track",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-engine",
        target: "document",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "filesystem_failure",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "flights",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "laws",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "merge",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "metadata",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "network",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "partial_key",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "reconcile",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "selection",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "split",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "transfer",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-engine",
        target: "tuning",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "witness",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-engine",
        target: "work",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-faults",
        target: "archives",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-faults",
        target: "http",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-faults",
        target: "precondition",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-faults",
        target: "schedule",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-platform",
        target: "capability",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-platform",
        target: "capability_race",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-platform",
        target: "counting",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-platform",
        target: "credentials",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-platform",
        target: "filesystem",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-platform",
        target: "locking",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-platform",
        target: "publication",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "adapter_suite",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "credentials",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "ftp",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "help",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-sources",
        target: "http",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "object_store",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "provider",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "search",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-sources",
        target: "secrets",
        tier: Tier::Integration,
    },
    Suite {
        package: "fetchloom-view",
        target: "isolation",
        tier: Tier::Unit,
    },
    Suite {
        package: "fetchloom-cli",
        target: "command",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cli",
        target: "materialize",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cli",
        target: "policy",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cli",
        target: "surface",
        tier: Tier::System,
    },
    Suite {
        package: "fetchloom-cli",
        target: "transfer",
        tier: Tier::System,
    },
];

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
    declinations: Vec<String>,
}

impl Report {
    fn new() -> Self {
        Self {
            steps: Vec::new(),
            degrades: Vec::new(),
            declinations: Vec::new(),
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
    if let Some(name) = crate::argument_value(arguments, "--tier") {
        let Some(tier) = Tier::named(name) else {
            eprintln!("no tier is named {name}. The tiers are unit, integration, system");
            return false;
        };
        let mut report = Report::new();
        let built = if tier == Tier::System {
            volumes(workspace, &mut report)
        } else {
            BTreeMap::new()
        };
        tier_step(tier, workspace, &mut report, &built);
        summary(&report);
        return !report.failed();
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
        tiers_up_to(Tier::Integration, workspace, &mut report);
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
        let record = workspace
            .join("target")
            .join(format!("declined-{target}.txt"));
        let _ = std::fs::remove_file(&record);
        test.env("FETCHLOOM_TEST_DECLINED", &record);
        report.step(&format!("test {target}"), test);
        report.declinations.extend(declinations(&record));
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

fn tiers_up_to(highest: Tier, workspace: &Path, report: &mut Report) -> bool {
    let built = if highest == Tier::System {
        volumes(workspace, report)
    } else {
        BTreeMap::new()
    };
    let mut passed = true;
    for tier in [Tier::Unit, Tier::Integration, Tier::System] {
        if tier > highest {
            break;
        }
        passed &= tier_step(tier, workspace, report, &built);
    }
    passed
}

fn tier_step(
    tier: Tier,
    workspace: &Path,
    report: &mut Report,
    built: &BTreeMap<String, String>,
) -> bool {
    let mut packages: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for suite in SUITES.iter().filter(|suite| suite.tier == tier) {
        packages
            .entry(suite.package)
            .or_default()
            .push(suite.target);
    }
    let mut arguments: Vec<String> = vec!["test".to_owned()];
    for (package, targets) in &packages {
        arguments.push("-p".to_owned());
        arguments.push((*package).to_owned());
        for target in targets {
            arguments.push("--test".to_owned());
            arguments.push((*target).to_owned());
        }
    }
    let borrowed: Vec<&str> = arguments.iter().map(String::as_str).collect();
    let mut command = cargo(workspace, &borrowed);
    for (name, value) in built {
        command.env(name, value);
    }
    if !built.is_empty() {
        command.env("FETCHLOOM_VERIFY_VOLUMES", "1");
    }
    let record = workspace
        .join("target")
        .join(format!("declined-{}.txt", tier.label()));
    let _ = std::fs::remove_file(&record);
    command.env("FETCHLOOM_TEST_DECLINED", &record);
    let passed = report.step(&format!("test {}", tier.label()), command);
    report.declinations.extend(declinations(&record));
    passed
}

fn declinations(record: &Path) -> Vec<String> {
    let Ok(written) = std::fs::read_to_string(record) else {
        return Vec::new();
    };
    let mut found: Vec<String> = written
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    found.sort_unstable();
    found.dedup();
    found
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
        "{} of {} steps passed, {skipped} skipped, {} degradations, {} tests declined",
        report.steps.len() - failed - skipped,
        report.steps.len() - skipped,
        report.degrades.len(),
        report.declinations.len()
    );
    for step in report.steps.iter().filter(|step| step.skipped) {
        println!(
            "NOT VERIFIED {}: {}. This run proves nothing about what that step covers, and it is \n             not counted above.",
            step.name, step.declined
        );
    }
    for declined in &report.declinations {
        println!("{declined}, so it ran and proved nothing about what it covers.");
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

    use super::{HOOK, LANES, SUITES, declinations, install_hook, lane};

    fn targets_on_disk() -> Vec<(String, String)> {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("crates");
        let mut found = Vec::new();
        for package in std::fs::read_dir(&crates).unwrap() {
            let package = package.unwrap().path();
            let name = format!(
                "fetchloom-{}",
                package.file_name().unwrap().to_string_lossy()
            );
            let tests = package.join("tests");
            let Ok(entries) = std::fs::read_dir(&tests) else {
                continue;
            };
            for entry in entries {
                let path = entry.unwrap().path();
                let stem = path.file_stem().unwrap().to_string_lossy().into_owned();
                if path.is_dir() {
                    if path.join("main.rs").is_file() {
                        found.push((name.clone(), stem));
                    }
                    continue;
                }
                if path.extension().is_some_and(|kind| kind == "rs") {
                    found.push((name.clone(), stem));
                }
            }
        }
        found
    }

    #[test]
    fn every_test_target_this_workspace_builds_is_in_exactly_one_tier() {
        let mut named: Vec<(String, String)> = SUITES
            .iter()
            .map(|suite| (suite.package.to_owned(), suite.target.to_owned()))
            .collect();
        let before = named.len();
        named.sort();
        named.dedup();
        assert_eq!(before, named.len(), "a target is named by two tiers");

        let mut found = targets_on_disk();
        found.sort();
        assert!(
            found.len() > 40,
            "the walk found {} test targets, so it proves nothing",
            found.len()
        );
        assert_eq!(
            found, named,
            "a test target this workspace builds is in no tier, or a tier names one that is not there"
        );
    }

    fn test_sources() -> Vec<std::path::PathBuf> {
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("crates");
        let mut pending: Vec<std::path::PathBuf> = std::fs::read_dir(&crates)
            .unwrap()
            .map(|entry| entry.unwrap().path().join("tests"))
            .filter(|path| path.is_dir())
            .collect();
        let mut found = Vec::new();
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|kind| kind == "rs") {
                    found.push(path);
                }
            }
        }
        found
    }

    #[test]
    fn every_volume_a_test_asks_for_is_declined_by_name_when_the_machine_has_none() {
        let asking = ["scratch_on(", "volume_directories(", "volumes(Property::"];
        let mut checked = 0usize;
        let mut silent = Vec::new();
        for path in test_sources() {
            if path.components().any(|part| part.as_os_str() == "support") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (number, line) in lines.iter().enumerate() {
                if !asking.iter().any(|call| line.contains(call)) {
                    continue;
                }
                checked += 1;
                let first = number.saturating_sub(3);
                let last = (number + 4).min(lines.len());
                let around = lines[first..last].join("\n");
                if !around.contains("require!") && !around.contains("decline!") {
                    silent.push(format!("{}:{}", path.display(), number + 1));
                }
            }
        }
        assert!(
            checked > 10,
            "the walk found {checked} volume requests, so it proves nothing"
        );
        assert!(
            silent.is_empty(),
            "a test asks for a volume and passes in silence when the machine has none: {silent:?}"
        );
    }

    #[test]
    fn a_declination_each_test_wrote_is_reported_once_and_in_order() {
        let directory =
            std::env::temp_dir().join(format!("fetchloom-xtask-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let record = directory.join("declined.txt");
        std::fs::write(
            &record,
            "NOT VERIFIED second: needs a second volume\nNOT VERIFIED first: needs a second user\n\nNOT VERIFIED second: needs a second volume\n",
        )
        .unwrap();

        assert_eq!(
            declinations(&record),
            vec![
                "NOT VERIFIED first: needs a second user".to_owned(),
                "NOT VERIFIED second: needs a second volume".to_owned(),
            ]
        );
        std::fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_run_where_no_test_declined_reports_nothing() {
        assert!(declinations(std::path::Path::new("no-such-record.txt")).is_empty());
    }

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
    fn every_lane_runs_somewhere_in_ci() {
        let named = lanes_the_workflows_name();
        for lane in &LANES {
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
            .filter(|lane| !lane.arch.is_empty() && lane.here())
            .map(|lane| lane.name)
            .collect();
        assert!(here.len() <= 1, "{here:?} all claim this machine");
    }
}
