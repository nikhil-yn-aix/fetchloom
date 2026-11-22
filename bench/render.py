"""render measured results as light and dark SVG cards and a markdown page."""
import argparse
from collections import Counter, defaultdict
from functools import cache
import html
import json
from pathlib import Path
import re
from statistics import median

import ab
import fixtures

# what each network profile stands for, in words a reader knows from their own connection, in the
# order the results lead with: the links most people have before the lab's own
LINKS = {"home": "home broadband", "poor": "poor connection", "lan": "local network", "local": "this machine's disk"}
TITLES = {"s1": "one large file", "s2r": "many files through a redirect", "s3": "tiny files",
          "s4": "download and unpack", "s5": "same data, new project", "s6": "sync again, nothing changed",
          "s7": "resume after an interruption", "s8": "a compressed archive", "s9": "nothing changed, 10,000 files",
          "s10": "the link drops for 10 s", "s11": "the team's shared folder", "s12": "the next version",
          "s13": "a full integrity check"}


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
    floors = [r.get("in_flight_floor_ms") or 0 for r in rows if r.get("peak_in_flight") is not None]
    process = [r["process"]["wall_seconds"] for r in successful if r.get("process")]
    return dict(clock=next((r["clock"] for r in successful if r.get("clock")), None),
                import_seconds=metric("import_seconds"), process=median(process) if process else None,
                median=median(times) if times else None, low=min(times) if times else None,
                high=max(times) if times else None, failures=sum(r.get("status") == "failed" for r in rows),
                timeouts=sum(r.get("status") == "failed" and bool(r.get("timeout")) for r in rows),
                skipped=sum(r.get("status") == "skipped" for r in rows), rounds=len(rows),
                cpu=median(cpu) if cpu else None, memory=metric("memory_bytes"), largest=metric("maxrss_bytes"),
                written=metric("written_bytes"), disk=metric("disk_bytes"),
                requests=metric("requests"), connections=metric("connections"), body_bytes_ratio=metric("body_bytes_ratio"),
                in_flight=max(peaks) if peaks else None, in_flight_floor_ms=max(floors, default=0),
                over_budget=sum(bool(r.get("over_budget")) for r in rows))


def link(profile, shaping=None):
    """a network profile in plain words, with its rate, round trip and loss when its shaping is
    known, such as home broadband, about 100 Mbit and 20 ms."""
    name = LINKS.get(profile, profile)
    if not shaping:
        return name
    if profile == "local":
        return f"{name}, no network"
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
           "s10": "a {size} download whose link drops at 10 s for 10 s, with nothing stopped",
           "s11": "{files} of {size} each, pushed by one teammate to a shared folder and got from it by another with an empty cache; the table times the get",
           "s12": "{files} of {size} each already in place, brought to the dataset's next version, in which one file in ten changed",
           "s13": "{files} of {size} each already in place, every byte checked against its SHA-256 from a cold disk cache"}

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


# the analysis plan, fixed before any data: a ratio is another tool's time over fetchloom's in the same
# round, and its interval decides. even means the whole interval sits inside EVEN
EVEN = (.95, 1.05)
# the control arm, the same fetchloom run again in each headline case, never a rival; on the links
# without loss its interval must sit inside CONTROL_GATE, or the machine was too noisy for a verdict.
# on the lossy link its width is the network's own
CONTROL = "fetchloom+control"
CONTROL_GATE = (.97, 1.03)
GATED = ("lan", "home", "local")
# the only cases a claim comes from, each fetchloom against one tool frozen from the pilot before
# the runs: comparisons sharing a 5% error, bonferroni-adjusted
HEADLINE = fixtures.HEADLINE
FAMILY = 1 - .05 / len(HEADLINE)
# a row that only describes needs no 10 pairs, only two for an interval at all
DESCRIBED = 2
SEED = 1729


def word(interval, rival):
    """what an interval of a rival's time over fetchloom's says: fetchloom faster, the rival faster,
    even, or inconclusive."""
    low, high = interval
    if low > EVEN[1]:
        return "fetchloom faster"
    if high < EVEN[0]:
        return f"{rival} faster"
    if EVEN[0] <= low and high <= EVEN[1]:
        return "even"
    return "inconclusive"


def over_budget(measured):
    """why runs were not at the same politeness, as a phrase after the tool's name, or None."""
    over = sum(bool(row.get("over_budget")) for row in measured.values())
    return f"went over the budget in {over} of {len(measured)} runs" if over else None


def standing(base, measured, comparison, minimum=ab.MIN_PAIRS):
    """why a comparison with fetchloom cannot stand, as a phrase after the tool's name, or None: a
    failed run, a run over the politeness budget, rounds other than fetchloom's, or fewer than
    `minimum` paired rounds. the measurements stay in the table either way."""
    failed = sum(row.get("status") != "passed" for row in measured.values())
    if failed:
        return f"failed {failed} of {len(measured)} runs"
    if over_budget(measured):
        return over_budget(measured)
    if measured.keys() != base.keys():
        return "ran other rounds than fetchloom"
    if comparison["pairs"] < minimum or comparison["interval"] is None or None in comparison["interval"]:
        return f"has {comparison['pairs']} paired rounds, fewer than {minimum}"
    return None


def judge(case, runs, comparator=None):
    """a case's comparisons and verdict: each other tool's time over fetchloom's, rounds paired, at
    FAMILY in a headline case and 95% elsewhere; the control arm's 95% interval; and the one verdict, a
    headline case's against the comparator frozen before its runs, or the reason there is none. every
    other comparison only describes, as a case outside the headline set does against the fastest
    other tool that passed every run. a reason that holds for the whole case, such as a noisy
    machine, a campaign cut short of the rounds it planned or no frozen comparator, withholds it."""
    key = (case["filesystem"], case["profile"], case["scenario"])
    headline = key in HEADLINE
    level = FAMILY if headline else .95
    planned = case.get("planned")
    plan = set(range(1, planned + 1)) if isinstance(planned, int) and planned > 0 else None
    rounds = {tool: {row["round"]: row for row in rows if isinstance(row.get("round"), int)} for tool, rows in runs.items()}
    base = rounds.get(ab.BASE, {})
    comparisons = {}
    for tool, measured in rounds.items():
        if tool == ab.BASE:
            continue
        comparison = ab.paired(base, measured, level=.95 if tool == CONTROL else level, seed=f"{SEED}/{'/'.join(key)}/{tool}")
        comparisons[tool] = dict(comparison, failed=sum(row.get("status") != "passed" for row in measured.values()),
                                 rounds=len(measured),
                                 unmet=standing(base, measured, comparison, ab.MIN_PAIRS if headline else DESCRIBED))
    control = comparisons.get(CONTROL)
    own_failed = sum(row.get("status") != "passed" for row in base.values())
    if not base:
        withheld = "fetchloom did not run"
    elif plan is None:
        withheld = "its campaign's meta.json records no planned rounds for it"
    elif base.keys() != plan:
        # a campaign stopped early stops every arm alike, so its rounds still pair; only the plan shows it
        withheld = (f"fetchloom ran {len(base)} of the {planned} planned rounds" if base.keys() < plan
                    else f"fetchloom ran rounds other than the {planned} planned")
    elif own_failed:
        withheld = f"fetchloom failed {own_failed} of {len(base)} runs"
    elif over_budget(base) or (CONTROL in rounds and over_budget(rounds[CONTROL])):
        withheld = f"fetchloom {over_budget(base) or over_budget(rounds[CONTROL])}, so no comparison is at the same politeness"
    elif headline and case["profile"] in GATED and control is None:
        withheld = "no control arm measured this machine's noise"
    elif headline and case["profile"] in GATED and control["unmet"]:
        withheld = f"the control arm {control['unmet']}, so it cannot measure this machine's noise"
    elif headline and case["profile"] in GATED and not (CONTROL_GATE[0] <= control["interval"][0]
                                                         and control["interval"][1] <= CONTROL_GATE[1]):
        withheld = (f"the control arm's 95% interval, {bounds(control)}, is not inside {CONTROL_GATE[0]} to {CONTROL_GATE[1]}, "
                    "so the machine was too noisy")
    elif headline and comparator is None:
        withheld = "no comparator was frozen for it before its runs"
    else:
        withheld = None
    summaries = case["summaries"]
    rivals = [tool for tool, comparison in comparisons.items()
              if not tool.startswith(ab.BASE + "+") and not comparison["failed"] and summaries[tool]["median"] is not None]
    # a headline case's rival is never chosen from the data that judges it
    rival = comparator if headline else min(rivals, key=lambda tool: (summaries[tool]["median"], tool)) if rivals else None
    named = f"{rival}, the frozen comparator," if headline else rival
    if withheld:
        reason = withheld
    elif rival is None:
        reason = "no other tool passed every run"
    elif rival not in comparisons:
        reason = f"{named} did not run"
    elif comparisons[rival]["unmet"]:
        reason = f"{named} {comparisons[rival]['unmet']}"
    else:
        reason = None
    # only the frozen comparator's verdict is one of the headline comparisons the 5% family error covers
    for tool, comparison in comparisons.items():
        comparison["word"] = word(comparison["interval"], tool) if headline and reason is None and tool == rival else None
    return dict(headline=headline, level=level, comparisons=comparisons, control=control, reason=reason, withheld=withheld,
                rival=rival if reason is None else None, comparator=comparator if headline else None)


def planned(metas):
    """the measured rounds each case's campaign planned before its runs, from its meta.json, as
    fs/profile/scenario."""
    return {f"{case['filesystem']}/{case['profile']}/{case['id']}": case.get("rounds")
            for meta in metas for case in meta.get("cases") or []}


def frozen(metas):
    """the comparator each headline case was frozen with before its runs, from the meta.json of the
    lab campaign that measured it, so one a later chunk recorded never judges it; ValueError when
    two campaigns froze different ones for a case."""
    chosen, recorded = {}, {}
    for meta in metas:
        cases = meta.get("cases")
        own = {f"{case['filesystem']}/{case['profile']}/{case['id']}" for case in cases or []}
        for case, tool in (meta.get("comparators") or {}).items():
            if recorded.setdefault(case, tool) != tool:
                raise ValueError(f"lab campaigns froze different comparators for {case}; render them separately")
            if not cases or case in own:
                chosen[case] = tool
    return chosen


def pick(rows):
    """the comparator to freeze for each headline case a pilot measured: the fastest other tool that
    passed every run within the budget, the control arm and fetchloom's variants never among them."""
    runs = defaultdict(lambda: defaultdict(list))
    for row in rows:
        if row.get("kind") == "lab" and not row.get("warmup") and "tool" in row:
            runs[row["filesystem"], row["profile"], row["scenario"]][row["tool"]].append(row)
    chosen = {}
    for key in HEADLINE:
        measured = {tool: summarize(values) for tool, values in runs.get(key, {}).items()
                    if not tool.startswith(ab.BASE) and not any(row.get("over_budget") for row in values)}
        passing = [tool for tool, entry in measured.items() if not entry["failures"] and entry["median"] is not None]
        if passing:
            chosen["/".join(key)] = min(passing, key=lambda tool: (measured[tool]["median"], tool))
    return chosen


def bounds(comparison):
    """an interval as the page says it, such as 0.98 to 1.04."""
    low, high = comparison["interval"]
    return f"{low:.2f} to {high:.2f}"


def percent(level):
    """a confidence level as the page says it, such as 95% or 99.375%."""
    return f"{100 * level:.3f}".rstrip("0").removesuffix(".") + "%"


def hero(case):
    """a headline case's verdict on a card, the seconds first: the label and the title, or None when
    the case has no verdict or an inconclusive one."""
    judged = case["judged"]
    rival = judged["rival"]
    if not judged["headline"] or rival is None:
        return None
    comparison = judged["comparisons"][rival]
    said = comparison["word"]
    if said == "inconclusive":
        return None
    summaries = case["summaries"]
    label = (f"{TITLES[case['scenario']]}, {link(case['profile'])}: {seconds(summaries[ab.BASE]['median'])} against "
             f"{seconds(summaries[rival]['median'])} for {rival}")
    if said == "even":
        return label, "even"
    result = comparison["median_ratio"]
    return label, f"{times(result)} faster" if said == "fetchloom faster" else f"{times(1 / result)} slower"

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
        # a case's plan is read from the campaign that lists it, so its rows must be that campaign's
        listed = {(case.get("filesystem"), case.get("profile"), case.get("id")) for case in meta.get("cases") or []}
        datasets = {dataset["service"]: dataset for dataset in meta.get("datasets", [])}
        if len(datasets) != len(meta.get("datasets", [])):
            raise ValueError(f"live metadata repeats a service in {path.with_name('meta.json')}")
        seen = set()
        for row in records:
            if row.get("kind") == "comparison" and meta["kind"] == "live":
                continue
            if row.get("kind") != meta["kind"] or row.get("quick") != meta["quick"]:
                raise ValueError(f"mixed benchmark conditions in {path}")
            if listed and row["kind"] == "lab" and (row.get("filesystem"), row.get("profile"), row.get("scenario")) not in listed:
                raise ValueError(f"a row of a case its campaign never listed in {path}")
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
    campaigns = [meta for meta in metas if meta.get("kind") == "lab"]
    check_chunks(campaigns)
    comparators = frozen(campaigns)
    plans = planned(campaigns)
    shaping = network_profiles(metas)
    cases = []
    for scenario, profile, fs in sorted(lab, key=lambda key: (lead(key[1]), key)):
        _, count, file_size = conditions[scenario, profile, fs]
        case = dict(scenario=scenario, profile=profile, filesystem=fs, count=count, file_size=file_size,
                    link=link(profile, shaping.get(profile)), summaries=summaries(lab[scenario, profile, fs]),
                    planned=plans.get(f"{fs}/{profile}/{scenario}"), runs=lab[scenario, profile, fs])
        cases.append(dict(case, judged=judge(case, lab[scenario, profile, fs], comparators.get(f"{fs}/{profile}/{scenario}"))))
    # a dropped link's cost is set against the same tool's s1, the same file on the same link
    for case in cases:
        if case["scenario"] == "s10" and conditions.get(("s1", case["profile"], case["filesystem"]), ())[1:] == (
                case["count"], case["file_size"]):
            case["unbroken"] = summaries(lab["s1", case["profile"], case["filesystem"]])
    services = {}
    for service in sorted(live):
        sample = next(iter(live[service].values()))[0]
        services[service] = dict(count=sample["count"], file_size=sample["file_size"],
                                 summaries=summaries(live[service]), judged=live_ratios(service, live[service]))
    return cases, services


def live_ratios(service, runs):
    """a live service's own tool's time over fetchloom's, rounds paired, with its 95% interval: a
    ratio that holds for that campaign alone, so it gets no verdict."""
    rounds = {tool: {row["round"]: row for row in rows if isinstance(row.get("round"), int)} for tool, rows in runs.items()}
    base = rounds.get(ab.BASE, {})
    return dict(comparisons={tool: dict(ab.paired(base, measured, seed=f"{SEED}/live/{service}/{tool}"), rounds=len(measured),
                                        failed=sum(row.get("status") == "failed" for row in measured.values()))
                             for tool, measured in rounds.items() if tool != ab.BASE})


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
    a hero for each headline case with a verdict that is not inconclusive."""
    charts.mkdir(parents=True, exist_ok=True)
    # every card here is generated, so a card the new results do not produce, such as a hero
    # without a verdict, must not survive from an earlier render
    for old in charts.glob("*.svg"):
        old.unlink()
    for case in cases:
        claimed = hero(case)
        for suffix, dark in (("", False), ("-dark", True)):
            chart(charts / f"{card_name(case)}{suffix}.svg", TITLES[case["scenario"]], subtitle(case), case["summaries"], dark)
            if claimed:
                label, said = claimed
                chart(charts / f"hero-{card_name(case)}{suffix}.svg", said, "", case["summaries"], dark, label=label)
    for service, measured in services.items():
        for suffix, dark in (("", False), ("-dark", True)):
            chart(charts / f"live-{service}{suffix}.svg", service,
                  f"{measured['count']:,} files of {size(measured['file_size'])}, live", measured["summaries"], dark)


# what each tool checks when nothing changed, which differs, so a no-op's times are never a win at
# equal checking
NOOP = ("s6", "s9")
NOOP_CHECKS = ("each tool checks something else when nothing changed: fetchloom trusts each file's size and modification "
               "time, hashing a file whose time is too recent to trust, DVC trusts its own record of each file, datalad "
               "that each file is in its store, curl asks the server with `-z`, and aria2c and pooch hash every file again.")
# what s7 and s10 count of a download asked for again
REFETCH = "server body bytes / dataset size"
# the clock of a library timed around its calls, as tools.py names it
WARM_CALL = "call inside a warm interpreter"
METHOD = ("a command-line tool's clock runs from its start to its exit, including interpreter startup, resolution and "
          "verification. pooch, a library, is timed around its calls inside an interpreter that has already imported it, with "
          "the import beside it. warmups are excluded. the server's log gives each run's connections and the most requests it "
          "had in flight to one host, HTTP/2 streams included: a request counts from its arrival until it was logged, and for "
          "at least a round trip, since a client sends its next request on a slot only after the last answer reaches it; a run "
          "past the budget is counted beside the tool, not failed. tables use successful runs only; failures are counted, a run "
          "past its configured deadline counts as timed out, and a tool with any failed or timed-out run has no chart bar. CPU "
          "is user plus system time of every process a command started, from the cgroup each run has to itself, so helpers "
          "and workers count; for pooch it is its calls' own. peak memory is the most anonymous memory those processes held "
          "at once, read from the cgroup every 20 ms, and largest process is the biggest single process's peak, as GNU time "
          "reports it. written is what a run wrote to the measured disk, from the cgroup's io.stat after a flush, and disk is "
          "the free-space change, so a link or reflink counts once. each is the median of a tool's passed runs. bars show "
          "the median; the thin line on each spans the fastest and slowest run. every tool runs 5 rounds, and each other tool's "
          "time is set over fetchloom's round by round: the table gives the median of those ratios with a paired percentile "
          "bootstrap interval, 20,000 resamples from seed 1729. "
          f"in the {len(HEADLINE)} headline cases, s1 and s2r at home and on the poor link, s5 on the local network, and s7, "
          "s10 and s12 at home, fetchloom, its control and the comparator frozen from the pilot run 5 rounds more, so each "
          f"verdict rests on 10 pairs, and every interval is {percent(FAMILY)}, since the {len(HEADLINE)} comparisons share a "
          "5% error. elsewhere intervals are 95%. only the frozen comparator's row in a headline case gets a verdict: fetchloom "
          "faster when the interval's lower end is above 1.05, the other tool faster when its upper end is below 0.95, even "
          "when the whole interval sits inside 0.95 to 1.05, and inconclusive otherwise. every other row describes and claims "
          "nothing. the control arm, fetchloom run again, is never a rival: on every link without loss its 95% interval must "
          "sit inside 0.97 to 1.03, or the machine was too noisy and the case has no verdict; on the poor link its width is the "
          "network's own. a case where fetchloom failed a run has no verdict. `hero-*` cards repeat the headline verdicts that "
          "are not inconclusive, for the README.\n")


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
        lines += ["## live services, on this machine's datacenter link\n",
                  "each service's own tool runs beside fetchloom in every round, in alternating order (ABBA), so a service's "
                  "slow minute meets both alike, and its time over fetchloom's pairs the rounds. the ratios hold for that "
                  "campaign alone, so they get no verdict and confirm none of the lab's. file comparisons retain layout and "
                  "content differences, and a content difference fails the run.\n"]
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
    # this machine's own disk is no network
    for name in sorted((name for name in shaping if name != "local"), key=lead):
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
    # in a headline case fetchloom, its control and the frozen rival run more rounds than the rest
    rounds = " or ".join(str(count) for count in sorted({item["rounds"] for item in summaries.values()}))
    judged = case["judged"]
    floor = max(item.get("in_flight_floor_ms") or 0 for item in summaries.values())
    lines = [f"## {case['scenario']}: {title}, {link(case['profile'])} ({case['filesystem']})\n",
             f"{case['link']}: {story(case['scenario'], case['count'], case['file_size'])}, median of {rounds} runs.\n",
             f"| tool | median | min to max | time over fetchloom's, {percent(judged['level'])} interval | verdict | CPU | peak memory | largest process | written | disk | requests | connections | most in flight to a host{', estimated' if floor else ''} | failures |",
             "|---|---:|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for tool, entry in sorted(summaries.items(), key=lambda item: item[1]["median"] or float("inf")):
        lines.append(f"| {tool} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {over(tool, judged)} | {said(tool, judged)} | {seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['largest'])} | {size(entry['written'])} | {size(entry['disk'])} | {shown(entry['requests'])} | {shown(entry['connections'])} | {in_flight(entry)} | {failures(entry)} |")
    lines += verdict_lines(case)
    if floor:
        lines.append(f"\nthe most in flight on this link is an estimate, with a floor of {floor} ms: with no round trip to wait "
                     f"for, a request counts only while nginx served it, less {floor} ms for its log's millisecond clock, so "
                     f"a request answered within {floor} ms is not counted, and a tool over the budget by such short "
                     "requests can go unflagged.")
    if case["scenario"] in NOOP:
        lines.append(f"\n{NOOP_CHECKS}")
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
    if case["scenario"] == "s11":
        lines += pushes(case)
    if case["scenario"] == "s13":
        lines.append("\neach tool runs its documented check of every file: fetchloom `fetchloom status --verify`, sha256sum "
                     "`sha256sum -c` over the files curl downloaded, pooch its `retrieve` of each file, which hashes the copy it "
                     "has against its known hash, and datalad `git annex fsck`. DVC has no command that hashes every file again, "
                     "and aria2c's `--check-integrity` runs only inside a download, asking the server for each file first, so "
                     "neither is here.\n")
    if case["scenario"] in ("s7", "s12"):
        lines += ["", f"| tool | {REFETCH} |", "|---|---:|"]
        for tool, entry in summaries.items():
            value = entry["body_bytes_ratio"]
            lines.append(f"| {tool} | {value:.3f}x |" if value is not None else f"| {tool} | n/a |")
    if case["scenario"] == "s7":
        lines.append("\nwall time is the second attempt. CPU adds both attempts, peak memory is the larger of the two and bytes cover both, written ones included; interruption is at 10 s, with a 5 s kill grace period.\n")
    if case["scenario"] == "s12":
        lines.append("\neach tool updates its own documented way: fetchloom and DVC sync and pull after a git pull brings the new "
                     "data.lock or data.dvc, datalad gets after `datalad update`, pooch retrieves each file by its new hash, "
                     "aria2c checks each file it has with `--check-integrity` and curl, which has no way to tell what changed, "
                     "downloads every file again. the dataset is the new version, so a tool that fetches only what changed asks "
                     "for about 0.1 of it.\n")
    if case["scenario"] in ("s7", "s10", "s12"):
        lines.append(f"{REFETCH} counts the body bytes of the server's whole and partial answers for the file, so it says what a "
                     "tool asked for again; the server cannot see packets the network sent twice.\n")
    return lines + ["", picture(card_name(case), title)]


def pushes(case):
    """the push table of the shared folder's case: each tool's push, its time over fetchloom's,
    rounds paired, with a 95% interval, and what it cost."""
    runs = case["runs"]
    pushed = {tool: {row["round"]: dict(row.get("push") or {}, status=row.get("status") if row.get("push") else "failed")
                     for row in rows if isinstance(row.get("round"), int)} for tool, rows in runs.items()}
    lines = ["", "| tool | push, median | min to max | time over fetchloom's, 95% interval | CPU | peak memory | written | disk |",
             "|---|---:|---|---:|---:|---:|---:|---:|"]
    for tool, rounds in pushed.items():
        entry = summarize(list(rounds.values()))
        ratio = ""
        if tool != ab.BASE:
            comparison = ab.paired(pushed.get(ab.BASE, {}), rounds, seed=f"{SEED}/{case['filesystem']}/{case['profile']}/s11-push/{tool}")
            ratio = over(tool, dict(comparisons={tool: dict(comparison, failed=entry["failures"], rounds=entry["rounds"])}))
        lines.append(f"| {tool} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {ratio} | "
                     f"{seconds(entry['cpu'])} | {size(entry['memory'])} | {size(entry['written'])} | {size(entry['disk'])} |")
    lines.append("\nthe push copies the first teammate's files into the shared folder, fetchloom into a folder mirror, DVC to a "
                 "local remote, datalad to a sibling and rsync as plain files. the get then fills the second teammate's project "
                 "from that folder: fetchloom syncs with it as a mirror, DVC pulls from the remote, datalad gets from the sibling "
                 "it cloned and rsync copies the files back. the lab's server answers neither, and the page cache is dropped "
                 "before each.\n")
    return lines


def over(tool, judged):
    """a tool's time over fetchloom's in the table: the median ratio and its interval, failed n of m
    for a tool with a failed run, or nothing for fetchloom itself."""
    comparison = judged["comparisons"].get(tool)
    if comparison is None:
        return ""
    if comparison["failed"]:
        return f"failed {comparison['failed']} of {comparison['rounds']}"
    if comparison["median_ratio"] is None:
        return "n/a"
    interval = comparison["interval"]
    shown_interval = f" [{interval[0]:.2f}, {interval[1]:.2f}]" if interval and None not in interval else ""
    return f"{comparison['median_ratio']:.2f}{shown_interval}"


def said(tool, judged):
    """what a tool's interval says against fetchloom, in the table's verdict column: the verdict, or
    why there is none, for a headline case's frozen comparator alone; withheld when the whole case
    has no verdict, as the line under the table says why; descriptive for every other tool."""
    comparison = judged["comparisons"].get(tool)
    if comparison is None or comparison["failed"]:
        return ""
    if tool == CONTROL:
        return "control arm, never a rival"
    if tool.startswith(ab.BASE + "+"):
        return "fetchloom variant, never a rival"
    if judged["withheld"]:
        return "withheld"
    if judged["headline"] and tool == judged["comparator"]:
        return comparison["word"] or comparison["unmet"]
    return "descriptive"


def verdict_lines(case):
    """the case's verdict, the seconds before the ratio, or why it has none, and what the control arm saw;
    a case outside the headline set describes its fastest other tool instead, with no verdict."""
    judged = case["judged"]
    rival = judged["rival"]
    if rival is None:
        lines = [f"\nno verdict: {judged['reason']}."]
    else:
        comparison = judged["comparisons"][rival]
        chosen = "the comparator frozen before these runs" if judged["headline"] else "the fastest other tool that passed every run"
        compared = (f"fetchloom {seconds(case['summaries'][ab.BASE]['median'])} against {seconds(case['summaries'][rival]['median'])} "
                    f"for {rival}, {chosen}; {rival}'s time over fetchloom's is {comparison['median_ratio']:.2f}, "
                    f"interval {bounds(comparison)}")
        lines = [f"\nverdict (headline, {percent(judged['level'])} interval): {compared}: {comparison['word']}." if judged["headline"]
                 else f"\ndescriptive, {percent(judged['level'])} interval, no verdict: {compared}."]
    control = judged["control"]
    if control is not None and control["interval"] is not None:
        noise = "" if case["profile"] in GATED else ", on this lossy link the network's own randomness"
        lines.append(f"the control arm, fetchloom run again, came to {control['median_ratio']:.2f}, 95% interval {bounds(control)}{noise}.")
    return lines


def live_section(service, measured):
    """a live service's heading, table of both clients with the other's time over fetchloom's, and
    its card."""
    judged = measured["judged"]
    lines = [f"### {service}\n", "| tool | median | min to max | time over fetchloom's, 95% interval | paired rounds | CPU | largest process | disk | requests | failures |",
             "|---|---:|---|---:|---:|---:|---:|---:|---:|---:|"]
    for name, entry in measured["summaries"].items():
        pairs = judged["comparisons"].get(name, {}).get("pairs", "")
        lines.append(f"| {name} | {seconds(entry['median'])} | {seconds(entry['low'])} to {seconds(entry['high'])} | {over(name, judged)} | {pairs} | {seconds(entry['cpu'])} | {size(entry['largest'])} | {size(entry['disk'])} | n/a | {entry['failures']}/{entry['rounds']} |")
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
    parser.add_argument("--comparators", action="store_true",
                        help="print, as json for run.py --comparators, the comparator each headline case of these pilot results freezes")
    args = parser.parse_args()
    if args.comparators:
        rows, _ = load_results(args.results.resolve())
        print(json.dumps(pick(rows), indent=2, sort_keys=True))
        return
    render(args.results.resolve(), Path(__file__).resolve().parent.parent)


if __name__ == "__main__":
    main()
