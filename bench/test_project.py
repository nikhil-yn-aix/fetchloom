import unittest

import fixtures
import live
import project


def lab(scenario, profile, tool, seconds, filesystem="ext4"):
    return dict(kind="lab", filesystem=filesystem, profile=profile, scenario=scenario, tool=tool, round=1, warmup=False,
                status="passed", wall_seconds=seconds / 2, run_seconds=seconds)


class ProjectionTests(unittest.TestCase):
    def test_each_pilot_run_counts_as_many_times_as_the_real_run_runs_its_arm(self):
        rows = [lab("s1", "home", "fetchloom", 36), lab("s1", "home", "fetchloom+control", 36), lab("s1", "home", "curl", 72),
                lab("s1", "home", "pooch", 144), lab("s3", "lan", "fetchloom", 36), lab("s13", "local", "pooch", 360),
                dict(kind="live", service="zenodo", tool="fetchloom", round=1, status="passed", run_seconds=36),
                dict(kind="live", service="zenodo", tool="zenodo_get", round=1, status="passed", run_seconds=72)]
        metas = [dict(kind="live", datasets=[dict(service="zenodo", reference_seconds=360)])]
        hours = project.hours(rows, metas, {"ext4/home/s1": ["curl"]})
        # fetchloom, its control and the frozen curl run 10 rounds and a warmup; pooch 5 and a warmup
        self.assertAlmostEqual(hours["home"], (36 * 11 + 36 * 11 + 72 * 11 + 144 * 6) / 3600)
        # a case outside the headline set runs every arm 5 rounds and a warmup; this machine's disk counts as lan
        self.assertAlmostEqual(hours["lan"], (36 * 6 + 360 * 6) / 3600)
        # a live service's reference once, then 10 rounds of both tools
        self.assertAlmostEqual(hours["live"], (360 + 10 * 36 + 10 * 72) / 3600)
        # a warmup or a row with no cost on record adds nothing
        self.assertEqual(project.hours([dict(lab("s3", "lan", "curl", 36), warmup=True), dict(lab("s3", "lan", "dvc", 36), run_seconds=None)],
                                       [], {}), {})

    def test_a_poor_run_shrinks_its_timed_part_with_the_size_that_shrinks_in_it(self):
        rows = [lab("s1", "poor", "curl", 360), lab("s2r", "poor", "curl", 360)]
        full = project.hours(rows, [], {})["poor"]
        (large, many), (small, few) = fixtures.SIZES["poor"], fixtures.scaled("poor", .5)
        self.assertEqual((small, few), (large // 2, many // 2))
        # half of each run is timed and halves; the untimed half stays
        self.assertAlmostEqual(project.hours(rows, [], {}, .5)["poor"], full * .75)
        # a run with no timed part on record shrinks nothing
        self.assertAlmostEqual(project.hours([dict(rows[0], wall_seconds=None)], [], {}, .5)["poor"], 360 * 6 / 3600)
        # never past the floor, however small the scale
        self.assertEqual(fixtures.scaled("poor", .01), fixtures.POOR_FLOOR)
        self.assertEqual(fixtures.scaled("home", .5), fixtures.SIZES["home"])

    def test_the_pilot_rules_shrink_the_poor_link_first_and_then_move_the_live_part(self):
        def rows(poor_seconds, live_seconds):
            return [lab("s2r", "home", tool, 36) for tool in ("fetchloom", "fetchloom+control", "curl", "aria2c")] + [
                lab("s1", "poor", "curl", poor_seconds),
                dict(kind="live", service="kaggle", tool="kagglehub", round=1, status="passed", run_seconds=live_seconds)]

        # within 8 hours nothing changes, and two tools within 5% are both frozen
        within = project.rules(rows(600, 36), [])
        self.assertEqual((within["scale"], within["live"], within["comparators"]),
                         (1, "with the run", {"ext4/home/s2r": ["aria2c", "curl"], "ext4/poor/s1": ["curl"]}))
        # over it, the poor link shrinks by the largest scale, in hundredths, that brings the projection to 8 hours
        over = project.rules(rows(3000, 36), [])
        self.assertLess(over["scale"], 1)
        self.assertLessEqual(sum(over["hours_after"].values()), project.SHRINK_ABOVE)
        self.assertGreater(sum(project.hours(rows(3000, 36), [], over["comparators"], over["scale"] + .01).values()), project.SHRINK_ABOVE)
        self.assertEqual(over["live"], "with the run")
        # still over 9 hours at the poor link's floor, the live part moves to a daytime run
        late = project.rules(rows(6000, 3600), [])
        self.assertEqual((late["poor_sizes"], late["live"]), (dict(large_bytes=fixtures.POOR_FLOOR[0], files=fixtures.POOR_FLOOR[1]), "daytime"))
        self.assertAlmostEqual(late["total_hours"], sum(late["hours_after"].values()) - late["hours_after"]["live"])

    def test_still_over_9_hours_without_the_live_part_s3_and_s9_become_their_own_block(self):
        def rows(tiny_seconds):
            # the poor case costs 2.2 hours, and 1.375 at the floor, and the live part 3
            return [lab("s1", "poor", "curl", 720), lab("s3", "lan", "aria2c", tiny_seconds), lab("s9", "lan", "pooch", tiny_seconds),
                    dict(kind="live", service="kaggle", tool="kagglehub", round=1, status="passed", run_seconds=1080)]

        # 6 hours of tiny files: moving the live part out is enough, so they stay
        enough = project.rules(rows(1800), [])
        self.assertEqual((enough["scale"], enough["live"], enough["apart"]), (.01, "daytime", []))
        self.assertAlmostEqual(enough["total_hours"], 1.375 + 6)
        # 9 hours of them: still over 9 hours without the live part, so s3 and s9 run as their own block,
        # each arm still 5 rounds and a warmup
        over = project.rules(rows(2700), [])
        self.assertEqual((over["scale"], over["live"], over["apart"]), (.01, "daytime", ["s3", "s9"]))
        self.assertAlmostEqual(over["apart_hours"], 2 * 2700 * 6 / 3600)
        self.assertAlmostEqual(over["total_hours"], 1.375)
        # within 9 hours after the poor sizes shrink, neither moves
        short = project.rules(rows(600), [])
        self.assertEqual((short["live"], short["apart"]), ("with the run", []))

def pilot(**changes):
    """a whole pilot of the real run: every case it measures, with fetchloom and curl one round each,
    and every live service's reference and one round of both tools."""
    cases = sorted(project.expected_cases())
    rows = [lab(scenario, profile, tool, 60, filesystem=fs) for fs, profile, scenario in cases for tool in ("fetchloom", "curl")]
    metas = [dict(kind="lab", quick=False, cases=[dict(filesystem=fs, profile=profile, id=scenario, rounds=1, warmup=False,
                                                       arm_rounds={"fetchloom": 1, "curl": 1}) for fs, profile, scenario in cases])]
    metas.append(dict(kind="live", quick=False, rounds=1,
                      datasets=[dict(service=service, tool=f"{service}-tool", reference_seconds=30) for service in live.RUN]))
    rows += [dict(kind="live", service=service, tool=tool, round=1, status="passed", run_seconds=20)
             for service in live.RUN for tool in ("fetchloom", f"{service}-tool")]
    return dict(dict(rows=rows, metas=metas), **changes)


class PilotTests(unittest.TestCase):
    def refused(self, rows, metas):
        with self.assertRaises(ValueError) as refused:
            project.decide(rows, metas)
        return str(refused.exception)

    def test_a_whole_pilot_is_projected(self):
        whole = pilot()
        decided = project.decide(whole["rows"], whole["metas"])
        self.assertGreater(sum(decided["hours"].values()), 0)

    def test_a_pilot_that_cannot_stand_for_the_real_run_projects_nothing(self):
        whole = pilot()
        lab_meta, live_meta = whole["metas"]
        first = whole["rows"][0]
        for label, rows, metas, said in [
            ("stopped after fetchloom's first run", [first], whole["metas"], "no pilot run of"),
            ("an empty runs.jsonl beside its plan", [], whole["metas"], "no pilot run of"),
            ("a smoke run", whole["rows"], [dict(lab_meta, quick=True), live_meta], "a smoke run"),
            ("a case it never reached", whole["rows"], [dict(lab_meta, cases=lab_meta["cases"][1:]), live_meta], "never measured"),
            ("two rounds of an arm", whole["rows"] + [dict(first, round=2)],
             [dict(lab_meta, cases=[dict(lab_meta["cases"][0], arm_rounds={"fetchloom": 2, "curl": 1})] + lab_meta["cases"][1:]),
              live_meta], "one round"),
            ("a warmup", whole["rows"] + [dict(first, round=0, warmup=True)], whole["metas"], "one round"),
            ("a cost it never recorded", [dict(first, run_seconds=None)] + whole["rows"][1:], whole["metas"], "no cost"),
            ("a cost that is not a number", [dict(first, run_seconds=float("nan"))] + whole["rows"][1:], whole["metas"], "no cost"),
            # a smoke run, more rounds or a run with no cost refuses on the live link too
            ("a second live round", whole["rows"] + [dict(whole["rows"][-1], round=2)], whole["metas"], "one round"),
            ("a live run with no cost", whole["rows"][:-1] + [dict(whole["rows"][-1], run_seconds=None)], whole["metas"], "no cost"),
        ]:
            with self.subTest(label):
                told = self.refused(rows, metas)
                self.assertIn("the pilot cannot stand for the real run", told)
                self.assertIn(said, told)


class LiveGapTests(unittest.TestCase):
    def test_a_live_pilot_that_missed_a_service_or_a_round_moves_live_to_the_daytime_run(self):
        whole = pilot()
        lab_meta, live_meta = whole["metas"]
        lab_rows = [row for row in whole["rows"] if row["kind"] == "lab"]
        lab_alone = project.rules(lab_rows, [lab_meta])
        for label, rows, metas, said in [
            ("a service it could not reach", [row for row in whole["rows"] if row.get("service") != "kaggle"],
             [lab_meta, dict(live_meta, datasets=[d for d in live_meta["datasets"] if d["service"] != "kaggle"])],
             "live kaggle never measured"),
            ("a round it never made", whole["rows"][:-1], whole["metas"], "no pilot run of live dataverse dataverse-tool"),
            ("a round that failed", whole["rows"][:-1] + [dict(whole["rows"][-1], status="failed")], whole["metas"],
             "live dataverse dataverse-tool failed its pilot round"),
            ("no live pilot at all", lab_rows, [lab_meta], "no live pilot"),
        ]:
            with self.subTest(label):
                decided = project.decide(rows, metas)
                self.assertEqual(decided["live"], "daytime")
                self.assertIn(said, decided["live_reason"])
                # the lab goes ahead as its own pilot projects it, with no live hours in it
                self.assertNotIn("live", decided["hours"])
                self.assertEqual({key: decided[key] for key in ("hours", "scale", "comparators", "apart", "total_hours")},
                                 {key: lab_alone[key] for key in ("hours", "scale", "comparators", "apart", "total_hours")})

    def test_a_lab_gap_still_refuses_beside_a_live_one(self):
        whole = pilot()
        lab_meta, live_meta = whole["metas"]
        with self.assertRaises(ValueError) as refused:
            project.decide([row for row in whole["rows"] if row.get("service") != "kaggle"],
                           [dict(lab_meta, cases=lab_meta["cases"][1:]), live_meta])
        self.assertIn("never measured", str(refused.exception))


if __name__ == "__main__":
    unittest.main()
