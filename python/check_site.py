"""check a built site: every docs address fetchloom prints reaches a page and its heading, every
heading keeps the id github gives it, no link names a markdown file, and no page loads anything
from another host."""

import argparse
import html.parser
from pathlib import Path
import posixpath
import re
import sys
from urllib.parse import urlsplit

from build_site import page_url
from test_docs import anchors

ROOT = Path(__file__).resolve().parents[1]
SITE_URL = "https://nikhil-yn-aix.github.io/fetchloom"
HOST = urlsplit(SITE_URL).netloc
PRINTED = re.compile(re.escape(SITE_URL) + r"[^\s\"'`<>()\[\]\\]*")
# links that name another page or a description, which a page view never fetches
NAVIGATION = {"alternate", "author", "canonical", "describedby", "help", "license", "next", "prev"}
LOADS = {"audio": ("src",), "embed": ("src",), "iframe": ("src",), "img": ("src", "srcset"), "input": ("src",),
         "object": ("data",), "script": ("src",), "source": ("src", "srcset"), "track": ("src",),
         "video": ("src", "poster")}
CSS = re.compile(r"""url\(\s*['"]?([^'")\s]+)|@import\s+['"]([^'"]+)""")


class Tags(html.parser.HTMLParser):
    """the ids, links and loaded addresses of one html page, in the order they appear."""

    def __init__(self):
        super().__init__()
        self.ids, self.links, self.loads = set(), [], []

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        for key in ("id", "name"):
            if attrs.get(key):
                self.ids.add(attrs[key])
        if tag in ("a", "area", "link") and attrs.get("href"):
            self.links.append(attrs["href"])
        if tag == "link" and attrs.get("href") and not NAVIGATION & set((attrs.get("rel") or "").split()):
            self.loads.append(attrs["href"])
        for key in LOADS.get(tag, ()):
            for part in (attrs.get(key) or "").split(","):
                if part.strip():
                    self.loads.append(part.split()[0])
        self.loads += [found[0] or found[1] for found in CSS.findall(attrs.get("style") or "")]

    def handle_data(self, data):
        if self.lasttag == "style":
            self.loads += [found[0] or found[1] for found in CSS.findall(data)]


def parsed(page):
    tags = Tags()
    tags.feed(page.read_text(encoding="utf-8"))
    return tags


def printed(root):
    """every docs address in src/ and tests/: written out, or put together from the SITE constant of
    src/cli/args.rs, where {name} stands for each command's page."""
    commands = sorted(page.stem for page in (root / "docs/user/commands").glob("*.md") if page.stem != "README")
    found = set()
    for folder in ("src", "tests"):
        for path in sorted((root / folder).rglob("*")):
            try:
                text = path.read_text(encoding="utf-8") if path.is_file() else ""
            except UnicodeDecodeError:
                continue
            found.update(match.rstrip(".,;:") for match in PRINTED.findall(text))
            if f'const SITE: &str = "{SITE_URL}";' in text:
                for tail in re.findall(r'\{SITE\}(/[^"\s\\]*)', text):
                    found.update(SITE_URL + tail.replace("{name}", name) for name in commands)
    return found


def page_file(site, path):
    """the file the site serves at ``path``, as github pages finds it, or none."""
    if path == "" or path.endswith("/"):
        candidate = site / path / "index.html"
    elif (site / path).is_dir():
        candidate = site / path / "index.html"
    else:
        candidate = site / path
    return candidate if candidate.is_file() else None


def unreached(site, url):
    """what is wrong with a docs address fetchloom prints, or none when it reaches a page, through a
    moved page's address if need be, with its #fragment on that page."""
    path, _, fragment = url[len(SITE_URL):].partition("#")
    address = SITE_URL + path
    file = page_file(site, path.lstrip("/"))
    if file:
        moved = re.search(r'<meta http-equiv="refresh" content="0; url=([^"]+)">', file.read_text(encoding="utf-8"))
        if moved:
            here = file.parent.relative_to(site).as_posix()
            target = posixpath.normpath(posixpath.join(here, moved[1]))
            file = page_file(site, "" if target == "." else target)
    if not file:
        return f"{address} reaches no page"
    if fragment and fragment not in parsed(file).ids:
        return f"{address} has no #{fragment} on {file.relative_to(site).as_posix()}"
    return None


def pages(site):
    return sorted(site.rglob("*.html"))


def markdown_links(site):
    """each link of the site that names a markdown file rather than the page made from it."""
    found = []
    for page in pages(site):
        for link in parsed(page).links:
            parts = urlsplit(link)
            if (not parts.netloc or parts.netloc == HOST) and parts.path.endswith(".md"):
                found.append(f"{page.relative_to(site).as_posix()} links {link}")
    return found


def github_ids(root, site):
    """each heading of a docs page whose built page lacks the id github gives it, so a link that
    works on github would miss on the site."""
    docs = root / "docs"
    found = []
    for source in sorted(docs.rglob("*.md")):
        if source.name == "SUMMARY.md":
            continue
        name = source.relative_to(docs).as_posix()
        built = site / page_url(name) / "index.html"
        if not built.is_file():
            found.append(f"{name} has no page")
            continue
        missing = anchors(source) - parsed(built).ids
        found += [f"{name} has no id {slug} on {built.relative_to(site).as_posix()}" for slug in sorted(missing)]
    return found


def external(site):
    """each address a page or stylesheet of the site would load from another host."""
    found = []
    for file in sorted([*site.rglob("*.html"), *site.rglob("*.css")]):
        text = file.read_text(encoding="utf-8")
        loads = [one or two for one, two in CSS.findall(text)] if file.suffix == ".css" else parsed(file).loads
        for address in loads:
            host = urlsplit(address).netloc
            if host and host != HOST:
                found.append(f"{file.relative_to(site).as_posix()} loads {address}")
    return found


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("site", type=Path, nargs="?", default=ROOT / "site")
    site = parser.parse_args().site.resolve()
    addresses = sorted(printed(ROOT))
    problems = [problem for problem in (unreached(site, url) for url in addresses) if problem]
    problems += markdown_links(site) + github_ids(ROOT, site) + external(site)
    for problem in problems:
        print(problem)
    if problems:
        sys.exit(f"{len(problems)} problems in {site}")
    print(f"{len(addresses)} addresses fetchloom prints reach their pages and headings; "
          f"{len(pages(site))} html files link no markdown and load nothing from another host")


if __name__ == "__main__":
    main()
