import contextlib
import io
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import lab
import run


USAGE = SimpleNamespace(ru_utime=1, ru_stime=2)


class TimerTests(unittest.TestCase):
    def test_invalid_deadlines_never_spawn(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            for field in ("timeout", "interrupt_after"):
                for invalid in (0, -1, float("nan"), float("inf")):
                    with self.subTest(field=field, value=invalid), \
                         mock.patch.object(run.os, "posix_spawn", return_value=987654) as spawn, \
                         mock.patch.object(run.os, "wait4", return_value=(987654, 0, USAGE)), \
                         mock.patch.object(run.signal, "setitimer"), \
                         self.assertRaises(ValueError):
                        try:
                            run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log", **{field: invalid})
                        finally:
                            spawn.assert_not_called()

    def test_cli_deadlines_are_checked_before_lab_access(self):
        import live
        for module in (run, live):
            for value in ("0", "-1", "nan", "inf"):
                with self.subTest(module=module.__name__, value=value), \
                     mock.patch.object(sys, "argv", [module.__name__, "--fetchloom", "/not-executed", "--timeout", value]), \
                     mock.patch.object(lab, "require_root", side_effect=AssertionError("lab was accessed")), \
                     contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as caught:
                    module.main()
                self.assertEqual(caught.exception.code, 2)

    def test_timer_setup_failure_kills_reaps_and_restores_handler(self):
        previous = signal.getsignal(signal.SIGALRM)
        with tempfile.TemporaryDirectory() as folder, \
             mock.patch.object(run.os, "posix_spawn", return_value=987654), \
             mock.patch.object(run.os, "wait4", return_value=(987654, 0, USAGE)) as wait, \
             mock.patch.object(run.os, "killpg") as kill, \
             mock.patch.object(run.signal, "signal") as handler, \
             mock.patch.object(run.signal, "setitimer", side_effect=[ValueError("timer setup failed"), None]), \
             mock.patch.object(lab, "end_group"):
            root = Path(folder)
            with self.assertRaisesRegex(ValueError, "timer setup failed"):
                run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log")
        kill.assert_called_with(987654, signal.SIGKILL)
        wait.assert_called_once_with(987654, 0)
        self.assertEqual(handler.call_args, mock.call(signal.SIGALRM, previous))

    def test_spawn_failure_restores_cwd_and_leaves_no_usage_file(self):
        before = os.getcwd()
        with tempfile.TemporaryDirectory() as folder, \
             mock.patch.object(run.os, "posix_spawn", side_effect=OSError("spawn failed")):
            root = Path(folder)
            with self.assertRaisesRegex(OSError, "spawn failed"):
                run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log")
            self.assertFalse((root / "log.usage").exists())
        self.assertEqual(os.getcwd(), before)

    def test_alarm_escalation_uses_fixed_events(self):
        for interrupt in (None, 10):
            handler = {}
            def install(signum, callback):
                handler[signum] = callback
            def wait(*args):
                alarm = handler[signal.SIGALRM]
                alarm(None, None)
                alarm(None, None)
                alarm(None, None)
                return 987654, 9, USAGE
            with self.subTest(interrupt=interrupt), tempfile.TemporaryDirectory() as folder, \
                 mock.patch.object(run.os, "posix_spawn", return_value=987654), \
                 mock.patch.object(run.os, "wait4", side_effect=wait), \
                 mock.patch.object(run.os, "killpg") as kill_group, \
                 mock.patch.object(run.os, "kill") as kill_child, \
                 mock.patch.object(run, "group", return_value=[987654, 987655]), \
                 mock.patch.object(run.signal, "signal", side_effect=install), \
                 mock.patch.object(run.signal, "setitimer") as timer, \
                 mock.patch.object(run.time, "monotonic", side_effect=[100, 102, 110, 116, 117]), \
                 mock.patch.object(lab, "end_group"):
                root = Path(folder)
                result = run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log", 20, interrupt)
            self.assertEqual(result["timeout"], interrupt is None)
            self.assertEqual(result["interruption_sent"], interrupt is not None)
            self.assertEqual(result["exit_code"], -9)
            self.assertIn(mock.call(987654, signal.SIGINT), kill_group.call_args_list)
            self.assertIn(mock.call(987654, signal.SIGKILL), kill_group.call_args_list)
            kill_child.assert_called_once_with(987655, signal.SIGKILL)
            self.assertEqual(timer.call_args_list[0], mock.call(signal.ITIMER_REAL, 18 if interrupt is None else 8))
            self.assertIn(mock.call(signal.ITIMER_REAL, 5), timer.call_args_list)
            self.assertIn(mock.call(signal.ITIMER_REAL, 1), timer.call_args_list)


class ProvenanceTests(unittest.TestCase):
    def test_metadata_identifies_bytes_at_a_reused_executable_path(self):
        import hashlib
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            binary = root / "client"
            args = SimpleNamespace(fs=[], seed=1729, quick=True, profiles=["lan"], fetchloom=binary)
            adapter = SimpleNamespace(version=lambda: "same version")
            records = []
            with mock.patch.object(lab, "ROOT", root), \
                 mock.patch.object(run, "native_versions", return_value=[]), \
                 mock.patch.object(run, "python_packages", return_value=[]), \
                 mock.patch.object(run, "repo_head", return_value="harness-head"), \
                 mock.patch.object(run.subprocess, "check_output", return_value="ext4 example rw"):
                for data in (b"first build", b"other build"):
                    binary.write_bytes(data)
                    meta = run.metadata(args, {"fetchloom": adapter})
                    self.assertIn("fetchloom_binary", meta)
                    identity = meta["fetchloom_binary"]
                    self.assertEqual(identity["path"], str(binary.resolve()))
                    self.assertEqual(identity["sha256"], hashlib.sha256(data).hexdigest())
                    self.assertEqual(identity["size"], len(data))
                    self.assertEqual(meta["repo_head"], "harness-head")
                    records.append(identity)
            self.assertNotEqual(records[0]["sha256"], records[1]["sha256"])


class MetadataWriteTests(unittest.TestCase):
    def test_failed_serialization_preserves_previous_metadata(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            path = root / "meta.json"
            run.write_metadata(path, dict(version=1))
            previous = path.read_bytes()
            with self.assertRaises(TypeError):
                run.write_metadata(path, dict(unserializable=object()))
            self.assertEqual(path.read_bytes(), previous)
            self.assertEqual(list(root.iterdir()), [path])


class ManyVariantTests(unittest.TestCase):
    def test_twelve_variants_keep_order_and_record_distinct_environments(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            prepared = root / "prepared" / "fetchloom"
            prepared.mkdir(parents=True)
            for name in ("data.toml", "data.lock"):
                (prepared / name).write_text(name)
            (root / "www").mkdir()
            case = dict(id="s2", base="s2", profile="lan", quick=False, count=1, size=3, key="sample",
                        server_dir=str(root / "www"), prepared=str(prepared.parent), members=[],
                        files=[dict(name="payload.bin", size=3, url="https://lab.flb/payload.bin",
                                    sha256="ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")])
            def measured(argv, cwd, env, logfile, *args, **kwargs):
                dest = Path(cwd) / "data" / "f00000"
                dest.mkdir(parents=True)
                (dest / "payload.bin").write_bytes(b"abc")
                return dict(wall_seconds=1, user_seconds=1, sys_seconds=1, maxrss_bytes=1,
                            exit_code=0, timeout=False, interruption_sent=False)
            def metadata(*args):
                return dict(commands={}, orders=[], cases=[])
            snapshots = []
            for iteration in range(2):
                out = root / f"results-{iteration}"
                argv = ["run.py", "--fetchloom", "/not-executed", "--only-tools", "fetchloom", "--fs", "ext4",
                        "--profiles", "lan", "--scenarios", "s2", "--rounds", "2", "--seed", "13", "--out", str(out)]
                for n in range(12):
                    argv += ["--variant", f"v{n}:FETCHLOOM_EXP_V{n}={n}"]
                with mock.patch.object(sys, "argv", argv), mock.patch.object(lab, "ROOT", root), \
                     mock.patch.object(lab, "require_root"), mock.patch.object(lab, "profile"), \
                     mock.patch.object(lab, "pin_server", create=True, return_value={}), \
                     mock.patch.object(lab, "drop_caches"), mock.patch.object(lab, "forget_connections"), \
                     mock.patch.object(run, "metadata", side_effect=metadata), \
                     mock.patch.object(run.os.path, "ismount", return_value=True), \
                     mock.patch.object(run.fixtures, "case_specs", return_value=[case]), \
                     mock.patch.object(run.fixtures, "build", return_value=case), \
                     mock.patch.object(run.fixtures, "prepare_native", return_value=case), \
                     mock.patch.object(run, "measure", side_effect=measured), \
                     mock.patch.object(run, "settled_free_bytes", return_value=100), \
                     mock.patch.object(run, "free_bytes", return_value=97), \
                     mock.patch.object(run.lab, "flush"), mock.patch.object(run.time, "sleep"), \
                     contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(run.main(), 0)
                meta = json.loads((out / "meta.json").read_text())
                rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
                self.assertEqual(len(rows), 39)
                self.assertEqual(sum(r["warmup"] for r in rows), 13)
                for row in rows:
                    settings = {key: value for key, value in row["environment"].items() if key.startswith("FETCHLOOM_EXP_")}
                    expected = {} if row["tool"] == "fetchloom" else {"FETCHLOOM_EXP_V" + row["tool"].split("+v")[1]: row["tool"].split("+v")[1]}
                    self.assertEqual(settings, expected)
                for order in meta["orders"]:
                    self.assertEqual(len(order["tools"]), 13)
                    self.assertEqual(len(set(order["tools"])), 13)
                snapshots.append(meta["orders"])
            self.assertEqual(*snapshots)


if __name__ == "__main__":
    unittest.main()
