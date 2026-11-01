"""Render measured results as light and dark SVG cards and a markdown page."""
import argparse
from collections import defaultdict
import html
import json
from pathlib import Path
from statistics import median

TITLES = {"s1": "one large file", "s2": "many small files", "s3": "tiny files",
          "s4": "download and unpack", "s5": "same data, new project", "s6": "sync again, nothing changed",
          "s7": "resume after an interruption"}


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
        return f"{value * 1000:.0f} ms"
    if value < 60:
        return f"{value:.2f} s"
    return f"{value / 60:.2f} min"


def size(value):
    if value is None:
        return "n/a"
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(value) < 1024 or unit == "TiB":
            return f"{value:.1f}".removesuffix(".0") + f" {unit}"
        value /= 1024


def ratio(value, base):
    if value is None or base is None or base <= 0:
        return "n/a"
    result = value / base
    return "even (<5%)" if abs(result - 1) < .05 else f"{result:.2f}x"


# instrument serif for the finding, geist mono for everything measured, both under the OFL. the
# subsets in fonts/ ride inside each svg, since github shows svgs as images that load nothing else.
FONTS = Path(__file__).with_name("fonts")
SERIF = "'Instrument Serif', Georgia, serif"
MONO = "'Geist Mono', ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"

# what each scenario does, in words a reader needs no benchmark design for
STORIES = {"s1": "{files} of {size}, downloaded and checked",
           "s2": "{files} of {size} each, downloaded and checked",
           "s3": "{files} of {size} each, downloaded and checked",
           "s4": "a {size} zip, downloaded, checked and unpacked",
           "s5": "{files} already in the cache, placed into a new project",
           "s6": "{files} already in place, each tool confirms nothing changed",
           "s7": "a {size} download stopped at 10 s, then run again to finish"}

# warm paper and ink, with one clay accent for fetchloom and quiet stone for everyone else
PALETTES = {
    False: dict(card="#faf9f5", edge="#e6e3d9", text="#141413", muted="#75726a", track="#efece4",
                accent="#c8532b", other="#b8b3a7", range="#141413"),
    True: dict(card="#1b1a17", edge="#34322c", text="#f2efe7", muted="#9b978c", track="#24231f",
               accent="#ee7a4c", other="#5c5850", range="#f2efe7"),
}

WIDTH = 760
PAD = 32
NAMES = 116
ROW = 32
BAR = 12
TIMES = 90


def font_faces():
    import base64
    faces = []
    for family, file, weight in (("Instrument Serif", "InstrumentSerif.woff2", 400),
                                 ("Geist Mono", "GeistMono-Regular.woff2", 400),
                                 ("Geist Mono", "GeistMono-Medium.woff2", 500)):
        data = base64.b64encode((FONTS / file).read_bytes()).decode()
        faces.append(f"@font-face{{font-family:'{family}';font-weight:{weight};"
                     f"src:url(data:font/woff2;base64,{data}) format('woff2')}}")
    return "".join(faces)

def times(result):
    """a ratio as people say it: 3x, 1.6x, 95x."""
    shown = f"{result:.0f}" if result >= 10 else f"{result:.1f}".removesuffix(".0")
    return f"{shown}\u00d7"


def compared(value, base):
    """"1.6x slower", "2.4x faster" or "even", against fetchloom."""
    result = value / base
    if abs(result - 1) < .05:
        return "even"
    return f"{times(result)} slower" if result > 1 else f"{times(1 / result)} faster"


def ordered(summaries):
    return sorted(summaries.items(), key=lambda pair: (bool(pair[1]["failures"]) or pair[1]["median"] is None,
                                                       pair[1]["median"] or float("inf"), pair[0]))


def verdict(summaries):
    """two or three words for a hero card: fetchloom against the fastest other tool."""
    own = summaries.get("fetchloom", {})
    rivals = [s["median"] for name, s in summaries.items()
              if name != "fetchloom" and s["median"] is not None and not s["failures"]]
    if own.get("failures") or own.get("median") is None or not rivals:
        return None
    result = min(rivals) / own["median"]
    if abs(result - 1) < .05:
        return "even"
    return f"{times(result)} faster" if result > 1 else f"{times(1 / result)} slower"

def story(scenario, count, file_size):
    files = f"{count:,} file" + ("" if count == 1 else "s")
    return STORIES.get(scenario, "{files}").format(files=files, size=size(file_size))


def text(x, y, value, colors, role="text", size=13, weight=400, anchor="start", family=MONO, spacing=0):
    extra = f' letter-spacing="{spacing}"' if spacing else ""
    return (f'<text x="{x:.1f}" y="{y:.1f}" fill="{colors[role]}" font-family="{family}" font-size="{size}" '
            f'font-weight="{weight}" text-anchor="{anchor}"{extra}>{html.escape(value)}</text>')


def chart(path, title, subtitle, summaries, dark=False, label=None):
    """a card of tools, fastest first. with a label it is a hero card: label, verdict, bars."""
    colors = PALETTES[dark]
    rows = ordered(summaries)
    reach = max((s["high"] for _, s in rows if s["median"] is not None and not s["failures"]), default=1)
    left = PAD + NAMES
    span = WIDTH - left - PAD - TIMES - 18
    head = 104 if label else 92
    top = head + 22
    height = top + ROW * len(rows) + 24
    parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
             f'viewBox="0 0 {WIDTH} {height}" role="img" aria-label="{html.escape(title, quote=True)}">',
             f"<style>{font_faces()}</style>",
             f'<rect x=".5" y=".5" width="{WIDTH - 1}" height="{height - 1}" rx="14" fill="{colors["card"]}" '
             f'stroke="{colors["edge"]}"/>']
    if label:
        parts += [text(PAD, 42, label.upper(), colors, "accent", 11, 500, spacing=1.4),
                  text(PAD, 86, title, colors, size=40, family=SERIF)]
    else:
        parts += [text(PAD, 56, title, colors, size=30, family=SERIF),
                  text(PAD, 80, subtitle, colors, "muted", 12)]
    for index, (name, result) in enumerate(rows):
        middle = top + ROW * index + ROW / 2
        mine = name == "fetchloom"
        parts.append(text(PAD, middle + 4, name, colors, "accent" if mine else "text", 13, 500 if mine else 400))
        parts.append(f'<rect x="{left}" y="{middle - BAR / 2:.1f}" width="{span}" height="{BAR}" '
                     f'rx="{BAR / 2}" fill="{colors["track"]}"/>')
        right = WIDTH - PAD
        if result["median"] is None or result["failures"]:
            parts.append(text(right, middle + 4, "failed" if result["failures"] else "skipped",
                              colors, "muted", 12.5, anchor="end"))
            continue
        width = max(span * result["median"] / reach, BAR)
        parts.append(f'<rect x="{left}" y="{middle - BAR / 2:.1f}" width="{width:.1f}" height="{BAR}" '
                     f'rx="{BAR / 2}" fill="{colors["accent" if mine else "other"]}"/>')
        if result["high"] > result["low"]:
            low = left + span * result["low"] / reach
            high = left + span * result["high"] / reach
            parts.append(f'<line x1="{low:.1f}" x2="{high:.1f}" y1="{middle:.1f}" y2="{middle:.1f}" '
                         f'stroke="{colors["range"]}" stroke-opacity=".3" stroke-width="1.5" stroke-linecap="round"/>')
        parts.append(text(right, middle + 4, seconds(result["median"]), colors, "text", 13, 500 if mine else 400,
                          anchor="end"))
    parts.append("</svg>")
    Path(path).write_text("\n".join(parts) + "\n")

def picture(name, title):
    escaped = html.escape(title, quote=True)
    return (f'<picture>\n  <source media="(prefers-color-scheme: dark)" srcset="../../assets/bench/{name}-dark.svg">\n'
            f'  <img src="../../assets/bench/{name}.svg" alt="{escaped}">\n</picture>\n')


def render(results, repo):
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
    lines.append("the clock includes the entire command. warmups are excluded. tables use successful runs only; failures are counted and a tool with any failed run has no chart bar. CPU is user plus system time, memory is the median command peak, and disk is the median free-space change. bars show the median; the thin line on each spans the fastest and slowest run. `hero-*` cards repeat the lan results with a verdict against the fastest other tool, for the README.\n")
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
        title = TITLES[scenario]
        subtitle = f"{sample['count']:,} \u00d7 {size(sample['file_size'])} \u00b7 {profile} \u00b7 {fs}"
        name = f"{scenario}-{profile}-{fs}"
        lines += [f"## {scenario}: {title} ({profile}, {fs})\n", story(scenario, sample["count"], sample["file_size"]) + f", median of {rounds} runs.\n",
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
            if profile == "lan" and verdict(summaries):
                chart(charts / f"hero-{scenario}-{fs}{'-dark' if dark else ''}.svg", verdict(summaries), "",
                      summaries, dark, label=title)
        lines += ["", picture(name, title)]
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
                  f"{sample['count']:,} files \u00b7 {size(sample['file_size'])} \u00b7 live", summaries, dark)
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
