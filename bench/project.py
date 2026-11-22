"""project the real run's hours from its pilot, and apply the pilot's rules the design declares.

the pilot runs one round of every case and tool, with no warmup, and one live round. each row's
run_seconds is what the run cost the whole run, its untimed work included. the real run gives every arm
5 rounds and a warmup, and fetchloom, its control and each comparator the pilot freezes 5 rounds more
in a headline case; a live service costs its reference download once and 10 rounds of both tools. a
poor run's timed part is taken as proportional to the size that shrinks in it, its untimed work as
fixed.
"""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path

import fixtures
import lab
import live
import render
import run

# past this many hours the poor link's sizes shrink, and past the second the live part moves to a
# separate daytime run, and then the tiny files to a block of their own
SHRINK_ABOVE, DEFER_ABOVE = 8, 9
# the cases of 10,000 tiny files, which leave the run as one block when it is still too long
APART = ("s3", "s9")


def part(profile):
    """the part of the real run a link belongs to: home, poor, or lan, with this machine's disk."""
    return "lan" if profile in ("lan", "local") else profile


def shrink(scenario, scale):
    """how much a poor case's runs shrink with the link's sizes: by its large file in s1, by its
    file count in s2r."""
    (large, many), (small, few) = fixtures.SIZES["poor"], fixtures.scaled("poor", scale)
    return small / large if scenario in ("s1", *fixtures.BROKEN) else few / many


def hours(rows, metas, comparators, scale=1):
    """the projected hours of each part of the real run, from what each pilot run cost and how many
    runs the real run gives its arm, the poor link's at the sizes `scale` gives."""
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
        cost = row["run_seconds"]
        if row["profile"] == "poor":
            cost += min(row.get("wall_seconds") or 0, cost) * (shrink(row["scenario"], scale) - 1)
        seconds[part(row["profile"])] += cost * (1 + (run.HEADLINE_ROUNDS if row["tool"] in longer else run.ROUNDS))
    if services:
        seconds["live"] += sum(references.get(service, 0) for service in services)
    return {name: value / 3600 for name, value in seconds.items()}


def expected_cases():
    """the cases the real run measures, as filesystem, link and scenario: every scenario on each link
    it applies to on ext4, and on xfs the second project and the no-op on the lan, where reflinks
    change the result."""
    ext4 = {("ext4", case["profile"], case["id"]) for case in fixtures.case_specs(list(lab.PROFILES), fixtures.ALL_SCENARIOS, False)}
    return ext4 | {("xfs", "lan", "s5"), ("xfs", "lan", "s6")}


def finite(value):
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value >= 0


def lacking(name, runs):
    """why one arm's pilot runs cannot stand for it: none, more than one round or a warmup, or no
    finite cost on record."""
    if not runs:
        return [f"no pilot run of {name}"]
    if len(runs) > 1 or any(row.get("round") != 1 or row.get("warmup") for row in runs):
        return [f"{name} ran other than one round"]
    if not finite(runs[0].get("run_seconds")):
        return [f"{name} has no cost on record"]
    return []


def unfit(rows, metas):
    """why these results cannot stand for the real run, each a phrase, or none: a smoke run, a case or
    a live service of the real run it never measured, an arm of a case with no run, more than one
    round or a warmup, or a run with no finite cost."""
    found = []
    labs = [meta for meta in metas if meta.get("kind") == "lab"]
    lives = [meta for meta in metas if meta.get("kind") == "live"]
    if any(meta.get("quick") for meta in metas) or any(row.get("quick") for row in rows):
        found.append("a smoke run, not a full-size pilot")
    planned = {(case.get("filesystem"), case.get("profile"), case.get("id")): case for meta in labs for case in meta.get("cases") or []}
    found += [f"{'/'.join(key)} never measured" for key in sorted(expected_cases() - planned.keys())]
    measured = defaultdict(list)
    for row in rows:
        if row.get("kind") == "lab" and "tool" in row:
            measured[row.get("filesystem"), row.get("profile"), row.get("scenario"), row["tool"]].append(row)
    for key, case in sorted(planned.items(), key=str):
        name = "/".join(map(str, key))
        arms = case.get("arm_rounds") or {}
        if not arms or case.get("warmup") or any(rounds != 1 for rounds in arms.values()):
            found.append(f"{name} is not planned as one round of each arm with no warmup")
        for arm in sorted(arms):
            found += lacking(f"{name} {arm}", measured.pop((*key, arm), []))
    found += [f"{'/'.join(map(str, key[:3]))} {key[3]} ran outside its plan" for key in sorted(measured, key=str)]
    if not lives:
        found.append("no live pilot")
    if any(meta.get("rounds") != 1 for meta in lives):
        found.append("the live pilot is not one round")
    datasets = {dataset.get("service"): dataset for meta in lives for dataset in meta.get("datasets") or []}
    for service in live.RUN if lives else ():
        dataset = datasets.get(service)
        if dataset is None:
            found.append(f"live {service} never measured")
            continue
        if not finite(dataset.get("reference_seconds")):
            found.append(f"live {service} has no reference download's cost on record")
        for tool in ("fetchloom", dataset.get("tool")):
            found += lacking(f"live {service} {tool}", [row for row in rows if row.get("kind") == "live"
                                                         and row.get("service") == service and row.get("tool") == tool])
    return found


def decide(rows, metas):
    """the pilot's rules applied to a pilot that can stand for the real run; ValueError naming what it
    lacks otherwise, since a run it never made would count as no time and freeze no rival."""
    found = unfit(rows, metas)
    if found:
        shown = "; ".join(found[:12]) + (f"; and {len(found) - 12} more" if len(found) > 12 else "")
        raise ValueError(f"the pilot cannot stand for the real run: {shown}")
    return rules(rows, metas)


def rules(rows, metas):
    """the pilot's rules applied: the comparators it freezes; the largest scale, in hundredths, that
    brings the projection to SHRINK_ABOVE, or the poor link's floor; whether the live part still fits
    under DEFER_ABOVE or moves to a daytime run; and, when it moves and the rest is still over
    DEFER_ABOVE, the APART cases that become their own block on the same build."""
    comparators = render.pick(rows)
    before = hours(rows, metas, comparators)
    scale = 1
    if sum(before.values()) > SHRINK_ABOVE:
        for step in range(99, 0, -1):
            scale = step / 100
            if sum(hours(rows, metas, comparators, scale).values()) <= SHRINK_ABOVE:
                break
    after = hours(rows, metas, comparators, scale)
    total = sum(after.values())
    deferred = total > DEFER_ABOVE
    if deferred:
        total -= after.get("live", 0)
    block = sum(hours([row for row in rows if row.get("scenario") in APART], metas, comparators, scale).values())
    apart = deferred and total > DEFER_ABOVE
    if apart:
        total -= block
    large, many = fixtures.scaled("poor", scale)
    return dict(comparators=comparators, hours=before, scale=scale, poor_sizes=dict(large_bytes=large, files=many),
                hours_after=after, live="daytime" if deferred else "with the run",
                apart=list(APART) if apart else [], apart_hours=block, total_hours=total)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pilot", type=Path, help="the pilot's lab and live runs, under one folder")
    args = parser.parse_args()
    try:
        rows, metas = render.load_results(args.pilot.resolve())
        decided = decide(rows, metas)
    except ValueError as error:
        parser.error(str(error))
    print(json.dumps(decided, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
