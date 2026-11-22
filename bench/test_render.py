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
                          body_bytes_ratio=1.0)
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
            rows = headline_rows(curl=2)
            (root / "runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
            (root / "meta.json").write_text(json.dumps(lab_meta(rows, comparators={"ext4/home/s1": "curl"})))
            render.render(root, root / "report")
            self.assertTrue((charts / "hero-s1-home-ext4.svg").exists())
            self.assertTrue((charts / "hero-s1-home-ext4-dark.svg").exists())
            rows[0] = dict(rows[0], status="failed")
            (root / "runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
            render.render(root, root / "report")
            self.assertTrue((charts / "s1-home-ext4.svg").exists())
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
            twice = dict(kind="lab", quick=False, cases=[dict(filesystem="ext4", profile="home", id="s1"),
                                                         dict(filesystem="ext4", profile="lan", id="s1")])
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


def shape(delay, loss, rate, queue=None, limit=None):
    """a network profile as meta.json records it."""
    return dict(delay_ms=delay, loss_percent=loss, rate_mbit=rate, queue_ms=queue, limit_packets=limit)


def lab_row(**changes):
    return dict(dict(kind="lab", scenario="s1", profile="lan", filesystem="ext4", tool="fetchloom", quick=True,
                     count=1, file_size=4, status="passed", wall_seconds=1, round=1), **changes)


def lab_meta(rows, rounds=None, **changes):
    """a full lab campaign's meta.json for these rows, each case planned for `rounds` measured
    rounds, by default as many as its rows ran."""
    ran = {}
    for row in rows:
        key = (row["filesystem"], row["profile"], row["scenario"])
        ran[key] = max(ran.get(key, 0), row["round"])
    cases = [dict(filesystem=fs, profile=profile, id=scenario, rounds=rounds or most) for (fs, profile, scenario), most in ran.items()]
    return dict(dict(kind="lab", quick=False, cases=cases), **changes)


def headline_rows(rounds=10, scenario="s1", profile="home", control=(1,), **walls):
    """ten full rounds of a case: fetchloom at 1 s, its control arm at the given walls in turn, and each
    other tool at its own wall."""
    rows = []
    for number in range(1, rounds + 1):
        rows.append(lab_row(scenario=scenario, profile=profile, quick=False, round=number))
        if control:
            rows.append(lab_row(scenario=scenario, profile=profile, quick=False, round=number, tool="fetchloom+control",
                                wall_seconds=control[number % len(control)]))
        rows += [lab_row(scenario=scenario, profile=profile, quick=False, round=number, tool=tool, wall_seconds=wall)
                 for tool, wall in walls.items()]
    return rows


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
            ("a case not planned", [lab_row(), lab_row(scenario="s3")], lab_meta([lab_row()], quick=True),
             "a row of a case its campaign never listed"),
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
    def test_a_live_service_sets_its_own_tool_against_fetchloom_round_by_round(self):
        expected = {"a": "1"}
        datasets = [dict(service=service, bytes=3, expected_sha256=expected) for service in ("figshare", "zenodo")]

        def live_row(service, tool, number, wall, status="passed"):
            return dict(kind="live", service=service, tool=tool, round=number, quick=False, count=1, file_size=3,
                        status=status, wall_seconds=wall, files=expected)

        rows = []
        for number in range(1, 11):
            # a slow minute of the service slows both tools of its round alike
            slow = 3 if number == 4 else 1
            rows += [live_row("zenodo", "fetchloom", number, slow), live_row("zenodo", "zenodo_get", number, 2 * slow),
                     live_row("figshare", "fetchloom", number, 1),
                     live_row("figshare", "datahugger", number, 2, "failed" if number == 7 else "passed")]
        page = self.render(rows, dict(kind="live", quick=False, datasets=datasets))
        self.assertIn("## live services, on this machine's datacenter link\n", page)
        self.assertIn("in alternating order (ABBA)", page)
        self.assertNotIn("without ratios", page)
        self.assertIn("| tool | median | min to max | time over fetchloom's, 95% interval | paired rounds | CPU |", page)
        self.assertIn("| zenodo_get | 2.00 s | 2.00 s to 6.00 s | 2.00 [2.00, 2.00] | 10 |", page)
        self.assertIn("| fetchloom | 1.00 s | 1.00 s to 3.00 s |  |  |", page)
        self.assertIn("| datahugger | 2.00 s | 2.00 s to 2.00 s | failed 1 of 10 | 9 |", page)

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
                    network_profiles=dict(lan=shape(0, 0, None), poor=shape(300, 3, 10, 1000, 959)))
        page = self.render([lab_row(quick=False), lab_row(quick=False, round=0, warmup=True, status="failed")], meta)
        self.assertIn("- measured CPUs: `[2, 3]`; nginx threads: `{'41': [4]}`", page)
        self.assertIn(f"- supplied executable: `/home/fl/bin/fetchloom`; SHA-256: `{'a' * 64}`", page)
        self.assertIn('- recorded `unknown`: lab run on `{"ext4": "ext4 rw"}`', page)
        self.assertIn("| poor | poor connection | 300 ms | 3% | 10 Mbit/s | 1 s, 959 packets |\n"
                      "| lan | local network | 0 ms | 0% | unlimited | none |", page)
        self.assertIn("1 warmup commands failed; their records remain in runs.jsonl.", page)
        self.assertNotIn("smoke-test", page)

    def test_home_and_poor_links_lead_and_each_link_is_said_in_plain_words(self):
        import xml.etree.ElementTree as ET
        shaping = dict(lan=shape(0, 0, None), home=shape(20, 0, 100, 500, 4250), poor=shape(300, 3, 10, 1000, 959))
        rows = [lab_row(profile=profile, quick=False) for profile in ("lan", "poor", "home")]
        rows += [lab_row(profile=profile, quick=False, tool="curl", wall_seconds=2) for profile in ("lan", "home")]
        page = self.render(rows, dict(kind="lab", quick=False, network_profiles=shaping))
        self.assertEqual([line for line in page.splitlines() if line.startswith("## s1")],
                         ["## s1: one large file, home broadband (ext4)", "## s1: one large file, poor connection (ext4)",
                          "## s1: one large file, local network (ext4)"])
        self.assertIn("home broadband, about 100 Mbit and 20 ms: 1 file of 4 B, downloaded and checked", page)
        self.assertIn("\npoor connection, about 10 Mbit, 300 ms and 3% loss: 1 file", page)
        self.assertIn("\nlocal network, no added delay or limit: 1 file", page)
        self.assertIn("| profile | stands for | round-trip delay | loss per direction | server egress | queue at the server |\n"
                      "|---|---|---:|---:|---:|---:|\n| home | home broadband | 20 ms | 0% | 100 Mbit/s | 0.5 s, 4,250 packets |\n"
                      "| poor | poor connection | 300 ms | 3% | 10 Mbit/s | 1 s, 959 packets |", page)
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        svg = "{http://www.w3.org/2000/svg}"
        texts = [element.text for element in ET.parse(cards / "s1-home-ext4.svg").iter(svg + "text")]
        self.assertEqual(texts[1], "1 file of 4 B, home broadband, about 100 Mbit and 20 ms, ext4")

    def test_a_hero_comes_only_from_a_headline_case_with_a_verdict(self):
        # fetchloom failed a run of the headline case; s3 on lan is clear, but describes and claims nothing
        rows = headline_rows(curl=2) + headline_rows(scenario="s3", profile="lan", curl=2)
        rows[0] = dict(rows[0], status="failed")
        self.render(rows, lab_meta(rows, comparators={"ext4/home/s1": "curl"}))
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        self.assertEqual(sorted(card.name for card in cards.glob("hero-*")), [])

    def test_a_campaign_cut_short_of_its_planned_rounds_publishes_no_verdict(self):
        # every arm stopped after 10 of the 20 planned rounds, so each still pairs with fetchloom
        rows = headline_rows(curl=2)
        page = self.render(rows, lab_meta(rows, rounds=20, comparators={"ext4/home/s1": "curl"}))
        self.assertIn("| curl | 2.00 s | 2.00 s to 2.00 s | 2.00 [2.00, 2.00] | withheld |", page)
        self.assertIn("\nno verdict: fetchloom ran 10 of the 20 planned rounds.", page)
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        self.assertEqual(sorted(card.name for card in cards.glob("hero-*")), [])

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
        rows = headline_rows(curl=3)
        rows[0] = dict(rows[0], wall_seconds=2)
        self.render(rows, lab_meta(rows, comparators={"ext4/home/s1": "curl"}))
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        drawn = sorted(card.name for card in cards.glob("*.svg"))
        self.assertEqual(drawn, ["hero-s1-home-ext4-dark.svg", "hero-s1-home-ext4.svg", "s1-home-ext4-dark.svg", "s1-home-ext4.svg"])
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

    def test_cards_set_every_word_in_the_site_serif_as_written(self):
        """one face rides in each card, source serif 4, and a hero card names its scenario as written,
        never in capitals."""
        import xml.etree.ElementTree as ET
        self.render(headline_rows(curl=3), lab_meta(headline_rows(curl=3), comparators={"ext4/home/s1": "curl"}))
        cards = Path(self.printed.getvalue().split(" and ")[1].strip())
        svg = "{http://www.w3.org/2000/svg}"
        hero = ET.parse(cards / "hero-s1-home-ext4.svg")
        self.assertEqual({element.get("font-family") for element in hero.iter(svg + "text")}, {render.SERIF})
        self.assertEqual(hero.find(svg + "style").text.count("@font-face"), 1)
        # the seconds come first, then the ratio
        self.assertEqual([element.text for element in hero.iter(svg + "text")][:2],
                         ["one large file, home broadband: 1.00 s against 3.00 s for curl", "3\u00d7 faster"])

    def test_each_tool_shows_its_connections_and_the_most_requests_it_had_in_flight(self):
        rows = [lab_row(connections=2, peak_in_flight={"lab.flb": 4}, over_budget=False),
                lab_row(tool="curl", wall_seconds=2, connections=1, peak_in_flight={"lab.flb": 6, "store.flb": 1}, over_budget=True)]
        page = self.render(rows, dict(kind="lab", quick=True, budget=4))
        self.assertIn("| connections | most in flight to a host | failures |", page)
        self.assertIn("| n/a | 2 | 4 | 0/1 |", page)
        self.assertIn("| n/a | 1 | 6, over the budget in 1 of 1 runs | 0/1 |", page)
        self.assertIn("- politeness budget: at most 4 requests in flight to one host, for every tool", page)

    def test_each_tool_shows_its_whole_trees_memory_beside_its_largest_process_and_what_it_wrote(self):
        rows = [lab_row(user_seconds=.5, sys_seconds=.25, memory_bytes=300 << 20, maxrss_bytes=120 << 20, written_bytes=64 << 20,
                        disk_bytes=32 << 20)]
        page = self.render(rows, dict(kind="lab", quick=True))
        self.assertIn("| CPU | peak memory | largest process | written | disk | requests |", page)
        self.assertIn("| 750 ms | 300 MiB | 120 MiB | 64 MiB | 32 MiB |", page)
        self.assertIn("peak memory is the most anonymous memory those processes held at once, read from the cgroup every 20 ms", page)

    def test_the_most_in_flight_on_lan_is_called_an_estimate_with_its_floor(self):
        rows = [lab_row(peak_in_flight={"lab.flb": 3}, in_flight_floor_ms=2), lab_row(tool="curl", peak_in_flight={"lab.flb": 4}),
                lab_row(profile="home", peak_in_flight={"lab.flb": 4}, in_flight_floor_ms=0)]
        page = self.render(rows, dict(kind="lab", quick=True, budget=4))
        cases = {part.splitlines()[0]: part for part in page.split("\n## ")[1:]}
        lan = next(part for heading, part in cases.items() if heading.startswith("s1: ") and render.LINKS["lan"] in heading)
        home = next(part for heading, part in cases.items() if heading.startswith("s1: ") and render.LINKS["home"] in heading)
        self.assertIn("| connections | most in flight to a host, estimated | failures |", lan)
        self.assertIn("\nthe most in flight on this link is an estimate, with a floor of 2 ms: with no round trip to wait for, a "
                      "request counts only while nginx served it, less 2 ms for its log's millisecond clock, so a request "
                      "answered within 2 ms is not counted, and a tool over the budget by such short requests can go unflagged.", lan)
        self.assertIn("| connections | most in flight to a host | failures |", home)
        self.assertNotIn("an estimate", home)

    def test_a_library_timed_inside_a_warm_interpreter_says_so_with_its_import_beside(self):
        rows = [lab_row(), lab_row(tool="pooch", wall_seconds=2, clock="call inside a warm interpreter", import_seconds=.42,
                                   process=dict(wall_seconds=3.1, user_seconds=1, sys_seconds=0))]
        page = self.render(rows, dict(kind="lab", quick=True))
        self.assertIn("\npooch's times are its calls inside a warm interpreter, as a notebook makes them, and its CPU is theirs: "
                      "importing it took 420 ms more, and its whole process 3.10 s.", page)
        self.assertNotIn("fetchloom's times are", page)

    def test_a_dropped_link_is_set_against_the_same_tools_unbroken_run(self):
        rows = [lab_row(profile="home", wall_seconds=22), lab_row(profile="home", tool="curl", wall_seconds=24),
                lab_row(scenario="s10", profile="home", wall_seconds=33, body_bytes_ratio=1.0),
                lab_row(scenario="s10", profile="home", tool="curl", wall_seconds=41, body_bytes_ratio=1.02)]
        page = self.render(rows, dict(kind="lab", quick=True))
        self.assertIn("| tool | with the drop | unbroken, s1 on this link | added | server body bytes / dataset size |", page)
        self.assertIn("| fetchloom | 33.00 s | 22.00 s | +11.00 s | 1.000x |", page)
        self.assertIn("| curl | 41.00 s | 24.00 s | +17.00 s | 1.020x |", page)

    def test_a_dropped_link_without_the_same_file_unbroken_has_nothing_to_set_against(self):
        rows = [lab_row(profile="home", file_size=2), lab_row(scenario="s10", profile="home", wall_seconds=33, body_bytes_ratio=1.0)]
        page = self.render(rows, dict(kind="lab", quick=True))
        self.assertIn("| fetchloom | 33.00 s | n/a | n/a | 1.000x |", page)

    def test_a_verdict_gives_the_seconds_before_the_ratio(self):
        rows = headline_rows(curl=2, aria2c=1.5)
        page = self.render(rows, lab_meta(rows, comparators={"ext4/home/s1": "curl"}))
        self.assertIn("| time over fetchloom's, 99.375% interval | verdict |", page)
        self.assertIn("downloaded and checked, median of 10 runs.", page)
        self.assertIn("| curl | 2.00 s | 2.00 s to 2.00 s | 2.00 [2.00, 2.00] | fetchloom faster |", page)
        # faster in these runs, but not the comparator frozen before them, so it only describes
        self.assertIn("| aria2c | 1.50 s | 1.50 s to 1.50 s | 1.50 [1.50, 1.50] | descriptive |", page)
        self.assertIn("| fetchloom+control | 1.00 s | 1.00 s to 1.00 s | 1.00 [1.00, 1.00] | control arm, never a rival |", page)
        # against the comparator frozen before the runs, though aria2c was faster in them
        self.assertIn("\nverdict (headline, 99.375% interval): fetchloom 1.00 s against 2.00 s for curl, the comparator frozen "
                      "before these runs; curl's time over fetchloom's is 2.00, interval 2.00 to 2.00: fetchloom faster.", page)
        self.assertIn("the control arm, fetchloom run again, came to 1.00, 95% interval 1.00 to 1.00.", page)

    def test_a_no_op_says_what_each_tool_checks(self):
        page = self.render([lab_row(scenario="s6"), lab_row(scenario="s6", tool="pooch", wall_seconds=3)], dict(kind="lab", quick=True))
        self.assertIn("\neach tool checks something else when nothing changed: fetchloom trusts each file's size and modification time", page)
        self.assertIn("and aria2c and pooch hash every file again.", page)

    def test_the_next_version_says_how_each_tool_updates_and_what_it_fetched_again(self):
        page = self.render([lab_row(scenario="s12", profile="home", count=1000, file_size=102400, body_bytes_ratio=.1),
                            lab_row(scenario="s12", profile="home", count=1000, file_size=102400, tool="curl", body_bytes_ratio=1.0)],
                           dict(kind="lab", quick=True))
        self.assertIn("## s12: the next version, home broadband (ext4)", page)
        self.assertIn("1,000 files of 100 KiB each already in place, brought to the dataset's next version, in which one file in ten changed", page)
        self.assertIn("| tool | server body bytes / dataset size |\n|---|---:|\n| fetchloom | 0.100x |\n| curl | 1.000x |", page)
        self.assertIn("curl, which has no way to tell what changed, downloads every file again", page)

    def test_the_shared_folder_sets_each_push_against_fetchloom_beside_the_get(self):
        rows = [lab_row(scenario="s11", round=n, push=dict(wall_seconds=2, user_seconds=1, sys_seconds=0, memory_bytes=1 << 20,
                                                             written_bytes=4 << 20, disk_bytes=4 << 20)) for n in (1, 2)]
        rows += [lab_row(scenario="s11", round=n, tool="rsync", wall_seconds=.5, push=dict(wall_seconds=n, user_seconds=0,
                                                                                         sys_seconds=0, written_bytes=4 << 20))
                 for n in (1, 2)]
        page = self.render(rows, dict(kind="lab", quick=True))
        self.assertIn("## s11: the team's shared folder, local network (ext4)", page)
        self.assertIn("| tool | push, median | min to max | time over fetchloom's, 95% interval | CPU | peak memory | written | disk |\n"
                      "|---|---:|---|---:|---:|---:|---:|---:|\n"
                      "| fetchloom | 2.00 s | 2.00 s to 2.00 s |  | 1.00 s | 1 MiB | 4 MiB | 4 MiB |\n"
                      "| rsync | 1.50 s | 1.00 s to 2.00 s | 0.71 [0.50, 1.00] | 0 ms | n/a | 4 MiB | n/a |", page)
        self.assertIn("the lab's server answers neither", page)

    def test_a_resume_without_a_byte_count_says_so(self):
        page = self.render([lab_row(scenario="s7", profile="home")], dict(kind="lab", quick=True))
        self.assertIn("| tool | server body bytes / dataset size |\n|---|---:|\n| fetchloom | n/a |", page)
        self.assertIn("server body bytes / dataset size counts the body bytes of the server's whole and partial answers for the file, "
                      "so it says what a tool asked for again; the server cannot see packets the network sent twice.", page)

    def test_the_command_renders_its_results_into_this_repository(self):
        with mock.patch.object(render, "render") as rendered, mock.patch.object(sys, "argv", ["render.py", "results"]):
            render.main()
        rendered.assert_called_once_with(Path("results").resolve(), Path(render.__file__).resolve().parent.parent)


class VerdictTests(unittest.TestCase):
    def judged(self, rows, scenario="s1", profile="home", filesystem="ext4", comparator=None, planned=10):
        runs = {}
        for row in rows:
            runs.setdefault(row["tool"], []).append(row)
        case = dict(scenario=scenario, profile=profile, filesystem=filesystem, summaries=render.summaries(runs), planned=planned)
        return render.judge(case, runs, comparator)

    def test_a_headline_case_sets_fetchloom_against_the_comparator_frozen_before_its_runs(self):
        rows = headline_rows(curl=2, aria2c=1.5)
        # faster still, but it failed a run
        rows += [lab_row(profile="home", quick=False, round=n, tool="pooch", wall_seconds=1.2,
                         status="failed" if n == 3 else "passed") for n in range(1, 11)]
        judged = self.judged(rows, comparator="aria2c")
        self.assertEqual((judged["headline"], judged["rival"], judged["reason"], judged["comparator"]), (True, "aria2c", None, "aria2c"))
        # the data that judges a headline case never chooses its rival: not the fastest, and none at all unfrozen
        self.assertEqual(self.judged(rows, comparator="curl")["rival"], "curl")
        self.assertEqual(self.judged(rows)["reason"], "no comparator was frozen for it before its runs")
        self.assertEqual((self.judged(rows, comparator="pooch")["rival"], self.judged(rows, comparator="pooch")["reason"]),
                         (None, "pooch, the frozen comparator, failed 1 of 10 runs"))
        self.assertEqual(self.judged(rows, comparator="dvc")["reason"], "dvc, the frozen comparator, did not run")
        self.assertAlmostEqual(judged["level"], 1 - .05 / 8)
        aria2c = judged["comparisons"]["aria2c"]
        self.assertEqual((aria2c["pairs"], aria2c["word"]), (10, "fetchloom faster"))
        self.assertEqual([round(value, 9) for value in (aria2c["median_ratio"], *aria2c["interval"])], [1.5, 1.5, 1.5])
        self.assertEqual((judged["comparisons"]["pooch"]["failed"], judged["comparisons"]["pooch"]["word"]), (1, None))

    def test_the_control_arm_is_never_a_rival_and_its_noise_withholds_a_headline_verdict_without_loss(self):
        # on the poor link fetchloom against itself may spread as the network does, and is still no rival
        judged = self.judged(headline_rows(scenario="s2r", profile="poor", control=(.5,), curl=2), scenario="s2r", profile="poor",
                             comparator="curl")
        self.assertEqual((judged["headline"], judged["rival"], judged["reason"]), (True, "curl", None))
        noisy = self.judged(headline_rows(control=(.8, 1.25), curl=2), comparator="curl")
        self.assertIsNone(noisy["rival"])
        self.assertRegex(noisy["reason"], r"^the control arm's 95% interval, 0\.\d\d to 1\.\d\d, is not inside 0\.97 to 1\.03, so the machine was too noisy$")
        alone = self.judged(headline_rows(control=(), curl=2), comparator="curl")
        self.assertEqual((alone["rival"], alone["reason"]), (None, "no control arm measured this machine's noise"))
        # the control runs in the headline cases alone, so a case that only describes needs none
        described = self.judged(headline_rows(scenario="s3", profile="lan", control=(), curl=2), scenario="s3", profile="lan")
        self.assertEqual((described["rival"], described["reason"], described["withheld"]), ("curl", None, None))

    def test_a_verdict_needs_ten_paired_rounds_and_a_fetchloom_that_never_failed(self):
        poor = self.judged(headline_rows(rounds=9, scenario="s2r", profile="poor", curl=2), scenario="s2r", profile="poor",
                           comparator="curl", planned=9)
        self.assertEqual((poor["rival"], poor["reason"]), (None, "curl, the frozen comparator, has 9 paired rounds, fewer than 10"))
        rows = headline_rows(curl=2)
        rows[0] = dict(rows[0], status="failed")
        self.assertEqual(self.judged(rows)["reason"], "fetchloom failed 1 of 10 runs")

    def test_the_control_gate_needs_every_round_passed_and_paired_and_withholds_every_verdict_of_its_case(self):
        rows = headline_rows(curl=2)
        failed = [dict(row, status="failed") if (row["tool"], row["round"]) == ("fetchloom+control", 4) else row for row in rows]
        missing = [row for row in rows if (row["tool"], row["round"]) != ("fetchloom+control", 10)]
        for changed, planned, said in (
                (failed, 10, "the control arm failed 1 of 10 runs, so it cannot measure this machine's noise"),
                (missing, 10, "the control arm ran other rounds than fetchloom, so it cannot measure this machine's noise"),
                (headline_rows(rounds=9, curl=2), 9, "the control arm has 9 paired rounds, fewer than 10, so it cannot measure this machine's noise")):
            judged = self.judged(changed, comparator="curl", planned=planned)
            self.assertEqual((judged["rival"], judged["reason"], judged["withheld"]), (None, said, said))
            # the table's verdicts are withheld too, though curl's interval alone would say fetchloom faster
            self.assertIsNone(judged["comparisons"]["curl"]["word"])
            self.assertEqual(render.said("curl", judged), "withheld")
        # on the lossy link the control arm reports the network's spread and gates nothing
        poor = self.judged([dict(row, scenario="s2r", profile="poor") for row in failed], scenario="s2r", profile="poor",
                           comparator="curl")
        self.assertEqual((poor["rival"], poor["withheld"]), ("curl", None))

    def test_every_arm_must_run_the_rounds_its_campaign_planned(self):
        # cut short together after 10 of 20 planned rounds, every arm still pairs with fetchloom
        cut = headline_rows(curl=2)
        judged = self.judged(cut, comparator="curl", planned=20)
        said = "fetchloom ran 10 of the 20 planned rounds"
        self.assertEqual((judged["rival"], judged["reason"], judged["withheld"]), (None, said, said))
        self.assertEqual((judged["comparisons"]["curl"]["word"], render.said("curl", judged)), (None, "withheld"))
        self.assertIsNone(render.hero(dict(scenario="s1", profile="home", summaries={}, judged=judged)))
        self.assertEqual(self.judged(cut, comparator="curl")["rival"], "curl")
        # no plan on record, or rounds the plan never had, keep no plan either
        self.assertEqual(self.judged(cut, comparator="curl", planned=None)["reason"],
                         "its campaign's meta.json records no planned rounds for it")
        self.assertEqual(self.judged(cut, comparator="curl", planned=8)["reason"], "fetchloom ran rounds other than the 8 planned")
        # fetchloom ran its plan, the control arm stopped short with the comparator: still no verdict
        short = [row for row in headline_rows(rounds=20, curl=2) if row["tool"] == "fetchloom" or row["round"] <= 10]
        self.assertEqual(self.judged(short, comparator="curl", planned=20)["withheld"],
                         "the control arm ran other rounds than fetchloom, so it cannot measure this machine's noise")

    def test_a_run_over_the_budget_withholds_its_verdict_and_keeps_its_measurements(self):
        rows = headline_rows(curl=2, aria2c=1.5)
        impolite = [dict(row, over_budget=row["tool"] == "aria2c" and row["round"] == 3) for row in rows]
        judged = self.judged(impolite, comparator="aria2c")
        aria2c = judged["comparisons"]["aria2c"]
        # the frozen comparator was impolite once, so the case has no verdict, though its ratio stays
        self.assertEqual((judged["rival"], judged["reason"]), (None, "aria2c, the frozen comparator, went over the budget in 1 of 10 runs"))
        self.assertEqual((aria2c["word"], render.said("aria2c", judged), round(aria2c["median_ratio"], 9)),
                         (None, "went over the budget in 1 of 10 runs", 1.5))
        own = self.judged([dict(row, over_budget=row["tool"] == "fetchloom" and row["round"] == 7) for row in rows], comparator="curl")
        self.assertEqual((own["rival"], own["reason"]),
                         (None, "fetchloom went over the budget in 1 of 10 runs, so no comparison is at the same politeness"))
        self.assertEqual([render.said(tool, own) for tool in ("curl", "aria2c")], ["withheld", "withheld"])
        self.assertEqual(round(own["comparisons"]["curl"]["median_ratio"], 9), 2)

    def test_a_pilot_freezes_the_fastest_polite_tool_that_passed_for_each_headline_case_alone(self):
        rows = headline_rows(rounds=1, curl=2, aria2c=1.5, pooch=1.2, dvc=1.1)
        rows = [dict(row, status="failed") if row["tool"] == "pooch" else dict(row, over_budget=True) if row["tool"] == "dvc" else row
                for row in rows]
        rows += headline_rows(rounds=1, scenario="s3", profile="lan", datalad=.5)
        # the control arm, faster than all, is fetchloom and never a comparator
        rows += [lab_row(profile="home", quick=False, tool="fetchloom+control", wall_seconds=.1, round=1)]
        self.assertEqual(render.pick([dict(row, warmup=False) for row in rows]), {"ext4/home/s1": "aria2c"})

    def test_campaigns_that_froze_different_comparators_cannot_be_read_as_one(self):
        self.assertEqual(render.frozen([dict(comparators={"ext4/home/s1": "curl"}), dict(comparators={"ext4/home/s1": "curl"}), {}]),
                         {"ext4/home/s1": "curl"})
        with self.assertRaisesRegex(ValueError, "froze different comparators for ext4/home/s1"):
            render.frozen([dict(comparators={"ext4/home/s1": "curl"}), dict(comparators={"ext4/home/s1": "aria2c"})])
        # a comparator counts only from the campaign that measured its case, never from a later chunk's record
        def measured(scenario):
            return [dict(filesystem="ext4", profile="home", id=scenario)]
        self.assertEqual(render.frozen([dict(cases=measured("s1")),
                                        dict(cases=measured("s2r"), comparators={"ext4/home/s1": "curl", "ext4/home/s2r": "aria2c"})]),
                         {"ext4/home/s2r": "aria2c"})

    def test_only_the_frozen_comparator_gets_a_verdict_and_every_other_tool_describes(self):
        rows = headline_rows(curl=2, aria2c=1.5, pooch=.5)
        judged = self.judged(rows, comparator="curl")
        self.assertEqual({tool: comparison["word"] for tool, comparison in judged["comparisons"].items()},
                         {"fetchloom+control": None, "curl": "fetchloom faster", "aria2c": None, "pooch": None})
        self.assertEqual([render.said(tool, judged) for tool in ("curl", "aria2c", "pooch")], ["fetchloom faster", "descriptive", "descriptive"])
        # a headline case with no comparator frozen before its runs has no verdict at all, nor any row
        unfrozen = self.judged(rows)
        self.assertEqual((unfrozen["rival"], unfrozen["withheld"]), (None, "no comparator was frozen for it before its runs"))
        self.assertEqual({comparison["word"] for comparison in unfrozen["comparisons"].values()}, {None})
        self.assertEqual({render.said(tool, unfrozen) for tool in ("curl", "aria2c", "pooch")}, {"withheld"})
        self.assertIsNone(render.hero(dict(scenario="s1", profile="home", summaries={}, judged=unfrozen)))

    def test_a_headline_verdict_rests_on_ten_rounds_and_every_other_row_on_five(self):
        rows = headline_rows(curl=2) + [row for row in headline_rows(control=(), aria2c=1.5) if row["tool"] == "aria2c" and row["round"] <= 5]
        judged = self.judged(rows, comparator="curl")
        self.assertEqual((judged["rival"], judged["comparisons"]["curl"]["pairs"], judged["comparisons"]["curl"]["word"]),
                         ("curl", 10, "fetchloom faster"))
        aria2c = judged["comparisons"]["aria2c"]
        self.assertEqual((aria2c["pairs"], round(aria2c["median_ratio"], 9), aria2c["word"], render.said("aria2c", judged)),
                         (5, 1.5, None, "descriptive"))
        # a case outside the headline set runs every tool 5 rounds, and still describes its fastest one
        described = self.judged([row for row in headline_rows(scenario="s3", profile="lan", control=(), curl=.5) if row["round"] <= 5],
                                scenario="s3", profile="lan", planned=5)
        self.assertEqual((described["rival"], described["reason"], described["comparisons"]["curl"]["pairs"]), ("curl", None, 5))

    def test_a_case_outside_the_headline_set_describes_its_fastest_tool_with_no_verdict(self):
        rows = headline_rows(scenario="s3", profile="lan", curl=.5, aria2c=.8)
        judged = self.judged(rows, scenario="s3", profile="lan")
        self.assertEqual((judged["headline"], judged["level"], judged["rival"], judged["reason"]), (False, .95, "curl", None))
        self.assertEqual({comparison["word"] for comparison in judged["comparisons"].values()}, {None})
        self.assertEqual([render.said(tool, judged) for tool in ("curl", "aria2c")], ["descriptive", "descriptive"])
        self.assertIsNone(render.hero(dict(scenario="s3", profile="lan", summaries={}, judged=judged)))
        case = dict(scenario="s3", profile="lan", summaries=render.summaries({"fetchloom": rows[:1], "curl": rows[2:3]}), judged=judged)
        self.assertEqual(render.verdict_lines(case)[0],
                         "\ndescriptive, 95% interval, no verdict: fetchloom 1.00 s against 500 ms for curl, the fastest other tool "
                         "that passed every run; curl's time over fetchloom's is 0.50, interval 0.50 to 0.50.")


class WordingTests(unittest.TestCase):
    def test_numbers_read_as_people_say_them(self):
        self.assertEqual([render.seconds(value) for value in (None, .0042, 2.5, 90)], ["n/a", "4 ms", "2.50 s", "1.50 min"])
        self.assertEqual([render.size(value) for value in (None, 512, 1536, 3 * 1024 ** 5)], ["n/a", "512 B", "1.5 KiB", "3072 TiB"])
        self.assertEqual([render.times(value) for value in (12.4, 1.6, 3.0)], ["12\u00d7", "1.6\u00d7", "3\u00d7"])

    def test_a_link_reads_as_its_name_and_shaping(self):
        self.assertEqual([render.link(*pair) for pair in (("home", shape(20, 0, 100)), ("poor", shape(300, 3, 10)),
                                                         ("lan", shape(0, 0, None)), ("home", None), ("cable", shape(5, 0, None)))],
                         ["home broadband, about 100 Mbit and 20 ms", "poor connection, about 10 Mbit, 300 ms and 3% loss",
                          "local network, no added delay or limit", "home broadband", "cable, no rate limit and 5 ms"])

    def test_even_is_an_interval_inside_five_percent_and_anything_unclear_is_inconclusive(self):
        for interval, said in [([1.06, 1.2], "fetchloom faster"), ([.8, .94], "curl faster"), ([.95, 1.05], "even"),
                               ([.97, 1.02], "even"), ([.96, 1.06], "inconclusive"), ([1.05, 1.3], "inconclusive"),
                               ([.9, .95], "inconclusive"), ([.5, 2], "inconclusive")]:
            self.assertEqual(render.word(interval, "curl"), said, interval)
        self.assertEqual(render.no_bar(dict(failures=0, timeouts=0)), "skipped")

    def test_eight_headline_comparisons_share_a_five_percent_error(self):
        self.assertEqual(sorted(render.HEADLINE), sorted([
            ("ext4", "home", "s1"), ("ext4", "poor", "s1"), ("ext4", "home", "s2r"), ("ext4", "poor", "s2r"),
            ("ext4", "lan", "s5"), ("ext4", "home", "s7"), ("ext4", "home", "s10"), ("ext4", "home", "s12")]))
        self.assertAlmostEqual(render.FAMILY, 1 - .05 / 8)
        self.assertEqual((render.percent(render.FAMILY), render.percent(.95)), ("99.375%", "95%"))


if __name__ == "__main__":
    unittest.main()
