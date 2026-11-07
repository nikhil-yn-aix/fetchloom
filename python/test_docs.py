"""Links to book source files that mdBook does not publish."""

from pathlib import Path
import re
import unittest
from urllib.parse import unquote, urlsplit


class DocumentationLinks(unittest.TestCase):
    def test_summary_links_use_the_repository(self):
        docs = Path(__file__).resolve().parents[1] / "docs"
        summary = (docs / "SUMMARY.md").resolve()
        for page in docs.rglob("*.md"):
            fence = None
            for number, line in enumerate(page.read_text(encoding="utf-8").splitlines(), 1):
                marker = re.match(r"^\s*(`{3,}|~{3,})", line)
                if marker:
                    if fence is None:
                        fence = marker[1][0]
                    elif marker[1][0] == fence:
                        fence = None
                    continue
                if fence:
                    continue
                for link in re.findall(r"\[[^]]*\]\(([^)\s]+)\)", line):
                    target = urlsplit(link)
                    if target.scheme or target.netloc or not target.path:
                        continue
                    with self.subTest(page=page.relative_to(docs).as_posix(), line=number):
                        self.assertNotEqual(
                            (page.parent / unquote(target.path)).resolve(),
                            summary,
                            "SUMMARY.md is not a rendered page; link to its repository URL",
                        )


if __name__ == "__main__":
    unittest.main()
