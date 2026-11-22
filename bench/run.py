"""run sequential, checked linux benchmark cases."""
import argparse
from collections import Counter, defaultdict
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
import uuid

import fixtures
import lab
import tools

# measured rounds after one warmup: every arm runs ROUNDS, and in a headline case fetchloom, its
# control and the frozen rival run on to HEADLINE_ROUNDS, so each verdict rests on 10 pairs
ROUNDS = 5
HEADLINE_ROUNDS = 10
# s10's link drops this many seconds into the download, for this many seconds
LINK_DROP = (10, 10)
TIME = "/usr/bin/time"
# how often an interruption counted from a command's own anchor looks for it
POLL = .01
# a command measured in a cgroup waits here until it is in it, so everything it starts is there too
GATE = 'read -r _ <&3 && exec "$0" "$@" 3<&-'


def verify(outputs):
    """what is wrong with a run's outputs: a line per wrong or unreadable file, or one saying there
    were none to check; empty when all match."""
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
    """the value as seconds; ValueError unless it is a finite number above 0."""
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


def measure(argv, cwd, env, logfile, timeout=1200, interrupt_after=None, *, cpus=None, during=None, anchor=None,
            tree=None):
    """wall, cpu, peak memory and exit code of argv under GNU time in its own process group, SIGINT
    at the deadline then SIGKILL, and whether it timed out or was interrupted. `during`, such as a
    lab.LinkDrop, begins as the command starts and ends before this returns, whatever happened.
    `anchor`, a callable giving the time.monotonic() reading a command's own work began at, or None
    until it has, moves the interruption and `during` from the command's start to that moment.
    `tree`, a lab.Tree, holds the command from before it runs: then cpu is every process's in it and
    memory_bytes the most anonymous memory they held at once, with GNU time's figures beside them."""
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
    gate = None
    handler_set = False
    previous_handler = signal.getsignal(signal.SIGALRM)
    # an interruption counted from an anchor looks for it every POLL seconds until it is there
    waiting = interrupt_after is not None and anchor is not None
    anchored = None

    def alarm(_signal, _frame):
        nonlocal interrupted, timed_out, signalled, stage, waiting, anchored
        if waiting:
            elapsed = time.monotonic() - start
            at = anchor()
            if at is None and elapsed < timeout:
                signal.setitimer(signal.ITIMER_REAL, POLL)
                return
            waiting = False
            if at is not None:
                anchored = at - start
                fire = min(anchored + interrupt_after, timeout)
                if fire > elapsed:
                    signal.setitimer(signal.ITIMER_REAL, fire - elapsed)
                    return
        stage += 1
        if stage == 1:
            now = time.monotonic()
            try:
                os.killpg(pid, signal.SIGINT)
            except ProcessLookupError:
                return
            signalled = now
            if anchor is not None and interrupt_after is not None:
                interrupted = anchored is not None and anchored + interrupt_after < timeout
            else:
                interrupted = interrupt_after is not None and interrupt_after < timeout
            timed_out = not interrupted
            signal.setitimer(signal.ITIMER_REAL, 5)
        elif stage == 2:
            # GNU time survives so it can report the killed command's status and usage
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
            # affinity is inherited before the client can execute or create workers
            with lab.affinity(cpus):
                # posix_spawn avoids giving GNU time a forked copy of the harness's peak memory
                actions = [(os.POSIX_SPAWN_DUP2, log, 1), (os.POSIX_SPAWN_DUP2, log, 2)]
                if tree is None:
                    start = time.monotonic()
                    pid = os.posix_spawn(TIME, launcher, dict(env), setsid=True, file_actions=actions)
                else:
                    held, gate = os.pipe()
                    try:
                        pid = os.posix_spawn("/bin/sh", ["sh", "-c", GATE, *launcher], dict(env), setsid=True,
                                             file_actions=[*actions, (os.POSIX_SPAWN_DUP2, held, 3)])
                    finally:
                        os.close(held)
        finally:
            try:
                os.chdir(home)
            finally:
                os.close(log)
        if tree is not None:
            tree.adopt(pid)
            before = tree.cpu()
            start = time.monotonic()
            os.write(gate, b"\n")
            os.close(gate)
            gate = None
            tree.begin()
        if during is not None:
            during.begin(start, anchor)
        signal.signal(signal.SIGALRM, alarm)
        handler_set = True
        deadline = POLL if waiting else min(timeout, interrupt_after) if interrupt_after is not None else timeout
        signal.setitimer(signal.ITIMER_REAL, max(1e-6, deadline - (time.monotonic() - start)))
        _, status, usage = os.wait4(pid, 0)
        waited = True
        now = time.monotonic()
    except BaseException:
        if gate is not None:
            # a command still at its gate sees it close and never runs
            os.close(gate)
            gate = None
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
        try:
            if handler_set:
                try:
                    signal.setitimer(signal.ITIMER_REAL, 0)
                finally:
                    signal.signal(signal.SIGALRM, previous_handler)
        finally:
            try:
                if during is not None:
                    during.end()
            finally:
                if tree is not None:
                    tree.end()
    # a shell may exit before a child handles SIGINT; keep the next run isolated
    if signalled is not None:
        lab.end_group(pid, signalled + 5 - time.monotonic())
    result = dict(wall_seconds=now - start, user_seconds=usage.ru_utime, sys_seconds=usage.ru_stime,
                  maxrss_bytes=None, exit_code=os.waitstatus_to_exitcode(status), timeout=timed_out,
                  interruption_sent=interrupted, cpu_affinity=effective_cpus,
                  timeout_seconds=timeout, interrupt_after_seconds=interrupt_after, anchor_seconds=anchored)
    if tree is not None:
        user, system = tree.cpu()
        # what GNU time's wait saw: the command and the children it waited for, not a helper it left
        result.update(rusage=dict(user_seconds=usage.ru_utime, sys_seconds=usage.ru_stime),
                      user_seconds=user - before[0], sys_seconds=system - before[1],
                      memory_bytes=tree.peak, memory_samples=tree.samples)
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
    """the pids in a process group, read from /proc."""
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


def parse_access(lines, file_paths, rtt_ms=0):
    """what the server's json access log says a run asked for: requests, bytes served, the body bytes
    of whole and partial answers for these file paths, each host's and protocol's requests, the
    connections, and per host the most requests and connections in flight at once, with the
    request time at or under which a request is not counted in flight, 0 when every one is."""
    result = dict(requests=0, bytes_served=0, file_bytes_served=0, hosts=Counter(), protocols=Counter(),
                  connections=0, most_requests_per_connection=0)
    spans, connections = defaultdict(list), defaultdict(list)
    for line in lines:
        if not line.strip():
            continue
        row = json.loads(line)
        result["requests"] += 1
        result["bytes_served"] += row["bytes"]
        parts = row["request"].split()
        # a refusal's or a redirect's page is no part of a file
        if len(parts) >= 2 and urlsplit(parts[1]).path in file_paths and row.get("status") in (200, 206):
            result["file_bytes_served"] += row["bytes"]
        host = row.get("host", "")
        result["hosts"][host] += 1
        result["protocols"][row.get("protocol", "")] += 1
        result["most_requests_per_connection"] = max(result["most_requests_per_connection"],
                                                     row.get("connection_requests", 0))
        if "msec" in row and "request_time" in row:
            span = in_flight_span(row["msec"], row["request_time"], rtt_ms)
            spans[host].append(span)
            connections[host, row.get("connection")].append(span)
    result["connections"] = len({connection for _, connection in connections})
    joined = defaultdict(list)
    for (host, _), held in connections.items():
        joined[host].append((min(start for start, _ in held), max(end for _, end in held)))
    result["peak_in_flight"] = {host: most_at_once(held) for host, held in sorted(spans.items())}
    result["peak_connections"] = {host: most_at_once(held) for host, held in sorted(joined.items())}
    result["in_flight_floor_ms"] = SLACK_MS if rtt_ms <= SLACK_MS else 0
    result["hosts"], result["protocols"] = dict(result["hosts"]), dict(result["protocols"])
    return result


# nginx logs whole milliseconds of a clock it reads once per batch of events, so an arrival may look
# up to 2 ms early beside the end before it
SLACK_MS = 2


def in_flight_span(msec, request_time, rtt_ms):
    """when a request was open, in whole ms: from its arrival until nginx logged it, and for at least
    a round trip, since a client cannot send its next request on a slot before the last answer
    reaches it; both ends less the log's slack, so back-to-back requests never seem to overlap."""
    end = round(msec * 1000)
    start = end - round(request_time * 1000)
    return start, max(end, start + rtt_ms) - SLACK_MS


def most_at_once(spans):
    """the most spans open at one moment; one that ends as another starts does not overlap it."""
    events = sorted([(start, 1) for start, end in spans if end > start] + [(end, -1) for start, end in spans if end > start])
    most = now = 0
    for _, step in events:
        now += step
        most = max(most, now)
    return most


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
    """the commit the repository is at, asking windows git under wsl too; unavailable when neither
    answers."""
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


def native_versions(packages=("nginx", "aria2", "curl", "git-annex", "git", "iproute2",
                              "python3", "xfsprogs", "e2fsprogs", "unzip", "openssl", "time")):
    """each package with its installed debian version or not installed; a note when dpkg cannot
    answer."""
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
    """name==version for every package in the lab's python env, or a note why there is none."""
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
    """the executable's path, sha256 and size; RuntimeError if it changes while it is hashed."""
    path = Path(path).resolve(strict=True)
    before = path.stat()
    digest = fixtures.sha256(path)
    after = path.stat()
    if (before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns) != (
            after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns):
        raise RuntimeError(f"measured executable changed while recording its identity: {path}")
    return dict(path=str(path), sha256=digest, size=after.st_size)


def metadata(args, adapters):
    """the campaign record: machine, kernel, filesystems, versions of every tool and package, the
    fetchloom build and the run settings."""
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
                seed=args.seed, quick=args.quick, profiles=args.profiles, network_profiles=lab.network_profiles(),
                budget=tools.BUDGET, comparators=getattr(args, "comparators", {}),
                commands={}, orders=[], cases=[],
                native_packages=native_versions(),
                lock_sha256=hashlib.sha256(Path(__file__).with_name("uv.lock").read_bytes()).hexdigest(),
                installed_packages=python_packages())


def write_metadata(path, value):
    """replaces the file whole, syncing it and its folder, so a crash leaves the old or the new."""
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
    """adds a row as one json line and syncs it, so a crash keeps every finished run."""
    with path.open("a") as f:
        f.write(json.dumps(row) + "\n")
        f.flush()
        os.fsync(f.fileno())


def exact_command(argv, workdir):
    """argv with the scripts and lists it reads, so a row records exactly what ran."""
    result = {"argv": [str(x) for x in argv]}
    for name in ("command.sh", "retrieve.py", "registry.json", "aria2.txt", "SHA256SUMS"):
        path = workdir / name
        if path.exists():
            result[name] = path.read_text()
    return result


def one_run(tool, case, workdir, out, number, warmup, timeout, *, cpus=None, run_id=None):
    """one measured run of a tool on a case, from cold caches unless the scenario is warm; appends
    the row to runs.jsonl and returns it. its log is named by a run id of one length for every arm,
    as every path a run touches is, so no arm's names cost more lookups."""
    run_id = run_id or uuid.uuid4().hex[:6]
    row = dict(kind="lab", tool=tool.name, scenario=case["id"], profile=case["profile"],
               filesystem=workdir.parent.name, round=number, warmup=warmup, quick=case["quick"],
               count=case["count"], file_size=case["size"], status="failed", run_id=run_id, log=f"{run_id}.log")
    log = out / row["log"]
    tree = None
    try:
        tools.reset(workdir)
        cold = case["id"] not in fixtures.WARM
        tool.prepare(case, workdir)
        argv = tool.command(case, workdir)
        row["command"] = exact_command(argv, workdir)
        row["command"]["cwd"] = str(tool.cwd(workdir))
        row["environment"] = {key: value for key, value in tool.env(workdir).items()
                              if key.startswith("FETCHLOOM_") or key in (
                                  "HOME", "XDG_CACHE_HOME", "POOCH_CACHE", "TMPDIR", "REQUESTS_CA_BUNDLE",
                                  "SSL_CERT_FILE", "DVC_NO_ANALYTICS", "DVC_SITE_CACHE_DIR", "GIT_CONFIG_NOSYSTEM", "GIT_CONFIG_GLOBAL")}
        if cold and case["id"] in fixtures.CHECKED:
            # a check reads this machine's disk alone, so no server file is read back in
            lab.drop_caches(())
        elif cold:
            server_files = list(Path(case["server_dir"]).rglob("*"))
            server_files += list((lab.ROOT / "www" / "dvc" / case["key"]).rglob("*"))
            lab.drop_caches(path for path in server_files if path.is_file())
        if case["id"] == "s10":
            # each run starts on the whole profile, whatever an earlier drop left behind
            lab.profile(case["profile"])
        lab.forget_connections()
        access = lab.ROOT / "access.jsonl"
        access.write_text("")
        before = settled_free_bytes(workdir)
        row["free_before_bytes"] = before
        if case["id"] in fixtures.SHARED:
            row["push"] = push(tool, case, workdir, log, timeout, cpus, before)
            before = settled_free_bytes(workdir)
            row["free_before_get_bytes"] = before
        # one cgroup holds the run's every process, both attempts of a resume included
        tree = lab.Tree(uuid.uuid4().hex)
        if case["id"] == "s7":
            first = tool.timed(measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout, interrupt_after=10,
                                       cpus=cpus, anchor=tool.anchor(workdir), tree=tree), workdir)
            row["first_attempt"] = first
            argv = tool.command(case, workdir)
            row["resume_command"] = exact_command(argv, workdir)
            row["resume_command"]["cwd"] = str(tool.cwd(workdir))
        drop = lab.LinkDrop(case["profile"], *LINK_DROP) if case["id"] == "s10" else None
        row.update(tool.timed(measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout, cpus=cpus, during=drop,
                                      anchor=tool.anchor(workdir) if drop is not None else None, tree=tree), workdir))
        if drop is not None:
            row["link_drop"] = drop.record
        if case["id"] == "s7":
            row["user_seconds"] += first["user_seconds"]
            row["sys_seconds"] += first["sys_seconds"]
            for peak in ("maxrss_bytes", "memory_bytes"):
                peaks = (row.get(peak), first.get(peak))
                row[peak] = None if None in peaks else max(peaks)
        lab.flush(workdir)
        # written back by the flush, the run's dirty pages are still its cgroup's
        row["written_bytes"] = tree.written(os.stat(workdir).st_dev)
        row["free_after_bytes"] = free_bytes(workdir)
        row["disk_bytes"] = before - row["free_after_bytes"]
        time.sleep(0.05)
        paths = {fixtures.served(item["url"]) for item in case["files"] + case.get("previous", [])}
        if tool.name == "dvc":
            # DVC addresses file bodies by MD5, not by the input filename
            for path in (lab.ROOT / "www" / "dvc" / case["key"]).rglob("*"):
                if path.is_file() and not path.name.endswith(".dir"):
                    paths.add("/" + str(path.relative_to(lab.ROOT / "www")))
        row.update(parse_access(access.read_text().splitlines(), paths, lab.PROFILES[case["profile"]]["delay_ms"]))
        # flagged rather than failed: the budget is shown, not asserted
        row["over_budget"] = max(row["peak_in_flight"].values(), default=0) > tools.BUDGET
        row["checks"] = verify(tool.outputs(case, workdir))
        if case.get("base") in fixtures.REDIRECTED and case["id"] not in fixtures.WARM:
            unseen = [host for host in (lab.HOST, lab.STORE) if not row["hosts"].get(host)]
            if unseen:
                row["checks"].append(f"the server log shows no request to {' or '.join(unseen)}")
        if case["id"] in fixtures.REFETCHED:
            # what the application asked for again, not the wire: nginx never sees a retransmitted packet
            row["dataset_bytes"] = sum(item["size"] for item in case["files"])
            row["body_bytes_ratio"] = row["file_bytes_served"] / row["dataset_bytes"]
        if case["id"] == "s7" and not first["interruption_sent"]:
            row["checks"].append("first attempt ended before the prescribed interruption")
        if case["id"] in fixtures.SHARED:
            if row["push"]["exit_code"] != 0 or row["push"]["timeout"]:
                row["checks"].append(f"the push failed with exit {row['push']['exit_code']}")
            if row["requests"]:
                row["checks"].append(f"the push and the get asked the lab server {row['requests']} times, not the shared folder")
        if case["id"] in fixtures.CHECKED and row["requests"]:
            row["checks"].append(f"the check asked the lab server {row['requests']} times, not this machine's disk")
        if drop is not None:
            row["checks"] += dropped(drop.record, row.get("process", row)["wall_seconds"])
        if row["exit_code"] == 0 and not row["timeout"] and not row["checks"]:
            row["status"] = "passed"
    except Exception as error:
        row["error"] = f"{type(error).__name__}: {error}"
    finally:
        if tree is not None:
            tree.close()
        if (workdir / "prepare.log").exists():
            shutil.copyfile(workdir / "prepare.log", out / f"{run_id}-prepare.log")
    append(out / "runs.jsonl", row)
    return row


def push(tool, case, workdir, log, timeout, cpus, before):
    """the first teammate's timed push to the shared folder, in a cgroup of its own, with what it wrote
    and the disk it took; then, outside the clock, the second teammate's project and empty caches,
    and a cold page cache for their get."""
    argv = tool.push(case, workdir)
    command = dict(exact_command(argv, workdir), cwd=str(tool.cwd(workdir)))
    tree = lab.Tree(uuid.uuid4().hex)
    try:
        result = tool.timed(measure(argv, tool.cwd(workdir), tool.env(workdir), log, timeout, cpus=cpus, tree=tree), workdir)
        lab.flush(workdir)
        result["written_bytes"] = tree.written(os.stat(workdir).st_dev)
    finally:
        tree.close()
    result.update(disk_bytes=before - free_bytes(workdir), command=command)
    tool.receive(case, workdir)
    lab.drop_caches(())
    return result


def dropped(record, ended):
    """what is wrong with a link drop that ended `ended` seconds into its run: one that failed, or one
    the download did not outlast; empty when the run went on after the link came back."""
    if record["error"]:
        return [f"the link drop failed: {record['error']}"]
    if record["dropped_at"] is None or ended < record["dropped_at"]:
        return ["the download ended before the link dropped"]
    if record["restored_at"] is None or ended < record["restored_at"]:
        return ["the download ended before the link came back"]
    return []


def csv_choices(value, allowed):
    """distinct comma-separated values from allowed, for argparse."""
    values = value.split(",")
    if not values or any(x not in allowed for x in values) or len(set(values)) != len(values):
        raise argparse.ArgumentTypeError(f"choose distinct comma-separated values from {','.join(allowed)}")
    return values


def case_rng(seed, fs, case):
    """the source of one case's tool orders, fixed by the seed, filesystem and case."""
    return random.Random(f"{seed}/{fs}/{case['profile']}/{case['id']}")


def comparators_choice(value):
    """a json file of the comparator each headline case froze from its pilot, `render.py
    --comparators` output, as fs/profile/scenario to the name of a tool other than fetchloom."""
    try:
        chosen = json.loads(Path(value).read_text())
    except (OSError, ValueError) as error:
        raise argparse.ArgumentTypeError(f"cannot read the frozen comparators: {error}") from None
    names = {cls.name for cls in tools.CLASSES} - {tools.Fetchloom.name}
    if not isinstance(chosen, dict) or not all(
            isinstance(case, str) and isinstance(tool, str) and len(case.split("/")) == 3 and tool in names
            and case.split("/")[0] in ("ext4", "xfs") and case.split("/")[1] in lab.PROFILES
            and case.split("/")[2] in fixtures.ALL_SCENARIOS for case, tool in chosen.items()):
        raise argparse.ArgumentTypeError("frozen comparators map fs/profile/scenario to another tool's name")
    return chosen


def variant_choice(value):
    try:
        return tools.variant(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from None


def arguments():
    """the command line of a lab campaign."""
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
    parser.add_argument("--comparators", type=comparators_choice, default={},
                        help="the comparator each headline case froze from the pilot, from render.py --comparators")
    parser.add_argument("--control", action="store_true",
                        help="run fetchloom again in each headline case, as the control arm that measures this machine's noise")
    return parser


def campaign(parser, args):
    """the lab lock, the tool adapters and the cases a campaign measures; exits through the parser,
    before the server is pinned, when the campaign cannot be measured as asked."""
    if (args.rounds is not None and args.rounds < 1) or args.timeout <= 0:
        parser.error("rounds and timeout must be positive")
    if not args.out.resolve().is_relative_to(lab.ROOT):
        parser.error(f"write results under {lab.ROOT}; copy them out after measurement")
    if not (lab.ROOT / "owned.json").exists():
        parser.error("run lab.py setup first")
    variants = args.variant + ([tools.CONTROL] if args.control else [])
    if len({label for label, _ in variants}) != len(variants):
        parser.error("each variant needs its own label, and control is the control arm's")
    lock = lab.exclusive()
    args.fetchloom = args.fetchloom.resolve()
    adapters = tools.available(args.fetchloom, variants, args.only_tools)
    cases = [case for case in fixtures.case_specs(args.profiles, args.scenarios, args.quick)
             if any(tool.supports(case) for tool in adapters.values())]
    if not cases:
        parser.error("no cases match these scenario/profile combinations")
    for fs in args.fs:
        for case in cases:
            tool = args.comparators.get(f"{fs}/{case['profile']}/{case['id']}")
            if tool is not None and (tool not in adapters or not adapters[tool].supports(case)):
                parser.error(f"{tool}, the comparator frozen for {fs}/{case['profile']}/{case['id']}, is not measured in it")
    if (args.out / "runs.jsonl").exists():
        parser.error("output already contains runs; use a new directory")
    args.out.mkdir(parents=True, exist_ok=True)
    for fs in args.fs:
        if not os.path.ismount(lab.ROOT / "fs" / fs):
            parser.error(f"{fs} is not mounted")
    try:
        lab.trees()
    except RuntimeError as error:
        parser.error(str(error))
    return lock, adapters, cases


CONTROL = f"{tools.Fetchloom.name}+{tools.CONTROL[0]}"


def eligible(adapters, fs, case):
    """the adapters that measure a case: each tool that supports it, the control arm in a headline
    case alone."""
    headline = (fs, case["profile"], case["id"]) in fixtures.HEADLINE
    return {name: tool for name, tool in adapters.items() if tool.supports(case) and (headline or name != CONTROL)}


def rounds_of(args, fs, case, names):
    """each arm's measured rounds in a case, warmup aside: one in a smoke run, --rounds for every arm
    when it is given, and otherwise ROUNDS, with fetchloom, its control and the comparator frozen
    for a headline case running on to HEADLINE_ROUNDS."""
    if args.quick or args.rounds:
        return {name: 1 if args.quick else args.rounds for name in names}
    key = f"{fs}/{case['profile']}/{case['id']}"
    longer = (tools.Fetchloom.name, CONTROL, args.comparators.get(key)) if tuple(key.split("/")) in fixtures.HEADLINE else ()
    return {name: HEADLINE_ROUNDS if name in longer else ROUNDS for name in names}


def estimate(args, adapters, cases):
    """seconds of timed work plus warmups, for planning: a minute a run, 30 s an interrupted one and
    3 s a smoke run."""
    total = 0
    for fs in args.fs:
        for case in cases:
            runs = sum(rounds + 1 for rounds in rounds_of(args, fs, case, eligible(adapters, fs, case)).values())
            total += runs * (30 if case["id"] == "s7" else 3 if args.quick else 60)
    return total


def measure_case(args, fs, specification, adapters, meta):
    """builds and prepares one case outside the clock, then runs each tool that supports it once a
    round, warmup first, in a seeded shuffled order; returns how many runs failed."""
    meta_path = args.out / "meta.json"
    lab.profile("lan")
    case = fixtures.build(specification, args.seed)
    measured = eligible(adapters, fs, case)
    print(f"preparing {fs}/{case['profile']}/{case['id']}", flush=True)
    preparation_errors = {}
    for name in measured:
        try:
            case = fixtures.prepare_native(case, args.fetchloom, [measured[name].prepared_as])
        except Exception as error:
            preparation_errors[name] = f"native preparation failed: {error}"
    # the plan is on record before the first run, so a campaign cut short shows it
    arms = rounds_of(args, fs, case, measured)
    rounds = arms.get(tools.Fetchloom.name, max(arms.values(), default=0))
    meta["cases"].append(dict(filesystem=fs, **case, rounds=rounds, arm_rounds=arms))
    lab.profile(case["profile"])
    # each case draws its own orders, so a campaign split into chunks shuffles as one run
    # would, rather than every chunk replaying the same sequence
    rng = case_rng(args.seed, fs, case)
    failures = 0
    for number in range(max(arms.values(), default=0) + 1):
        order = [name for name in measured if number <= arms[name]]
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
                meta["runs"] = meta.get("runs", 0) + 1
                row = one_run(measured[name], case, workdir, args.out, number, number == 0, args.timeout, cpus=args.cpus,
                              run_id=f"{meta['runs']:06}")
            meta["commands"][f"{fs}/{case['profile']}/{case['id']}/{name}"] = row.get("command")
            failures += row["status"] != "passed"
            wall = f"{row['wall_seconds']:.3f}s" if "wall_seconds" in row else row.get("error", "no timing")
            print(f"{fs}/{case['profile']}/{case['id']} round {number}/{arms[name]} {name}: {row['status']} {wall}", flush=True)
            if workdir.exists():
                shutil.rmtree(workdir)
    return failures


def main():
    parser = arguments()
    args = parser.parse_args()
    lab.require_root()
    lock, adapters, cases = campaign(parser, args)
    server_cpus = lab.pin_server(args.cpus)
    meta = metadata(args, adapters)
    meta.update(cpu_affinity=list(args.cpus), nginx_cpu_affinity=server_cpus)
    meta["estimated_seconds"] = estimate(args, adapters, cases)
    print(f"estimated timed work plus warmups: {meta['estimated_seconds'] / 3600:.1f} hours; preparation is additional", flush=True)
    failures = sum(measure_case(args, fs, specification, adapters, meta) for fs in args.fs for specification in cases)
    write_metadata(args.out / "meta.json", meta)
    print(f"recorded {failures} failed runs (including warmups); results: {args.out}")
    lock.close()
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
