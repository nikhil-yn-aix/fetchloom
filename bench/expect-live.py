# /// script
# requires-python = ">=3.12"
# ///
"""downloads live datasets once with fetchloom and writes bench/expect/live/<id>.sha256 from it.

usage: uv run bench/expect-live.py [--only id,id] [--force]

by default it takes every dataset a live plan in bench/plans names, skipping those whose expect
file exists unless --force. each download goes into a new temporary folder with its own empty
cache, keeps archives packed as the source serves them, as the live plans compare them, and is
verified: fetchloom checks the checksums a source states, and each file's sha256 computed here
must equal the one fetchloom recorded. the bytes received count in the shared ledger in
bench/results against the 25 GB budget.
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
import tomllib
from datetime import UTC, datetime
from pathlib import Path

BENCH = Path(__file__).resolve().parent
LEDGER = Path(os.environ.get("BENCH_LEDGER", BENCH / "results" / "ledger.csv"))
BUDGET = 25 * 10**9


def remove_tree(path):
    """removes a folder holding files fetchloom sealed read-only, which windows will not delete."""
    def unseal(func, target, _):
        os.chmod(target, stat.S_IWRITE)
        func(target)

    if os.path.lexists(path):
        shutil.rmtree(path, onexc=unseal)


def ledger_total():
    if not LEDGER.exists():
        return 0
    with open(LEDGER, newline="", encoding="utf-8") as file:
        return sum(int(row["bytes"]) for row in csv.DictReader(file))


def ledger_add(dataset, received):
    new = not LEDGER.exists()
    LEDGER.parent.mkdir(parents=True, exist_ok=True)
    with open(LEDGER, "a", newline="", encoding="utf-8") as file:
        writer = csv.writer(file)
        if new:
            writer.writerow(["when", "plan", "workload", "tool", "phase", "rep", "bytes"])
        when = datetime.now(UTC).isoformat(timespec="seconds")
        writer.writerow([when, "expect", dataset, "fetchloom", "first", 0, received])


def planned():
    """the dataset ids the live plans name, in plan order."""
    ids = []
    for plan in sorted((BENCH / "plans").glob("live-*.toml")):
        for workload in tomllib.loads(plan.read_text(encoding="utf-8-sig")).get("workload", []):
            ids.append(workload["id"])
    return ids


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        while chunk := file.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def expect(dataset, target):
    """downloads one dataset and writes its expect file; returns whether it worked."""
    size = int(dataset.get("total_bytes", 0))
    if ledger_total() + size > BUDGET:
        print(f"{dataset['id']}: stopped, it would pass the budget", flush=True)
        return False
    scratch = Path(tempfile.mkdtemp(prefix=f"expect-{dataset['id']}-"))
    try:
        out = scratch / "out"
        env = {**os.environ, "FETCHLOOM_CACHE": str(scratch / "cache")}
        done = subprocess.run(
            ["fetchloom", "get", dataset["reference"], "-o", str(out), "--no-extract", "--yes", "--json"],
            env=env, capture_output=True, text=True, encoding="utf-8",
        )
        try:
            document = json.loads(done.stdout)
        except json.JSONDecodeError:
            document = {"status": "error", "errors": [{"message": done.stderr.strip()[-300:]}]}
        received = int((document.get("work") or {}).get("bytes_down") or 0)
        if received:
            ledger_add(dataset["id"], received)
        if done.returncode or document.get("status") != "ok":
            errors = document.get("errors") or [{}]
            print(f"{dataset['id']}: fetchloom failed: {errors[0].get('message', '')}", flush=True)
            return False
        recorded = {f["path"]: f["sha256"] for f in document["result"]["files"]}
        lines = []
        for path in sorted(p for p in out.rglob("*") if p.is_file()):
            relative = path.relative_to(out).as_posix()
            digest = sha256(path)
            if recorded.get(relative) != digest:
                print(f"{dataset['id']}: {relative} hashed to {digest}, fetchloom recorded "
                      f"{recorded.get(relative)}", flush=True)
                return False
            lines.append(f"{digest}  {relative}\n")
        if len(lines) != len(recorded):
            print(f"{dataset['id']}: {len(lines)} files on disk, fetchloom listed {len(recorded)}", flush=True)
            return False
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text("".join(lines), encoding="utf-8", newline="\n")
        print(f"{dataset['id']}: {len(lines)} files, {received / 1e6:.1f} MB received", flush=True)
        return True
    finally:
        remove_tree(scratch)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--only", help="comma-separated dataset ids")
    parser.add_argument("--force", action="store_true", help="download again when an expect file exists")
    parser.add_argument("--datasets", type=Path, default=BENCH / "datasets.toml")
    args = parser.parse_args()
    datasets = {d["id"]: d for d in tomllib.loads(args.datasets.read_text(encoding="utf-8-sig"))["dataset"]}
    ids = args.only.split(",") if args.only else planned()
    unknown = [i for i in ids if i not in datasets]
    if unknown:
        sys.exit(f"not in {args.datasets}: {', '.join(unknown)}")
    failed = []
    for dataset_id in ids:
        target = BENCH / "expect" / "live" / f"{dataset_id}.sha256"
        if target.exists() and not args.force:
            print(f"{dataset_id}: kept {target.name}", flush=True)
            continue
        if not expect(datasets[dataset_id], target):
            failed.append(dataset_id)
    print(f"{ledger_total() / 1e9:.2f} GB counted so far", flush=True)
    if failed:
        sys.exit(f"failed: {', '.join(failed)}")


if __name__ == "__main__":
    main()
