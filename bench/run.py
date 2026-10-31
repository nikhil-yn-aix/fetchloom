"""Run sequential, checked Linux benchmark cases."""
import argparse
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import platform
import random
import shutil
import signal
import subprocess
import time
from urllib.parse import urlsplit

import fixtures
import lab
import tools

# measured rounds per profile, after one warmup. lossy links take minutes per run, so they get fewer
ROUNDS = {"lan": 5, "home": 5, "far": 3, "poor": 3}


def verify(outputs):
    failures = []
    if not outputs:
        return ["no expected outputs"]
    for path, expected in outputs:
        try:
            actual = fixtures.sha256(path)
            if actual != expected:
                failures.append(f"wrong sha256: {path}: {actual}, expected {expected}")
        except OSError as error:
            failures.append(f"{path}: {error}")
    return failures


def measure(argv, cwd, env, logfile, timeout=1200, interrupt_after=None):
    start = time.monotonic()
    pid = os.fork()
    if pid == 0:
        try:
            os.setsid()
            os.chdir(cwd)
            with open(logfile, "ab", buffering=0) as log:
                os.dup2(log.fileno(), 1)
                os.dup2(log.fileno(), 2)
                os.execvpe(str(argv[0]), [str(x) for x in argv], dict(env))
        except BaseException as error:
            os.write(2, (str(error) + "\n").encode())
            os._exit(127)
    interrupted = False
    timed_out = False
    signalled = None

    def alarm(_signal, _frame):
        nonlocal interrupted, timed_out, signalled
        if signalled is None:
            signalled = time.monotonic()
            interrupted = interrupt_after is not None and interrupt_after < timeout
            timed_out = not interrupted
            try:
                os.killpg(pid, signal.SIGINT)
            except ProcessLookupError:
                return
            signal.setitimer(signal.ITIMER_REAL, 5)
        else:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    previous_handler = signal.signal(signal.SIGALRM, alarm)
    signal.setitimer(signal.ITIMER_REAL, min(timeout, interrupt_after) if interrupt_after is not None else timeout)
    try:
        _, status, usage = os.wait4(pid, 0)
        now = time.monotonic()
    except BaseException:
        try:
            os.killpg(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        os.wait4(pid, 0)
        raise
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous_handler)
    # a shell may exit before a child handles SIGINT; keep the next run isolated.
    if signalled is not None:
        deadline = signalled + 5
        while True:
            try:
                os.killpg(pid, 0)
            except ProcessLookupError:
                break
            if time.monotonic() >= deadline:
                os.killpg(pid, signal.SIGKILL)
                break
            time.sleep(0.02)
    return dict(wall_seconds=now - start, user_seconds=usage.ru_utime,
                sys_seconds=usage.ru_stime, maxrss_bytes=usage.ru_maxrss * 1024,
                exit_code=os.waitstatus_to_exitcode(status), timeout=timed_out,
                interruption_sent=interrupted)


def parse_access(lines, file_paths):
    result = dict(requests=0, bytes_served=0, file_bytes_served=0)
    for line in lines:
        if not line.strip():
            continue
        row = json.loads(line)
        result["requests"] += 1
        result["bytes_served"] += row["bytes"]
        parts = row["request"].split()
        if len(parts) >= 2 and urlsplit(parts[1]).path in file_paths:
            result["file_bytes_served"] += row["bytes"]
    return result


def free_bytes(path):
    stat = os.statvfs(path)
    return stat.f_bavail * stat.f_frsize


def repo_head():
    repo = Path(__file__).resolve().parent.parent
    for argv in (["git", "-C", str(repo), "rev-parse", "HEAD"],
                 ["git.exe", "-C", subprocess.check_output(["wslpath", "-w", str(repo)], text=True).strip(),
                  "rev-parse", "HEAD"] if shutil.which("wslpath") else []):
        if not argv:
            continue
        try:
            return subprocess.check_output(argv, stderr=subprocess.DEVNULL, text=True).strip()
        except (OSError, subprocess.CalledProcessError):
            pass
    return "unavailable"


def metadata(args, adapters):
    cpu = next((line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines()
                if line.startswith("model name")), platform.machine())
    memory = next(line for line in Path("/proc/meminfo").read_text().splitlines() if line.startswith("MemTotal"))
    return dict(schema_version=1, kind="lab", date=datetime.now(timezone.utc).isoformat(),
                machine=dict(cpu=cpu, cores=os.cpu_count(), memory=memory, architecture=platform.machine()),
                kernel=platform.release(), distro=platform.freedesktop_os_release(),
                lab_layout=json.loads((lab.ROOT / "owned.json").read_text()),
                backing_filesystem=subprocess.check_output(["findmnt", "-n", "-T", str(lab.ROOT),
                                                            "-o", "FSTYPE,SOURCE,OPTIONS"], text=True).strip(),
                filesystems={fs: subprocess.check_output(["findmnt", "-n", "-o", "FSTYPE,OPTIONS", str(lab.ROOT / "fs" / fs)], text=True).strip()
                             for fs in args.fs},
                tool_versions={name: tool.version() for name, tool in adapters.items()},
                fetchloom_version=tools.Fetchloom(args.fetchloom).version(), repo_head=repo_head(),
                seed=args.seed, quick=args.quick, profiles=args.profiles, network_profiles=lab.PROFILES,
                commands={}, orders=[], cases=[],
                native_packages=subprocess.check_output(["dpkg-query", "-W", "nginx", "aria2", "wget", "curl",
                                                         "git-annex", "git", "iproute2", "python3", "xfsprogs",
                                                         "e2fsprogs", "unzip", "openssl"], text=True).splitlines(),
                requirements=Path(__file__).with_name("requirements.txt").read_text(),
                installed_packages=subprocess.check_output(["/opt/fl-bench/venv/bin/pip", "freeze"], text=True).splitlines())


def append(path, row):
    with path.open("a") as f:
        f.write(json.dumps(row) + "\n")
        f.flush()
        os.fsync(f.fileno())


def exact_command(argv, workdir):
    result = {"argv": [str(x) for x in argv]}
    for name in ("command.sh", "retrieve.py", "registry.json", "urls.txt", "aria2.txt", "SHA256SUMS"):
        path = workdir / name
        if path.exists():
            result[name] = path.read_text()
    return result


def one_run(tool, case, workdir, out, number, warmup, timeout):
    row = dict(kind="lab", tool=tool.name, scenario=case["id"], profile=case["profile"],
               filesystem=workdir.parent.name, round=number, warmup=warmup, quick=case["quick"],
               count=case["count"], file_size=case["size"], status="failed")
    label = f"{row['filesystem']}-{case['profile']}-{case['id']}-{number}-{tool.name}"
    log = out / (label + ".log")
    try:
        tools.reset(workdir)
        cold = case["id"] not in ("s5", "s6")
        tool.prepare(case, workdir, cold=cold)
        argv = tool.command(case, workdir)
        row["command"] = exact_command(argv, workdir)
        row["command"]["cwd"] = str(tool.cwd(workdir))
        row["environment"] = {key: value for key, value in tool.env(workdir).items()
                              if key in ("HOME", "XDG_CACHE_HOME", "FETCHLOOM_CACHE", "POOCH_CACHE",
                                         "TMPDIR", "REQUESTS_CA_BUNDLE", "SSL_CERT_FILE", "DVC_NO_ANALYTICS")}
        if cold:
            server_files = list(Path(case["server_dir"]).rglob("*"))
            server_files += list((lab.ROOT / "www" / "dvc" / case["key"]).rglob("*"))
            lab.drop_caches(path for path in server_files if path.is_file())
        access = lab.ROOT / "access.jsonl"
        access.write_text("")
        before = free_bytes(workdir)
        row["free_before_bytes"] = before
        if case["id"] == "s7":
            first = measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout, interrupt_after=10)
            row["first_attempt"] = first
            argv = tool.command(case, workdir)
            row["resume_command"] = exact_command(argv, workdir)
            row["resume_command"]["cwd"] = str(tool.cwd(workdir))
        row.update(measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout))
        if case["id"] == "s7":
            row["user_seconds"] += first["user_seconds"]
            row["sys_seconds"] += first["sys_seconds"]
            row["maxrss_bytes"] = max(row["maxrss_bytes"], first["maxrss_bytes"])
        os.sync()
        row["free_after_bytes"] = free_bytes(workdir)
        row["disk_bytes"] = before - row["free_after_bytes"]
        time.sleep(0.05)
        paths = {urlsplit(item["url"]).path for item in case["files"]}
        if tool.name == "dvc":
            # DVC addresses file bodies by MD5, not by the input filename.
            for path in (lab.ROOT / "www" / "dvc" / case["key"]).rglob("*"):
                if path.is_file() and not path.name.endswith(".dir"):
                    paths.add("/" + str(path.relative_to(lab.ROOT / "www")))
        row.update(parse_access(access.read_text().splitlines(), paths))
        row["checks"] = verify(tool.outputs(case, workdir))
        if case["id"] == "s7":
            row["download_ratio"] = row["file_bytes_served"] / sum(item["size"] for item in case["files"])
            if not first["interruption_sent"]:
                row["checks"].append("first attempt ended before the prescribed interruption")
        if row["exit_code"] == 0 and not row["timeout"] and not row["checks"]:
            row["status"] = "passed"
    except Exception as error:
        row["error"] = f"{type(error).__name__}: {error}"
    finally:
        if (workdir / "prepare.log").exists():
            shutil.copyfile(workdir / "prepare.log", out / (label + "-prepare.log"))
    append(out / "runs.jsonl", row)
    return row


def csv_choices(value, allowed):
    values = value.split(",")
    if not values or any(x not in allowed for x in values) or len(set(values)) != len(values):
        raise argparse.ArgumentTypeError(f"choose distinct comma-separated values from {','.join(allowed)}")
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetchloom", required=True, type=Path)
    parser.add_argument("--profiles", type=lambda x: csv_choices(x, lab.PROFILES), default=list(lab.PROFILES))
    parser.add_argument("--scenarios", type=lambda x: csv_choices(x, fixtures.ALL_SCENARIOS), default=list(fixtures.ALL_SCENARIOS))
    parser.add_argument("--fs", type=lambda x: csv_choices(x, ("ext4", "xfs")), default=["ext4", "xfs"])
    parser.add_argument("--rounds", type=int)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--seed", type=int, default=1729)
    parser.add_argument("--out", type=Path, default=lab.ROOT / "results" / "lab")
    parser.add_argument("--only-tools", type=lambda x: csv_choices(x, tuple(t.name for t in tools.CLASSES)))
    parser.add_argument("--timeout", type=float, default=1200)
    args = parser.parse_args()
    lab.require_root()
    if args.rounds is not None and args.rounds < 1 or args.timeout <= 0:
        parser.error("rounds and timeout must be positive")
    if not args.out.resolve().is_relative_to(lab.ROOT):
        parser.error("write results under /srv/fl-bench; copy them out after measurement")
    if not (lab.ROOT / "owned.json").exists():
        parser.error("run lab.py setup first")
    lock = lab.exclusive()
    args.fetchloom = args.fetchloom.resolve()
    adapters = tools.available(args.fetchloom)
    if args.only_tools:
        adapters = {name: adapters[name] for name in args.only_tools}
    cases = [case for case in fixtures.case_specs(args.profiles, args.scenarios, args.quick)
             if any(tool.supports(case) for tool in adapters.values())]
    if not cases:
        parser.error("no cases match these scenario/profile combinations")
    if (args.out / "runs.jsonl").exists():
        parser.error("output already contains runs; use a new directory")
    args.out.mkdir(parents=True, exist_ok=True)
    for fs in args.fs:
        if not os.path.ismount(lab.ROOT / "fs" / fs):
            parser.error(f"{fs} is not mounted")
    meta = metadata(args, adapters)
    meta_path = args.out / "meta.json"
    rng = random.Random(args.seed)
    estimate = 0
    for case in cases:
        rounds = 1 if args.quick else args.rounds or ROUNDS[case["profile"]]
        estimate += (rounds + 1) * sum(t.supports(case) for t in adapters.values()) * (30 if case["id"] == "s7" else 3 if args.quick else 60)
    estimate *= len(args.fs)
    meta["estimated_seconds"] = estimate
    print(f"estimated timed work plus warmups: {estimate / 3600:.1f} hours; preparation is additional", flush=True)
    failures = 0
    for fs in args.fs:
        for specification in cases:
            lab.profile("lan")
            case = fixtures.build(specification, args.seed)
            eligible = {name: tool for name, tool in adapters.items() if tool.supports(case)}
            print(f"preparing {fs}/{case['profile']}/{case['id']}", flush=True)
            preparation_errors = {}
            for name in eligible:
                try:
                    case = fixtures.prepare_native(case, args.fetchloom, [name])
                except Exception as error:
                    preparation_errors[name] = f"native preparation failed: {error}"
            rounds = 1 if args.quick else args.rounds or ROUNDS[case["profile"]]
            meta["cases"].append(dict(filesystem=fs, **case, rounds=rounds))
            lab.profile(case["profile"])
            for number in range(rounds + 1):
                order = list(eligible)
                rng.shuffle(order)
                meta["orders"].append(dict(filesystem=fs, profile=case["profile"], scenario=case["id"], round=number, tools=order))
                meta_path.write_text(json.dumps(meta, indent=2) + "\n")
                for name in order:
                    workdir = lab.ROOT / "fs" / fs / "work"
                    if name in preparation_errors:
                        row = dict(kind="lab", filesystem=fs, profile=case["profile"], scenario=case["id"],
                                   tool=name, round=number, warmup=number == 0, quick=args.quick,
                                   count=case["count"], file_size=case["size"], status="failed",
                                   error=preparation_errors[name])
                        append(args.out / "runs.jsonl", row)
                    else:
                        row = one_run(eligible[name], case, workdir, args.out, number, number == 0, args.timeout)
                    meta["commands"][f"{fs}/{case['profile']}/{case['id']}/{name}"] = row.get("command")
                    failed = row["status"] != "passed"
                    failures += failed
                    wall = f"{row['wall_seconds']:.3f}s" if "wall_seconds" in row else row.get("error", "no timing")
                    print(f"{fs}/{case['profile']}/{case['id']} round {number}/{rounds} {name}: {row['status']} {wall}", flush=True)
                    if workdir.exists():
                        shutil.rmtree(workdir)
    meta_path.write_text(json.dumps(meta, indent=2) + "\n")
    print(f"recorded {failures} failed runs (including warmups); results: {args.out}")
    lock.close()
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
