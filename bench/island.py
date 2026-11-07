"""Check a CPU island against quiet A/A noise and a disjoint-core compiler load."""
import argparse
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

import ab
import fixtures
import lab

SCENARIOS = ("s2", "s3", "s5")


def disjoint(cpus, build_cpus):
    if not cpus or not build_cpus or set(cpus) & set(build_cpus):
        raise ValueError("measurement and build CPU lists must be nonempty and disjoint")


def campaign_command(binary, output, cpus, rounds, seed):
    return [sys.executable, "-B", str(Path(__file__).with_name("run.py")), "--fetchloom", str(binary),
            "--only-tools", "fetchloom", "--profiles", "lan", "--scenarios", ",".join(SCENARIOS),
            "--fs", "ext4", "--rounds", str(rounds), "--seed", str(seed),
            "--variant", "aa:FETCHLOOM_EXP_NONE=1", "--cpus", ",".join(map(str, cpus)), "--out", str(output)]


def build_commands(cargo, target, jobs):
    return ([cargo, "clean", "-p", "fetchloom", "--target-dir", str(target)],
            [cargo, "build", "--locked", "--release", "--jobs", str(jobs), "--target-dir", str(target)])


def write_state(path, value):
    pending = path.with_suffix(path.suffix + ".new")
    pending.write_text(json.dumps(value) + "\n")
    pending.replace(path)


def build_load(args):
    owner = json.loads((args.target / ".fl-bench-island.json").read_text())
    if owner != dict(output=str(args.ready.parent.resolve()), worktree=str(args.worktree.resolve())):
        raise ValueError("compiler target does not belong to this island check")
    with lab.affinity(args.cpus):
        clean, build = build_commands(args.cargo, args.target, args.jobs)
        iteration = 0
        while True:
            subprocess.run(clean, cwd=args.worktree, check=True)
            with subprocess.Popen(build, cwd=args.worktree) as process:
                write_state(args.ready, dict(pid=os.getpid(), compiler_pid=process.pid,
                                             cpu_affinity=sorted(os.sched_getaffinity(0)),
                                             iteration=iteration, command=build))
                code = process.wait()
            if code:
                return code
            iteration += 1


def stop(process, signum=signal.SIGTERM):
    if process is None:
        return
    try:
        os.killpg(process.pid, signum)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGKILL)
        process.wait()
    lab.end_group(process.pid)


def campaign(args, output, load=None):
    command = campaign_command(args.fetchloom, output, args.cpus, args.rounds, args.seed)
    with (args.out / (output.name + ".log")).open("w") as log:
        process = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        try:
            while True:
                if load is not None and load.poll() is not None:
                    raise RuntimeError(f"compiler load ended during measurement; exit {load.returncode}")
                try:
                    code = process.wait(timeout=.2)
                    break
                except subprocess.TimeoutExpired:
                    pass
            if load is not None and load.poll() is not None:
                raise RuntimeError(f"compiler load ended during measurement; exit {load.returncode}")
            if code:
                raise RuntimeError(f"benchmark command failed with exit {code}; see {output.name}.log")
        finally:
            stop(process, signal.SIGINT)
    rows = [json.loads(line) for line in (output / "runs.jsonl").read_text().splitlines() if line.strip()]
    return ab.compare(rows, seed=args.seed)


def compare_noise(quiet, loaded, baseline, *, effect=.03, noise_multiple=2, rounds=20):
    reference = {f"{r['fs']}/{r['profile']}/{r['scenario']}": r for r in baseline}
    campaigns = [{r["case"]: r for r in records if r["variant"] == "fetchloom+aa"}
                 for records in (quiet, loaded)]
    results = []
    for scenario in SCENARIOS:
        key = f"ext4/lan/{scenario}"
        old = reference[key]["metrics"]["wall"]
        sigma = old["log_ratio_robust_sd"]
        if sigma is None or sigma < 0 or not math.isfinite(sigma):
            raise ValueError(f"quiet noise has no usable paired wall spread for {key}")
        limit = max(sigma * noise_multiple, math.log1p(.01))
        reasons = []
        arms = []
        for name, records in zip(("quiet", "loaded"), campaigns):
            result = records[key]
            wall = result["metrics"]["wall"]
            arms.append(wall)
            bounds = wall["bootstrap_95_median_ratio"]
            if (any(result["failed"].values()) or any(result["unmatched"].values())
                    or wall["n_log_pairs"] < rounds or wall["missing_metric_pairs"] or wall["nonpositive_pairs"]):
                reasons.append(f"{name}: failed, missing or insufficient paired observations")
            if bounds is None or not 1 - effect <= bounds[0] <= bounds[1] <= 1 + effect:
                reasons.append(f"{name}: A/A interval is outside the chosen equivalence band")
            spread = wall["log_ratio_robust_sd"]
            if spread is None or spread > limit:
                reasons.append(f"{name}: paired noise exceeds the phase-2-based limit")
        before, after = arms
        ratio = after["plain_median"] / before["plain_median"] if before["plain_median"] and after["plain_median"] else None
        if ratio is None or abs(ratio - 1) > effect:
            reasons.append("loaded versus quiet-island central wall time changed beyond the chosen band")
        historical = old["plain_median"]
        results.append(dict(case=key, passed=not reasons, reasons=reasons,
                            phase2_log_ratio_robust_sd=sigma, allowed_log_ratio_robust_sd=limit,
                            phase2_plain_median=historical, quiet_island=before, loaded_island=after,
                            loaded_over_quiet_island=ratio,
                            loaded_over_phase2=after["plain_median"] / historical if historical and after["plain_median"] else None))
    return results


def check(args):
    lab.require_root()
    disjoint(args.cpus, args.build_cpus)
    if args.rounds < 10 or args.jobs < 1 or args.jobs > len(args.build_cpus):
        raise ValueError("use at least 10 rounds and 1 to the build CPU count jobs")
    if not math.isfinite(args.effect) or not 0 < args.effect < 1:
        raise ValueError("effect must be a finite fraction between 0 and 1")
    if not math.isfinite(args.noise_multiple) or args.noise_multiple < 1:
        raise ValueError("noise-multiple must be finite and at least 1")
    args.out = args.out.resolve()
    args.target = args.target.resolve()
    args.worktree = args.worktree.resolve()
    args.fetchloom = args.fetchloom.resolve()
    if not args.out.is_relative_to(lab.ROOT.resolve()):
        raise ValueError(f"write results under {lab.ROOT}")
    if args.target.is_relative_to(args.worktree) or args.target.is_relative_to(lab.ROOT / "fs"):
        raise ValueError("the compiler target must be outside source and measured filesystems")
    if args.out.exists() or args.target.exists():
        raise ValueError("output and compiler target must be new paths")
    baseline = json.loads(args.noise.read_text())
    keys = {(r["fs"], r["profile"], r["scenario"]) for r in baseline}
    if any(("ext4", "lan", scenario) not in keys for scenario in SCENARIOS):
        raise ValueError("noise.json must contain ext4/lan s2, s3 and s5")
    args.out.mkdir(parents=True)
    args.target.mkdir(parents=True)
    write_state(args.target / ".fl-bench-island.json", dict(output=str(args.out), worktree=str(args.worktree)))
    report = dict(cpu_affinity=list(args.cpus), build_cpu_affinity=list(args.build_cpus), rounds=args.rounds,
                  seed=args.seed, effect=args.effect, noise_multiple=args.noise_multiple,
                  quiet_noise=str(args.noise.resolve()), quiet_noise_sha256=fixtures.sha256(args.noise),
                  binary=str(args.fetchloom), binary_sha256=fixtures.sha256(args.fetchloom),
                  build_worktree=str(args.worktree), build_target=str(args.target),
                  build_commands=build_commands(args.cargo, args.target, args.jobs), passed=False)
    load = None
    try:
        quiet = campaign(args, args.out / "quiet-island")
        ready = args.out / "build-ready.json"
        command = [sys.executable, "-B", str(Path(__file__).resolve()), "build-load", "--cpus",
                   ",".join(map(str, args.build_cpus)), "--worktree", str(args.worktree),
                   "--target", str(args.target), "--jobs", str(args.jobs), "--cargo", args.cargo, "--ready", str(ready)]
        with (args.out / "build.log").open("w") as log:
            load = subprocess.Popen(command, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + 60
            while not ready.exists():
                if load.poll() is not None:
                    raise RuntimeError(f"compiler load failed to start; exit {load.returncode}")
                if time.monotonic() >= deadline:
                    raise RuntimeError("compiler load did not report readiness")
                time.sleep(.05)
            report["build_start"] = json.loads(ready.read_text())
            if report["build_start"]["cpu_affinity"] != list(args.build_cpus):
                raise RuntimeError("compiler load has the wrong CPU affinity")
            loaded = campaign(args, args.out / "loaded-island", load)
            report["build_end"] = json.loads(ready.read_text())
        report["cases"] = compare_noise(quiet, loaded, baseline, effect=args.effect,
                                       noise_multiple=args.noise_multiple, rounds=args.rounds)
        report["passed"] = all(r["passed"] for r in report["cases"])
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        stop(load)
        (args.out / "island.json").write_text(json.dumps(report, indent=2, allow_nan=False) + "\n")
    print(json.dumps(report, indent=2, allow_nan=False))
    return 0 if report["passed"] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    inspect = actions.add_parser("check", help="run under the operator's exclusive measurement lock")
    inspect.add_argument("--fetchloom", required=True, type=Path)
    inspect.add_argument("--cpus", required=True, type=lab.cpu_choice)
    inspect.add_argument("--build-cpus", required=True, type=lab.cpu_choice)
    inspect.add_argument("--noise", required=True, type=Path)
    inspect.add_argument("--out", required=True, type=Path)
    inspect.add_argument("--worktree", required=True, type=Path, help="checkout used only as compiler input")
    inspect.add_argument("--target", required=True, type=Path, help="new, dedicated compiler target directory")
    inspect.add_argument("--rounds", type=int, default=20)
    inspect.add_argument("--seed", type=int, default=1729)
    inspect.add_argument("--jobs", type=int, default=3)
    inspect.add_argument("--cargo", default="cargo")
    inspect.add_argument("--effect", type=float, default=.03)
    inspect.add_argument("--noise-multiple", type=float, default=2)
    worker = actions.add_parser("build-load", help=argparse.SUPPRESS)
    worker.add_argument("--cpus", required=True, type=lab.cpu_choice)
    worker.add_argument("--worktree", required=True, type=Path)
    worker.add_argument("--target", required=True, type=Path)
    worker.add_argument("--jobs", required=True, type=int)
    worker.add_argument("--cargo", required=True)
    worker.add_argument("--ready", required=True, type=Path)
    args = parser.parse_args()
    try:
        return check(args) if args.action == "check" else build_load(args)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.error(str(error))


if __name__ == "__main__":
    raise SystemExit(main())
