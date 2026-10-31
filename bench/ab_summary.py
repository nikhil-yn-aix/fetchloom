"""median and range per side of an ab.ps1 csv, and the change from a to b."""
import csv
import statistics
import sys

for path in sys.argv[1:]:
    rows = list(csv.DictReader(open(path, encoding="utf-8")))
    sides = {}
    for row in rows:
        if row["exit"] != "0":
            continue
        sides.setdefault(row["side"], []).append(float(row["seconds"]))
    out = [path.rsplit("\\", 1)[-1]]
    for side in ("a", "b"):
        values = sides.get(side, [])
        if values:
            out.append(f"{side}: {statistics.median(values):.2f} s ({min(values):.2f} to {max(values):.2f}), n={len(values)}")
    if "a" in sides and "b" in sides:
        a, b = statistics.median(sides["a"]), statistics.median(sides["b"])
        out.append(f"b vs a: {(b - a) / a * 100:+.1f}%")
    failed = sum(1 for row in rows if row["exit"] != "0")
    if failed:
        out.append(f"failed runs: {failed}")
    print(" | ".join(out))
