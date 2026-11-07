import contextlib
import io
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


if __name__ == "__main__":
    unittest.main()
