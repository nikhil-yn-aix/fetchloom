import tempfile
import os
import unittest


class LiveTests(unittest.TestCase):
    def test_live_checker_excludes_only_native_checksum_reports(self):
        from pathlib import Path
        import shutil
        import live
        root = Path(tempfile.mkdtemp(prefix="fl-bench-")) / "unit-live-files"
        (root / "generated").mkdir(parents=True)
        try:
            (root / "value").write_bytes(b"abc")
            (root / "generated" / "checksums123.json").write_text("{}")
            actual = live.fingerprint(root, client="datahugger")
            self.assertEqual(actual, {"value": "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"})
            self.assertEqual(len(live.fingerprint(root)), 2)
        finally:
            shutil.rmtree(root)
    def test_live_comparison_reports_layout_and_content_separately(self):
        import live
        self.assertEqual(live.compare({"a": "1"}, {"x/a": "1"})["status"], "layout-difference")
        self.assertEqual(live.compare({"a": "1"}, {"a": "2"})["status"], "content-difference")
        self.assertEqual(live.compare({"a": "1"}, {"a": "1"})["status"], "identical")
        self.assertEqual(live.compare({}, {})["status"], "empty")
        self.assertEqual(live.compare({"a": "1", "b": "1"}, {"a": "1"})["status"], "content-difference")

@unittest.skipUnless(os.environ.get("FL_BENCH_LIVE_CHECKS") == "1", "requires public-service access")
class NativeIntegrityTests(unittest.TestCase):
    def check_client(self, service, message):
        import live
        import tools
        import lab
        import run
        from pathlib import Path
        import shutil
        dataset = live.catalog(service, True)
        dataset["expected_sha256"] = {item["name"]: "0" * 64 for item in dataset["files"]}
        work = lab.ROOT / "fs" / "ext4" / "negative-check"
        tools.reset(work)
        try:
            for name in ("home", "cache", "scratch"):
                (work / name).mkdir()
            env = tools.Tool("/home/fl/bin/fetchloom").env(work)
            env["KAGGLEHUB_CACHE"] = str(work / "cache" / "kaggle")
            result = run.measure(live.command(dataset["tool"], dataset, work, Path("/home/fl/bin/fetchloom")),
                                 work, env, work / "log", 120)
            self.assertFalse(result["timeout"])
            self.assertEqual(result["exit_code"], 1)
            self.assertIn(message, (work / "log").read_text())
        finally:
            shutil.rmtree(work)

    def test_hf_rejects_a_wrong_expected_hash(self):
        self.check_client("huggingface", "computed checksum did NOT match")

    def test_kaggle_rejects_a_wrong_expected_hash(self):
        self.check_client("kaggle", "wrong SHA-256")

if __name__ == "__main__":
    unittest.main()
