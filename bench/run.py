"""Run sequential, checked Linux benchmark cases."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
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

# measured rounds per profile, after one warmup. lossy links spread one tool's runs by several times,
# so they get more rounds for a median that holds
ROUNDS = {"lan": 5, "home": 5, "far": 7, "poor": 7}
TIME = "/usr/bin/time"


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


def positive_seconds(value):
    try:
        result = float(value)
    except (TypeError, ValueError, OverflowError):
        raise ValueError("deadline must be a finite positive number of seconds") from None
    if not math.isfinite(result) or result <= 0:
        raise ValueError("deadline must be a finite positive number of seconds")
    return result


def seconds_choice(value):
    try:
        return positive_seconds(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from None


def measure(argv, cwd, env, logfile, timeout=1200, interrupt_after=None, *, cpus=None):
    timeout = positive_seconds(timeout)
    if interrupt_after is not None:
        interrupt_after = positive_seconds(interrupt_after)
    if not os.access(TIME, os.X_OK):
        raise RuntimeError(f"{TIME} is missing; install GNU time")
    usage_file = Path(f"{logfile}.usage").resolve()
    usage_file.unlink(missing_ok=True)
    launcher = [TIME, "-o", str(usage_file), "-f", "%M %x", *(str(x) for x in argv)]
    home = os.getcwd()
    effective_cpus = sorted(cpus if cpus is not None else os.sched_getaffinity(0))
    interrupted = False
    timed_out = False
    signalled = None
    stage = 0
    pid = None
    waited = False
    handler_set = False
    previous_handler = signal.getsignal(signal.SIGALRM)

    def alarm(_signal, _frame):
        nonlocal interrupted, timed_out, signalled, stage
        stage += 1
        if stage == 1:
            now = time.monotonic()
            try:
                os.killpg(pid, signal.SIGINT)
            except ProcessLookupError:
                return
            signalled = now
            interrupted = interrupt_after is not None and interrupt_after < timeout
            timed_out = not interrupted
            signal.setitimer(signal.ITIMER_REAL, 5)
        elif stage == 2:
            # GNU time survives so it can report the killed command's status and usage.
            for member in group(pid):
                if member != pid:
                    try:
                        os.kill(member, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
            signal.setitimer(signal.ITIMER_REAL, 1)
        else:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass

    try:
        log = os.open(logfile, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
        try:
            os.chdir(cwd)
            # affinity is inherited before the client can execute or create workers.
            with lab.affinity(cpus):
                start = time.monotonic()
                # posix_spawn avoids giving GNU time a forked copy of the harness's peak memory.
                pid = os.posix_spawn(TIME, launcher, dict(env), setsid=True,
                                     file_actions=[(os.POSIX_SPAWN_DUP2, log, 1), (os.POSIX_SPAWN_DUP2, log, 2)])
        finally:
            try:
                os.chdir(home)
            finally:
                os.close(log)
        signal.signal(signal.SIGALRM, alarm)
        handler_set = True
        deadline = min(timeout, interrupt_after) if interrupt_after is not None else timeout
        signal.setitimer(signal.ITIMER_REAL, max(1e-6, deadline - (time.monotonic() - start)))
        _, status, usage = os.wait4(pid, 0)
        waited = True
        now = time.monotonic()
    except BaseException:
        if pid is not None:
            try:
                os.killpg(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            if not waited:
                try:
                    os.wait4(pid, 0)
                except ChildProcessError:
                    pass
            lab.end_group(pid)
        usage_file.unlink(missing_ok=True)
        raise
    finally:
        if handler_set:
            try:
                signal.setitimer(signal.ITIMER_REAL, 0)
            finally:
                signal.signal(signal.SIGALRM, previous_handler)
    # a shell may exit before a child handles SIGINT; keep the next run isolated.
    if signalled is not None:
        lab.end_group(pid, signalled + 5 - time.monotonic())
    result = dict(wall_seconds=now - start, user_seconds=usage.ru_utime, sys_seconds=usage.ru_stime,
                  maxrss_bytes=None, exit_code=os.waitstatus_to_exitcode(status), timeout=timed_out,
                  interruption_sent=interrupted, cpu_affinity=effective_cpus,
                  timeout_seconds=timeout, interrupt_after_seconds=interrupt_after)
    try:
        report = usage_file.read_text().splitlines()
        usage_file.unlink()
    except OSError:
        report = []
    if report:
        peak, code = report[-1].split()
        result["maxrss_bytes"] = int(peak) * 1024
        result["exit_code"] = int(code)
        for line in report[:-1]:
            if line.startswith("Command terminated by signal "):
                result["exit_code"] = -int(line.rsplit(" ", 1)[1])
    return result


def group(pgid):
    members = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            fields = (entry / "stat").read_text().rsplit(")", 1)[1].split()
        except (OSError, IndexError):
            continue
        if int(fields[2]) == pgid:
            members.append(int(entry.name))
    return members


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


def settled_free_bytes(path, attempts=50):
    """free space once the filesystem has finished freeing what the last run removed."""
    lab.flush(path)
    last = free_bytes(path)
    for _ in range(attempts):
        time.sleep(0.05)
        now = free_bytes(path)
        if now == last:
            return now
        last = now
    return last


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


def native_versions(packages=("nginx", "aria2", "wget", "curl", "git-annex", "git", "iproute2",
                              "python3", "xfsprogs", "e2fsprogs", "unzip", "openssl", "time")):
    try:
        result = subprocess.run(["dpkg-query", "-W", "-f=${Package}\t${db:Status-Status}\t${Version}\n",
                                 *packages], capture_output=True, text=True)
    except OSError as error:
        return [f"native package inventory unavailable: {error}"]
    if result.returncode not in (0, 1):
        return [f"native package inventory unavailable: {result.stderr.strip()}"]
    installed = {}
    for line in result.stdout.splitlines():
        name, status, version = line.split("\t", 2)
        if status == "installed":
            installed[name] = version
    return [f"{name}\t{installed.get(name, 'not installed')}" for name in packages]


def python_packages():
    try:
        return subprocess.check_output(
            [str(lab.VENV / "bin" / "python"), "-c",
             "import importlib.metadata as m\n"
             "print('\\n'.join(sorted(f'{d.name}=={d.version}' for d in m.distributions())))"],
            env=lab.environment(), text=True).splitlines()
    except FileNotFoundError:
        return ["no python env"]
    except (OSError, subprocess.CalledProcessError) as error:
        return [f"python env unavailable: {error}"]


def binary_identity(path):
    path = Path(path).resolve(strict=True)
    before = path.stat()
    digest = fixtures.sha256(path)
    after = path.stat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
            after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
        raise RuntimeError(f"measured executable changed while recording its identity: {path}")
    return dict(path=str(path), sha256=digest, size=after.st_size)


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
                repo_head=repo_head(),
                fetchloom_binary=binary_identity(args.fetchloom) if any(
                    name == "fetchloom" or name.startswith("fetchloom+") for name in adapters) else None,
                seed=args.seed, quick=args.quick, profiles=args.profiles, network_profiles=lab.PROFILES,
                commands={}, orders=[], cases=[],
                native_packages=native_versions(),
                lock_sha256=hashlib.sha256(Path(__file__).with_name("uv.lock").read_bytes()).hexdigest(),
                installed_packages=python_packages())


def write_metadata(path, value):
    pending = path.with_suffix(path.suffix + ".new")
    try:
        with pending.open("w") as output:
            json.dump(value, output, indent=2, allow_nan=False)
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        pending.replace(path)
        folder = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(folder)
        finally:
            os.close(folder)
    finally:
        pending.unlink(missing_ok=True)


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


def one_run(tool, case, workdir, out, number, warmup, timeout, *, cpus=None):
    row = dict(kind="lab", tool=tool.name, scenario=case["id"], profile=case["profile"],
               filesystem=workdir.parent.name, round=number, warmup=warmup, quick=case["quick"],
               count=case["count"], file_size=case["size"], status="failed")
    label = f"{row['filesystem']}-{case['profile']}-{case['id']}-{number}-{tool.name}"
    log = out / (label + ".log")
    try:
        tools.reset(workdir)
        cold = case["id"] not in ("s5", "s6")
        tool.prepare(case, workdir)
        argv = tool.command(case, workdir)
        row["command"] = exact_command(argv, workdir)
        row["command"]["cwd"] = str(tool.cwd(workdir))
        row["environment"] = {key: value for key, value in tool.env(workdir).items()
                              if key.startswith("FETCHLOOM_") or key in (
                                  "HOME", "XDG_CACHE_HOME", "POOCH_CACHE", "TMPDIR", "REQUESTS_CA_BUNDLE",
                                  "SSL_CERT_FILE", "DVC_NO_ANALYTICS", "DVC_SITE_CACHE_DIR", "GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_GLOBAL")}
        if cold:
            server_files = list(Path(case["server_dir"]).rglob("*"))
            server_files += list((lab.ROOT / "www" / "dvc" / case["key"]).rglob("*"))
            lab.drop_caches(path for path in server_files if path.is_file())
        lab.forget_connections()
        access = lab.ROOT / "access.jsonl"
        access.write_text("")
        before = settled_free_bytes(workdir)
        row["free_before_bytes"] = before
        if case["id"] == "s7":
            first = measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout, interrupt_after=10, cpus=cpus)
            row["first_attempt"] = first
            argv = tool.command(case, workdir)
            row["resume_command"] = exact_command(argv, workdir)
            row["resume_command"]["cwd"] = str(tool.cwd(workdir))
        row.update(measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout, cpus=cpus))
        if case["id"] == "s7":
            row["user_seconds"] += first["user_seconds"]
            row["sys_seconds"] += first["sys_seconds"]
            peaks = (row["maxrss_bytes"], first["maxrss_bytes"])
            row["maxrss_bytes"] = None if None in peaks else max(peaks)
        lab.flush(workdir)
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


def case_rng(seed, fs, case):
    return random.Random(f"{seed}/{fs}/{case['profile']}/{case['id']}")


def variant_choice(value):
    try:
        return tools.variant(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from None


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
    parser.add_argument("--variant", action="append", default=[], type=variant_choice,
                        help="label:FETCHLOOM_X=1,... runs fetchloom again with these settings, "
                             "shuffled into the same rounds; repeatable")
    parser.add_argument("--timeout", type=seconds_choice, default=1200)
    parser.add_argument("--cpus", type=lab.cpu_choice, default=os.environ.get("FL_BENCH_CPUS", ""),
                        help="CPU list inherited by measured commands and nginx; defaults to available CPUs")
    args = parser.parse_args()
    lab.require_root()
    if args.rounds is not None and args.rounds < 1 or args.timeout <= 0:
        parser.error("rounds and timeout must be positive")
    if not args.out.resolve().is_relative_to(lab.ROOT):
        parser.error(f"write results under {lab.ROOT}; copy them out after measurement")
    if not (lab.ROOT / "owned.json").exists():
        parser.error("run lab.py setup first")
    if len({label for label, _ in args.variant}) != len(args.variant):
        parser.error("each variant needs its own label")
    lock = lab.exclusive()
    args.fetchloom = args.fetchloom.resolve()
    adapters = tools.available(args.fetchloom, args.variant, args.only_tools)
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
    server_cpus = lab.pin_server(args.cpus)
    meta = metadata(args, adapters)
    meta.update(cpu_affinity=list(args.cpus), nginx_cpu_affinity=server_cpus)
    meta_path = args.out / "meta.json"
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
                    case = fixtures.prepare_native(case, args.fetchloom, [eligible[name].prepared_as])
                except Exception as error:
                    preparation_errors[name] = f"native preparation failed: {error}"
            rounds = 1 if args.quick else args.rounds or ROUNDS[case["profile"]]
            meta["cases"].append(dict(filesystem=fs, **case, rounds=rounds))
            lab.profile(case["profile"])
            # each case draws its own orders, so a campaign split into chunks shuffles as one run
            # would, rather than every chunk replaying the same sequence
            rng = case_rng(args.seed, fs, case)
            for number in range(rounds + 1):
                order = list(eligible)
                rng.shuffle(order)
                meta["orders"].append(dict(filesystem=fs, profile=case["profile"], scenario=case["id"], round=number, tools=order))
                write_metadata(meta_path, meta)
                for name in order:
                    workdir = lab.ROOT / "fs" / fs / "work"
                    if name in preparation_errors:
                        row = dict(kind="lab", filesystem=fs, profile=case["profile"], scenario=case["id"],
                                   tool=name, round=number, warmup=number == 0, quick=args.quick,
                                   count=case["count"], file_size=case["size"], status="failed",
                                   error=preparation_errors[name])
                        append(args.out / "runs.jsonl", row)
                    else:
                        row = one_run(eligible[name], case, workdir, args.out, number, number == 0, args.timeout, cpus=args.cpus)
                    meta["commands"][f"{fs}/{case['profile']}/{case['id']}/{name}"] = row.get("command")
                    failed = row["status"] != "passed"
                    failures += failed
                    wall = f"{row['wall_seconds']:.3f}s" if "wall_seconds" in row else row.get("error", "no timing")
                    print(f"{fs}/{case['profile']}/{case['id']} round {number}/{rounds} {name}: {row['status']} {wall}", flush=True)
                    if workdir.exists():
                        shutil.rmtree(workdir)
    write_metadata(meta_path, meta)
    print(f"recorded {failures} failed runs (including warmups); results: {args.out}")
    lock.close()
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
