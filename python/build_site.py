"""build the documentation site and its agent exports without deployment credentials."""

import base64
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
WORDMARK = '<pre class="fl-wordmark" role="img" aria-label="fetchloom"></pre>'
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


def add_wordmark(root, site):
    """the wordmark the cli prints on its welcome, from src/output.rs, at the top of the home page."""
    source = (root / "src/output.rs").read_text(encoding="utf-8")
    found = re.search(r'const WORDMARK: &str = r"(.*?)";', source, re.S)
    if not found:
        raise ValueError("src/output.rs has no WORDMARK")
    home = site / "index.html"
    page = home.read_text(encoding="utf-8")
    if WORDMARK not in page:
        raise ValueError("the home page has no place for the wordmark")
    drawn = WORDMARK.replace("></pre>", f">{html.escape(found[1].strip(chr(10)))}</pre>")
    home.write_text(page.replace(WORDMARK, drawn), encoding="utf-8")


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
    color_transcripts(SITE)
    add_wordmark(ROOT, SITE)
    add_mermaid(SITE, ROOT / "docs-site/.cache")
    write_redirects(SITE, redirects(ROOT))
    export_agents(ROOT, SITE)


if __name__ == "__main__":
    main()
