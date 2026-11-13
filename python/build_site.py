"""build the documentation site and its agent exports without deployment credentials."""

import base64
from datetime import datetime
import hashlib
import html
import json
from pathlib import Path
import posixpath
import re
import shutil
import subprocess
import sys
import tarfile
import tomllib
import urllib.request

import transcript

ROOT = Path(__file__).resolve().parents[1]
# site_dir in zensical.toml
SITE = ROOT / "site"
SITE_URL = "https://nikhil-yn-aix.github.io/fetchloom/"
BLOB = "https://github.com/nikhil-yn-aix/fetchloom/blob/main/"
# mermaid's build without lazy loading, from npm, with the integrity npm publishes for it; the theme
# loads it from unpkg on any page with a diagram unless the page brings it first
MERMAID = ("https://registry.npmjs.org/@mermaid-js/tiny/-/tiny-12.3.0.tgz",
           "sha512-90BO2kQvmQWj9YjBX2IsLHCw61rN69dPTn5wFfDu+b9aT7qtzfzTA3wpKWeaRAKK85refSo9How5EOeY39DWfA==")
# the pin of src/output.rs: a line each for its head, its shaft and its point, as a terminal draws
# them, each beside its ascii
PIN = re.compile(r"const PIN: \[\(&str, &str\); \d+\] = \[(.*?)\];", re.S)
# a real run the home page shows: each command's transcript, the data.lock the add wrote, and the
# hub's answers, which tests/cli/docs.rs replays to hold the transcripts to what fetchloom prints
RECORDED = "docs-site/home"
# where the home page's label goes: the figure naming the file of data.lock it shows
LABEL = re.compile(r'<figure class="fl-label" data-file="([^"]+)">')
STYLESHEET = "docs-site/theme/assets/stylesheets/fetchloom.css"
MOVED = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>fetchloom</title>
<link rel="canonical" href="{canonical}">
<script>location.replace({target_js} + location.hash)</script>
<meta http-equiv="refresh" content="0; url={target}">
</head>
<body>
<p>this page is now at <a href="{target}">{canonical}</a></p>
</body>
</html>
"""


def page_url(page):
    """the address of the docs page ``page`` within the site: a folder's README.md is the folder's
    address, any other page a folder of its own name."""
    path = page[:-len(".md")]
    if path == "README" or path.endswith("/README"):
        return path[:-len("README")]
    return path + "/"


def relink(match):
    """a markdown link of llms.txt as the site serves it: a docs page becomes its site page, a skill
    its copy beside llms.txt, another repository file its page on github; an address stays."""
    label, url = match.group(1), match.group(2)
    if re.match(r"[a-z]+:", url):
        return match.group(0)
    path, separator, fragment = url.partition("#")
    if path.startswith("docs/") and path.endswith(".md"):
        url = page_url(path[len("docs/"):]) + separator + fragment
    elif not path.startswith("skills/"):
        url = BLOB + url
    return f"[{label}]({url})"


def export_agents(root, site):
    """write the agent skills, llms.txt with the site's links and llms-full.txt, every user page in
    SUMMARY.md's order, into ``site``."""
    shutil.copytree(root / "skills", site / "skills", dirs_exist_ok=True)
    llms = re.sub(r"\[([^\]]*)\]\(([^)\s]+)\)", relink,
                  (root / "llms.txt").read_text(encoding="utf-8"))
    llms = llms.rstrip("\n") + "\n\n## optional\n\n- [full user guide](llms-full.txt): every user guide page in one file\n"
    (site / "llms.txt").write_text(llms, encoding="utf-8")
    pages = re.findall(r"\]\((user/[^)\s]+\.md)\)", (root / "docs/SUMMARY.md").read_text(encoding="utf-8"))
    if not pages:
        raise ValueError("docs/SUMMARY.md lists no user guide pages")
    parts = [f"# fetchloom user guide\n\nevery page of the fetchloom user guide in reading order, from {SITE_URL}user/\n"]
    for page in pages:
        body = (root / "docs" / page).read_text(encoding="utf-8").strip()
        parts.append(f"---\n\nsource: {SITE_URL}{page_url(page)}\n\n{body}\n")
    (site / "llms-full.txt").write_text("\n".join(parts), encoding="utf-8")
    print(f"llms-full.txt has {len(pages)} pages")


def redirects(root):
    """the (address, page) rows of docs-site/redirects.txt: each address the site once served and
    the docs page that answers it now."""
    rows = []
    for line in (root / "docs-site/redirects.txt").read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            address, page = line.split()
            rows.append((address, page))
    return rows


def write_redirects(site, rows):
    """a page at each old address that sends the visitor on to its page's address, with the
    #fragment they came with."""
    for address, page in rows:
        url = page_url(page)
        if not (site / url / "index.html").is_file():
            raise ValueError(f"{address} redirects to {page}, which the site does not have")
        moved = site / address
        if moved.exists():
            raise ValueError(f"{address} is a page of the site, so nothing can redirect from it")
        relative = posixpath.relpath(url or ".", posixpath.dirname(address) or ".")
        target = "./" if relative == "." else relative + "/"
        moved.parent.mkdir(parents=True, exist_ok=True)
        moved.write_text(MOVED.format(canonical=html.escape(SITE_URL + url), target=html.escape(target),
                                      target_js=json.dumps(target)), encoding="utf-8")
    print(f"{len(rows)} old addresses redirect")


def matches(data, integrity):
    """whether ``data`` has the subresource ``integrity``, such as npm publishes for a package."""
    algorithm, _, digest = integrity.partition("-")
    return hashlib.new(algorithm, data).digest() == base64.b64decode(digest)


def add_mermaid(site, cache):
    """mermaid's script and licence, which pages with a diagram load from the site itself; the
    package comes from ``cache`` when one there still matches its integrity."""
    url, integrity = MERMAID
    archive = cache / url.rsplit("/", 1)[1]
    if not archive.is_file() or not matches(archive.read_bytes(), integrity):
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read()
        if not matches(data, integrity):
            raise ValueError(f"{url} does not have the integrity pinned for it")
        cache.mkdir(parents=True, exist_ok=True)
        archive.write_bytes(data)
    with tarfile.open(archive) as package:
        script = package.extractfile("package/dist/mermaid.tiny.js").read()
        licence = package.extractfile("package/LICENSE").read()
    folder = site / "assets/javascripts"
    (folder / "mermaid.tiny.js").write_bytes(script)
    (folder / "mermaid.LICENSE.txt").write_bytes(licence)


def pin(root):
    """the lines of the pin fetchloom draws as its mark, from src/output.rs: its head, its shaft and
    its point. ValueError when it has none, or a line the site has no shape for."""
    found = PIN.search((root / "src/output.rs").read_text(encoding="utf-8"))
    lines = re.findall(r'\("([^"]+)", "[^"]*"\)', found[1]) if found else []
    if not lines or lines[0] != "\u25cf" or any(line not in ("\u25cf", "\u2502", "\u2575") for line in lines):
        raise ValueError("src/output.rs has no PIN")
    return lines


def number(value):
    """``value`` as an svg writes it, with no more digits than it needs."""
    return f"{value:.2f}".rstrip("0").rstrip(".")


def drawn(lines, head, shaft, square=False, inside=""):
    """an svg of the pin a terminal draws as ``lines``, a cell 10 units wide and 20 tall: a head
    ``head`` units across, on a shaft ``shaft`` wide that ends in a point. ``square`` centers it in
    a square, as a favicon."""
    radius, half = number(head / 2), shaft / 2
    left, width = number(5 - half), number(shaft)
    parts = []
    for row, line in enumerate(lines):
        top, middle = 20 * row, 20 * row + 10
        if line == "\u25cf":
            # drawn clockwise, as the shaft is, so where they cross is filled rather than cut out
            parts.append(f"M{number(5 - head / 2)} {middle}a{radius} {radius} 0 1 1 {number(head)} 0"
                         f"a{radius} {radius} 0 1 1 {number(-head)} 0z")
            parts.append(f"M{left} {middle}h{width}v10h-{width}z")
        elif line == "\u2502":
            parts.append(f"M{left} {top}h{width}v20h-{width}z")
        else:
            parts.append(f"M{left} {top}h{width}L5 {middle}z")
    top = 10 - head / 2
    height = 20 * len(lines) - 10 - top
    across = height if square else max(head, 10)
    box = " ".join(number(value) for value in (5 - across / 2, top, across, height))
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{box}">{inside}'
            f'<path d="{"".join(parts)}"/></svg>')


def accents(root):
    """the accent of the site's light colors and of its dark ones, from its stylesheet."""
    css = (root / STYLESHEET).read_text(encoding="utf-8")
    found = [re.search(r'\[data-md-color-scheme="' + scheme + r'"\] \{[^}]*?--fl-accent: (#[0-9a-f]{6});', css)
             for scheme in ("default", "slate")]
    if not all(found):
        raise ValueError(f"{STYLESHEET} has no accent for each color scheme")
    return [match[1] for match in found]


def add_pin(root, site):
    """the pin src/output.rs hangs beside the welcome's title, drawn from the same lines for the site:
    pin.svg, which the stylesheet sets beside the name, and the favicon, drawn heavier, as a small
    size of a typeface is, in the accent of the reader's colors."""
    lines = pin(root)
    light, dark = accents(root)
    colors = f"<style>path{{fill:{light}}}@media (prefers-color-scheme:dark){{path{{fill:{dark}}}}}</style>"
    images = site / "assets/images"
    images.mkdir(parents=True, exist_ok=True)
    (images / "pin.svg").write_text(drawn(lines, 9, 1.6) + "\n", encoding="utf-8")
    (images / "favicon.svg").write_text(drawn(lines, 18, 4.4, square=True, inside=colors) + "\n", encoding="utf-8")


def label(recorded, path):
    """the home page's label for the file at ``path`` in the dataset data.lock pins: where it came
    from, when it was locked, its size, licence and SHA-256, in eight groups of eight."""
    lock = tomllib.loads((recorded / "data.lock").read_text(encoding="utf-8"))
    for dataset in lock.get("dataset", []):
        for pinned in dataset.get("files", []):
            if pinned["path"] != path:
                continue
            ref, _, commit = dataset["resolved"].rpartition("@")
            locked = datetime.fromisoformat(dataset["locked"].replace("Z", "+00:00"))
            groups = "".join(f"<span>{pinned['sha256'][i:i + 8]}</span>" for i in range(0, 64, 8))
            rows = [("File", f"{pinned['path']}, {pinned['size']:,} bytes"),
                    ("From", f"{ref} at {commit[:7]}"),
                    ("Locked", f"{locked.day} {locked:%B %Y}"),
                    ("License", dataset.get("license", "none stated"))]
            shown = "".join(f"<dt>{name}</dt><dd>{html.escape(value)}</dd>" for name, value in rows)
            return (f'<figure class="fl-label"><p class="fl-label-file">data.lock</p>'
                    f'<p class="fl-label-name">{html.escape(dataset["name"])}</p><dl>{shown}'
                    f'<dt>SHA&#8209;256</dt><dd class="fl-hash">{groups}</dd></dl>')
    raise ValueError(f"{RECORDED}/data.lock pins no {path}")


def runs(recorded):
    """each recorded transcript by the command it starts with, without the lines of work under way
    that a terminal draws as a bar and replaces."""
    found = {}
    for path in sorted(recorded.glob("*.txt")):
        typed, _, said = path.read_text(encoding="utf-8").partition("\n")
        kept = [line for line in said.rstrip("\n").split("\n")
                if not (transcript.verb_line(line) or ("",))[0] in transcript.WORK]
        found[typed] = "\n".join([typed, *kept])
    return found


def add_home(root, site):
    """the home page's label and transcripts, made from the run recorded in docs-site/home: the
    label from data.lock, each transcript from the output of the command a block of the page shows,
    in fetchloom's colors, a line to a line, so a narrow screen wraps each under its text as a
    narrow terminal does. ValueError when the page has no label or no transcript it recorded."""
    recorded = root / RECORDED
    home = site / "index.html"
    page = home.read_text(encoding="utf-8")
    page, labels = LABEL.subn(lambda match: label(recorded, html.unescape(match[1])), page)
    known = runs(recorded)
    shown = []

    def run(match):
        typed = html.unescape(match["body"]).strip("\n")
        if typed not in known:
            return match[0]
        shown.append(typed)
        lines = "".join(f'<span class="fl-line">{line}</span>' for line in transcript.render(known[typed]).split("\n"))
        return ('<div class="language-text highlight fl-transcript fl-run"><pre><span></span>'
                f"<code>{lines}</code></pre></div>")
    page = transcript.BLOCK.sub(run, page)
    if not labels or not shown:
        raise ValueError("the home page has no label or no recorded run to show")
    home.write_text(page, encoding="utf-8")


def color_transcripts(site):
    """every page with fetchloom's output drawn in the colors the cli gives it."""
    for page in site.rglob("*.html"):
        text = page.read_text(encoding="utf-8")
        colored = transcript.colored(text)
        if colored != text:
            page.write_text(colored, encoding="utf-8")


def main():
    shutil.rmtree(SITE, ignore_errors=True)
    subprocess.run([sys.executable, "-m", "zensical", "build", "--clean", "--strict"], cwd=ROOT, check=True)
    add_home(ROOT, SITE)
    color_transcripts(SITE)
    add_pin(ROOT, SITE)
    add_mermaid(SITE, ROOT / "docs-site/.cache")
    write_redirects(SITE, redirects(ROOT))
    export_agents(ROOT, SITE)


if __name__ == "__main__":
    main()
