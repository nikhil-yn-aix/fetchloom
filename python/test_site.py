"""the same site exports serve pull-request checks and pages deployment."""

from pathlib import Path
import tempfile
import unittest

from build_site import export_agents


class SiteTests(unittest.TestCase):
    def test_exports_rewrite_links_and_follow_summary_order(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "skills/fetchloom").mkdir(parents=True)
            (root / "skills/fetchloom/SKILL.md").write_text("skill", encoding="utf-8")
            (root / "docs/user").mkdir(parents=True)
            (root / "docs/user/one.md").write_text("first", encoding="utf-8")
            (root / "docs/user/two.md").write_text("second", encoding="utf-8")
            (root / "docs/SUMMARY.md").write_text("[two](user/two.md)\n[one](user/one.md)", encoding="utf-8")
            (root / "llms.txt").write_text("[one](docs/user/one.md#heading) [skill](skills/fetchloom/SKILL.md) [code](src/lib.rs)", encoding="utf-8")
            site = root / "site"
            site.mkdir()
            export_agents(root, site)
            links = (site / "llms.txt").read_text(encoding="utf-8")
            self.assertIn("[one](user/one.html#heading)", links)
            self.assertIn("[skill](skills/fetchloom/SKILL.md)", links)
            self.assertIn("[code](https://github.com/nikhil-yn-aix/fetchloom/blob/main/src/lib.rs)", links)
            body = (site / "llms-full.txt").read_text(encoding="utf-8")
            self.assertLess(body.index("second"), body.index("first"))
            self.assertEqual((site / "skills/fetchloom/SKILL.md").read_text(encoding="utf-8"), "skill")
