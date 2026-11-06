import os
import tempfile
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
    @unittest.skipUnless(os.environ.get("FL_BENCH_LAB_CHECKS") == "1", "needs a set-up lab")
    def test_a_profile_without_a_rate_drops_the_last_cap(self):
        import subprocess
        def server_shaping():
            return subprocess.check_output(["ip", "netns", "exec", lab.NS, "tc", "qdisc", "show", "dev",
                                            lab.SERVER, "root"], text=True).split()
        for name in ("poor", "lan", "home", "far", "lan"):
            lab.profile(name)
            rate = lab.PROFILES[name][2]
            shown = server_shaping()
            self.assertEqual("rate" in shown, bool(rate), (name, shown))
            if rate:
                self.assertIn(f"{rate}Mbit", shown, (name, shown))

    def test_environment_inherits_no_keys_or_settings(self):
        from unittest import mock
        inherited = dict(PATH="/usr/bin", LANG="C.UTF-8", HF_TOKEN="hf_x", HUGGING_FACE_HUB_TOKEN="hf_y",
                         ZENODO_TOKEN="z", KAGGLE_KEY="k", KAGGLE_CONFIG_DIR="/k", NETRC="/n",
                         DATAVERSE_API_TOKEN="d", AWS_SECRET_ACCESS_KEY="a", FETCHLOOM_JOBS="1",
                         XDG_CONFIG_HOME="/c", DBUS_SESSION_BUS_ADDRESS="unix:path=/b")
        with mock.patch.dict(lab.os.environ, inherited, clear=True):
            env = lab.environment()
        self.assertEqual({key for key in inherited if key in env}, {"PATH", "LANG"})

    def test_exclusive_lock_rejects_overlapping_runs(self):
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
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
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
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
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
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

    def test_peak_memory_excludes_the_harness(self):
        import run
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        ballast = bytearray(os.urandom(200 * 1024 * 1024))
        try:
            small = run.measure(["/bin/true"], root, os.environ, root / "log", 5)
            self.assertLess(small["maxrss_bytes"], 32 * 1024 * 1024)
            large = run.measure(["python3", "-c", "b = bytearray(100 * 1024 * 1024); b[::4096] = b'x' * len(b[::4096])"],
                                root, os.environ, root / "log", 30)
            self.assertGreater(large["maxrss_bytes"], 100 * 1024 * 1024)
            self.assertLess(large["maxrss_bytes"], 160 * 1024 * 1024)
            self.assertEqual(run.measure(["bash", "-c", "kill -INT $$"], root, os.environ, root / "log", 5)["exit_code"], -2)
            self.assertEqual(run.measure(["bash", "-c", "exit 3"], root, os.environ, root / "log", 5)["exit_code"], 3)
            self.assertEqual(run.measure(["no-such-benchmark-tool"], root, os.environ, root / "log", 5)["exit_code"], 127)
            self.assertEqual(sorted(path.name for path in root.iterdir()), ["log"])
        finally:
            del ballast
            shutil.rmtree(root)

    def test_timeout_escalates_for_a_group_ignoring_interrupts(self):
        import os
        from pathlib import Path
        import shutil
        import run
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            result = run.measure(["bash", "-c", "trap '' INT; sleep 30 & wait"], root,
                                 os.environ, root / "log", .25)
            self.assertTrue(result["timeout"])
            self.assertEqual(result["exit_code"], -9)
            self.assertIsNotNone(result["maxrss_bytes"])
            self.assertGreaterEqual(result["wall_seconds"], 5)
        finally:
            shutil.rmtree(root)

    def test_preparation_timeout_kills_the_whole_group(self):
        import subprocess
        from pathlib import Path
        from unittest import mock
        import shutil
        import tools
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        child = root / "child"
        try:
            class Slow(tools.Tool):
                def command(self, scenario, workdir):
                    return tools.script(workdir / "command.sh", [tools.shell(["bash", "-c", 'echo $$ > "$0"; exec sleep 60', child])])
            with mock.patch.object(lab, "PREPARE_TIMEOUT", 1):
                with self.assertRaises(subprocess.TimeoutExpired):
                    Slow("fetchloom").untimed({}, root)
            with self.assertRaises(ProcessLookupError):
                os.kill(int(child.read_text()), 0)
        finally:
            shutil.rmtree(root)

    def test_preparation_passes_input_and_reports_failure(self):
        import subprocess
        from pathlib import Path
        import shutil
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
        try:
            with (root / "out").open("w") as out:
                lab.run_group(["cat"], input="batch\n", text=True, stdout=out)
            self.assertEqual((root / "out").read_text(), "batch\n")
            with self.assertRaises(subprocess.CalledProcessError):
                lab.run_group(["false"])
        finally:
            shutil.rmtree(root)

    def test_each_case_draws_its_own_reproducible_orders(self):
        import run
        tools = ["a", "b", "c", "d", "e", "f", "g"]
        def orders(fs, profile, scenario):
            rng = run.case_rng(1729, fs, dict(profile=profile, id=scenario))
            drawn = []
            for _ in range(6):
                order = list(tools)
                rng.shuffle(order)
                drawn.append(order)
            return drawn
        self.assertEqual(orders("ext4", "lan", "s1"), orders("ext4", "lan", "s1"))
        self.assertNotEqual(orders("ext4", "lan", "s1"), orders("ext4", "lan", "s3"))
        self.assertNotEqual(orders("ext4", "lan", "s1"), orders("ext4", "home", "s1"))

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
