"""render measured results as light and dark SVG cards and a markdown page."""
import argparse
from collections import Counter, defaultdict
from functools import cache
import html
import json
from pathlib import Path
import re
from statistics import median

# what each network profile stands for, in words a reader knows from their own connection, in the
# order the results lead with: the links most people have before the lab's own
LINKS = {"home": "home broadband", "poor": "poor connection", "lan": "local network"}
TITLES = {"s1": "one large file", "s2r": "many files through a redirect", "s3": "tiny files",
          "s4": "download and unpack", "s5": "same data, new project", "s6": "sync again, nothing changed",
          "s7": "resume after an interruption", "s8": "a compressed archive", "s9": "nothing changed, 10,000 files",
          "s10": "the link drops for 10 s"}


def summarize(rows):
    """one tool's runs in one case: median, fastest and slowest passed run, failure counts, medians
    of the other measures, the most requests any run had in flight to one host and how many runs
    went over the budget."""
    successful = [r for r in rows if r.get("status") == "passed"]
    times = [r["wall_seconds"] for r in successful]
    def metric(name):
        values = [r[name] for r in successful if r.get(name) is not None]
        return median(values) if values else None
    cpu = [r.get("user_seconds", 0) + r.get("sys_seconds", 0) for r in successful]
    peaks = [max(r["peak_in_flight"].values(), default=0) for r in rows if r.get("peak_in_flight") is not None]
    process = [r["process"]["wall_seconds"] for r in successful if r.get("process")]
    return dict(clock=next((r["clock"] for r in successful if r.get("clock")), None),
                import_seconds=metric("import_seconds"), process=median(process) if process else None,
                median=median(times) if times else None, low=min(times) if times else None,
                high=max(times) if times else None, failures=sum(r.get("status") == "failed" for r in rows),
                timeouts=sum(r.get("status") == "failed" and bool(r.get("timeout")) for r in rows),
                skipped=sum(r.get("status") == "skipped" for r in rows), rounds=len(rows),
                cpu=median(cpu) if cpu else None, memory=metric("maxrss_bytes"), disk=metric("disk_bytes"),
                requests=metric("requests"), connections=metric("connections"), body_bytes_ratio=metric("body_bytes_ratio"),
                in_flight=max(peaks) if peaks else None, over_budget=sum(bool(r.get("over_budget")) for r in rows))


def link(profile, shaping=None):
    """a network profile in plain words, with its rate, round trip and loss when its shaping is
    known, such as home broadband, about 100 Mbit and 20 ms."""
    name = LINKS.get(profile, profile)
    if not shaping:
        return name
    delay, loss, rate = shaping["delay_ms"], shaping["loss_percent"], shaping["rate_mbit"]
    if not (delay or loss or rate):
        return f"{name}, no added delay or limit"
    facts = [f"{rate} Mbit" if rate else "no rate limit", f"{delay} ms"] + ([f"{loss}% loss"] if loss else [])
    return f"{name}, {'about ' if rate else ''}{', '.join(facts[:-1])} and {facts[-1]}"


def lead(profile):
    """where a profile's results go on the page: the links in LINKS in their order, then the rest by
    name."""
    order = list(LINKS)
    return (order.index(profile), "") if profile in LINKS else (len(order), profile)


def no_bar(result):
    """what a chart row without a bar says: a tool that only ran out of time did not fail."""
    if not result["failures"]:
        return "skipped"
    return "timed out" if result.get("timeouts") == result["failures"] else "failed"


def shown(value):
    """a value as the table shows it, or n/a."""
    return "n/a" if value is None else value


def in_flight(entry):
    """the most requests a tool's runs had in flight to one host, and how many runs went over the
    budget, or n/a without a server log."""
    if entry.get("in_flight") is None:
        return "n/a"
    over = entry.get("over_budget")
    return f"{entry['in_flight']}" + (f", over the budget in {over} of {entry['rounds']} runs" if over else "")


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


# the site's serif, the latin subset @fontsource-variable/source-serif-4 5.3.1 publishes, with its OFL
# in fonts/. each svg carries it inline, since github shows svgs as images that load nothing else
FONTS = Path(__file__).with_name("fonts")
SERIF = "'Source Serif 4', Georgia, serif"

# what each scenario does, in words a reader needs no benchmark design for
STORIES = {"s1": "{files} of {size}, downloaded and checked",
           "s2r": "{files} of {size} each, each link sent on to a second host, downloaded and checked",
           "s3": "{files} of {size} each, downloaded and checked",
           "s4": "a zip of {files} of {size} each, downloaded, checked and unpacked",
           "s5": "{files} already in the cache, placed into a new project",
           "s6": "{files} already in place, each tool confirms nothing changed",
           "s7": "a {size} download stopped at 10 s, then run again to finish",
           "s8": "a tar.gz of {files} of csv, about {size} each, downloaded, checked and unpacked",
           "s9": "{files} already in place, each tool confirms nothing changed",
           "s10": "a {size} download whose link drops at 10 s for 10 s, with nothing stopped"}

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
    """the rows of every runs.jsonl under results and their meta.json, oldest first; ValueError for
    mixed conditions, a run recorded twice or live bytes off the reference."""
    paths = sorted(results.rglob("runs.jsonl"))
    if not paths:
        raise ValueError("no runs.jsonl found")
    all_rows, metas = [], []
    live_origins = {}
    for path in paths:
        meta, records = read_campaign(path)
        datasets = {dataset["service"]: dataset for dataset in meta.get("datasets", [])}
        if len(datasets) != len(meta.get("datasets", [])):
            raise ValueError(f"live metadata repeats a service in {path.with_name('meta.json')}")
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
            if row["kind"] == "live":
                check_live(row, path, datasets, live_origins)
        all_rows.extend(records)
        metas.append(meta)
    return all_rows, sorted(metas, key=lambda meta: meta.get("date", ""))


def read_campaign(path):
    """the metadata beside a runs.jsonl and its rows; ValueError when the metadata is missing or
    names no kind and no quick setting."""
    meta_path = path.with_name("meta.json")
    if not meta_path.is_file():
        raise ValueError(f"missing metadata for {path}")
    meta = json.loads(meta_path.read_text())
    if not isinstance(meta, dict) or meta.get("kind") not in ("lab", "live") or not isinstance(meta.get("quick"), bool):
        raise ValueError(f"invalid campaign metadata in {meta_path}")
    return meta, [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def check_live(row, path, datasets, origins):
    """ValueError unless a live row comes from the one campaign of its service and matches that
    campaign's reference dataset; records the campaign in origins."""
    service = row["service"]
    if service in origins and origins[service] != path:
        raise ValueError(f"live service {service} was measured twice; render campaigns separately")
    origins[service] = path
    dataset = datasets.get(service)
    if dataset is None or not dataset.get("expected_sha256"):
        raise ValueError(f"live metadata has no reference dataset for {service}")
    expected = dataset["expected_sha256"]
    if (row.get("count"), row.get("file_size")) != (len(expected), dataset.get("bytes")):
        raise ValueError(f"mixed live benchmark conditions for {service}")
    if row.get("status") == "passed" and "files" in row and Counter(row["files"].values()) != Counter(expected.values()):
        raise ValueError(f"live checked bytes do not match metadata for {service}")


def group(rows, metas):
    """lab cases in page order and live services by name, each with its file count, size and tool
    summaries, warmups left out; ValueError when a case mixes sizes or is measured twice."""
    conditions = {}
    lab = defaultdict(lambda: defaultdict(list))
    live = defaultdict(lambda: defaultdict(list))
    for row in rows:
        if row.get("warmup") or row.get("kind") not in ("lab", "live"):
            continue
        if row["kind"] == "live":
            if "tool" in row:
                live[row["service"]][row["tool"]].append(row)
            continue
        key = (row["scenario"], row["profile"], row["filesystem"])
        condition = (row.get("quick"), row.get("count"), row.get("file_size"))
        if conditions.setdefault(key, condition) != condition:
            raise ValueError(f"mixed benchmark conditions for {key}; render campaigns separately")
        lab[key][row["tool"]].append(row)
    check_chunks([meta for meta in metas if meta.get("kind") == "lab"])
    shaping = network_profiles(metas)
    cases = []
    for scenario, profile, fs in sorted(lab, key=lambda key: (lead(key[1]), key)):
        _, count, file_size = conditions[scenario, profile, fs]
        cases.append(dict(scenario=scenario, profile=profile, filesystem=fs, count=count, file_size=file_size,
                          link=link(profile, shaping.get(profile)), summaries=summaries(lab[scenario, profile, fs])))
    # a dropped link's cost is set against the same tool's s1, the same file on the same link
    for case in cases:
        if case["scenario"] == "s10" and conditions.get(("s1", case["profile"], case["filesystem"]), ())[1:] == (
                case["count"], case["file_size"]):
            case["unbroken"] = summaries(lab["s1", case["profile"], case["filesystem"]])
    services = {}
    for service in sorted(live):
        sample = next(iter(live[service].values()))[0]
        services[service] = dict(count=sample["count"], file_size=sample["file_size"],
                                 summaries=summaries(live[service]))
    return cases, services


def check_chunks(campaigns):
    """ValueError unless lab campaigns read as one: a campaign may be measured in chunks, as long
    as each lists its cases, no case is measured twice and none mixes smoke and full runs."""
    measured = [(case["filesystem"], case["profile"], case["id"]) for meta in campaigns for case in meta.get("cases") or []]
    if len(campaigns) > 1 and (any(not meta.get("cases") for meta in campaigns)
                               or len(measured) != len(set(measured))
                               or len({meta.get("quick") for meta in campaigns}) > 1):
        raise ValueError("lab campaigns measure a case twice or mix smoke and full runs; render them separately")


def summaries(runs):
    """each tool's runs summarized, in the order the tools first ran."""
    return {tool: summarize(values) for tool, values in runs.items()}


def files(count):
    """a file count in words, such as 1 file or 1,000 files."""
    return f"{count:,} file" + ("" if count == 1 else "s")


def card_name(case):
    return f"{case['scenario']}-{case['profile']}-{case['filesystem']}"


def subtitle(case):
    return f"{files(case['count'])} of {size(case['file_size'])}, {case['link']}, {case['filesystem']}"


def network_profiles(metas):
    """each profile's recorded shaping, delay, loss and rate first, from the first campaign that
    records them."""
    return next((meta["network_profiles"] for meta in metas if meta.get("network_profiles")), {})


def draw(charts, cases, services):
    """every card, light and dark, in place of an earlier render's: one per case and live service, and
    a hero from each scenario and filesystem's first case on the page when it has a verdict."""
    charts.mkdir(parents=True, exist_ok=True)
    # every card here is generated, so a card the new results do not produce, such as a hero
    # without a verdict, must not survive from an earlier render
    for old in charts.glob("*.svg"):
        old.unlink()
    led = set()
    for case in cases:
        title = TITLES[case["scenario"]]
        first = (case["scenario"], case["filesystem"]) not in led
        led.add((case["scenario"], case["filesystem"]))
        # a hero never falls back to another link, which would pick the link that flatters
        hero = verdict(case["summaries"]) if first else None
        for suffix, dark in (("", False), ("-dark", True)):
            chart(charts / f"{card_name(case)}{suffix}.svg", title, subtitle(case), case["summaries"], dark)
            if hero:
                chart(charts / f"hero-{case['scenario']}-{case['filesystem']}{suffix}.svg", hero, "",
                      case["summaries"], dark, label=f"{title}, {link(case['profile'])}")
    for service, measured in services.items():
        for suffix, dark in (("", False), ("-dark", True)):
            chart(charts / f"live-{service}{suffix}.svg", service,
                  f"{measured['count']:,} files of {size(measured['file_size'])}, live", measured["summaries"], dark)


# what s7 and s10 count of a download asked for again
REFETCH = "server body bytes / dataset size"
# the clock of a library timed around its calls, as tools.py names it
WARM_CALL = "call inside a warm interpreter"
METHOD = ("a command-line tool's clock runs from its start to its exit, including interpreter startup, resolution and verification. pooch, a library, is timed around its calls inside an interpreter that has already imported it, with the import beside it. warmups are excluded. the server's log gives each run's connections and the most requests it had in flight to one host, HTTP/2 streams included: a request counts from its arrival until it was logged, and for at least a round trip, since a client sends its next request on a slot only after the last answer reaches it; a run past the budget is counted beside the tool, not failed. tables use successful runs only; failures are counted, a run past its configured deadline counts as timed out, and a tool with any failed or timed-out run has no chart bar. CPU is user plus system time, memory is the median command peak, and disk is the median free-space change. bars show the median; the thin line on each spans the fastest and slowest run. `hero-*` cards repeat each scenario's first result below, home broadband where it was measured, with a verdict against the fastest other tool, for the README.\n")


def page(rows, metas, cases, services):
    """the results page in markdown: the conditions, a section per lab case, the live services and
    the notes each run left."""
    lines = ["# benchmark results\n", "measurements from the recorded commands and checked outputs.\n", "## conditions\n"]
    if any(meta.get("quick") for meta in metas):
        lines.append("these are smoke-test sizes and rounds, not full benchmark results.\n")
    lines += machines(metas)
    lines += networks(metas)
    lines.append(METHOD)
    warmup_failures = sum(bool(row.get("warmup")) and row.get("status") == "failed" for row in rows)
    if warmup_failures:
        lines.append(f"{warmup_failures} warmup commands failed; their records remain in runs.jsonl.\n")
    for case in cases:
        lines += case_section(case)
    if any(row.get("kind") == "live" for row in rows):
        lines += ["## live services\n", "live timings show ranges without ratios. file comparisons retain layout and content differences.\n"]
    for service, measured in services.items():
        lines += live_section(service, measured)
    lines += notes(rows)
    lines += ["", "sources: `meta.json` and `runs.jsonl` in the supplied results directory. [benchmark design](benchmark-design.md) defines the cases and fairness rules.\n"]
    return "\n".join(lines)


def networks(metas):
    """the shaping of each network profile as a table, home and poor links first."""
    shaping = network_profiles(metas)
    if not shaping:
        return []
    lines = ["| profile | stands for | round-trip delay | loss per direction | server egress | queue at the server |",
             "|---|---|---:|---:|---:|---:|"]
    for name in sorted(shaping, key=lead):
        shape = shaping[name]
        rate = f"{shape['rate_mbit']} Mbit/s" if shape["rate_mbit"] else "unlimited"
        queue = f"{shape['queue_ms'] / 1000:g} s, {shape['limit_packets']:,} packets" if shape.get("limit_packets") else "none"
        lines.append(f"| {name} | {link(name)} | {shape['delay_ms']} ms | {shape['loss_percent']}% | {rate} | {queue} |")
    return lines + [""]


def notes(rows):
    """a line for each skipped run and each live comparison."""
    lines = []
    for row in rows:
        if row.get("status") == "skipped":
            lines.append(f"- {row.get('service', row.get('tool', 'run'))} skipped: {row.get('reason', 'not recorded')}")
        if row.get("kind") == "comparison":
            lines.append(f"- {row['service']} round {row['round']}: `{row['comparison']['status']}`; differences: `{json.dumps(row['comparison'], sort_keys=True)}`")
    return lines


def machines(metas):
    """each campaign's machine, versions and cases as list lines; chunks of one campaign share a
    machine and versions, which are shown once."""
    lines = []
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
        if meta.get("budget"):
            setting.append(f"- politeness budget: at most {meta['budget']} requests in flight to one host, for every tool")
        cases = ", ".join(sorted({f"{case['filesystem']}/{case['profile']}/{case['id']}" for case in meta.get("cases") or []}))
        recorded = f"- recorded `{meta.get('date', 'unknown')}`" + (f": {cases}" if cases else f": {meta.get('kind', 'unknown')} run")
        if meta.get("filesystems"):
            recorded += f" on `{json.dumps(meta['filesystems'], sort_keys=True)}`"
        if setting == shared:
            lines.insert(len(lines) - 1, recorded)
            continue
        shared = setting
        lines += setting + [recorded, ""]
    return lines


def case_section(case):
    """a lab case's heading, story, table of every tool and its card."""
    summaries = case["summaries"]
    title = TITLES[case["scenario"]]
    rounds = max(item["rounds"] for item in summaries.values())
    lines = [f"## {case['scenario']}: {title}, {link(case['profile'])} ({case['filesystem']})\n",
             f"{case['link']}: {story(case['scenario'], case['count'], case['file_size'])}, median of {rounds} runs.\n",
             "| tool | median | min to max | ratio to fetchloom | CPU | peak memory | disk | requests | connections | most in flight to a host | failures |",
             "|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|"]
    own = summaries.get("fetchloom", {})
    base = None if own.get("failures") else own.get("median")
    for tool, entry in sorted(summaries.items(), key=lambda item: item[1]["median"] or float("inf")):
        ratio_text = "n/a" if entry["failures"] else ratio(entry["median"], base)
        lines.append(f"| {tool} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {ratio_text} | {seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['disk'])} | {shown(entry['requests'])} | {shown(entry['connections'])} | {in_flight(entry)} | {failures(entry)} |")
    for tool, entry in summaries.items():
        if entry.get("clock") == WARM_CALL:
            lines.append(f"\n{tool}'s times are its calls inside a warm interpreter, as a notebook makes them, and its CPU is "
                         f"theirs: importing it took {seconds(entry['import_seconds'])} more, and its whole process "
                         f"{seconds(entry['process'])}.")
    if case["scenario"] == "s10":
        unbroken = case.get("unbroken", {})
        lines += ["", f"| tool | with the drop | unbroken, s1 on this link | added | {REFETCH} |", "|---|---:|---:|---:|---:|"]
        for tool, entry in summaries.items():
            plain = unbroken.get(tool, {}).get("median")
            added = f"{entry['median'] - plain:+.2f} s" if entry["median"] is not None and plain is not None else "n/a"
            value = entry["body_bytes_ratio"]
            lines.append(f"| {tool} | {seconds(entry['median'])} | {seconds(plain)} | {added} | "
                         f"{f'{value:.3f}x' if value is not None else 'n/a'} |")
        lines.append("\nthe link drops at 10 s for 10 s, both ways, and nothing is stopped: new packets are lost while those "
                     "already queued still arrive. the unbroken time is the same tool's median in s1, the same file on the same link.\n")
    if case["scenario"] == "s7":
        lines += ["", f"| tool | {REFETCH} |", "|---|---:|"]
        for tool, entry in summaries.items():
            value = entry["body_bytes_ratio"]
            lines.append(f"| {tool} | {value:.3f}x |" if value is not None else f"| {tool} | n/a |")
        lines.append("\nwall time is the second attempt. CPU adds both attempts, peak memory is the larger of the two and bytes cover both; interruption is at 10 s, with a 5 s kill grace period.\n")
    if case["scenario"] in ("s7", "s10"):
        lines.append(f"{REFETCH} counts the body bytes of the server's whole and partial answers for the file, so it says what a "
                     "tool asked for again; the server cannot see packets the network sent twice.\n")
    return lines + ["", picture(card_name(case), title)]


def live_section(service, measured):
    """a live service's heading, table of both clients and its card."""
    lines = [f"### {service}\n", "| tool | median | min to max | CPU | peak memory | disk | requests | failures |", "|---|---:|---|---:|---:|---:|---:|---:|"]
    for name, entry in measured["summaries"].items():
        lines.append(f"| {name} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['disk'])} | n/a | {entry['failures']}/{entry['rounds']} |")
    return lines + ["", picture(f"live-{service}", service)]


def render(results, repo):
    """the svg cards in docs/assets/bench and docs/dev/performance/benchmarks.md from every campaign
    under results; ValueError, before any old card goes, for results that do not read as one."""
    rows, metas = load_results(results)
    cases, services = group(rows, metas)
    markdown = page(rows, metas, cases, services)
    charts = repo / "docs" / "assets" / "bench"
    draw(charts, cases, services)
    output = repo / "docs" / "dev" / "performance" / "benchmarks.md"
    output.write_text(markdown)
    print(f"wrote {output} and {charts}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("results", type=Path)
    args = parser.parse_args()
    render(args.results.resolve(), Path(__file__).resolve().parent.parent)


if __name__ == "__main__":
    main()
