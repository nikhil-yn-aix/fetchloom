import json
import math
from pathlib import Path
import tempfile
import unittest

import ab


def row(tool, number, wall=1, **values):
    return dict(kind="lab", tool=tool, filesystem="ext4", profile="lan", scenario="s2",
                round=number, warmup=False, status="passed", quick=False, count=1,
                file_size=8, wall_seconds=wall, **values)


class PairedTests(unittest.TestCase):
    def test_log_median_uses_corresponding_rounds(self):
        rows = [r for number, (a, b) in enumerate(zip((10, 100, 1000), (9, 500, 900)), 1)
                for r in (row("fetchloom", number, a), row("fetchloom+aa", number, b))]
        [result] = ab.compare(rows)
        self.assertAlmostEqual(result["ratios"]["wall"], .9)
        self.assertEqual((result["wins"], result["rounds"]), (2, 3))

    def test_missing_metric_does_not_pair_different_rounds(self):
        rows = [r for number, (a, b) in enumerate(((100, None), (None, 200), (100, 100)), 1)
                for r in (row("fetchloom", number, maxrss_bytes=a),
                          row("fetchloom+aa", number, maxrss_bytes=b))]
        [result] = ab.compare(rows)
        self.assertEqual(result["ratios"]["memory"], 1)
        metric = result["metrics"]["memory"]
        self.assertEqual(metric["n_available_pairs"], 1)
        self.assertEqual(metric["missing_metric_pairs"], 2)

    def test_even_log_median_is_geometric_midpoint(self):
        rows = [row("fetchloom", 1, 2), row("fetchloom+aa", 1, 1),
                row("fetchloom", 2, 4), row("fetchloom+aa", 2, 3)]
        [result] = ab.compare(rows)
        self.assertAlmostEqual(result["ratios"]["wall"], math.sqrt(.5 * .75))

    def test_zero_and_negative_pairs_keep_wins_without_log_ratios(self):
        rows = [r for number, (a, b) in enumerate(((0, 0), (0, 1), (2, -1), (4, 2)), 1)
                for r in (row("fetchloom", number, disk_bytes=a),
                          row("fetchloom+aa", number, disk_bytes=b))]
        [result] = ab.compare(rows)
        disk = result["metrics"]["disk"]
        self.assertEqual((disk["n_available_pairs"], disk["n_log_pairs"]), (4, 1))
        self.assertEqual(disk["wins"], dict(plain=1, variant=2, ties=1))
        self.assertEqual(disk["nonpositive_pairs"], 3)
        self.assertEqual(disk["decision"], "incomplete")

    def test_missing_cpu_and_nonfinite_values_are_unavailable(self):
        rows = [row("fetchloom", 1, user_seconds=1, sys_seconds=None, maxrss_bytes=float("nan")),
                row("fetchloom+aa", 1, user_seconds=1, sys_seconds=1, maxrss_bytes=1)]
        [result] = ab.compare(rows)
        self.assertIsNone(result["ratios"]["cpu"])
        self.assertIsNone(result["ratios"]["memory"])
        self.assertEqual(result["metrics"]["cpu"]["n_available_pairs"], 0)

    def test_values_past_what_a_float_holds_are_unavailable(self):
        # an int past 1.8e308 makes math.isfinite overflow, as a ratio past e**709 makes math.exp
        self.assertIsNone(ab.number(10 ** 400))
        self.assertEqual(ab.number(10 ** 300), 10 ** 300)
        self.assertIsNone(ab.exponent(1000))
        self.assertEqual(ab.exponent(0), 1)
        # a ratio of 1e600 has a log, but no float can hold the ratio itself
        rows = [r for n in range(1, 11) for r in (row("fetchloom", n, 1e-300), row("fetchloom+aa", n, 1e300))]
        [result] = ab.compare(rows)
        wall = result["metrics"]["wall"]
        self.assertAlmostEqual(wall["log_ratio_median"], math.log(1e300) - math.log(1e-300))
        self.assertIsNone(wall["bootstrap_95_median_ratio"])

    def test_constant_effect_has_exact_interval_and_threshold(self):
        rows = [r for n in range(1, 11) for r in (row("fetchloom", n, 2), row("fetchloom+aa", n, 1))]
        [result] = ab.compare(rows)
        wall = result["metrics"]["wall"]
        self.assertEqual(wall["bootstrap_95_median_ratio"], [.5, .5])
        self.assertAlmostEqual(wall["log_ratio_median"], math.log(.5))
        self.assertEqual((wall["decision"], wall["wins"]["variant"], wall["n_log_pairs"]), ("win", 10, 10))
        self.assertIn("95%", ab.table([result]))
        self.assertIn("upper", ab.table([result]))

    def test_bootstrap_seed_is_stable_when_other_variants_are_added(self):
        rows = [r for n in range(1, 11) for r in (row("fetchloom", n, n), row("fetchloom+aa", n, n * (.8 + n / 50)))]
        [first] = ab.compare(rows, resamples=200, seed=19)
        expanded = ab.compare(rows + [row("fetchloom+other", n, n * 2) for n in range(1, 11)],
                              resamples=200, seed=19)
        self.assertEqual(first, next(r for r in expanded if r["variant"] == "fetchloom+aa"))
        self.assertEqual(first, ab.compare(list(reversed(rows)), resamples=200, seed=19)[0])

    def test_failures_missing_rounds_and_small_samples_cannot_win(self):
        rows = [r for n in range(1, 11) for r in (row("fetchloom", n, 2), row("fetchloom+aa", n, 1))]
        [few] = ab.compare(rows[:2])
        self.assertEqual(few["metrics"]["wall"]["decision"], "insufficient")
        failed = dict(rows[-1], status="failed")
        [result] = ab.compare(rows[:-1] + [failed])
        self.assertEqual(result["metrics"]["wall"]["decision"], "failed")
        [missing] = ab.compare(rows[:-1])
        self.assertEqual(missing["metrics"]["wall"]["decision"], "incomplete")

    def test_comparison_parameters_are_validated(self):
        for arguments in (dict(threshold=-1), dict(threshold=1), dict(threshold=float("nan")),
                          dict(resamples=0), dict(min_pairs=1)):
            with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                ab.compare([], **arguments)

    def test_cli_writes_json_and_states_the_win_rule(self):
        import json
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "runs.jsonl").write_text(json.dumps(row("fetchloom", 1)) + "\n" +
                                           json.dumps(row("fetchloom+aa", 1, .9)) + "\n")
            output = root / "comparison.json"
            result = subprocess.run([sys.executable, str(Path(ab.__file__)), str(root), "--resamples", "100",
                                     "--threshold", "0.05", "--json", str(output)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("upper < 0.9500", result.stdout)
            data = json.loads(output.read_text())
            self.assertEqual(data["threshold"], .05)
            self.assertEqual(data["results"][0]["metrics"]["wall"]["n_log_pairs"], 1)


    def test_cli_reports_unreadable_results_as_a_usage_error_and_writes_json_only_when_asked(self):
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            missing = subprocess.run([sys.executable, str(Path(ab.__file__)), str(root)], capture_output=True, text=True)
            self.assertEqual(missing.returncode, 2)
            self.assertIn(f"ab.py: error: [Errno 2] No such file or directory: '{root / 'runs.jsonl'}'", missing.stderr)
            (root / "runs.jsonl").write_text(json.dumps(row("fetchloom", 1)) + "\n" + json.dumps(row("fetchloom+aa", 1, .9)) + "\n")
            result = subprocess.run([sys.executable, str(Path(ab.__file__)), str(root), "--resamples", "100"],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["runs.jsonl"])


class InputTests(unittest.TestCase):
    def test_duplicate_round_cannot_replace_a_failure(self):
        rows = [dict(row("fetchloom", 1), status="failed"), row("fetchloom", 1), row("fetchloom+aa", 1)]
        for supplied in (rows, list(reversed(rows))):
            with self.assertRaisesRegex(ValueError, "duplicate"):
                ab.compare(supplied)

    def test_paired_rows_must_have_the_same_workload(self):
        for changes in (dict(quick=True), dict(count=2), dict(file_size=16), dict(cpu_affinity=[1])):
            with self.subTest(changes=changes), self.assertRaisesRegex(ValueError, "conditions"):
                ab.compare([row("fetchloom", 1), dict(row("fetchloom+aa", 1), **changes)])

    def test_round_identity_is_a_positive_integer(self):
        for number in (True, 0, -1, "1"):
            with self.subTest(number=number), self.assertRaisesRegex(ValueError, "round"):
                ab.compare([row("fetchloom", number), row("fetchloom+aa", number)])

    def test_unmatched_rounds_are_named(self):
        [result] = ab.compare([row("fetchloom", 1), row("fetchloom", 2), row("fetchloom+aa", 1)])
        self.assertEqual(result["unmatched"], dict(base=[2], variant=[]))
        self.assertEqual(result["metrics"]["wall"]["decision"], "incomplete")

    def test_warmups_and_other_tools_are_left_out_and_a_failed_round_is_counted(self):
        rows = [dict(row("fetchloom", 0, 1), warmup=True), dict(row("fetchloom+lean", 0, 99), warmup=True),
                row("fetchloom", 1, 2), row("fetchloom+lean", 1, 1),
                row("fetchloom", 2, 4), row("fetchloom+lean", 2, 3),
                row("fetchloom", 3, 1), dict(row("fetchloom+lean", 3, 9), status="failed"),
                row("curl", 1, .1)]
        [result] = ab.compare(rows)
        self.assertEqual((result["case"], result["variant"]), ("ext4/lan/s2", "fetchloom+lean"))
        self.assertEqual((result["rounds"], result["wins"]), (2, 2))
        self.assertEqual(result["failed"], {"base": 0, "variant": 1})
        self.assertIn("fetchloom+lean", ab.table([result]))


if __name__ == "__main__":
    unittest.main()
