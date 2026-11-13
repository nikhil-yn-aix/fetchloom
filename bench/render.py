"""Render measured results as light and dark SVG cards and a markdown page."""
import argparse
from collections import Counter, defaultdict
from functools import cache
import html
import json
from pathlib import Path
import re
from statistics import median

TITLES = {"s1": "one large file", "s2": "many small files", "s3": "tiny files",
          "s4": "download and unpack", "s5": "same data, new project", "s6": "sync again, nothing changed",
          "s7": "resume after an interruption", "s8": "a compressed archive", "s9": "nothing changed, 10,000 files"}


def summarize(rows):
    """one tool's runs in one case: median, fastest and slowest passed run, failure counts, and
    medians of the other measures."""
    successful = [r for r in rows if r.get("status") == "passed"]
    times = [r["wall_seconds"] for r in successful]
    def metric(name):
        values = [r[name] for r in successful if r.get(name) is not None]
        return median(values) if values else None
    cpu = [r.get("user_seconds", 0) + r.get("sys_seconds", 0) for r in successful]
    return dict(median=median(times) if times else None, low=min(times) if times else None,
                high=max(times) if times else None, failures=sum(r.get("status") == "failed" for r in rows),
                timeouts=sum(r.get("status") == "failed" and bool(r.get("timeout")) for r in rows),
                skipped=sum(r.get("status") == "skipped" for r in rows), rounds=len(rows),
                cpu=median(cpu) if cpu else None, memory=metric("maxrss_bytes"), disk=metric("disk_bytes"),
                requests=metric("requests"), download_ratio=metric("download_ratio"))


def no_bar(result):
    """what a chart row without a bar says: a tool that only ran out of time did not fail."""
    if not result["failures"]:
        return "skipped"
    return "timed out" if result.get("timeouts") == result["failures"] else "failed"


def failures(entry):
    """failures over rounds, such as 2/5, with timed out when every failure was one."""
    text = f"{entry['failures']}/{entry['rounds']}"
    return text + " timed out" if entry["failures"] and entry.get("timeouts") == entry["failures"] else text


def seconds(value):
    """a duration in ms, s or min, or n/a."""
    if value is None:
        return "n/a"
    if value < 1:
        return f"{value * 1000:.0f} ms"
    if value < 60:
        return f"{value:.2f} s"
    return f"{value / 60:.2f} min"


def size(value):
    """a byte count in binary units, such as 1.5 KiB, or n/a."""
    if value is None:
        return "n/a"
    for unit in ("B", "KiB", "MiB", "GiB", "TiB"):
        if abs(value) < 1024 or unit == "TiB":
            return f"{value:.1f}".removesuffix(".0") + f" {unit}"
        value /= 1024


def ratio(value, base):
    """value over base, such as 1.23x, even (<5%) within 5% either way, or n/a without a value
    and a positive base."""
    if value is None or base is None or base <= 0:
        return "n/a"
    result = value / base
    return "even (<5%)" if abs(result - 1) < .05 else f"{result:.2f}x"


# source serif 4 for every word and number, as on the site: the latin subset
# @fontsource-variable/source-serif-4 5.3.1 publishes, unchanged, under the OFL beside it in fonts/.
# it rides inside each svg, since github shows svgs as images that load nothing else.
FONTS = Path(__file__).with_name("fonts")
SERIF = "'Source Serif 4', Georgia, serif"

# what each scenario does, in words a reader needs no benchmark design for
STORIES = {"s1": "{files} of {size}, downloaded and checked",
           "s2": "{files} of {size} each, downloaded and checked",
           "s3": "{files} of {size} each, downloaded and checked",
           "s4": "a zip of {files} of {size} each, downloaded, checked and unpacked",
           "s5": "{files} already in the cache, placed into a new project",
           "s6": "{files} already in place, each tool confirms nothing changed",
           "s7": "a {size} download stopped at 10 s, then run again to finish",
           "s8": "a tar.gz of {files} of csv, about {size} each, downloaded, checked and unpacked",
           "s9": "{files} already in place, each tool confirms nothing changed"}

# the site's stylesheet, whose colors the cli's themes share and the cards take
STYLESHEET = Path(__file__).resolve().parents[1] / "docs-site/theme/assets/stylesheets/fetchloom.css"


def mix(color, ground, share):
    """``share`` of ``color`` laid over ``ground``, both #rrggbb."""
    channels = [round(share * int(color[i:i + 2], 16) + (1 - share) * int(ground[i:i + 2], 16)) for i in (1, 3, 5)]
    return "#" + "".join(f"{channel:02x}" for channel in channels)


@cache
def palette(dark):
    """the card's colors, from the site's light or dark scheme: a label's paper, its ink and muted
    text, its accent for fetchloom, and for every other tool a grey between muted text and track."""
    css = STYLESHEET.read_text(encoding="utf-8")
    scheme = "slate" if dark else "default"
    block = re.search(r'\[data-md-color-scheme="' + scheme + r'"\] \{([^}]*)\}', css)[1]
    site = dict(re.findall(r"--fl-(\w+): (#[0-9a-f]{6});", block))
    return dict(card=site["surface"], edge=site["hair"], text=site["ink"], muted=site["muted"], track=site["track"],
                accent=site["accent"], other=mix(site["muted"], site["track"], .45), range=site["ink"])

WIDTH = 760
PAD = 32
NAMES = 128
ROW = 32
BAR = 12
TIMES = 90


def font_faces():
    """the card's font as an @font-face rule carrying the file inline, with figures that line up."""
    import base64
    data = base64.b64encode((FONTS / "source-serif-4-latin-wght-normal.woff2").read_bytes()).decode()
    return ("@font-face{font-family:'Source Serif 4';font-weight:200 900;"
            f"src:url(data:font/woff2;base64,{data}) format('woff2')}}"
            "text{font-variant-numeric:lining-nums tabular-nums}")

def times(result):
    """a ratio as people say it: 3x, 1.6x, 95x."""
    shown = f"{result:.0f}" if result >= 10 else f"{result:.1f}".removesuffix(".0")
    return f"{shown}\u00d7"


def ordered(summaries):
    """tools fastest first, those with a failure or no median last."""
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
    """what a scenario does in words, with its file count and size."""
    files = f"{count:,} file" + ("" if count == 1 else "s")
    return STORIES.get(scenario, "{files}").format(files=files, size=size(file_size))


def text(x, y, value, colors, role="text", size=13, weight=400, anchor="start"):
    """an svg text element; the value is escaped."""
    return (f'<text x="{x:.1f}" y="{y:.1f}" fill="{colors[role]}" font-family="{SERIF}" font-size="{size}" '
            f'font-weight="{weight}" text-anchor="{anchor}">{html.escape(value)}</text>')


def chart(path, title, subtitle, summaries, dark=False, label=None):
    """a card of tools, fastest first. with a label it is a hero card: label, verdict, bars."""
    colors = palette(dark)
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
             f'<rect x=".5" y=".5" width="{WIDTH - 1}" height="{height - 1}" fill="{colors["card"]}" '
             f'stroke="{colors["edge"]}"/>']
    if label:
        parts += [text(PAD, 42, label, colors, "muted", 14),
                  text(PAD, 86, title, colors, size=40, weight=560)]
    else:
        parts += [text(PAD, 56, title, colors, size=30, weight=560),
                  text(PAD, 80, subtitle, colors, "muted", 13)]
    for index, (name, result) in enumerate(rows):
        middle = top + ROW * index + ROW / 2
        mine = name == "fetchloom"
        parts.append(text(PAD, middle + 4, name, colors, "accent" if mine else "text", 14, 600 if mine else 400))
        parts.append(f'<rect x="{left}" y="{middle - BAR / 2:.1f}" width="{span}" height="{BAR}" '
                     f'fill="{colors["track"]}"/>')
        right = WIDTH - PAD
        if result["median"] is None or result["failures"]:
            parts.append(text(right, middle + 4, no_bar(result), colors, "muted", 14, anchor="end"))
            continue
        width = max(span * result["median"] / reach, BAR)
        parts.append(f'<rect x="{left}" y="{middle - BAR / 2:.1f}" width="{width:.1f}" height="{BAR}" '
                     f'fill="{colors["accent" if mine else "other"]}"/>')
        if result["high"] > result["low"]:
            low = left + span * result["low"] / reach
            high = left + span * result["high"] / reach
            parts.append(f'<line x1="{low:.1f}" x2="{high:.1f}" y1="{middle:.1f}" y2="{middle:.1f}" '
                         f'stroke="{colors["range"]}" stroke-opacity=".3" stroke-width="1.5"/>')
        parts.append(text(right, middle + 4, seconds(result["median"]), colors, "text", 14, 600 if mine else 400,
                          anchor="end"))
    parts.append("</svg>")
    Path(path).write_text("\n".join(parts) + "\n")

def picture(name, title):
    """markdown for a card that follows the reader's light or dark theme."""
    escaped = html.escape(title, quote=True)
    return (f'<picture>\n  <source media="(prefers-color-scheme: dark)" srcset="../../assets/bench/{name}-dark.svg">\n'
            f'  <img src="../../assets/bench/{name}.svg" alt="{escaped}">\n</picture>\n')


def load_results(results):
    """every runs.jsonl under results, each with its meta.json; ValueError for mixed conditions, a
    run recorded twice or live bytes off the reference. returns the rows and the metadata oldest
    first."""
    paths = sorted(results.rglob("runs.jsonl"))
    if not paths:
        raise ValueError("no runs.jsonl found")
    all_rows, metas = [], []
    live_origins = {}
    for path in paths:
        meta_path = path.with_name("meta.json")
        if not meta_path.is_file():
            raise ValueError(f"missing metadata for {path}")
        meta = json.loads(meta_path.read_text())
        if not isinstance(meta, dict) or meta.get("kind") not in ("lab", "live") or not isinstance(meta.get("quick"), bool):
            raise ValueError(f"invalid campaign metadata in {meta_path}")
        records = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
        datasets = {dataset["service"]: dataset for dataset in meta.get("datasets", [])}
        if len(datasets) != len(meta.get("datasets", [])):
            raise ValueError(f"live metadata repeats a service in {meta_path}")
        seen = set()
        for row in records:
            if row.get("kind") == "comparison" and meta["kind"] == "live":
                continue
            if row.get("kind") != meta["kind"] or row.get("quick") != meta["quick"]:
                raise ValueError(f"mixed benchmark conditions in {path}")
            if "tool" not in row:
                if meta["kind"] == "live" and row.get("status") == "skipped":
                    continue
                raise ValueError(f"a measured row has no tool in {path}")
            if "round" in row:
                identity = (row["kind"], row.get("filesystem"), row.get("profile"), row.get("scenario", row.get("service")),
                            row["tool"], row["round"])
                if identity in seen:
                    raise ValueError(f"duplicate measured row in {path}: {identity}")
                seen.add(identity)
            if row["kind"] != "live":
                continue
            service = row["service"]
            if service in live_origins and live_origins[service] != path:
                raise ValueError(f"live service {service} was measured twice; render campaigns separately")
            live_origins[service] = path
            dataset = datasets.get(service)
            if dataset is None or not dataset.get("expected_sha256"):
                raise ValueError(f"live metadata has no reference dataset for {service}")
            expected = dataset["expected_sha256"]
            if (row.get("count"), row.get("file_size")) != (len(expected), dataset.get("bytes")):
                raise ValueError(f"mixed live benchmark conditions for {service}")
            if row.get("status") == "passed" and "files" in row and Counter(row["files"].values()) != Counter(expected.values()):
                raise ValueError(f"live checked bytes do not match metadata for {service}")
        all_rows.extend(records)
        metas.append(meta)
    return all_rows, sorted(metas, key=lambda meta: meta.get("date", ""))


def render(results, repo):
    """the svg cards in docs/assets/bench and docs/dev/performance/benchmarks.md, from every
    campaign under results."""
    all_rows, metas = load_results(results)
    rows = [row for row in all_rows if not row.get("warmup") and row.get("kind") in ("lab", "live")]
    conditions = {}
    for row in rows:
        if row.get("kind") != "lab":
            continue
        key = (row["scenario"], row["profile"], row["filesystem"])
        condition = (row.get("quick"), row.get("count"), row.get("file_size"))
        if key in conditions and conditions[key] != condition:
            raise ValueError(f"mixed benchmark conditions for {key}; render campaigns separately")
        conditions[key] = condition
    campaigns = [meta for meta in metas if meta.get("kind") == "lab"]
    # a campaign may be measured in chunks, as long as no case is measured twice
    measured = [(case["filesystem"], case["profile"], case["id"]) for meta in campaigns for case in meta.get("cases") or []]
    if len(campaigns) > 1 and (any(not meta.get("cases") for meta in campaigns)
                               or len(measured) != len(set(measured))
                               or len({meta.get("quick") for meta in campaigns}) > 1):
        raise ValueError("lab campaigns measure a case twice or mix smoke and full runs; render them separately")
    charts = repo / "docs" / "assets" / "bench"
    charts.mkdir(parents=True, exist_ok=True)
    # every card here is generated, so a card the new results do not produce, such as a hero
    # without a verdict, must not survive from an earlier render
    for old in charts.glob("*.svg"):
        old.unlink()
    lines = ["# benchmark results\n", "measurements from the recorded commands and checked outputs.\n", "## conditions\n"]
    if any(meta.get("quick") for meta in metas) or any(row.get("quick") for row in rows):
        lines.append("these are smoke-test sizes and rounds, not full benchmark results.\n")
    shared = None
    for meta in metas:
        machine = meta.get("machine", {})
        setting = [f"- CPU: {machine.get('cpu', 'unknown')}; cores: {machine.get('cores', 'unknown')}; memory: {machine.get('memory', 'unknown')}",
                   f"- kernel: `{meta.get('kernel', 'unknown')}`; seed: `{meta.get('seed', 'unknown')}`",
                   f"- fetchloom: `{meta.get('tool_versions', {}).get('fetchloom', 'unknown')}`; repository: `{meta.get('repo_head', 'unknown')}`",
                   "- versions: " + "; ".join(f"{name} `{version}`" for name, version in meta.get("tool_versions", {}).items())]
        if meta.get("cpu_affinity") is not None:
            setting.append(f"- measured CPUs: `{meta['cpu_affinity']}`; nginx threads: `{meta.get('nginx_cpu_affinity', {})}`")
        if meta.get("fetchloom_binary"):
            setting.append(f"- supplied executable: `{meta['fetchloom_binary']['path']}`; SHA-256: `{meta['fetchloom_binary']['sha256']}`")
        cases = ", ".join(sorted({f"{case['filesystem']}/{case['profile']}/{case['id']}" for case in meta.get("cases") or []}))
        recorded = f"- recorded `{meta.get('date', 'unknown')}`" + (f": {cases}" if cases else f": {meta.get('kind', 'unknown')} run")
        if meta.get("filesystems"):
            recorded += f" on `{json.dumps(meta['filesystems'], sort_keys=True)}`"
        # chunks of one campaign share a machine and versions, which are shown once
        if setting == shared:
            lines.insert(len(lines) - 1, recorded)
            continue
        shared = setting
        lines += setting + [recorded, ""]
    networks = next((meta["network_profiles"] for meta in metas if meta.get("network_profiles")), {})
    if networks:
        lines += ["| profile | round-trip delay | loss per direction | server egress |", "|---|---:|---:|---:|"]
        for name, values in networks.items():
            rate = f"{values[2]} Mbit/s" if values[2] else "unlimited"
            lines.append(f"| {name} | {values[0]} ms | {values[1]}% | {rate} |")
        lines.append("")
    lines.append("the clock includes the entire command. warmups are excluded. tables use successful runs only; failures are counted, a run past its configured deadline counts as timed out, and a tool with any failed or timed-out run has no chart bar. CPU is user plus system time, memory is the median command peak, and disk is the median free-space change. bars show the median; the thin line on each spans the fastest and slowest run. `hero-*` cards repeat the lan results with a verdict against the fastest other tool, for the README.\n")
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
        files = f"{sample['count']:,} file" + ("" if sample["count"] == 1 else "s")
        subtitle = f"{files} of {size(sample['file_size'])}, {profile} network, {fs}"
        name = f"{scenario}-{profile}-{fs}"
        lines += [f"## {scenario}: {title} ({profile}, {fs})\n", story(scenario, sample["count"], sample["file_size"]) + f", median of {rounds} runs.\n",
                  "| tool | median | min to max | ratio to fetchloom | CPU | peak memory | disk | requests | failures |",
                  "|---|---:|---|---:|---:|---:|---:|---:|---:|"]
        base_summary = summaries.get("fetchloom", {})
        base = None if base_summary.get("failures") else base_summary.get("median")
        for tool, entry in sorted(summaries.items(), key=lambda item: item[1]["median"] or float("inf")):
            ratio_text = "n/a" if entry["failures"] else ratio(entry["median"], base)
            lines.append(f"| {tool} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {ratio_text} | {seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['disk'])} | {entry['requests'] if entry['requests'] is not None else 'n/a'} | {failures(entry)} |")
        if scenario == "s7":
            lines += ["", "| tool | bytes served / file size |", "|---|---:|"]
            for tool, entry in summaries.items():
                value = entry["download_ratio"]
                lines.append(f"| {tool} | {value:.3f}x |" if value is not None else f"| {tool} | n/a |")
            lines.append("\nwall time is the second attempt. CPU adds both attempts, peak memory is the larger of the two and bytes cover both; interruption is at 10 s, with a 5 s kill grace period.\n")
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
                  f"{sample['count']:,} files of {size(sample['file_size'])}, live", summaries, dark)
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
