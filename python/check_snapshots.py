"""reject snapshots no successful OS or filesystem test referenced."""

import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def require_referenced(expected, reports):
    if not expected or not reports:
        raise ValueError("snapshot inventory or reference reports are missing")
    referenced = set().union(*reports)
    missing, unknown = expected - referenced, referenced - expected
    if missing or unknown:
        raise ValueError(f"unreferenced snapshots: {sorted(missing)}; unknown snapshots: {sorted(unknown)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", type=Path)
    args = parser.parse_args()
    tracked = subprocess.check_output(["git", "ls-files", "-z", "--", "*.snap", "*.snap.new", "*.pending-snap"], cwd=ROOT, text=True)
    files = set(filter(None, tracked.split("\0")))
    pending = {name for name in files if not name.endswith(".snap")}
    if pending:
        raise ValueError(f"pending snapshots must not be committed: {sorted(pending)}")
    reports = [set(json.loads(path.read_text(encoding="utf-8"))) for path in args.reports.rglob("snapshots.json")]
    require_referenced(files, reports)
    print(f"all {len(files)} snapshots were referenced across {len(reports)} test runs")


if __name__ == "__main__":
    main()
