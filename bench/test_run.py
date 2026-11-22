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

    def test_what_runs_beside_a_command_begins_as_it_starts_and_ends_before_its_result(self):
        class Beside:
            def __init__(self):
                self.steps = []

            def begin(self, start, anchor=None):
                self.steps.append(("begin", isinstance(start, float)))

            def end(self):
                self.steps.append(("end",))

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            beside = Beside()
            with mock.patch.object(run.os, "posix_spawn", return_value=987654), \
                 mock.patch.object(run.os, "wait4", return_value=(987654, 0, USAGE)), \
                 mock.patch.object(run.signal, "setitimer"):
                run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log", during=beside)
            self.assertEqual(beside.steps, [("begin", True), ("end",)])
            # and ends even when the wait for the command fails
            beside = Beside()
            with mock.patch.object(run.os, "posix_spawn", return_value=987654), \
                 mock.patch.object(run.os, "wait4", side_effect=[OSError("wait failed"), (987654, 0, USAGE)]), \
                 mock.patch.object(run.os, "killpg"), mock.patch.object(run.signal, "setitimer"), \
                 mock.patch.object(lab, "end_group"), self.assertRaisesRegex(OSError, "wait failed"):
                run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log", during=beside)
            self.assertEqual(beside.steps, [("begin", True), ("end",)])

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

    def test_an_interruption_counts_from_the_commands_own_anchor(self):
        for marks, times, said in [
            # looked for once before the calls began, then 10 s after they did, 3 s into the run
            ([None, 103.0], [100, 100.002, 100.01, 103.0, 113, 114, 114.5], dict(interruption_sent=True, timeout=False, anchor_seconds=3.0)),
            # never began before the deadline, which is then a timeout
            ([None], [100, 100.002, 120.5, 120.5, 121, 121.5], dict(interruption_sent=False, timeout=True, anchor_seconds=None)),
        ]:
            handler, looked = {}, iter(marks + [marks[-1]] * 5)

            def wait(*args):
                for _ in range(len(marks) + 1):
                    handler[signal.SIGALRM](None, None)
                return 987654, 2, USAGE

            with self.subTest(marks=marks), tempfile.TemporaryDirectory() as folder, \
                 mock.patch.object(run.os, "posix_spawn", return_value=987654), \
                 mock.patch.object(run.os, "wait4", side_effect=wait), \
                 mock.patch.object(run.os, "killpg") as kill, \
                 mock.patch.object(run.signal, "signal", side_effect=handler.__setitem__), \
                 mock.patch.object(run.signal, "setitimer") as timer, \
                 mock.patch.object(run.time, "monotonic", side_effect=times), mock.patch.object(lab, "end_group"):
                root = Path(folder)
                result = run.measure(["true"], root, {"PATH": "/usr/bin:/bin"}, root / "log", 20, 10, anchor=lambda: next(looked))
            self.assertEqual({key: result[key] for key in said}, said)
            kill.assert_called_once_with(987654, signal.SIGINT)
            delays = [call.args[1] for call in timer.call_args_list]
            if said["anchor_seconds"]:
                self.assertEqual([round(delay, 6) for delay in delays[:4]], [.008, .01, 10.0, 5])
            else:
                self.assertEqual([round(delay, 6) for delay in delays[:2]], [.008, 5])

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
            case = dict(id="s3", base="s3", profile="lan", quick=False, count=1, size=3, key="sample",
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
                        "--profiles", "lan", "--scenarios", "s3", "--rounds", "2", "--seed", "13", "--out", str(out)]
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
                # every run's log has a name of its own, of one length
                self.assertEqual([row["log"] for row in rows], [f"{n:06}.log" for n in range(1, 40)])
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


class CampaignTests(unittest.TestCase):
    """whole campaigns through run.main, with the lab and every client stood in for."""

    def campaign(self, cases, *arguments):
        """the meta.json and rows of a fetchloom campaign over these cases, every run passing."""
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        (root / "owned.json").write_text("{}")
        (root / "www").mkdir()
        prepared = root / "prepared" / "fetchloom"
        prepared.mkdir(parents=True)
        for name in ("data.toml", "data.lock"):
            (prepared / name).write_text(name)
        payload = dict(name="payload.bin", size=3, url="https://lab.flb/payload.bin",
                       sha256="ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        cases = [dict(dict(base=case["id"], quick=False, count=1, size=3, key=f"{case['profile']}-{case['id']}",
                           server_dir=str(root / "www"), prepared=str(prepared.parent), members=[], files=[payload]), **case)
                 for case in cases]

        def measured(argv, cwd, env, logfile, *args, **kwargs):
            (Path(cwd) / "data" / "f00000").mkdir(parents=True)
            (Path(cwd) / "data" / "f00000" / "payload.bin").write_bytes(b"abc")
            return dict(wall_seconds=1, user_seconds=1, sys_seconds=1, maxrss_bytes=1, exit_code=0, timeout=False)

        argv = ["run.py", "--fetchloom", "/not-executed", "--only-tools", "fetchloom", "--fs", "ext4",
                "--out", str(root / "results"), *arguments]
        with mock.patch.object(sys, "argv", argv), mock.patch.object(lab, "ROOT", root), \
             mock.patch.object(lab, "require_root"), mock.patch.object(lab, "profile"), \
             mock.patch.object(lab, "pin_server", return_value={}), \
             mock.patch.object(lab, "drop_caches"), mock.patch.object(lab, "forget_connections"), \
             mock.patch.object(run, "metadata", side_effect=lambda *args: dict(commands={}, orders=[], cases=[])), \
             mock.patch.object(run.os.path, "ismount", return_value=True), \
             mock.patch.object(run.fixtures, "case_specs", return_value=cases), \
             mock.patch.object(run.fixtures, "build", side_effect=lambda case, seed: case), \
             mock.patch.object(run.fixtures, "prepare_native", side_effect=lambda case, *args: case), \
             mock.patch.object(run, "measure", side_effect=measured), \
             mock.patch.object(run, "settled_free_bytes", return_value=100), \
             mock.patch.object(run, "free_bytes", return_value=97), \
             mock.patch.object(run.lab, "flush"), mock.patch.object(run.time, "sleep"), \
             contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(run.main(), 0)
        meta = json.loads((root / "results" / "meta.json").read_text())
        rows = [json.loads(line) for line in (root / "results" / "runs.jsonl").read_text().splitlines()]
        return meta, rows

    def test_the_control_arm_runs_in_the_headline_cases_alone(self):
        meta, rows = self.campaign([dict(id="s1", profile="home"), dict(id="s4", profile="home")], "--control", "--rounds", "2")
        ran = {(row["scenario"], row["tool"]) for row in rows}
        self.assertEqual(ran, {("s1", "fetchloom"), ("s1", "fetchloom+control"), ("s4", "fetchloom")})
        control = [row for row in rows if row["tool"] == "fetchloom+control"]
        self.assertEqual([row["round"] for row in control], [0, 1, 2])
        self.assertEqual({row["environment"]["FETCHLOOM_EXP_NONE"] for row in control}, {"1"})
        self.assertEqual({tuple(sorted(order["tools"])) for order in meta["orders"] if order["scenario"] == "s4"}, {("fetchloom",)})


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
        # a refusal's or a redirect's page is no part of the file
        lines = ['{"request": "GET /key/a.bin HTTP/1.1", "status": 200, "bytes": 10}', "", '{"request": "-", "status": 400, "bytes": 3}',
                 '{"request": "GET /key/a.bin?x=1 HTTP/2.0", "status": 206, "bytes": 5}', '{"request": "HEAD /other HTTP/1.1", "status": 200, "bytes": 0}',
                 '{"request": "GET /key/a.bin HTTP/1.1", "status": 416, "bytes": 197}', '{"request": "GET /key/a.bin HTTP/1.1", "status": 302, "bytes": 145}']
        result = run.parse_access(lines, {"/key/a.bin"})
        self.assertEqual({key: result[key] for key in ("requests", "bytes_served", "file_bytes_served")},
                         dict(requests=6, bytes_served=360, file_bytes_served=15))

    def test_the_server_log_format_reads_back_as_the_parser_expects(self):
        sample = {"$request": "GET /key/a.bin HTTP/2.0", "$status": "200", "$body_bytes_sent": "13", "$host": "lab.flb",
                  "$server_protocol": "HTTP/2.0", "$connection": "7", "$connection_requests": "2",
                  "$request_time": "0.004", "$msec": "1700000000.125"}
        line = lab.ACCESS_FORMAT
        # a longer name first, so $connection does not eat $connection_requests
        for name in sorted(sample, key=len, reverse=True):
            line = line.replace(name, sample[name])
        result = run.parse_access([line], {"/key/a.bin"}, rtt_ms=20)
        self.assertEqual((result["file_bytes_served"], result["hosts"], result["protocols"], result["connections"],
                          result["most_requests_per_connection"], result["peak_in_flight"]),
                         (13, {"lab.flb": 1}, {"HTTP/2.0": 1}, 1, 2, {"lab.flb": 1}))

    def test_requests_in_flight_count_streams_and_last_at_least_a_round_trip(self):
        def line(host, connection, end_ms, took_ms, protocol="HTTP/2.0"):
            return json.dumps(dict(request="GET /a HTTP/2.0", status=200, bytes=1, host=host, protocol=protocol,
                                   connection=connection, connection_requests=1, request_time=took_ms / 1000,
                                   msec=1700000000 + end_ms / 1000))
        # four streams of one connection, each sending again 21 ms after its answer was handed over in
        # under a millisecond: a client's own pace at home, never more than four at once
        paced = [line("lab.flb", 1, worker + 21 * turn, 0) for worker in range(4) for turn in range(5)]
        result = run.parse_access(paced, set(), rtt_ms=20)
        self.assertEqual((result["peak_in_flight"], result["peak_connections"], result["connections"], result["in_flight_floor_ms"]),
                         ({"lab.flb": 4}, {"lab.flb": 1}, 1, 0))
        # the same answers on lan, where nothing holds a request open but the server's own time, so
        # those answered within the log's 2 ms are missed and the row says where its count stops
        lan = run.parse_access(paced, set(), rtt_ms=0)
        self.assertEqual((lan["peak_in_flight"], lan["in_flight_floor_ms"]), ({"lab.flb": 0}, 2))
        # six streams sent at once are six in flight however fast the server answered them
        burst = [line("lab.flb", 1, 5, 0) for _ in range(6)]
        self.assertEqual(run.parse_access(burst, set(), rtt_ms=20)["peak_in_flight"], {"lab.flb": 6})
        # back to back on one connection, the next arrival logged a millisecond before the last end
        chained = [line("lab.flb", 2, 10, 10, "HTTP/1.1"), line("lab.flb", 2, 20, 11, "HTTP/1.1")]
        result = run.parse_access(chained, set(), rtt_ms=0)
        self.assertEqual((result["peak_in_flight"], result["protocols"]), ({"lab.flb": 1}, {"HTTP/1.1": 2}))
        # each host has its own budget, and each connection counts once
        hosts = [line(host, number, 30, 25) for number, host in enumerate(["lab.flb"] * 4 + ["store.flb"] * 4)]
        result = run.parse_access(hosts, set(), rtt_ms=20)
        self.assertEqual((result["peak_in_flight"], result["peak_connections"], result["connections"]),
                         ({"lab.flb": 4, "store.flb": 4}, {"lab.flb": 4, "store.flb": 4}, 8))

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
            (work / "aria2.txt").write_text("https://lab.flb/a\n")
            self.assertEqual(run.exact_command(["bash", work / "command.sh"], work),
                             {"argv": ["bash", str(work / "command.sh")], "command.sh": "set -eu\n", "aria2.txt": "https://lab.flb/a\n"})

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

        def measured(argv, cwd, env, log, timeout, interrupt_after=None, cpus=None, during=None, anchor=None):
            # a listing of the folder costs bytes too, but is no part of the file
            access = self.root / "access.jsonl"
            access.write_text(access.read_text() + '{"request": "GET /key/a.bin HTTP/1.1", "status": 206, "bytes": 3}\n'
                                                   '{"request": "GET /key/ HTTP/1.1", "status": 200, "bytes": 100}\n')
            return next(attempts)

        row = self.one_run(Downloader("unused"), measured, dict(self.case, id="s7", profile="home"))
        self.assertEqual((row["wall_seconds"], row["user_seconds"], row["sys_seconds"], row["maxrss_bytes"]), (3, 5, 10, 7))
        self.assertEqual(row["first_attempt"]["exit_code"], 130)
        self.assertEqual(row["resume_command"]["argv"], ["true"])
        self.assertEqual((row["dataset_bytes"], row["body_bytes_ratio"]), (4, 6 / 4))
        self.assertEqual(row["checks"], ["no expected outputs", "first attempt ended before the prescribed interruption"])
        self.assertEqual(row["status"], "failed")
        attempts = iter([dict(wall_seconds=10, user_seconds=1, sys_seconds=2, maxrss_bytes=7, exit_code=130, timeout=False,
                              interruption_sent=True),
                         dict(wall_seconds=3, user_seconds=4, sys_seconds=8, maxrss_bytes=5, exit_code=0, timeout=False,
                              interruption_sent=False)])
        row = self.one_run(Downloader("unused"), measured, dict(self.case, id="s7", profile="home"))
        self.assertEqual(row["checks"], ["no expected outputs"])

    def test_a_run_with_more_requests_in_flight_than_the_budget_is_flagged(self):
        class Fast(tools.Tool):
            name = "fast"

            def command(self, scenario, workdir):
                return ["true"]

        for streams, flagged in ((4, False), (5, True)):
            def measured(*args, **kwargs):
                # every stream sent at once on one connection, each answered within the millisecond
                (self.root / "access.jsonl").write_text("".join(
                    json.dumps(dict(request="GET /key/a.bin HTTP/2.0", status=200, bytes=1, host="lab.flb", protocol="HTTP/2.0",
                                    connection=1, connection_requests=n + 1, request_time=0.0, msec=1700000000.005)) + "\n"
                    for n in range(streams)))
                return dict(wall_seconds=1, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False)

            row = self.one_run(Fast("unused"), measured, dict(self.case, profile="home"))
            self.assertEqual((row["peak_in_flight"], row["connections"], row["over_budget"]), ({"lab.flb": streams}, 1, flagged))

    def test_a_redirected_case_fails_unless_the_log_shows_both_hosts(self):
        class Fetcher(tools.Tool):
            name = "fetcher"

            def command(self, scenario, workdir):
                return ["true"]

        redirected = dict(self.case, id="s2r", base="s2r", profile="home",
                          files=[dict(self.case["files"][0], url="https://lab.flb/r/key/a.bin")])
        for hosts, said in ((("lab.flb", "store.flb"), None), (("store.flb",), "lab.flb"), (("lab.flb",), "store.flb")):
            def measured(*args, **kwargs):
                # the 302's body is no part of the file; the storage host's answer is
                (self.root / "access.jsonl").write_text("".join(
                    json.dumps(dict(request=f"GET {'/r' if host == 'lab.flb' else ''}/key/a.bin HTTP/2.0",
                                    status=302 if host == "lab.flb" else 200, bytes=145 if host == "lab.flb" else 4,
                                    host=host, protocol="HTTP/2.0", connection=n, connection_requests=1,
                                    request_time=0.001, msec=1700000000.005)) + "\n" for n, host in enumerate(hosts)))
                return dict(wall_seconds=1, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False)

            row = self.one_run(Fetcher("unused"), measured, redirected)
            missing = [check for check in row["checks"] if check.startswith("the server log shows no request")]
            self.assertEqual(missing, [f"the server log shows no request to {said}"] if said else [], hosts)
            self.assertEqual(row["file_bytes_served"], 4 if "store.flb" in hosts else 0)
        # a warm case may send nothing at all
        row = self.one_run(Fetcher("unused"), lambda *args, **kwargs: dict(wall_seconds=1, user_seconds=0, sys_seconds=0,
                                                                             maxrss_bytes=1, exit_code=0, timeout=False),
                           dict(redirected, id="s5"))
        self.assertFalse([check for check in row["checks"] if check.startswith("the server log")])

    def test_a_library_run_keeps_the_times_of_its_calls_and_the_process_beside_them(self):
        class Library(tools.Pooch):
            def command(self, scenario, workdir):
                # what the script reports as it exits
                (workdir / self.CALLS).write_text(json.dumps(dict(import_seconds=.4, wall_seconds=2.0, user_seconds=.5, sys_seconds=.25)))
                return ["true"]

        row = self.one_run(Library("unused"), lambda *args, **kwargs: dict(wall_seconds=3.0, user_seconds=1.0, sys_seconds=.5,
                                                                             maxrss_bytes=7, exit_code=0, timeout=False))
        self.assertEqual((row["clock"], row["wall_seconds"], row["user_seconds"], row["sys_seconds"], row["import_seconds"]),
                         ("call inside a warm interpreter", 2.0, .5, .25, .4))
        self.assertEqual((row["process"], row["maxrss_bytes"]), (dict(wall_seconds=3.0, user_seconds=1.0, sys_seconds=.5), 7))

    def test_a_dropped_link_counts_only_when_the_download_outlasts_it(self):
        drop = dict(after_seconds=10, length_seconds=10, dropped_at=10.02, restored_at=20.03, error=None)
        for ended, said in ((31.5, []), (20.0, ["the download ended before the link came back"]),
                            (9.0, ["the download ended before the link dropped"])):
            self.assertEqual(run.dropped(drop, ended), said, ended)
        self.assertEqual(run.dropped(dict(drop, dropped_at=None, restored_at=None), 31.5), ["the download ended before the link dropped"])
        self.assertEqual(run.dropped(dict(drop, restored_at=None, error="RuntimeError: tc failed"), 31.5),
                         ["the link drop failed: RuntimeError: tc failed"])

    def test_a_run_whose_link_drops_starts_on_the_whole_profile_and_keeps_the_drop_record(self):
        class Fetcher(tools.Tool):
            name = "fetcher"

            def command(self, scenario, workdir):
                return ["true"]

        made = []

        class Drop:
            def __init__(self, profile, after, length):
                made.append((profile, after, length))
                self.record = dict(after_seconds=after, length_seconds=length, dropped_at=10.0, restored_at=20.0, error=None)

        shaped = []

        def measured(*args, during=None, **kwargs):
            shaped.append(("measured", during))
            (self.root / "access.jsonl").write_text('{"request": "GET /key/a.bin HTTP/1.1", "status": 200, "bytes": 6}\n')
            return dict(wall_seconds=33, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False)

        case = dict(self.case, id="s10", base="s1", profile="home")
        with mock.patch.object(lab, "LinkDrop", Drop), mock.patch.object(lab, "profile", side_effect=lambda name: shaped.append(name)):
            row = self.one_run(Fetcher("unused"), measured, case)
        self.assertEqual(made, [("home", 10, 10)])
        self.assertEqual([step if isinstance(step, str) else step[0] for step in shaped], ["home", "measured"])
        self.assertIsInstance(shaped[1][1], Drop)
        self.assertEqual((row["link_drop"]["dropped_at"], row["body_bytes_ratio"]), (10.0, 6 / 4))
        self.assertNotIn("the download ended before the link came back", row["checks"])

    def test_every_arm_logs_under_a_name_of_one_length(self):
        logs = []

        def measured(argv, cwd, env, logfile, *args, **kwargs):
            logs.append(Path(logfile).name)
            return dict(wall_seconds=1, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False)

        rows = []
        for name in ("a", "fetchloom+a-much-longer-label"):
            named = type("Named", (tools.Tool,), dict(name=name, command=lambda self, scenario, workdir: ["true"]))
            rows.append(self.one_run(named("unused"), measured))
        self.assertEqual([row["log"] for row in rows], logs)
        self.assertEqual(len({len(log) for log in logs}), 1, logs)
        self.assertNotEqual(*logs)

    def test_a_resume_and_a_dropped_link_count_from_the_tools_own_anchor(self):
        def began():
            return None

        class Library(tools.Tool):
            name = "library"

            def command(self, scenario, workdir):
                return ["true"]

            def anchor(self, workdir):
                return began

        given = []

        def measured(*args, during=None, anchor=None, interrupt_after=None, **kwargs):
            given.append((interrupt_after, anchor))
            return dict(wall_seconds=33, user_seconds=0, sys_seconds=0, maxrss_bytes=1, exit_code=0, timeout=False,
                        interruption_sent=interrupt_after is not None)

        self.one_run(Library("unused"), measured, dict(self.case, id="s7", base="s1", profile="home"))
        # the first attempt's ctrl-c counts from the anchor; the resume runs to its end
        self.assertEqual(given, [(10, began), (None, None)])
        given.clear()

        class Drop:
            def __init__(self, *args):
                self.record = dict(dropped_at=10.0, restored_at=20.0, error=None)

        with mock.patch.object(lab, "LinkDrop", Drop), mock.patch.object(lab, "profile"):
            self.one_run(Library("unused"), measured, dict(self.case, id="s10", base="s1", profile="home"))
        self.assertEqual(given, [(None, began)])

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
                '{"request": "GET /dvc/key/files/md5/90/0150983cd24fb0d6963f7d28e17f72 HTTP/1.1", "status": 200, "bytes": 3}\n'
                '{"request": "GET /dvc/key/files/md5/90/abc.dir HTTP/1.1", "status": 200, "bytes": 2}\n')
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
        self.assertEqual((self.root / f"{row['run_id']}-prepare.log").read_text(), "cloning\n")
        self.assertEqual(json.loads((self.root / "runs.jsonl").read_text()), row)


class MainPreparationTests(unittest.TestCase):
    def test_a_client_that_cannot_be_prepared_is_a_failed_row_in_every_round(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            case = dict(id="s2r", base="s2r", profile="home", quick=True, count=1, size=3, key="sample", members=[], files=[])
            argv = ["run.py", "--fetchloom", "/not-executed", "--only-tools", "fetchloom", "--fs", "ext4", "--profiles", "home",
                    "--scenarios", "s2r", "--quick", "--out", str(root / "results")]
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


class ComparatorTests(unittest.TestCase):
    def test_frozen_comparators_name_a_headline_case_and_another_tool(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "comparators.json"
            path.write_text(json.dumps({"ext4/home/s1": "aria2c", "ext4/lan/s5": "dvc"}))
            self.assertEqual(run.comparators_choice(str(path)), {"ext4/home/s1": "aria2c", "ext4/lan/s5": "dvc"})
            for bad in ([], {"ext4/home/s1": "fetchloom"}, {"ext4/home/s1": "wget"}, {"home/s1": "curl"},
                        {"ntfs/home/s1": "curl"}, {"ext4/far/s1": "curl"}, {"ext4/home/s2": "curl"}):
                path.write_text(json.dumps(bad))
                with self.subTest(bad=bad), self.assertRaisesRegex(argparse.ArgumentTypeError, "frozen comparators map"):
                    run.comparators_choice(str(path))
            with self.assertRaisesRegex(argparse.ArgumentTypeError, "cannot read the frozen comparators"):
                run.comparators_choice(str(Path(folder) / "absent.json"))

    def test_the_frozen_comparators_are_in_the_campaign_record(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "owned.json").write_text("{}")
            args = SimpleNamespace(fs=[], seed=1729, quick=True, profiles=["home"], comparators={"ext4/home/s1": "curl"})
            with mock.patch.object(lab, "ROOT", root), mock.patch.object(run, "native_versions", return_value=[]), \
                 mock.patch.object(run, "python_packages", return_value=[]), mock.patch.object(run, "repo_head", return_value="head"), \
                 mock.patch.object(run.subprocess, "check_output", return_value="ext4 example rw"):
                self.assertEqual(run.metadata(args, {})["comparators"], {"ext4/home/s1": "curl"})


class MainRefusalTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name).resolve()
        (self.root / "owned.json").write_text("{}")

    def refused(self, *arguments, mounted=True):
        argv = ["run.py", "--fetchloom", "/not-executed", "--only-tools", "fetchloom", "--fs", "ext4",
                "--profiles", "home", "--scenarios", "s2r", "--out", str(self.root / "results"), *arguments]
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
        for twice in (["--variant", "a:FETCHLOOM_EXP_A=1", "--variant", "a:FETCHLOOM_EXP_B=1"],
                      ["--control", "--variant", "control:FETCHLOOM_EXP_A=1"]):
            self.assertEqual(self.refused(*twice), "run.py: error: each variant needs its own label, and control is the control arm's")
        self.assertEqual(self.refused("--scenarios", "s3", "--profiles", "poor"),
                         "run.py: error: no cases match these scenario/profile combinations")
        self.assertEqual(self.refused(mounted=False), "run.py: error: ext4 is not mounted")
        frozen = self.root / "comparators.json"
        frozen.write_text(json.dumps({"ext4/home/s2r": "curl"}))
        self.assertEqual(self.refused("--comparators", str(frozen)),
                         "run.py: error: curl, the comparator frozen for ext4/home/s2r, is not measured in it")
        (self.root / "results").mkdir(exist_ok=True)
        (self.root / "results/runs.jsonl").write_text("")
        self.assertEqual(self.refused(), "run.py: error: output already contains runs; use a new directory")
        (self.root / "owned.json").unlink()
        self.assertEqual(self.refused(), "run.py: error: run lab.py setup first")


if __name__ == "__main__":
    unittest.main()
