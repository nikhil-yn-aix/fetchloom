"""check a built site: every docs address fetchloom prints reaches a page and its heading with no
redirect, every heading keeps the id github gives it, no link names a markdown file, and no page
loads anything from another host."""

import argparse
import html.parser
from pathlib import Path
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
    """every docs address in src/, tests/, README.md and the python helper: written out, or put
    together from the SITE constant of src/cli/args.rs, where {name} stands for each command's page."""
    commands = sorted(page.stem for page in (root / "docs/user/commands").glob("*.md")
                      if page.stem not in ("README", "global-options"))
    paths = [path for folder in ("src", "tests") for path in sorted((root / folder).rglob("*"))]
    found = set()
    for path in paths + [root / "README.md", root / "python/fetchloom/__init__.py"]:
        try:
            text = path.read_text(encoding="utf-8") if path.is_file() else ""
        except UnicodeDecodeError:
            continue
        found.update(match.rstrip(".,;:") for match in PRINTED.findall(text))
        site = re.search(r'const SITE: &str = "(' + re.escape(SITE_URL) + r'[^"]*)";', text)
        if site:
            for tail in re.findall(r'\{SITE\}([^"\s\\]*)', text):
                found.update(site[1] + tail.replace("{name}", name) for name in commands)
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
    """what is wrong with a docs address, or none when it reaches a page and its #fragment there
    with no redirect."""
    path, _, fragment = url[len(SITE_URL):].partition("#")
    address = SITE_URL + path
    file = page_file(site, path.lstrip("/"))
    if not file:
        return f"{address} reaches no page"
    # github pages answers a folder's address without its closing slash, the root's too, with a 301
    if not path.endswith("/") and (site / path.lstrip("/")).is_dir():
        return f"{address} redirects to {address}/"
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
    print(f"{len(addresses)} addresses fetchloom prints reach their pages and headings with no redirect; "
          f"{len(pages(site))} html files link no markdown and load nothing from another host")


if __name__ == "__main__":
    main()
