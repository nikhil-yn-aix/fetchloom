import tempfile
import os
import unittest


class LiveTests(unittest.TestCase):
    def test_live_checker_excludes_only_native_checksum_reports(self):
        from pathlib import Path
        import shutil
        import live
        root = Path(tempfile.mkdtemp(prefix="fl-bench-"))
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
            binary = os.environ.get("FL_BENCH_FETCHLOOM", "/home/fl/bin/fetchloom")
            env = tools.Tool(binary).env(work)
            env["KAGGLEHUB_CACHE"] = str(work / "cache" / "kaggle")
            result = run.measure(live.command(dataset["tool"], dataset, work, Path(binary)),
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

class RecordTests(unittest.TestCase):
    def dataset(self):
        return dict(service="zenodo", tool="zenodo_get", ref="zenodo:1", native="1", select=[],
                    quick=True, files=[dict(name="input.bin", size=3)], bytes=3,
                    expected_sha256={"input.bin": "digest"})

    def test_metadata_precedes_the_first_measured_command(self):
        import contextlib
        import io
        import json
        from pathlib import Path
        import sys
        from unittest import mock
        import live
        import lab
        import run
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            out = root / "results"
            calls = []
            def measured(argv, work, env, logfile, *args, **kwargs):
                meta_path = out / "meta.json"
                self.assertTrue(meta_path.is_file(), "live metadata must exist before measurement")
                meta = json.loads(meta_path.read_text())
                self.assertEqual(meta["datasets"][0]["expected_sha256"], {"input.bin": "digest"})
                self.assertTrue(meta["orders"])
                self.assertTrue(meta["commands"])
                calls.append(argv)
                if len(calls) == 2:
                    raise KeyboardInterrupt
                return dict(wall_seconds=1, exit_code=0, timeout=False)
            metadata = dict(commands={}, orders=[], cases=[], tool_versions={})
            with mock.patch.object(sys, "argv", ["live.py", "--fetchloom", "/unused", "--quick", "--services", "zenodo", "--out", str(out)]), \
                 mock.patch.object(lab, "ROOT", root), mock.patch.object(lab, "require_root"), \
                 mock.patch.object(lab, "pin_server", return_value={}), \
                 mock.patch.object(lab, "drop_caches"), mock.patch.object(run, "metadata", return_value=metadata), \
                 mock.patch.object(live, "versions", return_value={}), \
                 mock.patch.object(live, "catalog", return_value=self.dataset()), \
                 mock.patch.object(live, "reference", return_value={"input.bin": "digest"}), \
                 mock.patch.object(run, "measure", side_effect=measured), \
                 mock.patch.object(run, "free_bytes", return_value=100), mock.patch.object(live.os, "sync"), \
                 mock.patch.object(live, "fingerprint", return_value={"input.bin": "digest"}), \
                 contextlib.redirect_stdout(io.StringIO()), self.assertRaises(KeyboardInterrupt):
                live.main()
            rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["status"], "passed")
            self.assertEqual(len(json.loads((out / "meta.json").read_text())["commands"]), 2)

    def test_checker_exception_is_a_failed_record(self):
        import json
        from pathlib import Path
        from unittest import mock
        import live
        import lab
        import run
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            work = root / "work"
            meta = dict(commands={})
            with mock.patch.object(lab, "drop_caches"), \
                 mock.patch.object(run, "measure", return_value=dict(wall_seconds=1, exit_code=0, timeout=False)), \
                 mock.patch.object(run, "free_bytes", return_value=100), mock.patch.object(live.os, "sync"), \
                 mock.patch.object(live, "fingerprint", side_effect=OSError("checker read failed")):
                row = live.one_run("fetchloom", self.dataset(), work, root, 1, Path("/unused"), meta, 30)
            self.assertEqual(row["status"], "failed")
            self.assertIn("checker read failed", row["error"])
            self.assertEqual(json.loads((root / "runs.jsonl").read_text())["status"], "failed")


if __name__ == "__main__":
    unittest.main()
