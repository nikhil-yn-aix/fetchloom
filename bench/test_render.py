import tempfile
import unittest

class RenderTests(unittest.TestCase):
    def test_failed_runs_do_not_lower_median(self):
        import render
        summary = render.summarize([
            {"status": "passed", "wall_seconds": 3, "user_seconds": 1, "sys_seconds": .5},
            {"status": "failed", "wall_seconds": .1},
            {"status": "passed", "wall_seconds": 5, "user_seconds": 2, "sys_seconds": 1},
        ])
        self.assertEqual(summary["median"], 4)
        self.assertEqual(summary["low"], 3)
        self.assertEqual(summary["high"], 5)
        self.assertEqual(summary["failures"], 1)
        self.assertEqual(summary["cpu"], 2.25)
        self.assertIsNone(render.summarize([{"status": "failed"}])["median"])
        late = render.summarize([{"status": "failed", "timeout": True}, {"status": "passed", "wall_seconds": 1}])
        self.assertEqual((render.no_bar(late), render.failures(late)), ("timed out", "1/2 timed out"))
        mixed = render.summarize([{"status": "failed", "timeout": True}, {"status": "failed"}])
        self.assertEqual((render.no_bar(mixed), render.failures(mixed)), ("failed", "2/2"))

    def test_archive_story_names_its_members(self):
        import render
        self.assertEqual(render.story("s4", 10000, 4096),
                         "a zip of 10,000 files of 4 KiB each, downloaded, checked and unpacked")

    def test_resume_note_covers_cpu_and_memory_of_both_attempts(self):
        import json
        from pathlib import Path
        import shutil
        import render
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        (root / "report" / "docs" / "dev" / "performance").mkdir(parents=True)
        try:
            record = dict(kind="lab", scenario="s7", profile="home", filesystem="ext4", tool="fetchloom",
                          quick=True, count=1, file_size=4, status="passed", wall_seconds=1,
                          download_ratio=1.0)
            (root / "runs.jsonl").write_text(json.dumps(record))
            render.render(root, root / "report")
            page = (root / "report" / "docs" / "dev" / "performance" / "benchmarks.md").read_text()
            self.assertIn("wall time is the second attempt. CPU adds both attempts, peak memory is the larger "
                          "of the two and bytes cover both", page)
        finally:
            shutil.rmtree(root)

    def test_renderer_rejects_mixed_smoke_and_full_conditions(self):
        import json
        from pathlib import Path
        import shutil
        import render
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        (root / "report" / "docs" / "dev" / "performance").mkdir(parents=True)
        try:
            records = [dict(kind="lab", scenario="s1", profile="lan", filesystem="ext4",
                            tool="fetchloom", quick=quick, count=1, file_size=size,
                            status="passed", wall_seconds=1)
                       for quick, size in ((True, 2097152), (False, 1073741824))]
            (root / "runs.jsonl").write_text("\n".join(json.dumps(row) for row in records))
            with self.assertRaisesRegex(ValueError, "conditions"):
                render.render(root, root / "report")
        finally:
            shutil.rmtree(root)

    def test_renderer_joins_campaigns_that_measure_different_cases(self):
        import json
        from pathlib import Path
        import shutil
        import render
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        (root / "report" / "docs" / "dev" / "performance").mkdir(parents=True)
        try:
            for chunk, (fs, profile) in {"one": ("ext4", "lan"), "two": ("ext4", "home"), "three": ("xfs", "lan")}.items():
                (root / chunk).mkdir()
                record = dict(kind="lab", scenario="s1", profile=profile, filesystem=fs, tool="fetchloom",
                              quick=False, count=1, file_size=4, status="passed", wall_seconds=1)
                (root / chunk / "runs.jsonl").write_text(json.dumps(record))
                case = dict(filesystem=fs, profile=profile, id="s1")
                (root / chunk / "meta.json").write_text(json.dumps(dict(kind="lab", quick=False, cases=[case])))
            render.render(root, root / "report")
            for name in ("s1-lan-ext4", "s1-home-ext4", "s1-lan-xfs"):
                self.assertTrue((root / "report" / "docs" / "assets" / "bench" / f"{name}.svg").exists())
            page = (root / "report" / "docs" / "dev" / "performance" / "benchmarks.md").read_text()
            self.assertEqual(page.count("- CPU:"), 1)
            self.assertEqual(page.count("- recorded"), 3)
            self.assertIn("xfs/lan/s1", page)
            twice = dict(kind="lab", quick=False, cases=[dict(filesystem="ext4", profile="lan", id="s1")])
            (root / "two" / "meta.json").write_text(json.dumps(twice))
            with self.assertRaisesRegex(ValueError, "twice"):
                render.render(root, root / "report")
        finally:
            shutil.rmtree(root)

    def test_renderer_keeps_text_and_marks_failures(self):
        import json
        from pathlib import Path
        import shutil
        import xml.etree.ElementTree as ET
        import render
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        (root / "report" / "docs" / "dev" / "performance").mkdir(parents=True)
        try:
            record = dict(kind="lab", scenario="s1", profile="lan", filesystem="ext4",
                          tool="fetchloom", quick=True, count=1, file_size=4,
                          status="passed", wall_seconds=2)
            failed = dict(record, tool="curl", status="failed", wall_seconds=.001)
            (root / "runs.jsonl").write_text(json.dumps(record) + "\n" + json.dumps(failed))
            (root / "meta.json").write_text(json.dumps(dict(quick=True)))
            render.render(root, root / "report")
            for dark in ("", "-dark"):
                path = root / "report" / "docs" / "assets" / "bench" / f"s1-lan-ext4{dark}.svg"
                texts = ET.parse(path).findall(".//{http://www.w3.org/2000/svg}text")
                self.assertEqual(texts[0].text, "one large file")
                self.assertIn("failed", [item.text for item in texts])
                self.assertIn("2.00 s", [item.text for item in texts])
            page = (root / "report" / "docs" / "dev" / "performance" / "benchmarks.md").read_text()
            self.assertIn("smoke-test", page)
            self.assertIn("prefers-color-scheme: dark", page)
        finally:
            shutil.rmtree(root)


if __name__ == "__main__":
    unittest.main()
