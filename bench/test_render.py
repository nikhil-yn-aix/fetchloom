import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import render


class Quiet(unittest.TestCase):
    """keeps the line each render prints, which says where it wrote."""

    def setUp(self):
        self.printed = self.enterContext(contextlib.redirect_stdout(io.StringIO()))


class RenderTests(Quiet):
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
        self.assertEqual(render.story("s8", 100, 1 << 20),
                         "a tar.gz of 100 files of csv, about 1 MiB each, downloaded, checked and unpacked")

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
            (root / "meta.json").write_text(json.dumps(dict(kind="lab", quick=True)))
            render.render(root, root / "report")
            page = (root / "report" / "docs" / "dev" / "performance" / "benchmarks.md").read_text()
            self.assertIn("wall time is the second attempt. CPU adds both attempts, peak memory is the larger "
                          "of the two and bytes cover both", page)
        finally:
            shutil.rmtree(root)

    def test_rerender_drops_a_hero_card_without_a_verdict(self):
        import json
        from pathlib import Path
        import shutil
        import render
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        (root / "report" / "docs" / "dev" / "performance").mkdir(parents=True)
        charts = root / "report" / "docs" / "assets" / "bench"
        try:
            record = dict(kind="lab", scenario="s1", profile="lan", filesystem="ext4", tool="fetchloom",
                          quick=True, count=1, file_size=4, status="passed", wall_seconds=1)
            rival = dict(record, tool="curl", wall_seconds=2)
            (root / "runs.jsonl").write_text(json.dumps(record) + "\n" + json.dumps(rival))
            (root / "meta.json").write_text(json.dumps(dict(kind="lab", quick=True)))
            render.render(root, root / "report")
            self.assertTrue((charts / "hero-s1-ext4.svg").exists())
            self.assertTrue((charts / "hero-s1-ext4-dark.svg").exists())
            failed = dict(record, status="failed")
            (root / "runs.jsonl").write_text(json.dumps(failed) + "\n" + json.dumps(rival))
            render.render(root, root / "report")
            self.assertTrue((charts / "s1-lan-ext4.svg").exists())
            self.assertEqual(sorted(path.name for path in charts.glob("hero-*")), [])
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
            (root / "meta.json").write_text(json.dumps(dict(kind="lab", quick=False)))
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
            (root / "meta.json").write_text(json.dumps(dict(kind="lab", quick=True)))
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


class CampaignTests(Quiet):
    def write_live(self, root, chunk, service="huggingface", ref="dataset-one", size=4, quick=False):
        import json
        folder = root / chunk
        folder.mkdir()
        record = dict(kind="live", service=service, tool="fetchloom", round=1, warmup=False,
                      quick=quick, count=1, file_size=size, status="passed", wall_seconds=1)
        dataset = dict(service=service, ref=ref, bytes=size, select=[], expected_sha256={"file": "digest"})
        (folder / "runs.jsonl").write_text(json.dumps(record) + "\n")
        (folder / "meta.json").write_text(json.dumps(dict(kind="live", quick=quick, datasets=[dataset])))
        return folder

    def test_live_dataset_changes_cannot_be_pooled(self):
        from pathlib import Path
        import render
        for changes in (dict(size=8), dict(quick=True), dict(ref="dataset-two"), dict()):
            with self.subTest(changes=changes), tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                self.write_live(root, "one")
                self.write_live(root, "two", **changes)
                report = root / "report"
                (report / "docs" / "dev" / "performance").mkdir(parents=True)
                cards = report / "docs" / "assets" / "bench"
                cards.mkdir(parents=True)
                sentinel = cards / "kept.svg"
                sentinel.write_text("previous output")
                with self.assertRaisesRegex(ValueError, "live"):
                    render.render(root, report)
                self.assertEqual(sentinel.read_text(), "previous output")

    def test_distinct_live_services_can_share_a_report(self):
        from pathlib import Path
        import render
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.write_live(root, "one", service="zenodo")
            self.write_live(root, "two", service="kaggle")
            report = root / "report"
            (report / "docs" / "dev" / "performance").mkdir(parents=True)
            render.render(root, report)
            self.assertTrue((report / "docs" / "assets" / "bench" / "live-zenodo.svg").is_file())
            self.assertTrue((report / "docs" / "assets" / "bench" / "live-kaggle.svg").is_file())

    def test_missing_metadata_cannot_be_rendered_as_a_full_campaign(self):
        from pathlib import Path
        import render
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            campaign = self.write_live(root, "one", quick=True)
            (campaign / "meta.json").unlink()
            report = root / "report"
            (report / "docs" / "dev" / "performance").mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, "metadata"):
                render.render(root, report)
            self.assertFalse((report / "docs" / "assets").exists())

    def test_live_rows_must_match_their_own_dataset_metadata(self):
        from pathlib import Path
        import json
        import render
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            campaign = self.write_live(root, "one")
            path = campaign / "runs.jsonl"
            row = json.loads(path.read_text())
            row["count"] = 2
            path.write_text(json.dumps(row))
            report = root / "report"
            (report / "docs" / "dev" / "performance").mkdir(parents=True)
            with self.assertRaisesRegex(ValueError, "conditions"):
                render.render(root, report)


def lab_row(**changes):
    return dict(dict(kind="lab", scenario="s1", profile="lan", filesystem="ext4", tool="fetchloom", quick=True,
                     count=1, file_size=4, status="passed", wall_seconds=1, round=1), **changes)


class LoadResultsTests(Quiet):
    def campaign(self, root, rows, meta):
        root.mkdir(parents=True, exist_ok=True)
        (root / "runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
        if meta is not None:
            (root / "meta.json").write_text(json.dumps(meta))

    def test_results_that_cannot_be_read_as_one_campaign_are_refused(self):
        live = dict(kind="live", quick=True, datasets=[dict(service="zenodo", bytes=3, expected_sha256={"a": "1"})])
        live_row = dict(kind="live", service="zenodo", tool="fetchloom", round=1, quick=True, count=1, file_size=3,
                        status="passed", files={"a": "1"})
        cases = [
            ("no runs", None, None, "no runs.jsonl found"),
            ("a list for metadata", [lab_row()], [], "invalid campaign metadata"),
            ("another kind", [lab_row()], dict(kind="native", quick=True), "invalid campaign metadata"),
            ("quick unsaid", [lab_row()], dict(kind="lab"), "invalid campaign metadata"),
            ("a service twice", [live_row], dict(live, datasets=live["datasets"] * 2), "live metadata repeats a service"),
            ("a full run in a smoke campaign", [lab_row(quick=False)], dict(kind="lab", quick=True), "mixed benchmark conditions in"),
            ("no tool", [{k: v for k, v in lab_row().items() if k != "tool"}], dict(kind="lab", quick=True), "a measured row has no tool"),
            ("a round twice", [lab_row(), lab_row()], dict(kind="lab", quick=True), "duplicate measured row"),
            ("no reference", [dict(live_row, service="kaggle")], live, "live metadata has no reference dataset for kaggle"),
            ("other bytes", [dict(live_row, files={"a": "2"})], live, "live checked bytes do not match metadata for zenodo"),
        ]
        for case, rows, meta, said in cases:
            with self.subTest(case), tempfile.TemporaryDirectory() as folder:
                if rows is not None:
                    self.campaign(Path(folder), rows, meta)
                with self.assertRaisesRegex(ValueError, said):
                    render.load_results(Path(folder))

    def test_skips_and_comparisons_of_a_live_campaign_are_kept_apart_from_its_timings(self):
        meta = dict(kind="live", quick=True, datasets=[dict(service="zenodo", bytes=3, expected_sha256={"a": "1"})])
        rows = [dict(kind="live", service="zenodo", tool="fetchloom", round=1, quick=True, count=1, file_size=3,
                     status="passed", wall_seconds=1, files={"a": "1"}),
                dict(kind="live", service="figshare", quick=True, status="skipped", reason="the service did not answer"),
                dict(kind="comparison", service="zenodo", round=1, comparison=dict(status="identical"))]
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            self.campaign(root / "live", rows, meta)
            loaded, metas = render.load_results(root)
            self.assertEqual((loaded, metas), (rows, [meta]))
            (root / "report/docs/dev/performance").mkdir(parents=True)
            render.render(root, root / "report")
            page = (root / "report/docs/dev/performance/benchmarks.md").read_text()
        self.assertIn("- figshare skipped: the service did not answer", page)
        self.assertIn('- zenodo round 1: `identical`; differences: `{"status": "identical"}`', page)


class PageTests(Quiet):
    def render(self, rows, meta):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        (root / "runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
        (root / "meta.json").write_text(json.dumps(meta))
        (root / "report/docs/dev/performance").mkdir(parents=True)
        render.render(root, root / "report")
        page = root / "report/docs/dev/performance/benchmarks.md"
        self.assertEqual(self.printed.getvalue(), f"wrote {page} and {root / 'report/docs/assets/bench'}\n")
        return page.read_text()

    def test_the_conditions_name_the_machine_the_binary_and_each_network(self):
        meta = dict(kind="lab", quick=False, cpu_affinity=[2, 3], nginx_cpu_affinity={"41": [4]},
                    fetchloom_binary=dict(path="/home/fl/bin/fetchloom", sha256="a" * 64), filesystems={"ext4": "ext4 rw"},
                    network_profiles=dict(lan=[0, 0, None], far=[150, 1, 50]))
        page = self.render([lab_row(quick=False), lab_row(quick=False, round=0, warmup=True, status="failed")], meta)
        self.assertIn("- measured CPUs: `[2, 3]`; nginx threads: `{'41': [4]}`", page)
        self.assertIn(f"- supplied executable: `/home/fl/bin/fetchloom`; SHA-256: `{'a' * 64}`", page)
        self.assertIn('- recorded `unknown`: lab run on `{"ext4": "ext4 rw"}`', page)
        self.assertIn("| lan | 0 ms | 0% | unlimited |\n| far | 150 ms | 1% | 50 Mbit/s |", page)
        self.assertIn("1 warmup commands failed; their records remain in runs.jsonl.", page)
        self.assertNotIn("smoke-test", page)

    def test_runs_that_differ_show_their_range_and_one_case_keeps_one_size(self):
        import xml.etree.ElementTree as ET
        self.render([lab_row(), lab_row(round=2, wall_seconds=2)], dict(kind="lab", quick=True))
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        lines = ET.parse(cards / "s1-lan-ext4.svg").findall(".//{http://www.w3.org/2000/svg}line")
        self.assertEqual(len(lines), 1)
        self.printed.truncate(0)
        self.printed.seek(0)
        with self.assertRaisesRegex(ValueError, r"mixed benchmark conditions for \('s1', 'lan', 'ext4'\)"):
            self.render([lab_row(), lab_row(round=2, count=2)], dict(kind="lab", quick=True))

    def test_cards_bars_and_ranges_have_square_ends(self):
        import xml.etree.ElementTree as ET
        self.render([lab_row(), lab_row(round=2, wall_seconds=2), lab_row(tool="curl", wall_seconds=3)],
                    dict(kind="lab", quick=True))
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        drawn = sorted(card.name for card in cards.glob("*.svg"))
        self.assertEqual(drawn, ["hero-s1-ext4-dark.svg", "hero-s1-ext4.svg", "s1-lan-ext4-dark.svg", "s1-lan-ext4.svg"])
        for name in drawn:
            for element in ET.parse(cards / name).iter():
                self.assertFalse({"rx", "ry"} & set(element.attrib), (name, element.attrib))
                self.assertNotEqual(element.get("stroke-linecap"), "round", (name, element.attrib))

    def test_cards_take_the_colors_of_the_site(self):
        """a card's paper is the site's surface and fetchloom's bar its accent, in light and dark."""
        import re
        import xml.etree.ElementTree as ET
        css = (Path(__file__).resolve().parents[1] / "docs-site/theme/assets/stylesheets/fetchloom.css").read_text()
        self.render([lab_row(), lab_row(tool="curl", wall_seconds=3)], dict(kind="lab", quick=True))
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        for name, scheme in (("s1-lan-ext4.svg", "default"), ("s1-lan-ext4-dark.svg", "slate")):
            block = css.split(f'[data-md-color-scheme="{scheme}"] {{', 1)[1].split("}", 1)[0]
            site = dict(re.findall(r"--fl-([a-z]+): (#[0-9a-f]{6})", block))
            fills = [element.get("fill") for element in ET.parse(cards / name).iter() if element.tag.endswith("rect")]
            self.assertEqual(fills[0], site["surface"], name)
            self.assertIn(site["accent"], fills, name)
            self.assertNotIn(site["muted"], fills, name)

    def test_a_resume_without_a_byte_count_says_so(self):
        page = self.render([lab_row(scenario="s7", profile="home")], dict(kind="lab", quick=True))
        self.assertIn("| tool | bytes served / file size |\n|---|---:|\n| fetchloom | n/a |", page)

    def test_the_command_renders_its_results_into_this_repository(self):
        with mock.patch.object(render, "render") as rendered, mock.patch.object(sys, "argv", ["render.py", "results"]):
            render.main()
        rendered.assert_called_once_with(Path("results").resolve(), Path(render.__file__).resolve().parent.parent)


class WordingTests(unittest.TestCase):
    def test_numbers_read_as_people_say_them(self):
        self.assertEqual([render.seconds(value) for value in (None, .0042, 2.5, 90)], ["n/a", "4 ms", "2.50 s", "1.50 min"])
        self.assertEqual([render.size(value) for value in (None, 512, 1536, 3 * 1024 ** 5)], ["n/a", "512 B", "1.5 KiB", "3072 TiB"])
        self.assertEqual([render.ratio(*pair) for pair in ((2, None), (2, 0), (1.02, 1), (3, 1.5))], ["n/a", "n/a", "even (<5%)", "2.00x"])
        self.assertEqual([render.times(value) for value in (12.4, 1.6, 3.0)], ["12\u00d7", "1.6\u00d7", "3\u00d7"])

    def test_a_verdict_needs_a_measured_fetchloom_and_a_rival(self):
        def summary(median, failures=0):
            return dict(median=median, failures=failures)

        for summaries, verdict in [
            ({"fetchloom": summary(1), "curl": summary(2)}, "2\u00d7 faster"),
            ({"fetchloom": summary(2), "curl": summary(1)}, "2\u00d7 slower"),
            ({"fetchloom": summary(1), "curl": summary(1.02)}, "even"),
            ({"fetchloom": summary(1, failures=1), "curl": summary(2)}, None),
            ({"fetchloom": summary(1), "curl": summary(2, failures=1)}, None),
            ({"fetchloom": summary(None), "curl": summary(2)}, None),
        ]:
            with self.subTest(verdict=verdict, summaries=summaries):
                self.assertEqual(render.verdict(summaries), verdict)
        self.assertEqual(render.no_bar(dict(failures=0, timeouts=0)), "skipped")


if __name__ == "__main__":
    unittest.main()
