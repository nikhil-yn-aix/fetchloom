"""Compare fetchloom variants with matched rounds and paired bootstrap intervals."""
import argparse
from collections import defaultdict
import json
import math
from pathlib import Path
import random
from statistics import mean, median, stdev

BASE = "fetchloom"
METRICS = (("wall", "wall_seconds"), ("cpu", None), ("memory", "maxrss_bytes"),
           ("disk", "disk_bytes"), ("server_bytes", "bytes_served"), ("requests", "requests"))
RESAMPLES = 20000
MIN_PAIRS = 10
THRESHOLD = .03


def number(value):
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        return None
    try:
        return value if math.isfinite(value) else None
    except OverflowError:
        return None


def value(row, key):
    if key is None:
        user, system = number(row.get("user_seconds")), number(row.get("sys_seconds"))
        return number(user + system) if user is not None and system is not None else None
    return number(row.get(key))


def exponent(value):
    if value is None:
        return None
    try:
        return math.exp(value)
    except OverflowError:
        return None


def quantile(values, q):
    position = (len(values) - 1) * q
    lower, upper = math.floor(position), math.ceil(position)
    return values[lower] + (values[upper] - values[lower]) * (position - lower)


def bootstrap(values, seed, resamples):
    if not values:
        return None
    if min(values) == max(values):
        return [values[0], values[0]]
    rng = random.Random(seed)
    samples = sorted(median(rng.choices(values, k=len(values))) for _ in range(resamples))
    return [quantile(samples, .025), quantile(samples, .975)]


def summarize(pairs, metric, *, seed, resamples, threshold, min_pairs, complete, failed):
    values = [(value(a, metric), value(b, metric)) for a, b in pairs]
    available = [(a, b) for a, b in values if a is not None and b is not None]
    logs = [math.log(b) - math.log(a) for a, b in available if a > 0 and b > 0]
    center = median(logs) if logs else None
    log_interval = bootstrap(logs, seed, resamples)
    interval = [exponent(bound) for bound in log_interval] if log_interval is not None else None
    if interval is not None and None in interval:
        interval = None
    missing, nonpositive = len(pairs) - len(available), len(available) - len(logs)
    if failed:
        decision = "failed"
    elif not complete or missing or nonpositive:
        decision = "incomplete"
    elif len(logs) < min_pairs or interval is None:
        decision = "insufficient"
    elif interval[1] < 1 - threshold:
        decision = "win"
    else:
        decision = "not established"
    plain = [a for a, _ in available]
    average = mean(plain) if plain else None
    return dict(n_available_pairs=len(available), n_log_pairs=len(logs),
                missing_metric_pairs=missing, nonpositive_pairs=nonpositive,
                log_ratio_median=center,
                log_ratio_robust_sd=1.4826 * median(abs(x - center) for x in logs) if logs else None,
                median_ratio=exponent(center), bootstrap_95_log_ratio=log_interval,
                bootstrap_95_median_ratio=interval, bootstrap_seed=seed,
                plain_median=median(plain) if plain else None,
                plain_cv=stdev(plain) / average if len(plain) > 1 and average > 0 else None,
                wins=dict(plain=sum(a < b for a, b in available),
                          variant=sum(b < a for a, b in available), ties=sum(a == b for a, b in available)),
                decision=decision)


def compare(rows, *, threshold=THRESHOLD, resamples=RESAMPLES, seed=1729, min_pairs=MIN_PAIRS):
    if number(threshold) is None or not 0 <= threshold < 1:
        raise ValueError("threshold must be a finite fraction from 0 up to 1")
    if not isinstance(resamples, int) or isinstance(resamples, bool) or resamples < 1:
        raise ValueError("resamples must be a positive integer")
    if not isinstance(min_pairs, int) or isinstance(min_pairs, bool) or min_pairs < 2:
        raise ValueError("min-pairs must be an integer of at least 2")
    cases = defaultdict(lambda: defaultdict(dict))
    for row in rows:
        tool = row.get("tool", "")
        if row.get("warmup") or not (tool == BASE or tool.startswith(BASE + "+")):
            continue
        key = (row["filesystem"], row["profile"], row["scenario"])
        cases[key][tool][row["round"]] = row
    results = []
    for key in sorted(cases):
        tools = cases[key]
        base = tools.get(BASE, {})
        for name in sorted(t for t in tools if t != BASE):
            runs = tools[name]
            failed = {side: sum(r["status"] != "passed" for r in side_runs.values())
                      for side, side_runs in (("base", base), ("variant", runs))}
            unmatched = dict(base=sorted(base.keys() - runs.keys()), variant=sorted(runs.keys() - base.keys()))
            paired = [(base[n], runs[n]) for n in sorted(base.keys() & runs.keys())
                      if base[n]["status"] == runs[n]["status"] == "passed"]
            case = "/".join(key)
            metrics = {label: summarize(paired, metric, seed=f"{seed}/{case}/{name}/{label}",
                                       resamples=resamples, threshold=threshold, min_pairs=min_pairs,
                                       complete=bool(base) and base.keys() == runs.keys(), failed=any(failed.values()))
                       for label, metric in METRICS}
            results.append(dict(case=case, variant=name, rounds=len(paired),
                                wins=metrics["wall"]["wins"]["variant"], failed=failed, unmatched=unmatched,
                                ratios={label: result["median_ratio"] for label, result in metrics.items()},
                                metrics=metrics, threshold=threshold, min_pairs=min_pairs, resamples=resamples))
    return results


def table(results, threshold=THRESHOLD, min_pairs=MIN_PAIRS):
    lines = ["case | variant | metric | log median | ratio | 95% ratio interval | wins/n | ties | n log | missing/nonpositive | decision | failed (base/variant) | unmatched (base/variant)",
             "--- | --- | --- | ---: | ---: | --- | ---: | ---: | ---: | ---: | --- | ---: | ---:"]
    def cell(value):
        return f"{value:.6f}" if value is not None else "-"
    for result in results:
        for label, _ in METRICS:
            metric = result["metrics"][label]
            bounds = metric["bootstrap_95_median_ratio"]
            interval = f"[{cell(bounds[0])}, {cell(bounds[1])}]" if bounds is not None else "-"
            lines.append(" | ".join([result["case"], result["variant"], label,
                                     cell(metric["log_ratio_median"]), cell(metric["median_ratio"]), interval,
                                     f"{metric['wins']['variant']}/{metric['n_available_pairs']}",
                                     str(metric["wins"]["ties"]), str(metric["n_log_pairs"]),
                                     f"{metric['missing_metric_pairs']}/{metric['nonpositive_pairs']}", metric["decision"],
                                     f"{result['failed']['base']}/{result['failed']['variant']}",
                                     f"{len(result['unmatched']['base'])}/{len(result['unmatched']['variant'])}"]))
    lines.append("\nratios are exp(median(ln(variant) - ln(plain))) for positive complete pairs matched by round. "
                 "intervals are paired percentile bootstrap 95% intervals; wins/n also include finite nonpositive pairs.")
    policies = {(r["threshold"], r["min_pairs"]) for r in results} or {(threshold, min_pairs)}
    for effect, minimum in sorted(policies):
        lines.append(f"win requires interval upper < {1 - effect:.4f} (1.00 minus {100 * effect:g}%), "
                     f"at least {minimum} log pairs, and no failures, unmatched rounds or excluded metric pairs. "
                     "an interval does not establish power or correct for testing many variants.")
    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path, help="a lab output folder holding runs.jsonl")
    parser.add_argument("--threshold", type=float, default=THRESHOLD, help="required fractional gain, default .03")
    parser.add_argument("--resamples", type=int, default=RESAMPLES)
    parser.add_argument("--seed", type=int, default=1729, help="bootstrap seed, independent of run order")
    parser.add_argument("--min-pairs", type=int, default=MIN_PAIRS)
    parser.add_argument("--json", type=Path, help="also write complete statistics and analysis settings")
    args = parser.parse_args()
    try:
        rows = [json.loads(line) for line in (args.results / "runs.jsonl").read_text().splitlines() if line.strip()]
        results = compare(rows, threshold=args.threshold, resamples=args.resamples, seed=args.seed, min_pairs=args.min_pairs)
    except (OSError, ValueError, KeyError) as error:
        parser.error(str(error))
    print(table(results, args.threshold, args.min_pairs))
    if args.json:
        args.json.write_text(json.dumps(dict(schema_version=1, threshold=args.threshold, resamples=args.resamples,
                                           seed=args.seed, min_pairs=args.min_pairs, results=results),
                                       indent=2, allow_nan=False) + "\n")


if __name__ == "__main__":
    main()
