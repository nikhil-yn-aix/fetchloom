import unittest

import fixtures
import lab


class FixtureTests(unittest.TestCase):
    def test_seed_controls_entropy(self):
        a = fixtures.content(17, "a", 4096)
        self.assertEqual(len(a), 4096)
        self.assertEqual(a, fixtures.content(17, "a", 4096))
        self.assertNotEqual(a, fixtures.content(18, "a", 4096))
        self.assertNotEqual(a, fixtures.content(17, "b", 4096))
        self.assertGreater(len(set(a)), 240)

    def test_cases_use_only_design_profiles(self):
        cases = fixtures.case_specs(["lan", "home"], ["s1", "s3", "s7"], False)
        self.assertEqual([(c["profile"], c["id"]) for c in cases],
                         [("lan", "s1"), ("lan", "s3"), ("home", "s1"), ("home", "s7")])
        self.assertEqual(cases[0]["size"], 1073741824)
        self.assertEqual(cases[1]["count"], 10000)
        self.assertEqual(cases[1]["size"], 4096)

    def test_shaping_splits_roundtrip_and_limits_server(self):
        client, server = lab.shaping("home")
        self.assertIn("10ms", client)
        self.assertIn("10ms", server)
        self.assertNotIn("rate", client)
        self.assertEqual(server[-2:], ["rate", "100mbit"])



class MeasurementTests(unittest.TestCase):
    def test_exclusive_lock_rejects_overlapping_runs(self):
        from pathlib import Path
        import shutil
        root = Path("/srv/fl-bench/unit-lock")
        root.mkdir(parents=True, exist_ok=True)
        try:
            with lab.exclusive(root / "lock"):
                with self.assertRaises(RuntimeError):
                    lab.exclusive(root / "lock")
            with lab.exclusive(root / "lock"):
                pass
        finally:
            shutil.rmtree(root)

    def test_checker_rejects_missing_and_wrong_files(self):
        import run
        from pathlib import Path
        import shutil
        root = Path("/srv/fl-bench/unit-checks")
        root.mkdir(parents=True, exist_ok=True)
        try:
            path = root / "value"
            path.write_bytes(b"abc")
            self.assertEqual(run.verify([(path, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")]), [])
            self.assertEqual(len(run.verify([(path, "0" * 64)])), 1)
            self.assertEqual(len(run.verify([(root / "missing", "0" * 64)])), 1)
        finally:
            shutil.rmtree(root)

    def test_process_exit_timeout_and_interrupt_are_distinct(self):
        import run
        import os
        from pathlib import Path
        import shutil
        root = Path("/srv/fl-bench/unit-process")
        root.mkdir(parents=True, exist_ok=True)
        try:
            passed = run.measure(["/bin/true"], root, os.environ, root / "log", 2)
            self.assertEqual(passed["exit_code"], 0)
            failed = run.measure(["/bin/false"], root, os.environ, root / "log", 2)
            self.assertEqual(failed["exit_code"], 1)
            timeout = run.measure(["sleep", "10"], root, os.environ, root / "log", .05)
            self.assertTrue(timeout["timeout"])
            interrupted = run.measure(["sleep", "10"], root, os.environ, root / "log", 2, .05)
            self.assertTrue(interrupted["interruption_sent"])
            self.assertFalse(interrupted["timeout"])
            self.assertGreater(passed["wall_seconds"], 0)
        finally:
            shutil.rmtree(root)

    def test_timeout_escalates_for_a_group_ignoring_interrupts(self):
        import os
        from pathlib import Path
        import shutil
        import run
        root = Path("/srv/fl-bench/unit-kill")
        root.mkdir(parents=True, exist_ok=True)
        try:
            result = run.measure(["bash", "-c", "trap '' INT; sleep 30 & wait"], root,
                                 os.environ, root / "log", .25)
            self.assertTrue(result["timeout"])
            self.assertEqual(result["exit_code"], -9)
            self.assertGreaterEqual(result["wall_seconds"], 5)
        finally:
            shutil.rmtree(root)
    def test_access_counts_request_bytes_and_file_only_bytes(self):
        import run
        result = run.parse_access([
            '{"request":"HEAD /a HTTP/2.0","status":200,"bytes":0}',
            '{"request":"GET /a HTTP/2.0","status":206,"bytes":13}',
            '{"request":"GET /other HTTP/1.1","status":200,"bytes":7}',
        ], {"/a"})
        self.assertEqual(result, {"requests": 3, "bytes_served": 20, "file_bytes_served": 13})


if __name__ == "__main__":
    unittest.main()
