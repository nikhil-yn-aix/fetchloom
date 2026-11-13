"""fetchloom's terminal output in the docs, drawn as html in the colors the cli gives it.

a ```text block of fetchloom output becomes spans whose classes name the cli's tones, which the
site's stylesheet maps to the palette of src/output/style.rs. the rules are those of
src/output.rs; a line they do not cover stays as it is."""

import html
import re

# verb_color: what step gives a verb
WORK = {"Downloading", "Unpacking", "Waiting", "Resolving", "Checking", "Searching"}
WARN = {"Missing", "Extra", "Kept", "Edited", "Unlocked", "Untracked", "Outdated"}
FAIL = {"Failed", "Damaged", "Blocked"}
# bars, setup lines and tickers draw their verb as work under way
LIVE = {"Copying", "Finishing", "Locking", "Opening", "Pinning", "Placing", "Planning", "Reading"}
# a difference from mark or report, in difference's tone
DIFFERENCES = {"Differs", "Forgot", "Not placed", "Refused", "Restarting", "Unbuilt", "Unchecked"}
# a cancel or a stop the person chose, and what a ctrl-c brings
STOPS = {"Cancelled", "Stopped", "Stopping"}
# say() writes these in the verb column with no tone
SAID = {"Access", "Check", "Sign in"}
# label() writes a report's facts under these, bold in no tone
LABELS = {"Cache", "Copies", "Machine", "Needed", "Resources", "Settings", "Unfinished", "Unneeded", "Used by"}
# step_in tells these in the tone their caller chooses
CHOSEN = {"Running": "work"}
# status's report rows, whose -v rows under them are report rows too, not dim details
REPORTS = WARN | FAIL | {"Not placed", "Removed", "Unbuilt", "Unchecked"}

# the verbs a function other than step tells with the same words, known by what follows them
CONTEXT = [
    ("Found", re.compile(r"[\d,]+ differences? from data\.lock$"), "warn"),
    ("Replaced", re.compile(r" on the mirror$| · discarding "), "warn"),
    ("Removed", re.compile(r" · not in data\.toml, so "), "warn"),
    ("Skipped", re.compile(r" · it reads "), "warn"),
    ("Finishing", re.compile(r"\(press ctrl-c|complete; "), "warn"),
    ("Kept", re.compile(r"^a key for "), "done"),
    ("Signed in", re.compile(r"^to none of "), "faint"),
    ("Not placed", re.compile(r" · place = false in data\.toml$"), "faint"),
]

VERB = re.compile(r"[A-Z][A-Za-z]*(?: [a-z]+)*|[0-9]+")
BLOCK = re.compile(r'<div class="language-text highlight"><pre><span></span><code>(?P<body>[^<]*)</code></pre></div>')


def span(tone, text):
    return f'<span class="fl-{tone}">{html.escape(text, quote=False)}</span>' if text else ""


def accented(text, around=None):
    """``text`` with each command in backticks in the accent and the rest in ``around``."""
    shown, rest = [], text
    while (start := rest.find("`")) >= 0 and (end := rest.find("`", start + 1)) >= 0:
        shown += [span(around, rest[:start]) if around else html.escape(rest[:start], quote=False),
                  span("accent", rest[start:end + 1])]
        rest = rest[end + 1:]
    shown.append(span(around, rest) if around else html.escape(rest, quote=False))
    return "".join(shown)


def subject(text):
    """what a step is about in bold, its facts after ` · ` dim, as subject() in src/output.rs."""
    name, dot, facts = text.partition(" · ")
    if dot:
        return span("bold", name) + f'<span class="fl-dim"> · {accented(facts)}</span>'
    if text and " " not in text:
        return span("bold", text)
    return accented(text)


def tone_of(verb, text):
    """the tone a verb is told in, or none for a line say() writes."""
    for known, pattern, tone in CONTEXT:
        if verb == known and pattern.search(text):
            return tone
    if verb in SAID:
        return None
    if verb in LABELS:
        return "bold"
    if verb in CHOSEN:
        return CHOSEN[verb]
    if verb in WORK or verb in LIVE or verb.startswith("Would"):
        return "work"
    if verb in FAIL:
        return "fail"
    if verb in WARN or verb in DIFFERENCES or verb in STOPS:
        return "warn"
    if verb.isdigit():
        return "accent"
    return "done"


def verb_line(line):
    """(verb, its padding, the rest) when ``line`` has a verb right-aligned in the 11 columns steps
    use, then a space and its text."""
    column, gap, text = line[:11], line[11:12], line[12:]
    verb = column.lstrip()
    if gap != " " or not text[:1].strip() or not VERB.fullmatch(verb):
        return None
    return verb, column[:11 - len(verb)], text


def is_transcript(text):
    """whether a text block shows fetchloom's output rather than a file or a listing."""
    for line in text.splitlines():
        if line.startswith(("$ fetchloom", "warning: ", "error: ")) or re.match(r"  (help|hint|next): ", line):
            return True
        told = verb_line(line)
        if told and tone_of(told[0], told[2]):
            return True
    return False


def render(text):
    """the lines of a transcript as html, each in the colors fetchloom gives it."""
    out, context = [], None
    for line in text.split("\n"):
        out.append(drawn(line, context))
        told = verb_line(line)
        if line.startswith("error: "):
            context = "error"
        elif told:
            tone = tone_of(told[0], told[2])
            context = "found" if told[0] == "Found" and tone == "done" and " datasets" in told[2] else (
                "report" if told[0] in REPORTS and tone in ("warn", "fail") else "step")
        elif not line.startswith("  "):
            context = None
    return "\n".join(out)


def drawn(line, context):
    """one line as html, given what kind of line came before it."""
    if line.startswith("$ "):
        return span("dim", "$") + " " + span("typed", line[2:])
    for label, tone in (("warning:", "warn"), ("error:", "fail")):
        if line.startswith(label + " "):
            return span(tone, label) + " " + accented(line[len(label) + 1:])
    if line.startswith("  help: "):
        return "  " + span("accent", "help:") + " " + accented(line[8:])
    if line.startswith(("  hint: ", "  next: ")):
        return "  " + accented(line[2:], "dim")
    if context == "error" and line.startswith("  "):
        return "  " + accented(line[2:], "dim")
    if line.startswith(" " * 12) and not line.startswith(" " * 13):
        if context == "found":
            typed, gap, rest = line[12:].partition(" ")
            return " " * 12 + span("accent", typed) + html.escape(gap + rest, quote=False)
        if context == "step":
            return " " * 12 + accented(line[12:], "dim")
    told = verb_line(line)
    if not told:
        return html.escape(line, quote=False)
    verb, padding, text = told
    tone = tone_of(verb, text)
    if tone is None:
        return html.escape(line, quote=False)
    if tone == "faint":
        return span("dim", line)
    if verb == "Would":
        action, _, text = text.partition(" ")
        return padding + span(tone, f"{verb} {action}") + " " + subject(text)
    if verb == "Stopped":
        return padding + span(tone, verb) + " " + accented(text)
    return padding + span(tone, verb) + " " + subject(text)


def colored(page):
    """``page``'s html with each transcript block drawn in fetchloom's colors."""
    def block(match):
        text = html.unescape(match["body"])
        if not is_transcript(text):
            return match[0]
        return ('<div class="language-text highlight fl-transcript"><pre><span></span><code>'
                f"{render(text)}</code></pre></div>")
    return BLOCK.sub(block, page)
