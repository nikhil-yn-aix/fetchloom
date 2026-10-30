# /// script
# requires-python = ">=3.12"
# dependencies = ["psutil>=6.0"]
# ///
"""checks that fetchloom works on real datasets from every source, end to end.

usage: uv run bench/matrix.py bench/datasets.toml [--only id,id] [--resume id,id] [--results DIR]

for each dataset: add it on an empty cache, find status clean, sync a second project from the
same cache without downloading, restore a removed file offline, sync a third project on another
empty cache and get the same bytes, and for the datasets named by --resume, kill a sync at about
40% and finish it. fetchloom's own counters give the bytes received, which the shared ledger in
bench/results counts against the 25 GB budget.
"""

import argparse
import csv
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
from datetime import UTC, datetime
from pathlib import Path

import psutil

BENCH = Path(__file__).resolve().parent
LEDGER = Path(os.environ.get("BENCH_LEDGER", BENCH / "results" / "ledger.csv"))
BUDGET = 25 * 10**9
COLUMNS = ["dataset", "source", "cell", "check", "ok", "wall_s", "requests", "bytes_down",
           "resumes", "reflinks", "hardlinks", "copies", "exit", "note"]


def remove_tree(path):
    """removes a folder holding files fetchloom sealed read-only, which windows will not delete."""
    def unseal(func, target, _):
        os.chmod(target, stat.S_IWRITE)
        func(target)

    if os.path.lexists(path):
        shutil.rmtree(path, onexc=unseal)


def fetchloom(args, cache, timeout=7200):
    """runs fetchloom with --json on its own cache and returns seconds, exit code and the document."""
    env = {**os.environ, "FETCHLOOM_CACHE": str(cache)}
    started = time.perf_counter()
    done = subprocess.run(["fetchloom", *args, "--json", "--yes"], env=env, capture_output=True,
                          text=True, encoding="utf-8", timeout=timeout)
    wall = time.perf_counter() - started
    try:
        document = json.loads(done.stdout)
    except json.JSONDecodeError:
        document = {"status": "error", "errors": [{"message": done.stderr.strip()[-300:]}], "work": {}}
    return wall, done.returncode, document


def ledger_total():
    if not LEDGER.exists():
        return 0
    with open(LEDGER, newline="", encoding="utf-8") as file:
        return sum(int(row["bytes"]) for row in csv.DictReader(file))


def ledger_add(dataset, check, received):
    new = not LEDGER.exists()
    LEDGER.parent.mkdir(parents=True, exist_ok=True)
    with open(LEDGER, "a", newline="", encoding="utf-8") as file:
        writer = csv.writer(file)
        if new:
            writer.writerow(["when", "plan", "workload", "tool", "phase", "rep", "bytes"])
        when = datetime.now(UTC).isoformat(timespec="seconds")
        writer.writerow([when, "matrix", dataset, "fetchloom", check, 0, received])


def tree_hashes(root):
    """sha256 of every file under root by its path from root, skipping fetchloom's own folder."""
    found = {}
    for path in sorted(Path(root).rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_file() and not relative.startswith(".fetchloom") and relative not in ("data.toml", "data.lock"):
            digest = hashlib.sha256()
            with open(path, "rb") as file:
                while chunk := file.read(1 << 20):
                    digest.update(chunk)
            found[relative] = digest.hexdigest()
    return found


def row(dataset, check, ok, wall, code, document, note=""):
    work = document.get("work") or {}
    if not ok and not note:
        errors = document.get("errors") or []
        note = (errors[0].get("message", "") if errors else "")[:200]
    return {
        "dataset": dataset["id"], "source": dataset.get("source", ""), "cell": dataset.get("cell", ""),
        "check": check, "ok": ok, "wall_s": f"{wall:.2f}", "requests": work.get("requests", ""),
        "bytes_down": work.get("bytes_down", ""), "resumes": work.get("resumes", ""),
        "reflinks": work.get("reflinks", ""), "hardlinks": work.get("hardlinks", ""),
        "copies": work.get("copies", ""), "exit": code, "note": note,
    }


def copy_project(source, target):
    target.mkdir(parents=True)
    for name in ("data.toml", "data.lock"):
        shutil.copy2(source / name, target / name)


def killed_halfway(project, cache, size):
    """starts a sync and kills it once its unfinished downloads hold about 40% of size. says
    whether it got there; a download that finishes first leaves nothing to resume."""
    env = {**os.environ, "FETCHLOOM_CACHE": str(cache)}
    process = psutil.Popen(["fetchloom", "-C", str(project), "sync", "--yes", "-q"], env=env,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    unfinished = Path(cache) / "partial"
    while process.poll() is None:
        held = sum(p.stat().st_size for p in unfinished.rglob("*") if p.is_file()) if unfinished.exists() else 0
        if held >= 0.4 * size:
            for member in [*process.children(recursive=True), process]:
                try:
                    member.kill()
                except psutil.Error:
                    pass
            process.wait()
            return True
        time.sleep(0.2)
    return False


def check(dataset, scratch, resume, fresh, writer):
    """runs every check for one dataset and writes a row for each."""
    name = dataset["id"]
    size = int(dataset.get("total_bytes", 0))
    home = scratch / name
    first_cache, other_cache = home / "cache-1", home / "cache-2"
    first, second, third = home / "first", home / "second", home / "third"
    first.mkdir(parents=True)

    def record(check_name, ok, wall, code, document, note=""):
        result = row(dataset, check_name, ok, wall, code, document, note)
        writer.writerow(result)
        received = int((document.get("work") or {}).get("bytes_down") or 0)
        if received:
            ledger_add(name, check_name, received)
        print(f"{name} {check_name}: {'ok' if ok else 'FAILED'} {wall:.1f} s {note}", flush=True)
        return ok

    if ledger_total() + size * (3 if name in resume or name in fresh else 2) > BUDGET:
        print(f"{name}: skipped, it would pass the budget", flush=True)
        return
    wall, code, document = fetchloom(["-C", str(first), "add", dataset["reference"], "--as", name], first_cache)
    if not record("add", code == 0 and document.get("status") == "ok", wall, code, document):
        return
    wall, code, document = fetchloom(["-C", str(first), "status"], first_cache)
    status = document.get("result") or {}
    dirty = {key: value for key, value in status.items() if isinstance(value, list) and value}
    record("status", code == 0 and not dirty, wall, code, document, "" if not dirty else f"not clean: {list(dirty)}")

    copy_project(first, second)
    wall, code, document = fetchloom(["-C", str(second), "sync"], first_cache)
    work = document.get("work") or {}
    reused = code == 0 and not work.get("bytes_down")
    record("second", reused, wall, code, document, "" if reused or code else "downloaded again")

    placed = [p for p in (second / "data").rglob("*") if p.is_file()]
    if placed:
        gone = home / "moved-away"
        gone.mkdir()
        # moved rather than deleted: the file is read-only, and making it writable would also
        # unseal the cached object it is a hardlink of
        placed[0].rename(gone / placed[0].name)
        wall, code, document = fetchloom(["-C", str(second), "sync", "--offline"], first_cache)
        restored = code == 0 and placed[0].exists()
        record("offline", restored, wall, code, document, "" if restored or code else "not restored")

    if dataset.get("cell", "").startswith("S") or name in fresh:
        copy_project(first, third)
        wall, code, document = fetchloom(["-C", str(third), "sync"], other_cache)
        same = code == 0 and tree_hashes(first) == tree_hashes(third)
        record("fresh", same, wall, code, document, "" if same or code else "other bytes")
    else:
        print(f"{name} fresh: skipped to save budget; add already downloaded it once", flush=True)

    if name in resume:
        fourth = home / "fourth"
        copy_project(first, fourth)
        resume_cache = home / "cache-3"
        resume_cache.mkdir()
        if killed_halfway(fourth, resume_cache, size):
            wall, code, document = fetchloom(["-C", str(fourth), "sync"], resume_cache)
            work = document.get("work") or {}
            resumed = code == 0 and int(work.get("resumes") or 0) > 0
            again = int(work.get("bytes_down") or 0) / 1e6
            record("resume", resumed, wall, code, document, f"{again:.0f} MB after the kill")
        else:
            print(f"{name} resume: finished before 40%, nothing to resume", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("datasets")
    parser.add_argument("--only", help="comma-separated dataset ids")
    parser.add_argument("--resume", default="", help="comma-separated dataset ids to kill and resume")
    parser.add_argument("--fresh", default="",
                        help="comma-separated ids that also sync on another empty cache; small cells always do")
    parser.add_argument("--results", type=Path, default=BENCH / "results")
    args = parser.parse_args()
    data = tomllib.loads(Path(args.datasets).read_text(encoding="utf-8-sig"))
    datasets = data.get("dataset") or [{"id": key, **value} for key, value in data.items() if isinstance(value, dict)]
    only = set(args.only.split(",")) if args.only else None
    resume = set(filter(None, args.resume.split(",")))
    fresh = set(filter(None, args.fresh.split(",")))
    stamp = datetime.now(UTC).strftime("%Y%m%d-%H%M%S")
    out_dir = args.results / f"{stamp}-matrix-{sys.platform}"
    out_dir.mkdir(parents=True)
    scratch = Path(tempfile.mkdtemp(prefix="matrix-"))
    with open(out_dir / "matrix.csv", "w", newline="", encoding="utf-8") as file:
        writer = csv.DictWriter(file, fieldnames=COLUMNS)
        writer.writeheader()
        for dataset in datasets:
            if only is None or dataset["id"] in only:
                try:
                    check(dataset, scratch, resume, fresh, writer)
                finally:
                    file.flush()
                    remove_tree(scratch / dataset["id"])
    remove_tree(scratch)
    print(f"results in {out_dir}; {ledger_total() / 1e9:.2f} GB counted so far")


if __name__ == "__main__":
    main()
