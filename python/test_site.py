"""the same site exports serve pull-request checks and pages deployment."""

import contextlib
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

import build_site
from build_site import export_agents


class SiteTests(unittest.TestCase):
    def checkout(self, root, summary):
        (root / "skills/fetchloom").mkdir(parents=True)
        (root / "skills/fetchloom/SKILL.md").write_text("skill", encoding="utf-8")
        (root / "docs/user").mkdir(parents=True)
        (root / "docs/user/one.md").write_text("\nfirst\n\n", encoding="utf-8")
        (root / "docs/user/two.md").write_text("second", encoding="utf-8")
        (root / "docs/SUMMARY.md").write_text(summary, encoding="utf-8")
        (root / "llms.txt").write_text(
            "[one](docs/user/one.md#heading) [two](docs/user/two.md) [skill](skills/fetchloom/SKILL.md) "
            "[code](src/lib.rs) [chart](docs/assets/bench/chart.svg) [site](https://example.org/a) [mail](mailto:a@b.c)\n\n",
            encoding="utf-8",
        )
        site = root / "site"
        site.mkdir()
        return site

    def test_exports_rewrite_links_and_follow_summary_order(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            site = self.checkout(root, "[two](user/two.md)\n[one](user/one.md)")
            printed = io.StringIO()
            with contextlib.redirect_stdout(printed):
                export_agents(root, site)
            self.assertEqual(printed.getvalue(), "llms-full.txt has 2 pages\n")
            blob = "https://github.com/nikhil-yn-aix/fetchloom/blob/main/"
            self.assertEqual((site / "llms.txt").read_text(encoding="utf-8"),
                             "[one](user/one.html#heading) [two](user/two.html) [skill](skills/fetchloom/SKILL.md) "
                             f"[code]({blob}src/lib.rs) [chart]({blob}docs/assets/bench/chart.svg) "
                             "[site](https://example.org/a) [mail](mailto:a@b.c)\n\n"
                             "## optional\n\n- [full user guide](llms-full.txt): every user guide page in one file\n")
            pages = "https://nikhil-yn-aix.github.io/fetchloom/"
            self.assertEqual((site / "llms-full.txt").read_text(encoding="utf-8"),
                             "# fetchloom user guide\n\n"
                             f"every page of the fetchloom user guide in reading order, from {pages}user/README.html\n\n"
                             f"---\n\nsource: {pages}user/two.html\n\nsecond\n\n"
                             f"---\n\nsource: {pages}user/one.html\n\nfirst\n")
            self.assertEqual((site / "skills/fetchloom/SKILL.md").read_text(encoding="utf-8"), "skill")

    def test_a_summary_without_user_pages_is_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            site = self.checkout(root, "[developer guide](dev/README.md)")
            with self.assertRaisesRegex(ValueError, "docs/SUMMARY.md lists no user guide pages"):
                export_agents(root, site)
            self.assertFalse((site / "llms-full.txt").exists())

    def test_the_site_is_built_where_it_was_asked_for(self):
        with tempfile.TemporaryDirectory() as folder:
            here = os.getcwd()
            os.chdir(folder)
            try:
                with patch.object(build_site.subprocess, "run") as run, patch.object(build_site, "export_agents") as export, \
                     patch.object(sys, "argv", ["build_site.py", "--dest-dir", "out/site"]):
                    build_site.main()
            finally:
                os.chdir(here)
            # mdbook reads a relative folder from the book, so the one given is made absolute here first
            site = Path(folder, "out/site").resolve()
            run.assert_called_once_with(["mdbook", "build", "--dest-dir", str(site)], cwd=build_site.ROOT, check=True)
            export.assert_called_once_with(build_site.ROOT, site)
