"""Compare each fetchloom variant with the plain build, round by round, from a lab run."""
import argparse
from collections import defaultdict
import json
from pathlib import Path
from statistics import median

BASE = "fetchloom"
METRICS = (("wall", "wall_seconds"), ("cpu", None), ("memory", "maxrss_bytes"), ("disk", "disk_bytes"))


def value(row, key):
    if key is None:
        return row.get("user_seconds", 0) + row.get("sys_seconds", 0)
    return row.get(key)


def compare(rows):
    """per case and variant: how many rounds failed, the variant's median over the base's for each
    metric, and how many rounds the variant was faster in. a round counts only when both passed."""
    cases = defaultdict(lambda: defaultdict(dict))
    for row in rows:
        if row.get("warmup") or not row["tool"].startswith(BASE):
            continue
        key = (row["filesystem"], row["profile"], row["scenario"])
        cases[key][row["tool"]][row["round"]] = row
    results = []
    for key in sorted(cases):
        tools = cases[key]
        base = tools.get(BASE, {})
        for name in sorted(t for t in tools if t != BASE):
            runs = tools[name]
            failed = {side: sum(r["status"] != "passed" for r in side_runs.values())
                      for side, side_runs in (("base", base), ("variant", runs))}
            paired = [(base[n], runs[n]) for n in sorted(base.keys() & runs.keys())
                      if base[n]["status"] == runs[n]["status"] == "passed"]
            ratios = {}
            for label, metric in METRICS:
                a = [value(x, metric) for x, _ in paired if value(x, metric) is not None]
                b = [value(y, metric) for _, y in paired if value(y, metric) is not None]
                ratios[label] = median(b) / median(a) if a and b and median(a) else None
            wins = sum(value(y, "wall_seconds") < value(x, "wall_seconds") for x, y in paired)
            results.append(dict(case="/".join(key), variant=name, rounds=len(paired), wins=wins,
                                failed=failed, ratios=ratios))
    return results


def table(results):
    lines = ["case | variant | rounds | faster in | wall | cpu | memory | disk | failed (base/variant)",
             "--- | --- | --- | --- | --- | --- | --- | --- | ---"]
    for r in results:
        cells = [f"{r['ratios'][label]:.3f}" if r["ratios"][label] is not None else "-" for label, _ in METRICS]
        lines.append(" | ".join([r["case"], r["variant"], str(r["rounds"]), f"{r['wins']}/{r['rounds']}", *cells,
                                 f"{r['failed']['base']}/{r['failed']['variant']}"]))
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="a lab output folder holding runs.jsonl")
    args = parser.parse_args()
    rows = [json.loads(line) for line in (args.results / "runs.jsonl").read_text().splitlines() if line]
    print(table(compare(rows)))
    print("\nratios are the variant's median over the plain build's; below 1 is better.")


if __name__ == "__main__":
    main()
