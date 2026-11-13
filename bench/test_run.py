import argparse
import contextlib
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import lab
import run
import tools


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
        # gone: the command ended and was reaped before the cleanup reached it
        for gone in (False, True):
            with self.subTest(gone=gone), tempfile.TemporaryDirectory() as folder, \
                 mock.patch.object(run.os, "posix_spawn", return_value=987654), \
                 mock.patch.object(run.os, "wait4", return_value=(987654, 0, USAGE),
                                   side_effect=ChildProcessError if gone else None) as wait, \
                 mock.patch.object(run.os, "killpg", side_effect=ProcessLookupError if gone else None) as kill, \
                 mock.patch.object(run.signal, "signal") as handler, \
                 mock.patch.object(run.signal, "setitimer", side_effect=[ValueError("timer setup failed"), None]), \
                 mock.patch.object(lab, "end_group") as ended:
                root = Path(folder)
                with self.assertRaisesRegex(ValueError, "timer setup failed"):
                    run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log")
            kill.assert_called_with(987654, signal.SIGKILL)
            wait.assert_called_once_with(987654, 0)
            ended.assert_called_once_with(987654)
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
        # gone: each stage finds what it would kill already ended
        for interrupt, gone in ((None, False), (10, False), (None, True)):
            handler = {}
            def install(signum, callback):
                handler[signum] = callback
            def wait(*args):
                alarm = handler[signal.SIGALRM]
                alarm(None, None)
                alarm(None, None)
                alarm(None, None)
                return 987654, 9, USAGE
            with self.subTest(interrupt=interrupt, gone=gone), tempfile.TemporaryDirectory() as folder, \
                 mock.patch.object(run.os, "posix_spawn", return_value=987654), \
                 mock.patch.object(run.os, "wait4", side_effect=wait), \
                 mock.patch.object(run.os, "killpg", side_effect=[None, ProcessLookupError] if gone else None) as kill_group, \
                 mock.patch.object(run.os, "kill", side_effect=ProcessLookupError if gone else None) as kill_child, \
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

    def test_a_command_gone_by_its_deadline_is_not_a_timeout(self):
        handler = {}

        def wait(*args):
            handler[signal.SIGALRM](None, None)
            return 987654, 0, USAGE

        with tempfile.TemporaryDirectory() as folder, \
             mock.patch.object(run.os, "posix_spawn", return_value=987654), \
             mock.patch.object(run.os, "wait4", side_effect=wait), \
             mock.patch.object(run.os, "killpg", side_effect=ProcessLookupError) as kill, \
             mock.patch.object(run.signal, "signal", side_effect=handler.__setitem__), \
             mock.patch.object(run.signal, "setitimer") as timer, \
             mock.patch.object(lab, "end_group") as ended:
            root = Path(folder)
            result = run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log", 20)
        self.assertEqual((result["timeout"], result["interruption_sent"], result["exit_code"]), (False, False, 0))
        kill.assert_called_once_with(987654, signal.SIGINT)
        # nothing escalates, and no group is waited out
        self.assertEqual(timer.call_args_list[1:], [mock.call(signal.ITIMER_REAL, 0)])
        ended.assert_not_called()


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


class WarmTests(unittest.TestCase):
    def test_only_cold_scenarios_drop_the_page_cache(self):
        import tools

        class Stub(tools.Tool):
            name = "stub"

            def prepare(self, scenario, workdir):
                workdir.mkdir(parents=True, exist_ok=True)

            def command(self, scenario, workdir):
                return ["true"]

        measured = dict(wall_seconds=1, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False)
        dropped = {}
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "www").mkdir()
            for scenario in ("s3", "s6", "s9"):
                case = dict(id=scenario, profile="lan", quick=True, count=1, size=1, files=[], members=[],
                            key="key", server_dir=str(root / "www"))
                with mock.patch.object(lab, "ROOT", root), mock.patch.object(lab, "forget_connections"), \
                     mock.patch.object(lab, "drop_caches") as drop, mock.patch.object(lab, "flush"), \
                     mock.patch.object(run, "measure", return_value=measured), \
                     mock.patch.object(run, "settled_free_bytes", return_value=1), \
                     mock.patch.object(run, "free_bytes", return_value=1), mock.patch.object(run.time, "sleep"):
                    run.one_run(Stub("unused"), case, root / "fs" / "ext4" / "work", root, 1, False, 30)
                dropped[scenario] = drop.called
        self.assertEqual(dropped, {"s3": True, "s6": False, "s9": False})


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


class HelperTests(unittest.TestCase):
    def test_a_deadline_is_a_finite_positive_number_of_seconds(self):
        self.assertEqual(run.positive_seconds("2.5"), 2.5)
        for given in ["soon", None, 10 ** 400, float("inf"), float("nan"), 0, -1]:
            with self.subTest(given=given), self.assertRaisesRegex(ValueError, "finite positive number of seconds"):
                run.positive_seconds(given)
        with self.assertRaisesRegex(argparse.ArgumentTypeError, "finite positive number of seconds"):
            run.seconds_choice("0")

    def test_lists_name_each_choice_once(self):
        self.assertEqual(run.csv_choices("lan,far", ("lan", "far", "poor")), ["lan", "far"])
        for given in ["lan,lan", "lan,moon"]:
            with self.subTest(given), self.assertRaisesRegex(argparse.ArgumentTypeError, "choose distinct comma-separated values from lan,far"):
                run.csv_choices(given, ("lan", "far"))
        with self.assertRaisesRegex(argparse.ArgumentTypeError, "a variant needs a lowercase label"):
            run.variant_choice("Lean:FETCHLOOM_EXP_A=1")

    def test_the_access_log_counts_every_request_and_the_bytes_of_measured_files(self):
        lines = ['{"request": "GET /key/a.bin HTTP/1.1", "bytes": 10}', "", '{"request": "-", "bytes": 3}',
                 '{"request": "GET /key/a.bin?x=1 HTTP/2.0", "bytes": 5}', '{"request": "HEAD /other HTTP/1.1", "bytes": 0}']
        self.assertEqual(run.parse_access(lines, {"/key/a.bin"}), dict(requests=4, bytes_served=18, file_bytes_served=15))

    def test_free_space_still_moving_after_every_try_is_its_last_reading(self):
        with mock.patch.object(lab, "flush") as flush, mock.patch.object(run, "free_bytes", side_effect=range(100)), \
             mock.patch.object(run.time, "sleep"):
            self.assertEqual(run.settled_free_bytes("/lab", attempts=3), 3)
        flush.assert_called_once_with("/lab")

    def test_the_repository_head_falls_back_to_windows_git_and_then_to_unavailable(self):
        commands = []

        def output(argv, **_):
            commands.append(argv)
            if argv[0] == "wslpath":
                return "C:\\fetchloom\n"
            raise subprocess.CalledProcessError(128, argv)

        with mock.patch.object(run.shutil, "which", return_value="/usr/bin/wslpath"), \
             mock.patch.object(run.subprocess, "check_output", side_effect=output):
            self.assertEqual(run.repo_head(), "unavailable")
        self.assertEqual([argv[0] for argv in commands], ["wslpath", "git", "git.exe"])
        self.assertEqual(commands[-1], ["git.exe", "-C", "C:\\fetchloom", "rev-parse", "HEAD"])
        commands.clear()
        with mock.patch.object(run.shutil, "which", return_value=None), \
             mock.patch.object(run.subprocess, "check_output", side_effect=output):
            self.assertEqual(run.repo_head(), "unavailable")
        self.assertEqual([argv[0] for argv in commands], ["git"])
        with mock.patch.object(run.shutil, "which", return_value=None), \
             mock.patch.object(run.subprocess, "check_output", return_value="04966698\n"):
            self.assertEqual(run.repo_head(), "04966698")

    def test_the_package_inventory_says_what_is_installed_missing_or_unreadable(self):
        listed = "curl\tinstalled\t8.5.0\nwget\tconfig-files\t1.21\n"
        with mock.patch.object(run.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, listed, "")):
            self.assertEqual(run.native_versions(("curl", "wget", "aria2")),
                             ["curl\t8.5.0", "wget\tnot installed", "aria2\tnot installed"])
        with mock.patch.object(run.subprocess, "run", return_value=subprocess.CompletedProcess([], 2, "", " locked \n")):
            self.assertEqual(run.native_versions(("curl",)), ["native package inventory unavailable: locked"])
        with mock.patch.object(run.subprocess, "run", side_effect=FileNotFoundError(2, "No such file or directory", "dpkg-query")):
            self.assertEqual(run.native_versions(("curl",)),
                             ["native package inventory unavailable: [Errno 2] No such file or directory: 'dpkg-query'"])

    def test_an_executable_that_changes_while_it_is_hashed_is_refused(self):
        with tempfile.TemporaryDirectory() as folder:
            binary = Path(folder, "fetchloom")
            binary.write_bytes(b"build")
            real = Path.stat

            def stat(path, **options):
                # the second look sees the file rewritten in place
                found = real(path, **options)
                return found if not stat.seen else os.stat_result((*found[:6], found[6] + 1, *found[7:]))

            stat.seen = False
            self.assertEqual(run.binary_identity(binary), dict(path=str(binary.resolve()), sha256=run.fixtures.sha256(binary), size=5))

            def changing(path, **options):
                result = stat(path, **options)
                stat.seen = True
                return result

            with mock.patch.object(Path, "stat", changing), self.assertRaisesRegex(RuntimeError, "measured executable changed"):
                run.binary_identity(binary)

    def test_the_recorded_command_keeps_each_file_it_wrote(self):
        with tempfile.TemporaryDirectory() as folder:
            work = Path(folder)
            (work / "command.sh").write_text("set -eu\n")
            (work / "urls.txt").write_text("https://lab.flb/a\n")
            self.assertEqual(run.exact_command(["bash", work / "command.sh"], work),
                             {"argv": ["bash", str(work / "command.sh")], "command.sh": "set -eu\n", "urls.txt": "https://lab.flb/a\n"})

    @unittest.skipUnless(Path("/proc/self/stat").is_file(), "reads process groups from /proc")
    def test_a_process_group_lists_each_of_its_processes(self):
        child = "import subprocess, sys, time\nsubprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])\nprint('ready', flush=True)\ntime.sleep(30)\n"
        # a group of its own inside this session, so its group and its session differ
        process = subprocess.Popen([sys.executable, "-c", child], process_group=0, stdout=subprocess.PIPE, text=True)
        try:
            self.assertEqual(process.stdout.readline(), "ready\n")
            members = run.group(process.pid)
            self.assertIn(process.pid, members)
            self.assertEqual(len(members), 2)
            self.assertNotIn(os.getpid(), members)
        finally:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            process.stdout.close()

    def test_free_space_is_what_a_user_without_root_may_still_write(self):
        # 3 available fragments of 4 KiB; the 50 free blocks include those kept for root
        shown = os.statvfs_result((8192, 4096, 100, 50, 3, 10, 5, 5, 0, 255))
        with mock.patch.object(run.os, "statvfs", return_value=shown) as statvfs:
            self.assertEqual(run.free_bytes("/lab"), 12288)
        statvfs.assert_called_once_with("/lab")

    def test_a_process_group_is_read_from_the_fields_after_each_command_name(self):
        with tempfile.TemporaryDirectory() as folder:
            proc = Path(folder)
            # a name may hold ") ", and the field before the group is the parent, here 77 too
            for entry, line in [("12", "12 (a) b) S 1 77 77 0"), ("13", "13 (sh) S 77 78 78 0"), ("14", "14 (cut"),
                                ("self", "15 (fl) S 1 77 77 0")]:
                (proc / entry).mkdir()
                (proc / entry / "stat").write_text(line)
            # a process that ended between the listing and the read
            (proc / "16").mkdir()
            with mock.patch.object(run, "Path", lambda path: proc if path == "/proc" else Path(path)):
                self.assertEqual(run.group(77), [12])

    def test_a_run_without_gnu_time_is_refused_before_it_starts(self):
        with mock.patch.object(run, "TIME", "/no/such/time"), mock.patch.object(run.os, "posix_spawn") as spawn, \
             self.assertRaisesRegex(RuntimeError, "/no/such/time is missing; install GNU time"):
            run.measure(["true"], ".", {}, "/no/such/log", 30)
        spawn.assert_not_called()

    @unittest.skipUnless(os.access(run.TIME, os.X_OK), "GNU time")
    def test_a_command_killed_by_a_signal_reports_that_signal(self):
        with tempfile.TemporaryDirectory() as folder:
            log = Path(folder, "run.log")
            result = run.measure(["sh", "-c", "kill -9 $$"], folder, dict(os.environ), log, 30)
        self.assertEqual((result["exit_code"], result["timeout"]), (-9, False))


class OneRunTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        (self.root / "www").mkdir()
        self.work = self.root / "fs" / "ext4" / "work"
        self.case = dict(id="s1", profile="lan", quick=True, count=1, size=4, key="key", server_dir=str(self.root / "www"),
                         members=[], files=[dict(name="a.bin", size=4, url="https://lab.flb/key/a.bin", sha256="0" * 64)])

    def one_run(self, tool, measured, case=None):
        with mock.patch.object(lab, "ROOT", self.root), mock.patch.object(lab, "forget_connections"), \
             mock.patch.object(lab, "drop_caches"), mock.patch.object(lab, "flush"), \
             mock.patch.object(run, "measure", side_effect=measured), \
             mock.patch.object(run, "settled_free_bytes", return_value=10), \
             mock.patch.object(run, "free_bytes", return_value=4), mock.patch.object(run.time, "sleep"):
            return run.one_run(tool, case or self.case, self.work, self.root, 1, False, 30)

    def test_a_resume_adds_both_attempts_and_needs_the_first_to_be_stopped(self):
        class Downloader(tools.Tool):
            name = "downloader"

            def command(self, scenario, workdir):
                return ["true"]

            def outputs(self, scenario, workdir):
                return []

        attempts = iter([dict(wall_seconds=10, user_seconds=1, sys_seconds=2, maxrss_bytes=7, exit_code=130, timeout=False,
                              interruption_sent=False),
                         dict(wall_seconds=3, user_seconds=4, sys_seconds=8, maxrss_bytes=5, exit_code=0, timeout=False,
                              interruption_sent=False)])

        def measured(argv, cwd, env, log, timeout, interrupt_after=None, cpus=None):
            # a listing of the folder costs bytes too, but is no part of the file
            access = self.root / "access.jsonl"
            access.write_text(access.read_text() + '{"request": "GET /key/a.bin HTTP/1.1", "bytes": 3}\n'
                                                   '{"request": "GET /key/ HTTP/1.1", "bytes": 100}\n')
            return next(attempts)

        row = self.one_run(Downloader("unused"), measured, dict(self.case, id="s7", profile="home"))
        self.assertEqual((row["wall_seconds"], row["user_seconds"], row["sys_seconds"], row["maxrss_bytes"]), (3, 5, 10, 7))
        self.assertEqual(row["first_attempt"]["exit_code"], 130)
        self.assertEqual(row["resume_command"]["argv"], ["true"])
        self.assertEqual(row["download_ratio"], 6 / 4)
        self.assertEqual(row["checks"], ["no expected outputs", "first attempt ended before the prescribed interruption"])
        self.assertEqual(row["status"], "failed")
        attempts = iter([dict(wall_seconds=10, user_seconds=1, sys_seconds=2, maxrss_bytes=7, exit_code=130, timeout=False,
                              interruption_sent=True),
                         dict(wall_seconds=3, user_seconds=4, sys_seconds=8, maxrss_bytes=5, exit_code=0, timeout=False,
                              interruption_sent=False)])
        row = self.one_run(Downloader("unused"), measured, dict(self.case, id="s7", profile="home"))
        self.assertEqual(row["checks"], ["no expected outputs"])

    def test_dvc_bytes_count_by_the_paths_it_stores_files_under(self):
        class Dvc(tools.Tool):
            name = "dvc"

            def command(self, scenario, workdir):
                return ["true"]

            def outputs(self, scenario, workdir):
                return []

        store = self.root / "www/dvc/key/files/md5/90"
        store.mkdir(parents=True)
        (store / "0150983cd24fb0d6963f7d28e17f72").write_bytes(b"abc")
        (store / "abc.dir").write_bytes(b"[]")

        def measured(*args, **kwargs):
            (self.root / "access.jsonl").write_text(
                '{"request": "GET /dvc/key/files/md5/90/0150983cd24fb0d6963f7d28e17f72 HTTP/1.1", "bytes": 3}\n'
                '{"request": "GET /dvc/key/files/md5/90/abc.dir HTTP/1.1", "bytes": 2}\n')
            return dict(wall_seconds=1, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False)

        row = self.one_run(Dvc("unused"), measured)
        self.assertEqual((row["requests"], row["bytes_served"], row["file_bytes_served"]), (2, 5, 3))

    def test_a_run_that_breaks_is_a_failed_row_with_its_reason_and_preparation_log(self):
        class Broken(tools.Tool):
            name = "broken"

            def prepare(self, scenario, workdir):
                (workdir / "prepare.log").write_text("cloning\n")
                raise RuntimeError("the clone failed")

        row = self.one_run(Broken("unused"), lambda *args, **kwargs: None)
        self.assertEqual((row["status"], row["error"]), ("failed", "RuntimeError: the clone failed"))
        self.assertEqual((self.root / "ext4-lan-s1-1-broken-prepare.log").read_text(), "cloning\n")
        self.assertEqual(json.loads((self.root / "runs.jsonl").read_text()), row)


class MainPreparationTests(unittest.TestCase):
    def test_a_client_that_cannot_be_prepared_is_a_failed_row_in_every_round(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            case = dict(id="s2", base="s2", profile="lan", quick=True, count=1, size=3, key="sample", members=[], files=[])
            argv = ["run.py", "--fetchloom", "/not-executed", "--only-tools", "fetchloom", "--fs", "ext4", "--profiles", "lan",
                    "--scenarios", "s2", "--quick", "--out", str(root / "results")]
            with mock.patch.object(sys, "argv", argv), mock.patch.object(lab, "ROOT", root), \
                 mock.patch.object(lab, "require_root"), mock.patch.object(lab, "profile"), \
                 mock.patch.object(lab, "pin_server", return_value={}), \
                 mock.patch.object(run, "metadata", return_value=dict(commands={}, orders=[], cases=[])), \
                 mock.patch.object(run.os.path, "ismount", return_value=True), \
                 mock.patch.object(run.fixtures, "build", return_value=case), \
                 mock.patch.object(run.fixtures, "prepare_native", side_effect=RuntimeError("lock failed")), \
                 mock.patch.object(run, "one_run", side_effect=AssertionError("a run was measured")), \
                 contextlib.redirect_stdout(io.StringIO()) as shown:
                self.assertEqual(run.main(), 1)
            rows = [json.loads(line) for line in (root / "results/runs.jsonl").read_text().splitlines()]
        self.assertEqual([(row["round"], row["warmup"], row["status"], row["error"]) for row in rows],
                         [(0, True, "failed", "native preparation failed: lock failed"),
                          (1, False, "failed", "native preparation failed: lock failed")])
        self.assertIn("recorded 2 failed runs (including warmups)", shown.getvalue())


class MainRefusalTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name).resolve()
        (self.root / "owned.json").write_text("{}")

    def refused(self, *arguments, mounted=True):
        argv = ["run.py", "--fetchloom", "/not-executed", "--only-tools", "fetchloom", "--fs", "ext4",
                "--profiles", "lan", "--scenarios", "s2", "--out", str(self.root / "results"), *arguments]
        said = io.StringIO()
        with mock.patch.object(sys, "argv", argv), mock.patch.object(lab, "ROOT", self.root), \
             mock.patch.object(lab, "require_root"), mock.patch.object(run.os.path, "ismount", return_value=mounted), \
             mock.patch.object(lab, "exclusive"), mock.patch.object(lab, "pin_server", side_effect=AssertionError("the lab was used")), \
             contextlib.redirect_stderr(said), self.assertRaises(SystemExit) as stopped:
            run.main()
        self.assertEqual(stopped.exception.code, 2)
        return said.getvalue().splitlines()[-1]

    def test_a_campaign_that_cannot_be_measured_as_asked_never_starts(self):
        self.assertEqual(self.refused("--rounds", "0"), "run.py: error: rounds and timeout must be positive")
        self.assertEqual(self.refused("--out", "/elsewhere"), f"run.py: error: write results under {self.root}; copy them out after measurement")
        self.assertEqual(self.refused("--variant", "a:FETCHLOOM_EXP_A=1", "--variant", "a:FETCHLOOM_EXP_B=1"),
                         "run.py: error: each variant needs its own label")
        self.assertEqual(self.refused("--scenarios", "s3", "--profiles", "home"),
                         "run.py: error: no cases match these scenario/profile combinations")
        self.assertEqual(self.refused(mounted=False), "run.py: error: ext4 is not mounted")
        (self.root / "results").mkdir(exist_ok=True)
        (self.root / "results/runs.jsonl").write_text("")
        self.assertEqual(self.refused(), "run.py: error: output already contains runs; use a new directory")
        (self.root / "owned.json").unlink()
        self.assertEqual(self.refused(), "run.py: error: run lab.py setup first")


if __name__ == "__main__":
    unittest.main()
