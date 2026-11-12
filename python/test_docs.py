"""Links in the docs and the files around them."""

from pathlib import Path
import re
import unittest
from urllib.parse import unquote, urlsplit

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
            slug = re.sub(r"[^\w\- ]", "", heading[1].replace("`", "").lower()).replace(" ", "-")
            unique, count = slug, 1
            while unique in found:
                unique, count = f"{slug}-{count}", count + 1
            found.add(unique)
    return found


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

    def test_every_user_page_is_listed(self):
        """the site's sidebar, the user guide's index and llms.txt each name every user page; a page
        SUMMARY.md leaves out is not published."""
        user = ROOT / "docs" / "user"
        pages = {page.resolve() for page in user.rglob("*.md")}
        for index in (ROOT / "docs" / "SUMMARY.md", user / "README.md", ROOT / "llms.txt"):
            listed = {
                (index.parent / unquote(urlsplit(link).path)).resolve()
                for _, link in links(index)
                if not urlsplit(link).scheme
            }
            missing = sorted(
                page.relative_to(user).as_posix() for page in pages - listed - {index.resolve()}
            )
            with self.subTest(index=index.relative_to(ROOT).as_posix()):
                self.assertEqual(missing, [], "these user pages are not listed")


if __name__ == "__main__":
    unittest.main()
