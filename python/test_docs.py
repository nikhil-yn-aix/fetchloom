"""links in the docs and the files around them reach their files and headings."""

from pathlib import Path
import re
import tempfile
import unittest
from urllib.parse import unquote, urlsplit

import docs_index

ROOT = Path(__file__).resolve().parents[1]
BLOB = "https://github.com/nikhil-yn-aix/fetchloom/blob/main/"


def prose(page):
    """yields (line number, line) for every line of `page` outside code fences."""
    fence = None
    for number, line in enumerate(page.read_text(encoding="utf-8").splitlines(), 1):
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if marker:
            if fence is None:
                fence = marker[1][0]
            elif marker[1][0] == fence:
                fence = None
            continue
        if not fence:
            yield number, line


def links(page):
    """yields (line number, link) for every markdown link outside code fences."""
    for number, line in prose(page):
        for link in re.findall(r"\[[^]]*\]\(([^)\s]+)\)", line):
            yield number, link


def anchors(page):
    """the fragment github gives each heading of `page`."""
    found = set()
    for _, line in prose(page):
        heading = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
        if heading:
            slug = re.sub(r"[^\w\- ]", "", heading[1].lower()).replace(" ", "-")
            unique, count = slug, 1
            while unique in found:
                unique, count = f"{slug}-{count}", count + 1
            found.add(unique)
    return found


def nested_too_shallow(page):
    """yields (line number, line) for each line python-markdown, which builds the site, takes out of
    the list item above it: a nested item, or a block after a blank line or a code fence, indented
    less than four spaces past that item's marker. github keeps such a line in the item."""
    items, blank, fence = [], True, None
    for number, line in enumerate(page.read_text(encoding="utf-8").splitlines(), 1):
        marker = re.match(r"^\s*(`{3,}|~{3,})", line)
        if fence:
            if marker and marker[1][0] == fence:
                fence = None
            continue
        if not line.strip():
            blank = True
            continue
        indent = len(line) - len(line.lstrip(" "))
        item = re.match(r" *([-*+]|\d{1,9}[.)]) +\S", line)
        parents = [at for at in items if at < indent]
        shallow = bool(parents) and indent < parents[-1] + 4 and bool(item or marker or blank)
        if shallow:
            yield number, line
        elif item:
            items = parents + [indent]
        elif not indent and (blank or line.startswith("#")):
            items = []
        if marker:
            fence = marker[1][0]
        blank = False


class Readers(unittest.TestCase):
    """what the checks below read from a page, on small pages."""

    def page(self, text):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        path = Path(folder.name, "page.md")
        path.write_text(text, encoding="utf-8")
        return path

    def test_prose_leaves_out_each_fenced_block(self):
        # a fence closes only on its own character, and one left open hides the rest
        page = self.page("one\n```sh\nfetchloom sync\n~~~\n```\ntwo\n  ~~~~\n```\n  ~~~~\nthree\n````\nunclosed\n")
        self.assertEqual(list(prose(page)), [(1, "one"), (6, "two"), (10, "three")])

    def test_links_are_read_outside_fences(self):
        page = self.page("see [a](a.md) and [b](b.md#part)\n```\n[c](c.md)\n```\n[d](https://example.org)\n")
        self.assertEqual(list(links(page)), [(1, "a.md"), (1, "b.md#part"), (5, "https://example.org")])

    def test_anchors_are_the_fragments_github_gives_headings(self):
        page = self.page("# Getting Started\n## the `sync` command\n## local changes ##\n## what's new?\n## a  b\n"
                         "## données é\n```\n# not a heading\n```\n## local changes\n## local changes\n#hashtag\n")
        self.assertEqual(anchors(page), {"getting-started", "the-sync-command", "local-changes", "whats-new", "a--b",
                                         "données-é", "local-changes-1", "local-changes-2"})


    def test_lines_python_markdown_takes_out_of_their_list_item(self):
        page = self.page("1. step\n   - nested at three\n    - nested at four\n2. step\n\n   ```sh\n   fetchloom sync\n"
                         "   ```\n\n    ```sh\n    x\n    ```\nlazy continuation\n\na paragraph ends the list\n\n"
                         "  - a list of its own\n     - nested at three past it\n")
        self.assertEqual([number for number, _ in nested_too_shallow(page)], [2, 6, 18])


class Indexes(unittest.TestCase):
    """the index pages python/docs_index.py writes from SUMMARY.md."""

    def docs(self, files):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        for name, text in files.items():
            path = Path(folder.name, name)
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")
        return Path(folder.name)

    def test_an_index_lists_the_pages_one_level_below_it_with_their_first_sentence(self):
        docs = self.docs({
            "SUMMARY.md": "- [Guide](user/README.md)\n    - [Start](user/start/README.md)\n"
                          "        - [Install](user/start/install.md)\n    - [Notes](user/notes.md)\n",
            "user/README.md": f"# Guide\n\n{docs_index.MARKER}\n\nold table\n",
            "user/start/README.md": f"# Start\n\nno `{docs_index.MARKER}`, so nothing is written here:\n\n```sh\nx. y\n```\n",
            "user/start/install.md": "# Install\n\nrun it.\n",
            "user/notes.md": "# Notes\n\n`data.toml` names [the](a.md) sets `b. c`\nit keeps. the rest waits.\n",
        })
        self.assertEqual(docs_index.stale(docs), ["user/README.md"])
        self.assertEqual(docs_index.generate(docs), {"user/README.md": (
            f"# Guide\n\n{docs_index.MARKER}\n\n| Page | Summary |\n|---|---|\n"
            f"| [Start](start/README.md) | no `{docs_index.MARKER}`, so nothing is written here: |\n"
            "| [Notes](notes.md) | `data.toml` names the sets `b. c` it keeps. |\n")})

    def test_a_page_that_opens_with_anything_but_a_paragraph_has_no_first_sentence(self):
        for text in ("# t\n\n## a heading\n", "# t\n\n| a | table |\n", "# t\n\n```sh\nx. y\n```\n",
                     f"# t\n\n{docs_index.MARKER}\n", "# t\n"):
            with self.subTest(text=text):
                self.assertEqual(docs_index.first_sentence(text), "")

    def test_the_indexes_are_what_docs_index_writes(self):
        self.assertEqual(docs_index.stale(ROOT / "docs"), [], "run python3 python/docs_index.py")


class Rendering(unittest.TestCase):
    def test_lists_nest_as_the_site_reads_them(self):
        for page in sorted((ROOT / "docs").rglob("*.md")):
            with self.subTest(page=page.relative_to(ROOT).as_posix()):
                self.assertEqual(list(nested_too_shallow(page)), [],
                                 "indent a nested list or block four spaces past its item's marker")


class DocumentationLinks(unittest.TestCase):
    def test_summary_links_use_the_repository(self):
        docs = ROOT / "docs"
        summary = (docs / "SUMMARY.md").resolve()
        for page in docs.rglob("*.md"):
            for number, link in links(page):
                target = urlsplit(link)
                if target.scheme or target.netloc or not target.path:
                    continue
                with self.subTest(page=page.relative_to(docs).as_posix(), line=number):
                    self.assertNotEqual(
                        (page.parent / unquote(target.path)).resolve(),
                        summary,
                        "SUMMARY.md is not a rendered page; link to its repository URL",
                    )

    def test_the_readme_links_only_to_absolute_addresses(self):
        """pypi shows README.md without the repository around it, so a relative link breaks there."""
        relative = [f"{number}: {link}" for number, link in links(ROOT / "README.md")
                    if not re.match(r"[a-z]+:", link)]
        self.assertEqual(relative, [])

    def test_links_reach_their_files_and_headings(self):
        pages = [ROOT / "README.md", ROOT / "CONTRIBUTING.md", ROOT / "llms.txt"]
        for folder in ("docs", "bench", "skills", "python"):
            pages += sorted((ROOT / folder).rglob("*.md"))
        for page in pages:
            for number, link in links(page):
                path, _, fragment = link.partition("#")
                if link.startswith(BLOB):
                    target = ROOT / unquote(path[len(BLOB):])
                elif re.match(r"[a-z]+:", link):
                    continue
                else:
                    target = (page.parent / unquote(path)).resolve() if path else page
                with self.subTest(page=page.relative_to(ROOT).as_posix(), line=number, link=link):
                    self.assertTrue(target.exists(), "the link names no file")
                    if fragment and target.suffix == ".md":
                        self.assertIn(fragment, anchors(target), "the link names no heading")

    def test_the_summary_lists_every_page(self):
        """SUMMARY.md is the site's navigation: a page it leaves out is still built, with no place
        in it."""
        docs = ROOT / "docs"
        navigation = listed(docs / "SUMMARY.md")
        missing = sorted(page.relative_to(docs).as_posix() for page in docs.rglob("*.md")
                         if page.name != "SUMMARY.md" and page.resolve() not in navigation)
        self.assertEqual(missing, [], "SUMMARY.md does not list these pages")

    def test_llms_txt_names_only_pages_the_summary_lists(self):
        """llms.txt is a hand-kept choice of pages, each one the site's navigation holds."""
        docs = (ROOT / "docs").resolve()
        named = {page for page in listed(ROOT / "llms.txt") if page.is_relative_to(docs) and page.suffix == ".md"}
        outside = sorted(page.relative_to(docs).as_posix() for page in named - listed(docs / "SUMMARY.md"))
        self.assertEqual(outside, [], "llms.txt names pages SUMMARY.md does not list")


def listed(index):
    """the files the relative links of ``index`` name."""
    return {(index.parent / unquote(urlsplit(link).path)).resolve()
            for _, link in links(index) if not urlsplit(link).scheme and urlsplit(link).path}


if __name__ == "__main__":
    unittest.main()
