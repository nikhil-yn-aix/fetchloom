# /// script
# requires-python = ">=3.12"
# dependencies = ["psutil>=6.0"]
# ///
"""runs one benchmark plan and writes a row per run to runs.csv, then summary.md.

usage: uv run bench/harness.py bench/plans/<plan>.toml [--reps N] [--only tool,tool] [--results DIR]

the method is docs/dev/benchmarks.md. every first fetch gets a new, empty home for the tool's
caches and a new output folder; the second project reuses that home with another new folder.
each block runs every tool once in a random order from a recorded seed. a run counts only
when its files hash to what the workload expects; failures stay in the results.
"""

import argparse
import csv
import hashlib
import os
import platform
import random
import re
import shutil
import stat
import statistics
import subprocess
import sys
import tempfile
import time
import tomllib
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime
from fnmatch import fnmatch
from pathlib import Path

import psutil

BENCH = Path(__file__).resolve().parent
LEDGER = Path(os.environ.get("BENCH_LEDGER", BENCH / "results" / "ledger.csv"))
BUDGET = 25 * 10**9
SAMPLE_SECONDS = 0.05
COLUMNS = [
    "when", "plan", "layer", "profile", "workload", "tool", "phase", "rep", "position", "run",
    "wall_s", "cpu_s", "peak_mb", "net_mb", "files", "ok", "exit", "disk_mb", "note",
]


def remove_tree(path):
    """removes a folder holding files a tool sealed read-only, which windows will not delete."""
    def unseal(func, target, _):
        os.chmod(target, stat.S_IWRITE)
        func(target)

    if os.path.lexists(path):
        shutil.rmtree(path, onexc=unseal)


def fill(value, names):
    """expands {name} and {env:NAME} in strings, lists and tables, including nested names."""
    if isinstance(value, str):
        for _ in range(5):
            replaced = value
            for key, sub in names.items():
                replaced = replaced.replace("{" + key + "}", str(sub))
            for key in re.findall(r"\{env:([A-Za-z_][A-Za-z0-9_]*)\}", replaced):
                if key not in os.environ:
                    sys.exit(f"environment variable {key} is not set")
                replaced = replaced.replace("{env:" + key + "}", os.environ[key])
            if replaced == value:
                return value
            value = replaced
        return value
    if isinstance(value, list):
        return [fill(item, names) for item in value]
    if isinstance(value, dict):
        return {key: fill(item, names) for key, item in value.items()}
    return value


if os.name == "nt":
    import ctypes
    from ctypes import wintypes

    _kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    _kernel32.CreateJobObjectW.restype = wintypes.HANDLE
    _kernel32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    _kernel32.OpenProcess.restype = wintypes.HANDLE
    _kernel32.OpenProcess.argtypes = [wintypes.DWORD, wintypes.BOOL, wintypes.DWORD]
    _kernel32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    _kernel32.QueryInformationJobObject.argtypes = [
        wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD, ctypes.c_void_p,
    ]
    _kernel32.CloseHandle.argtypes = [wintypes.HANDLE]

    class _BasicAccounting(ctypes.Structure):
        _fields_ = [
            ("TotalUserTime", ctypes.c_longlong),
            ("TotalKernelTime", ctypes.c_longlong),
            ("ThisPeriodTotalUserTime", ctypes.c_longlong),
            ("ThisPeriodTotalKernelTime", ctypes.c_longlong),
            ("TotalPageFaultCount", wintypes.DWORD),
            ("TotalProcesses", wintypes.DWORD),
            ("ActiveProcesses", wintypes.DWORD),
            ("TotalTerminatedProcesses", wintypes.DWORD),
        ]

    class _BasicLimits(ctypes.Structure):
        _fields_ = [
            ("PerProcessUserTimeLimit", ctypes.c_longlong),
            ("PerJobUserTimeLimit", ctypes.c_longlong),
            ("LimitFlags", wintypes.DWORD),
            ("MinimumWorkingSetSize", ctypes.c_size_t),
            ("MaximumWorkingSetSize", ctypes.c_size_t),
            ("ActiveProcessLimit", wintypes.DWORD),
            ("Affinity", ctypes.c_size_t),
            ("PriorityClass", wintypes.DWORD),
            ("SchedulingClass", wintypes.DWORD),
        ]

    class _ExtendedLimits(ctypes.Structure):
        _fields_ = [
            ("BasicLimitInformation", _BasicLimits),
            ("IoInfo", ctypes.c_ulonglong * 6),
            ("ProcessMemoryLimit", ctypes.c_size_t),
            ("JobMemoryLimit", ctypes.c_size_t),
            ("PeakProcessMemoryUsed", ctypes.c_size_t),
            ("PeakJobMemoryUsed", ctypes.c_size_t),
        ]

    def _query(job, kind, info):
        if not _kernel32.QueryInformationJobObject(job, kind, ctypes.byref(info), ctypes.sizeof(info), None):
            raise OSError(ctypes.get_last_error(), "could not read the job's accounting")
        return info


class Accounting:
    """cpu time and peak memory of a process and everything it starts, from the operating
    system's own records, which include processes that already exited."""

    def __init__(self, process):
        if os.name == "nt":
            self.job = _kernel32.CreateJobObjectW(None, None)
            # PROCESS_SET_QUOTA | PROCESS_TERMINATE, what joining a job needs
            handle = _kernel32.OpenProcess(0x0100 | 0x0001, False, process.pid)
            if not self.job or not handle or not _kernel32.AssignProcessToJobObject(self.job, handle):
                raise OSError(ctypes.get_last_error(), "could not put the run in a job object")
            _kernel32.CloseHandle(handle)
        else:
            import resource

            self.before = resource.getrusage(resource.RUSAGE_CHILDREN)

    def cpu_seconds(self):
        if os.name == "nt":
            info = _query(self.job, 1, _BasicAccounting())
            # 100 ns units
            return (info.TotalUserTime + info.TotalKernelTime) / 1e7
        import resource

        after = resource.getrusage(resource.RUSAGE_CHILDREN)
        return (after.ru_utime - self.before.ru_utime) + (after.ru_stime - self.before.ru_stime)

    def peak_bytes(self, sampled):
        """windows: the job's peak committed memory. linux: the largest sampled tree total."""
        if os.name == "nt":
            return _query(self.job, 9, _ExtendedLimits()).PeakJobMemoryUsed
        return sampled


def run(argv, env, cwd, timeout, stderr_path):
    """runs argv and returns wall seconds, cpu seconds, peak MB, MB the machine received, the
    exit code and whether it timed out."""
    received = psutil.net_io_counters().bytes_recv
    started = time.perf_counter()
    with open(stderr_path, "wb") as stderr:
        process = psutil.Popen(argv, env=env, cwd=cwd, stdout=subprocess.DEVNULL, stderr=stderr)
        accounting = Accounting(process)
        sampled = 0
        timed_out = False
        while True:
            # waiting rather than sleeping ends the clock at the exit, not at the next sample
            try:
                process.wait(timeout=SAMPLE_SECONDS)
                break
            except psutil.TimeoutExpired:
                pass
            try:
                tree = [process, *process.children(recursive=True)]
            except psutil.Error:
                tree = [process]
            resident = 0
            for member in tree:
                try:
                    resident += member.memory_info().rss
                except psutil.Error:
                    pass
            sampled = max(sampled, resident)
            if time.perf_counter() - started > timeout:
                for member in reversed(tree):
                    try:
                        member.kill()
                    except psutil.Error:
                        pass
                timed_out = True
                process.wait()
                break
        wall = time.perf_counter() - started
        code = process.returncode
    cpu = accounting.cpu_seconds()
    peak = accounting.peak_bytes(sampled) / 1e6
    net = (psutil.net_io_counters().bytes_recv - received) / 1e6
    return wall, cpu, peak, net, code, timed_out


def expected(manifest):
    """the multiset of sha256 a correct run must leave, from lines of `<sha256>  <path>`."""
    hashes = Counter()
    for line in Path(manifest).read_text(encoding="utf-8-sig").splitlines():
        if line.strip():
            hashes[line.split()[0].lower()] += 1
    return hashes


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        while chunk := file.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def produced(folders, ignore):
    """the sha256 of every file under the folders, except paths matching an ignore pattern. the
    files are read on several threads: on windows the antivirus scans each new file when it is
    first opened, and one thread would wait for every scan in turn."""
    paths = []
    for folder in folders:
        for path in Path(folder).rglob("*"):
            relative = path.relative_to(folder).as_posix()
            if path.is_file() and not path.is_symlink() and not any(fnmatch(relative, p) for p in ignore):
                paths.append(path)
    with ThreadPoolExecutor(max_workers=8) as pool:
        hashes = Counter(pool.map(sha256_of, paths))
    return hashes, len(paths)


def disk_bytes(folders):
    """bytes of distinct files under the folders, so a hardlinked file counts once."""
    seen = set()
    total = 0
    for folder in folders:
        for path in Path(folder).rglob("*"):
            if path.is_file() and not path.is_symlink():
                info = path.stat()
                if (info.st_dev, info.st_ino) not in seen:
                    seen.add((info.st_dev, info.st_ino))
                    total += info.st_size
    return total


def ledger_total():
    if not LEDGER.exists():
        return 0
    with open(LEDGER, newline="", encoding="utf-8") as file:
        return sum(int(row["bytes"]) for row in csv.DictReader(file))


def ledger_add(plan, workload, tool, phase, rep, received):
    new = not LEDGER.exists()
    LEDGER.parent.mkdir(parents=True, exist_ok=True)
    with open(LEDGER, "a", newline="", encoding="utf-8") as file:
        writer = csv.writer(file)
        if new:
            writer.writerow(["when", "plan", "workload", "tool", "phase", "rep", "bytes"])
        when = datetime.now(UTC).isoformat(timespec="seconds")
        writer.writerow([when, plan, workload, tool, phase, rep, received])


def versions(tools, names):
    found = {}
    for name, tool in tools.items():
        command = fill(tool.get("version"), names)
        if not command:
            continue
        try:
            output = subprocess.run(command, capture_output=True, text=True, timeout=60)
            text = (output.stdout or output.stderr).strip().splitlines()
            found[name] = text[0] if text else "?"
        except (OSError, subprocess.TimeoutExpired) as err:
            found[name] = f"unavailable: {err}"
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("plan")
    parser.add_argument("--reps", type=int)
    parser.add_argument("--only", help="comma-separated tools to run")
    parser.add_argument("--results", type=Path, default=BENCH / "results",
                        help="where the block's folder goes; git ignores bench/results")
    args = parser.parse_args()
    plan = fill(tomllib.loads(Path(args.plan).read_text(encoding="utf-8-sig")), {})
    plan_names = {"repo": BENCH.parent.as_posix(), **plan.get("vars", {})}
    reps = args.reps or plan.get("repetitions", 3)
    only = set(args.only.split(",")) if args.only else None
    stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    out_dir = args.results / f"{stamp}-{plan['name']}"
    out_dir.mkdir(parents=True)
    seed = int(time.time())
    order_source = random.Random(seed)
    scratch = Path(tempfile.mkdtemp(prefix=f"bench-{plan['name']}-"))
    environment = {
        "plan": plan["name"], "layer": plan["layer"], "profile": plan.get("profile", ""),
        "started": stamp, "seed": seed, "os": platform.platform(), "cpu": platform.processor(),
        "python": sys.version.split()[0], "versions": versions(plan["tool"], plan_names),
    }
    (out_dir / "environment.txt").write_text(
        "\n".join(f"{key}: {value}" for key, value in environment.items()) + "\n", encoding="utf-8"
    )
    rows = []
    with open(out_dir / "runs.csv", "w", newline="", encoding="utf-8") as file:
        writer = csv.DictWriter(file, fieldnames=COLUMNS)
        writer.writeheader()
        for workload in plan["workload"]:
            # tools run in their own folders, so paths in the repository go through {repo}
            names = {**plan_names, **workload.get("vars", {})}
            want = expected(BENCH.parent / workload["expect"])
            members = [t for t in workload["tools"] if only is None or t in only]
            for rep in range(reps):
                order = members[:]
                order_source.shuffle(order)
                for position, tool_name in enumerate(order):
                    rows += run_tool(plan, workload, tool_name, names, want, rep, position,
                                     scratch, out_dir, writer)
                    file.flush()
    remove_tree(scratch)
    summarize(rows, plan, reps, out_dir)


def run_tool(plan, workload, tool_name, names, want, rep, position, scratch, out_dir, writer):
    """a first fetch by one tool and, when the tool keeps a cache, a second project."""
    tool = plan["tool"][tool_name]
    home = scratch / f"{workload['id']}-{tool_name}-{rep}-home"
    home.mkdir()
    phases = ["first", "second"] if tool.get("second") else ["first"]
    folders = []
    rows = []
    for phase in phases:
        if plan["layer"] == "live":
            spent = ledger_total()
            if spent + workload.get("bytes", 0) > BUDGET:
                sys.exit(f"stopping: {spent / 1e9:.1f} GB spent of {BUDGET / 1e9:.0f} GB")
        work = scratch / f"{workload['id']}-{tool_name}-{rep}-{phase}"
        out = work / "out"
        out.mkdir(parents=True)
        # one tag for both phases: a second project asks for the same url, and a tag that
        # changed between them would make it a new link
        run_id = f"{plan['name']}-{workload['id']}-{tool_name}-{rep}"
        local = {**names, "out": out, "home": home, "work": work, "run": run_id}
        argv = fill(tool[phase], local)
        env = {**os.environ, **fill(tool.get("env", {}), local)}
        log = out_dir / f"{run_id}-{phase}.stderr"
        wall, cpu, peak, net, code, timed_out = run(argv, env, work, workload.get("timeout", 10800), log)
        got, count = produced([work], tool.get("ignore", []))
        ok = code == 0 and not timed_out and not (want - got)
        folders.append(work)
        if plan["layer"] == "live":
            ledger_add(plan["name"], workload["id"], tool_name, phase, rep, int(net * 1e6))
        row = {
            "when": datetime.now(UTC).isoformat(timespec="seconds"),
            "plan": plan["name"], "layer": plan["layer"], "profile": plan.get("profile", ""),
            "workload": workload["id"], "tool": tool_name, "phase": phase, "rep": rep,
            "position": position, "run": run_id, "wall_s": f"{wall:.3f}", "cpu_s": f"{cpu:.3f}",
            "peak_mb": f"{peak:.1f}", "net_mb": f"{net:.1f}", "files": count, "ok": ok,
            "exit": code, "disk_mb": "", "note": "timed out" if timed_out else "",
        }
        row["disk_mb"] = f"{disk_bytes([home, *folders]) / 1e6:.1f}"
        writer.writerow(row)
        rows.append(row)
        print(f"{workload['id']} {tool_name} {phase} block {rep}: {wall:.2f} s "
              f"{'ok' if ok else 'FAILED'}", flush=True)
    remove_tree(home)
    for folder in folders:
        remove_tree(folder)
    return rows


def summarize(rows, plan, reps, out_dir):
    """a table per workload: completion, the median and range of correct runs, the ratio to a
    floor doing the same work, and, for a primary lab comparison only, a paired verdict."""
    lines = [f"# {plan['name']}", ""]
    if plan["layer"] == "live":
        lines += ["live timings are description only; see docs/dev/benchmarks.md.", ""]
    for workload in plan["workload"]:
        lines += [f"## {workload['id']}", ""]
        lines.append("| tool | phase | correct | wall median s | wall range s | x floor | "
                     "cpu median s | peak MB | disk MB | paired with fetchloom |")
        lines.append("|---|---|---|---|---|---|---|---|---|---|")
        everything = {}
        for row in rows:
            if row["workload"] == workload["id"]:
                everything.setdefault((row["tool"], row["phase"]), []).append(row)
        correct = {key: [r for r in runs if r["ok"]] for key, runs in everything.items()}
        floor = workload.get("floor")
        primary = plan["layer"] == "lab" and workload.get("primary", False)
        for (tool, phase), runs in sorted(everything.items()):
            good = correct[(tool, phase)]
            if not good:
                lines.append(f"| {tool} | {phase} | 0 of {len(runs)} | | | | | | | |")
                continue
            walls = sorted(float(r["wall_s"]) for r in good)
            base = correct.get((floor, phase))
            ratio = ""
            if base and tool != floor:
                ratio = f"{statistics.median(walls) / statistics.median(float(r['wall_s']) for r in base):.2f}"
            paired = pairing(good, correct.get(("fetchloom", phase), []), reps, primary) if tool != "fetchloom" else ""
            disks = [float(r["disk_mb"]) for r in good if r["disk_mb"]]
            lines.append(
                f"| {tool} | {phase} | {len(good)} of {len(runs)} | {statistics.median(walls):.2f} | "
                f"{walls[0]:.2f} to {walls[-1]:.2f} | {ratio} | "
                f"{statistics.median(float(r['cpu_s']) for r in good):.2f} | "
                f"{statistics.median(float(r['peak_mb']) for r in good):.0f} | "
                f"{statistics.median(disks):.0f} | {paired} |"
            )
        lines.append("")
    (out_dir / "summary.md").write_text("\n".join(lines), encoding="utf-8")
    print(f"results in {out_dir}")


def pairing(theirs, ours, reps, primary):
    """the other tool's time over fetchloom's in each block where both were correct. a verdict
    needs every block, a win in each, and ranges that do not overlap."""
    mine = {r["rep"]: float(r["wall_s"]) for r in ours}
    ratios = [float(r["wall_s"]) / mine[r["rep"]] for r in theirs if r["rep"] in mine]
    if not ratios:
        return ""
    text = f"{statistics.median(ratios):.2f} ({min(ratios):.2f} to {max(ratios):.2f})"
    if not primary or len(ratios) < reps:
        return text
    other = [float(r["wall_s"]) for r in theirs]
    if min(ratios) > 1 and min(other) > max(mine.values()):
        return text + ", fetchloom faster"
    if max(ratios) < 1 and max(other) < min(mine.values()):
        return text + ", fetchloom slower"
    return text + ", no clear difference"


if __name__ == "__main__":
    main()
