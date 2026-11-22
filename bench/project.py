"""project a campaign's hours from its pilot, and apply the pilot's rules the design declares.

the pilot runs one round of every case and tool, with no warmup, and one live round. each row's
run_seconds is what the run cost the campaign, its untimed work included. the campaign runs every arm
5 rounds and a warmup, and fetchloom, its control and each comparator the pilot freezes 5 rounds more
in a headline case; a live service costs its reference download once and 10 rounds of both tools. a
poor case's cost is taken as proportional to the size that shrinks in it.
"""
import argparse
from collections import defaultdict
import json
from pathlib import Path

import fixtures
import live
import render
import run

# past this many hours the poor link's sizes shrink, and past the second the live part moves to a
# separate daytime run
SHRINK_ABOVE, DEFER_ABOVE = 8, 9


def part(profile):
    """the part of the campaign a link belongs to: home, poor, or lan, with this machine's disk."""
    return "lan" if profile in ("lan", "local") else profile


def shrink(scenario, scale):
    """how much a poor case's runs shrink with the link's sizes: by its large file in s1, by its
    file count in s2r."""
    (large, many), (small, few) = fixtures.SIZES["poor"], fixtures.scaled("poor", scale)
    return small / large if scenario in ("s1", *fixtures.BROKEN) else few / many


def hours(rows, metas, comparators, scale=1):
    """the projected hours of each part of the campaign, from what each pilot run cost and how many
    runs the campaign gives its arm, the poor link's at the sizes `scale` gives."""
    seconds = defaultdict(float)
    references = {dataset["service"]: dataset.get("reference_seconds") or 0 for meta in metas
                  for dataset in meta.get("datasets") or []}
    services = set()
    for row in rows:
        if row.get("warmup") or row.get("run_seconds") is None or "tool" not in row:
            continue
        if row.get("kind") == "live":
            services.add(row["service"])
            seconds["live"] += live.ROUNDS * row["run_seconds"]
            continue
        if row.get("kind") != "lab":
            continue
        key = (row["filesystem"], row["profile"], row["scenario"])
        longer = {run.tools.Fetchloom.name, run.CONTROL, *comparators.get("/".join(key), [])} if key in fixtures.HEADLINE else set()
        cost = row["run_seconds"] * (1 + (run.HEADLINE_ROUNDS if row["tool"] in longer else run.ROUNDS))
        seconds[part(row["profile"])] += cost * (shrink(row["scenario"], scale) if row["profile"] == "poor" else 1)
    if services:
        seconds["live"] += sum(references.get(service, 0) for service in services)
    return {name: value / 3600 for name, value in seconds.items()}


def decide(rows, metas):
    """the pilot's rules applied: the comparators it freezes; the largest scale, in hundredths, that
    brings the projection to SHRINK_ABOVE, or the poor link's floor; and whether the live part still
    fits under DEFER_ABOVE or moves to a daytime run."""
    comparators = render.pick(rows)
    before = hours(rows, metas, comparators)
    scale = 1
    if sum(before.values()) > SHRINK_ABOVE:
        for step in range(99, 0, -1):
            scale = step / 100
            if sum(hours(rows, metas, comparators, scale).values()) <= SHRINK_ABOVE:
                break
    after = hours(rows, metas, comparators, scale)
    deferred = sum(after.values()) > DEFER_ABOVE
    large, many = fixtures.scaled("poor", scale)
    return dict(comparators=comparators, hours=before, scale=scale, poor_sizes=dict(large_bytes=large, files=many),
                hours_after=after, live="daytime" if deferred else "with the run",
                total_hours=sum(after.values()) - (after.get("live", 0) if deferred else 0))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pilot", type=Path, help="the pilot's lab and live campaigns, under one folder")
    args = parser.parse_args()
    rows, metas = render.load_results(args.pilot.resolve())
    print(json.dumps(decide(rows, metas), indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
