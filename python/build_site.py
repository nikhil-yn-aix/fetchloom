"""build the documentation site and its agent exports without deployment credentials."""

import base64
import collections
from datetime import datetime
import hashlib
import html
import io
import json
import math
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tomllib
import urllib.request
from urllib.parse import urlsplit

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
# the pin's head src/output.rs prints before the welcome's name, as a pin pushed into a sheet shows
# from above, and its ascii, which is none; the site's pins are the same pin from the side
PIN = re.compile(r'const PIN: \(&str, &str\) = \("([^"]*)", "([^"]*)"\);')
HEAD = "\u2022"
# the pin from the side, leaning 45 degrees with its head up and to the right, on a grid of 16: its
# head's middle and radius, its point, its needle's width across, how far before the point the needle
# narrows, and whether the head shows the glint a glass head catches. the needle runs along
# x + y = 16, through the middle of each pixel it crosses at 16 and 32 pixels, and the head covers
# whole pixels at both. as a typeface has optical sizes, each size has its own drawing: one for 16
# pixels and the header, one for 32, and a finer one that glints, from the label up
Pin = collections.namedtuple("Pin", "head radius point across narrows glints")
SIXTEEN = Pin((12, 4), 3, (1.5, 14.5), 1.25 / math.sqrt(2), 4.5, False)
THIRTY_TWO = Pin((12, 4), 3, (1.25, 14.75), .75 / math.sqrt(2), 5.5, False)
LARGE = Pin((12, 4), 2.6, (1.1, 14.9), .4, 6.5, True)
# what the welcome says fetchloom is for, under its name, which a link's card shows under it too
PURPOSE = re.compile(r'const PURPOSE: &str = "([^"]+)";')
# a real run the home page shows: each command's transcript, the data.lock the add wrote, and the
# hub's answers, which tests/cli/docs.rs replays to hold the transcripts to what fetchloom prints
RECORDED = "docs-site/home"
# where the home page's label goes: the figure naming the file of data.lock it shows
LABEL = re.compile(r'<figure class="fl-label" data-file="([^"]+)">')
STYLESHEET = "docs-site/theme/assets/stylesheets/fetchloom.css"


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
    """the pin's head src/output.rs prints, which the site's pins draw from the side. ValueError when
    it prints none, or a head other than the round one the site draws."""
    found = PIN.search((root / "src/output.rs").read_text(encoding="utf-8"))
    if not found:
        raise ValueError("src/output.rs has no PIN")
    if found[1] != HEAD:
        raise ValueError(f"src/output.rs prints the pin's head as {found[1]!r}, not the {HEAD!r} the site draws")
    return found[1]


def number(value):
    """``value`` as an svg writes it, with no more digits than it needs."""
    text = f"{value:.3f}".rstrip("0").rstrip(".")
    return "0" if text == "-0" else text


def shapes(drawing):
    """the polygon of a ``drawing``'s needle and the circles of its head and its glint, if it has
    one. the needle's sides run from the head's middle along its line, then straight in to the point;
    it goes round clockwise, as the head does, so the two fill as one shape."""
    (x, y), radius, point, across, narrows, glints = drawing
    slant = math.sqrt(.5)
    side = slant * across / 2
    neck = (point[0] + narrows * slant, point[1] - narrows * slant)
    needle = [(x + side, y + side), (neck[0] + side, neck[1] + side), point, (neck[0] - side, neck[1] - side),
              (x - side, y - side)]
    circles = [(x, y, radius)] + ([(x - .4 * radius, y - .4 * radius, .27 * radius)] if glints else [])
    return needle, circles


def svg(drawing, colors=None, style=""):
    """an svg of the pin as ``drawing`` draws it: in one color for a mask or for ``style`` to fill, or
    in ``colors``, the pin's and its glint's."""
    needle, circles = shapes(drawing)

    def ring(x, y, r):
        return (f"M{number(x - r)} {number(y)}a{number(r)} {number(r)} 0 1 1 {number(2 * r)} 0"
                f"a{number(r)} {number(r)} 0 1 1 {number(-2 * r)} 0z")
    body = "M" + "L".join(f"{number(x)} {number(y)}" for x, y in needle) + "z" + ring(*circles[0])
    paths = f'<path d="{body}"/>'
    if colors:
        paths = f'<path fill="{colors[0]}" d="{body}"/>' + "".join(
            f'<path fill="{colors[1]}" d="{ring(*circle)}"/>' for circle in circles[1:])
    return f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16">{style}{paths}</svg>\n'


def palette(root):
    """the site's light colors and its dark ones, by name, from its stylesheet."""
    css = (root / STYLESHEET).read_text(encoding="utf-8")
    found = {}
    for scheme in ("default", "slate"):
        block = re.search(r'\[data-md-color-scheme="' + scheme + r'"\] \{([^}]*)\}', css)
        found[scheme] = dict(re.findall(r"--fl-(\w+): (#[0-9a-f]{6});", block[1])) if block else {}
        if not {"paper", "surface", "ink", "muted", "accent"} <= found[scheme].keys():
            raise ValueError(f"{STYLESHEET} lacks a color the site's pins and icons need")
    return found


def glinted(accent):
    """the light on a glass head of ``accent``: the accent halfway to white."""
    return "#" + "".join(f"{(int(accent[i:i + 2], 16) + 255) // 2:02x}" for i in (1, 3, 5))


def add_pin(root, site):
    """the pin whose head src/output.rs prints before the welcome's name, drawn from the side for the
    site: pin.svg, the stylesheet's mark beside the site's name, and the favicon, in the accent of
    the reader's colors, both as 16 pixels draw it; and pin-default.svg and pin-slate.svg, finer and
    glinting, which pin the home page's label in each color scheme."""
    pin(root)
    colors = palette(root)
    light, dark = colors["default"]["accent"], colors["slate"]["accent"]
    style = f"<style>path{{fill:{light}}}@media (prefers-color-scheme:dark){{path{{fill:{dark}}}}}</style>"
    images = site / "assets/images"
    images.mkdir(parents=True, exist_ok=True)
    (images / "pin.svg").write_text(svg(SIXTEEN), encoding="utf-8")
    (images / "favicon.svg").write_text(svg(SIXTEEN, style=style), encoding="utf-8")
    for scheme, named in colors.items():
        accent = named["accent"]
        (images / f"pin-{scheme}.svg").write_text(svg(LARGE, colors=(accent, glinted(accent))), encoding="utf-8")


def rgb(color):
    return tuple(int(color[i:i + 2], 16) for i in (1, 3, 5))


def drawn(drawing, size, color, ground=None, span=1.0, at=None):
    """a ``size`` pixel square image of the pin as ``drawing`` draws it, in ``color`` on ``ground``
    or on nothing, its grid of 16 ``span`` of the image across, its middle at ``at`` or the image's,
    sampled finely, so each pixel takes as much of the color as the pin covers of it."""
    # pillow comes with the site's toolchain, not with the python the unit tests run under
    from PIL import Image, ImageDraw
    needle, circles = shapes(drawing)
    fine = 16 if size <= 64 else 4
    across = size * span / 16
    middle = at or (size / 2, size / 2)
    origin = (middle[0] - size * span / 2, middle[1] - size * span / 2)

    def point(x, y):
        return ((origin[0] + x * across) * fine, (origin[1] + y * across) * fine)

    def mask(parts):
        layer = Image.new("L", (size * fine, size * fine), 0)
        draw = ImageDraw.Draw(layer)
        for part in parts:
            if len(part) == 3:
                x, y, r = part
                draw.ellipse([*point(x - r, y - r), *point(x + r, y + r)], fill=255)
            else:
                draw.polygon([point(x, y) for x, y in part], fill=255)
        return layer.resize((size, size), Image.Resampling.BOX)
    image = Image.new("RGBA", (size, size), (*rgb(ground), 255) if ground else (0, 0, 0, 0))
    image.paste((*rgb(color), 255), mask=mask([needle, circles[0]]))
    for glint in circles[1:]:
        image.paste((*rgb(glinted(color)), 255), mask=mask([glint]))
    return image


def png(image):
    buffer = io.BytesIO()
    image.save(buffer, "PNG", optimize=True)
    return buffer.getvalue()


def ico(images):
    """an icon file holding each of ``images``, a png apiece, as every browser reads them."""
    head = struct.pack("<HHH", 0, 1, len(images))
    offset, entries, data = 6 + 16 * len(images), b"", b""
    for image in images:
        encoded = png(image)
        entries += struct.pack("<BBBBHHII", image.width % 256, image.height % 256, 0, 0, 1, 32, len(encoded), offset)
        offset += len(encoded)
        data += encoded
    return head + entries + data


def preview(root, colors):
    """the 1200 by 630 image a link to the site shows: the pin, glinting, beside fetchloom's name and,
    under it, the one line its welcome says it is for, on the sheet of ``colors``."""
    from PIL import ImageDraw, ImageFont
    card = drawn(LARGE, 630, colors["accent"], colors["paper"], span=.42, at=(240, 315))
    card = card.crop((0, 0, 1200, 630))
    card.paste((*rgb(colors["paper"]), 255), (630, 0, 1200, 630))
    fonts = root / "docs-site/theme/assets/fonts"
    name = ImageFont.truetype(str(fonts / "source-serif-4-latin-opsz-normal.woff2"), 132)
    name.set_variation_by_axes([600, 60])
    line = ImageFont.truetype(str(fonts / "source-serif-4-latin-opsz-normal.woff2"), 42)
    line.set_variation_by_axes([400, 20])
    draw = ImageDraw.Draw(card)
    draw.text((452, 300), "fetchloom", font=name, fill=colors["ink"], anchor="ls")
    lines = [""]
    for word in purpose(root).split():
        joined = f"{lines[-1]} {word}".strip()
        if lines[-1] and draw.textlength(joined, font=line) > 1200 - 452 - 96:
            lines.append(word)
        else:
            lines[-1] = joined
    for index, text in enumerate(lines):
        draw.text((456, 380 + 58 * index), text, font=line, fill=colors["muted"], anchor="ls")
    return card.convert("RGB")


def add_icons(root, site):
    """the rest of the browser's set, drawn from the same pin and colors: favicon.ico, the 16 and 32
    pixel drawings in the light accent, for browsers without svg icons; the 180 pixel icon apple
    devices take, and the 192 and 512 pixel icons of manifest.webmanifest, the large pin glinting on
    the dark sheet; and preview.png, which a link to the site shows."""
    pin(root)
    colors = palette(root)
    light, dark = colors["default"], colors["slate"]
    images = site / "assets/images"
    images.mkdir(parents=True, exist_ok=True)
    small = [drawn(SIXTEEN, 16, light["accent"]), drawn(THIRTY_TWO, 32, light["accent"])]
    (images / "favicon.ico").write_bytes(ico(small))
    (images / "apple-touch-icon.png").write_bytes(png(drawn(LARGE, 180, dark["accent"], dark["paper"], .62)))
    for size in (192, 512):
        (images / f"icon-{size}.png").write_bytes(png(drawn(LARGE, size, dark["accent"], dark["paper"], .62)))
    (images / "icon-mask.png").write_bytes(png(drawn(LARGE, 512, dark["accent"], dark["paper"], .5)))
    (images / "preview.png").write_bytes(png(preview(root, dark)))
    icons = [{"src": f"assets/images/icon-{size}.png", "sizes": f"{size}x{size}", "type": "image/png"} for size in (192, 512)]
    icons.append({"src": "assets/images/icon-mask.png", "sizes": "512x512", "type": "image/png", "purpose": "maskable"})
    manifest = {"name": "fetchloom", "short_name": "fetchloom", "description": purpose(root), "start_url": "./",
                "display": "browser", "background_color": dark["paper"], "theme_color": dark["paper"], "icons": icons}
    (site / "manifest.webmanifest").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")


def purpose(root):
    """what fetchloom's welcome says it is for, under its name, from src/cli/args.rs."""
    found = PURPOSE.search((root / "src/cli/args.rs").read_text(encoding="utf-8"))
    if not found:
        raise ValueError("src/cli/args.rs has no PURPOSE")
    return found[1]


def head(root, page, url):
    """the tags a page of the site adds to its head at ``url``, under the site's address: its icons,
    the browser's color under each color scheme, and the card a link to it shows."""
    colors = palette(root)
    title = re.search(r"<title>([^<]*)</title>", page)
    description = re.search(r'<meta name="description" content="([^"]*)">', page)
    image = SITE_URL + "assets/images/preview.png"
    alt = html.escape(f"fetchloom's pin beside its name and {purpose(root)}")
    tags = ['<link rel="icon" href="{0}assets/images/favicon.ico" sizes="32x32">',
            '<link rel="apple-touch-icon" href="{0}assets/images/apple-touch-icon.png">',
            '<link rel="manifest" href="{0}manifest.webmanifest">',
            f'<meta name="theme-color" media="(prefers-color-scheme: light)" content="{colors["default"]["paper"]}">',
            f'<meta name="theme-color" media="(prefers-color-scheme: dark)" content="{colors["slate"]["paper"]}">',
            '<meta property="og:type" content="website">',
            '<meta property="og:site_name" content="fetchloom">',
            f'<meta property="og:title" content="{title[1] if title else "fetchloom"}">',
            f'<meta property="og:url" content="{SITE_URL}{url}">',
            f'<meta property="og:image" content="{image}">',
            '<meta property="og:image:width" content="1200">',
            '<meta property="og:image:height" content="630">',
            f'<meta property="og:image:alt" content="{alt}">',
            '<meta name="twitter:card" content="summary_large_image">']
    if description:
        tags.insert(8, f'<meta property="og:description" content="{description[1]}">')
    # the 404 page answers at any depth, so it names the site's own folder
    up = urlsplit(SITE_URL).path if url == "404.html" else "../" * url.count("/")
    return "".join(tag.replace("{0}", up) for tag in tags)


def add_head(root, site):
    """every page's head with its icons, theme colors and link card."""
    for page in site.rglob("*.html"):
        text = page.read_text(encoding="utf-8")
        if "</head>" not in text:
            continue
        url = page.relative_to(site).as_posix().removesuffix("index.html")
        page.write_text(text.replace("</head>", head(root, text, url) + "</head>", 1), encoding="utf-8")


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
    add_icons(ROOT, SITE)
    add_head(ROOT, SITE)
    add_mermaid(SITE, ROOT / "docs-site/.cache")
    export_agents(ROOT, SITE)


if __name__ == "__main__":
    main()
