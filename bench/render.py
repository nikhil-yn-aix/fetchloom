"""Render measured results as paired transparent SVGs and a markdown page."""
import argparse
from collections import defaultdict
import html
import json
import os
from pathlib import Path
from statistics import median

TITLES = {"s1": "one large file", "s2": "many small files", "s3": "tiny files",
          "s4": "download and unpack", "s5": "second project", "s6": "nothing changed",
          "s7": "interrupted download"}


def summarize(rows):
    successful = [r for r in rows if r.get("status") == "passed"]
    times = [r["wall_seconds"] for r in successful]
    def metric(name):
        values = [r[name] for r in successful if r.get(name) is not None]
        return median(values) if values else None
    cpu = [r.get("user_seconds", 0) + r.get("sys_seconds", 0) for r in successful]
    return dict(median=median(times) if times else None, low=min(times) if times else None,
                high=max(times) if times else None, failures=sum(r.get("status") == "failed" for r in rows),
                skipped=sum(r.get("status") == "skipped" for r in rows), rounds=len(rows),
                cpu=median(cpu) if cpu else None, memory=metric("maxrss_bytes"), disk=metric("disk_bytes"),
                requests=metric("requests"), download_ratio=metric("download_ratio"))


def seconds(value):
    if value is None:
        return "n/a"
    if value < 1:
        return f"{value * 1000:.1f} ms"
    if value < 60:
        return f"{value:.2f} s"
    return f"{value / 60:.2f} min"


def size(value):
    if value is None:
        return "n/a"
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(value) < 1024 or unit == "TiB":
            return f"{value:.1f} {unit}"
        value /= 1024


def ratio(value, base):
    if value is None or base is None or base <= 0:
        return "n/a"
    result = value / base
    return "even (<5%)" if abs(result - 1) < .05 else f"{result:.2f}x"


def palette(dark):
    return dict(text="#e7eaf0" if dark else "#242b35", grid="#536171" if dark else "#dce1e7",
                accent="#60c5be" if dark else "#087f8c", neutral="#929dab" if dark else "#9098a3")


def chart(path, title, subtitle, summaries, dark=False):
    import matplotlib.pyplot as plt
    from matplotlib.ticker import MaxNLocator
    colors = palette(dark)
    rows = sorted(summaries.items(), key=lambda pair: (pair[1]["failures"] > 0 or pair[1]["median"] is None,
                                                       pair[1]["median"] or float("inf"), pair[0]))
    fig, ax = plt.subplots(figsize=(8, 4.6))
    fig.patch.set_alpha(0)
    ax.patch.set_alpha(0)
    good = [s["high"] for _, s in rows if s["median"] is not None and not s["failures"]]
    maximum = max(good, default=1)
    for y, (name, result) in enumerate(rows):
        if result["median"] is None or result["failures"]:
            ax.text(maximum * .02, y, "failed" if result["failures"] else "skipped", va="center", color=colors["text"], fontsize=10)
        else:
            center = result["median"]
            ax.barh(y, center, height=.55, color=colors["accent"] if name == "fetchloom" else colors["neutral"], zorder=3)
            ax.errorbar(center, y, xerr=[[center - result["low"]], [result["high"] - center]],
                        fmt="none", ecolor=colors["text"], elinewidth=.8, capsize=3, zorder=4)
            ax.text(result["high"] + maximum * .025, y, seconds(center), va="center", color=colors["text"], fontsize=9)
    ax.set_yticks(range(len(rows)), [name for name, _ in rows])
    ax.invert_yaxis()
    ax.set_xlim(0, maximum * 1.3)
    ax.set_xlabel("wall time (seconds)", color=colors["text"])
    ax.xaxis.set_major_locator(MaxNLocator(5))
    style_axes(ax, colors)
    fig.text(.19, .94, title, color=colors["text"], fontsize=15, fontweight="medium")
    fig.text(.19, .885, subtitle, color=colors["text"], fontsize=9)
    fig.subplots_adjust(left=.19, right=.97, top=.82, bottom=.14)
    fig.savefig(path, transparent=True, metadata={"Date": None})
    plt.close(fig)


def style_axes(ax, colors):
    ax.spines[["top", "right", "left"]].set_visible(False)
    ax.spines["bottom"].set_color(colors["grid"])
    ax.tick_params(axis="both", colors=colors["text"], length=0, pad=7)
    ax.set_axisbelow(True)
    ax.grid(axis="x", color=colors["grid"], linewidth=.6, alpha=.55)


def hero(path, groups, fs, dark=False):
    import matplotlib.pyplot as plt
    from matplotlib.ticker import FuncFormatter
    colors = palette(dark)
    rows = []
    for scenario, profile, filesystem in sorted(groups):
        if profile != "lan" or filesystem != fs:
            continue
        summary = groups[(scenario, profile, filesystem)]
        base = summary.get("fetchloom", {})
        if not base.get("median") or base.get("failures"):
            continue
        for name, entry in sorted(summary.items(), key=lambda item: item[1]["median"] or float("inf")):
            rows.append((scenario, name, None if entry["failures"] or entry["median"] is None else entry["median"] / base["median"]))
    fig, ax = plt.subplots(figsize=(8, max(4.6, len(rows) * .25 + 1.4)))
    values = [value for _, _, value in rows if value is not None]
    lower = min(.25, min(values, default=1) / 2)
    upper = max(2, max(values, default=1) * 1.6)
    for y, (scenario, name, value) in enumerate(rows):
        if value is None:
            ax.text(lower * 1.1, y, "failed", va="center", color=colors["text"], fontsize=8)
        else:
            ax.barh(y, value - lower, left=lower, height=.58,
                    color=colors["accent"] if name == "fetchloom" else colors["neutral"], zorder=3)
            ax.text(value * 1.06, y, f"{value:.2f}\u00d7", va="center", color=colors["text"], fontsize=8)
    ax.set_xscale("log")
    ax.set_xlim(lower, upper)
    ax.set_yticks(range(len(rows)), [f"{scenario} / {name}" for scenario, name, _ in rows], fontsize=8)
    ax.invert_yaxis()
    ax.axvline(1, color=colors["text"], linewidth=1, linestyle="--", zorder=4)
    ax.xaxis.set_major_formatter(FuncFormatter(lambda value, _: f"{value:g}\u00d7"))
    ax.set_xlabel("median wall time / fetchloom (log scale); dashed line = 1\u00d7", color=colors["text"])
    style_axes(ax, colors)
    fig.text(.23, .965, "local scenarios relative to fetchloom", color=colors["text"], fontsize=14)
    fig.text(.23, .94, f"lan; {fs}; each row uses its case's files and rounds", color=colors["text"], fontsize=9)
    fig.subplots_adjust(left=.23, right=.96, top=.91, bottom=.08)
    fig.savefig(path, transparent=True, metadata={"Date": None})
    plt.close(fig)


def picture(name, title):
    escaped = html.escape(title, quote=True)
    return (f'<picture>\n  <source media="(prefers-color-scheme: dark)" srcset="../../assets/bench/{name}-dark.svg">\n'
            f'  <img src="../../assets/bench/{name}.svg" alt="{escaped}">\n</picture>\n')


def render(results, repo):
    config = results / ".matplotlib"
    config.mkdir(exist_ok=True)
    os.environ["MPLCONFIGDIR"] = str(config)
    import matplotlib
    matplotlib.use("Agg")
    matplotlib.rcParams.update({"svg.fonttype": "none", "font.family": "DejaVu Sans", "font.size": 10,
                                "svg.hashsalt": "fetchloom-benchmark"})
    paths = sorted(results.rglob("runs.jsonl"))
    if not paths:
        raise ValueError("no runs.jsonl found")
    all_rows = [json.loads(line) for path in paths for line in path.read_text().splitlines() if line.strip()]
    rows = [row for row in all_rows if not row.get("warmup") and row.get("kind") in ("lab", "live")]
    metas = [json.loads(path.read_text()) for path in sorted(results.rglob("meta.json"))]
    conditions = {}
    for row in rows:
        if row.get("kind") != "lab":
            continue
        key = (row["scenario"], row["profile"], row["filesystem"])
        condition = (row.get("quick"), row.get("count"), row.get("file_size"))
        if key in conditions and conditions[key] != condition:
            raise ValueError(f"mixed benchmark conditions for {key}; render campaigns separately")
        conditions[key] = condition
    if sum(meta.get("kind") == "lab" for meta in metas) > 1:
        raise ValueError("multiple lab campaigns have separate conditions; render them separately")
    charts = repo / "docs" / "assets" / "bench"
    charts.mkdir(parents=True, exist_ok=True)
    lines = ["# benchmark results\n", "measurements from the recorded commands and checked outputs.\n", "## conditions\n"]
    if any(meta.get("quick") for meta in metas):
        lines.append("these are smoke-test sizes and rounds, not full benchmark results.\n")
    for meta in metas:
        machine = meta.get("machine", {})
        lines += [f"- recorded: `{meta.get('date', 'unknown')}`",
                  f"- CPU: {machine.get('cpu', 'unknown')}; cores: {machine.get('cores', 'unknown')}; memory: {machine.get('memory', 'unknown')}",
                  f"- kernel: `{meta.get('kernel', 'unknown')}`; seed: `{meta.get('seed', 'unknown')}`",
                  f"- fetchloom: `{meta.get('fetchloom_version', 'unknown')}`; repository: `{meta.get('repo_head', 'unknown')}`",
                  f"- filesystems: `{json.dumps(meta.get('filesystems', {}), sort_keys=True)}`",
                  "- versions: " + "; ".join(f"{name} `{version}`" for name, version in meta.get("tool_versions", {}).items()), ""]
    networks = next((meta["network_profiles"] for meta in metas if meta.get("network_profiles")), {})
    if networks:
        lines += ["| profile | round-trip delay | loss per direction | server egress |", "|---|---:|---:|---:|"]
        for name, values in networks.items():
            rate = f"{values[2]} Mbit/s" if values[2] else "unlimited"
            lines.append(f"| {name} | {values[0]} ms | {values[1]}% | {rate} |")
        lines.append("")
    lines.append("the clock includes the entire command. warmups are excluded. tables use successful runs only; failures are counted and a tool with any failed run has no chart bar. CPU is user plus system time, memory is the median command peak, and disk is the median free-space change. whiskers span the minimum and maximum.\n")
    warmup_failures = sum(bool(row.get("warmup")) and row.get("status") == "failed" for row in all_rows)
    if warmup_failures:
        lines.append(f"{warmup_failures} warmup commands failed; their records remain in runs.jsonl.\n")
    buckets = defaultdict(lambda: defaultdict(list))
    for row in rows:
        if row.get("kind") == "lab":
            buckets[(row["scenario"], row["profile"], row["filesystem"])][row["tool"]].append(row)
    groups = {key: {tool: summarize(values) for tool, values in group.items()} for key, group in buckets.items()}
    for (scenario, profile, fs), summaries in sorted(groups.items()):
        sample = next(iter(buckets[(scenario, profile, fs)].values()))[0]
        rounds = max(item["rounds"] for item in summaries.values())
        title = f"{scenario}: {TITLES[scenario]}"
        subtitle = f"{profile}; {fs}; {sample['count']:,} x {size(sample['file_size'])}; {rounds} measured rounds"
        name = f"{scenario}-{profile}-{fs}"
        lines += [f"## {title} ({profile}, {fs})\n", subtitle + "\n",
                  "| tool | median | min to max | ratio to fetchloom | CPU | peak memory | disk | requests | failures |",
                  "|---|---:|---|---:|---:|---:|---:|---:|---:|"]
        base_summary = summaries.get("fetchloom", {})
        base = None if base_summary.get("failures") else base_summary.get("median")
        for tool, entry in sorted(summaries.items(), key=lambda item: item[1]["median"] or float("inf")):
            ratio_text = "n/a" if entry["failures"] else ratio(entry["median"], base)
            lines.append(f"| {tool} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {ratio_text} | {seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['disk'])} | {entry['requests'] if entry['requests'] is not None else 'n/a'} | {entry['failures']}/{entry['rounds']} |")
        if scenario == "s7":
            lines += ["", "| tool | bytes served / file size |", "|---|---:|"]
            for tool, entry in summaries.items():
                value = entry["download_ratio"]
                lines.append(f"| {tool} | {value:.3f}x |" if value is not None else f"| {tool} | n/a |")
            lines.append("\nwall time is the second attempt. bytes cover both attempts; interruption is at 10 s, with a 5 s kill grace period.\n")
        for dark in (False, True):
            chart(charts / f"{name}{'-dark' if dark else ''}.svg", title, subtitle, summaries, dark)
        lines += ["", picture(name, title)]
    for fs in sorted({key[2] for key in groups if key[1] == "lan"}):
        for dark in (False, True):
            hero(charts / f"hero-{fs}{'-dark' if dark else ''}.svg", groups, fs, dark)
    live_groups = defaultdict(lambda: defaultdict(list))
    for row in rows:
        if row.get("kind") == "live" and "tool" in row:
            live_groups[row["service"]][row["tool"]].append(row)
    if any(row.get("kind") == "live" for row in all_rows):
        lines += ["## live services\n", "live timings show ranges without ratios. file comparisons retain layout and content differences.\n"]
    for service, group in sorted(live_groups.items()):
        lines += [f"### {service}\n", "| tool | median | min to max | CPU | peak memory | disk | requests | failures |", "|---|---:|---|---:|---:|---:|---:|---:|"]
        summaries = {name: summarize(values) for name, values in group.items()}
        for name, entry in summaries.items():
            lines.append(f"| {name} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['disk'])} | n/a | {entry['failures']}/{entry['rounds']} |")
        sample = next(iter(group.values()))[0]
        for dark in (False, True):
            chart(charts / f"live-{service}{'-dark' if dark else ''}.svg", service,
                  f"live; ext4; {sample['count']} files; {size(sample['file_size'])} total; {max(x['rounds'] for x in summaries.values())} rounds", summaries, dark)
        lines += ["", picture(f"live-{service}", service)]
    for row in all_rows:
        if row.get("status") == "skipped":
            lines.append(f"- {row.get('service', row.get('tool', 'run'))} skipped: {row.get('reason', 'not recorded')}")
        if row.get("kind") == "comparison":
            lines.append(f"- {row['service']} round {row['round']}: `{row['comparison']['status']}`; differences: `{json.dumps(row['comparison'], sort_keys=True)}`")
    lines += ["", "sources: `meta.json` and `runs.jsonl` in the supplied results directory. [benchmark design](benchmark-design.md) defines the cases and fairness rules.\n"]
    output = repo / "docs" / "dev" / "performance" / "benchmarks.md"
    output.write_text("\n".join(lines))
    print(f"wrote {output} and {charts}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    args = parser.parse_args()
    render(args.results.resolve(), Path(__file__).resolve().parent.parent)


if __name__ == "__main__":
    main()
