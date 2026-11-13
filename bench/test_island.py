import contextlib
import io
import json
import math
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import island
import lab
import run


def aa(case, **wall):
    """an a/a result of ab.py for ``case``, its wall metric changed by ``wall``."""
    return dict(case=case, variant="fetchloom+aa", failed=dict(base=0, variant=0), unmatched=dict(base=[], variant=[]),
                metrics=dict(wall=dict(dict(n_log_pairs=20, missing_metric_pairs=0, nonpositive_pairs=0,
                                            bootstrap_95_median_ratio=[.99, 1.01], log_ratio_robust_sd=.01,
                                            plain_median=1, median_ratio=1), **wall)))


class CpuTests(unittest.TestCase):
    def test_cpu_list_is_validated_against_available_cpus(self):
        self.assertEqual(lab.cpu_list("0-2,4,2", {0, 1, 2, 4}), (0, 1, 2, 4))
        self.assertEqual(lab.cpu_list("", {3, 5}), (3, 5))
        for value in ("1-0", "-1", "1,,2", "1-2-3", "x", "1,9"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                lab.cpu_list(value, {0, 1, 2, 4})

    def test_affinity_is_inherited_and_restored_even_on_error(self):
        before = os.sched_getaffinity(0)
        chosen = (min(before),)
        source = ("import json, os, subprocess, sys; "
                  "child=subprocess.check_output([sys.executable, '-c', "
                  "'import os; print(sorted(os.sched_getaffinity(0)))'], text=True); "
                  "print(json.dumps([sorted(os.sched_getaffinity(0)), json.loads(child)]))")
        with self.assertRaisesRegex(RuntimeError, "after spawn"):
            with lab.affinity(chosen):
                answer = json.loads(subprocess.check_output([sys.executable, "-B", "-c", source], text=True))
                self.assertEqual(answer, [list(chosen), list(chosen)])
                raise RuntimeError("after spawn")
        self.assertEqual(os.sched_getaffinity(0), before)

    def test_measured_spawn_inherits_selected_mask(self):
        from types import SimpleNamespace
        before = os.sched_getaffinity(0)
        chosen = (min(before),)
        seen = []
        def spawn(*args, **kwargs):
            seen.append(os.sched_getaffinity(0))
            return 987654
        with tempfile.TemporaryDirectory() as folder, \
             mock.patch.object(run.os, "posix_spawn", side_effect=spawn), \
             mock.patch.object(run.os, "wait4", return_value=(987654, 0, SimpleNamespace(ru_utime=1, ru_stime=2))), \
             mock.patch.object(run.signal, "setitimer"), \
             mock.patch.object(lab, "end_group"):
            root = Path(folder)
            result = run.measure(["true"], root, os.environ, root / "log", cpus=chosen)
        self.assertEqual(seen, [set(chosen)])
        self.assertEqual(result["cpu_affinity"], list(chosen))
        self.assertEqual(os.sched_getaffinity(0), before)

    def test_nginx_master_and_every_worker_thread_are_pinned(self):
        with tempfile.TemporaryDirectory() as folder:
            proc = Path(folder)
            for pid, name, tids in ((31, "nginx", (31,)), (32, "nginx", (32, 33)), (40, "helper", (40,))):
                (proc / str(pid) / "task").mkdir(parents=True)
                (proc / str(pid) / "comm").write_text(name + "\n")
                for tid in tids:
                    (proc / str(pid) / "task" / str(tid)).mkdir()
            with mock.patch.object(lab.subprocess, "check_output", return_value="31\n32\n40\n"), \
                 mock.patch.object(lab.os, "sched_setaffinity") as set_mask, \
                 mock.patch.object(lab.os, "sched_getaffinity", return_value={1, 2}):
                result = lab.pin_server((1, 2), proc=proc)
            self.assertEqual(set_mask.call_args_list, [mock.call(tid, (1, 2)) for tid in (31, 32, 33)])
            self.assertEqual(result, {"31": [1, 2], "32": [1, 2], "33": [1, 2]})

    def test_missing_nginx_cannot_silently_leave_the_server_unpinned(self):
        with mock.patch.object(lab.subprocess, "check_output", return_value=""):
            with self.assertRaisesRegex(RuntimeError, "nginx"):
                lab.pin_server((0,))


class IslandCheckTests(unittest.TestCase):
    def test_build_cores_must_be_disjoint(self):
        import island
        island.disjoint((0, 1), (2, 3))
        for first, second in (((0, 1), (1, 2)), ((), (1,)), ((0,), ())):
            with self.assertRaises(ValueError):
                island.disjoint(first, second)

    def test_campaign_is_only_the_requested_full_aa_cases(self):
        import island
        command = island.campaign_command(Path("/binary"), Path("/results"), (0, 2), 20, 17)
        self.assertIn("s2,s3,s5", command)
        self.assertIn("aa:FETCHLOOM_EXP_NONE=1", command)
        self.assertIn("0,2", command)
        self.assertNotIn("--quick", command)
        clean, build = island.build_commands("/cargo", Path("/private-target"), 3)
        self.assertEqual(clean[-2:], ["--target-dir", "/private-target"])
        self.assertEqual(build[-2:], ["--target-dir", "/private-target"])
        self.assertIn("--locked", build)

    def test_selected_cases_do_not_expand_into_a_cartesian_matrix(self):
        import island
        cases = ("ext4/lan/s3", "ext4/lan/s4", "xfs/lan/s4", "ext4/lan/s1", "ext4/home/s2")
        for case in cases:
            self.assertEqual(island.case_choice(case), case)
            fs, profile, scenario = case.split("/")
            command = island.campaign_command(Path("/binary"), Path("/results"), (0, 1), 10, 17, case)
            self.assertEqual(command[command.index("--fs") + 1], fs)
            self.assertEqual(command[command.index("--profiles") + 1], profile)
            self.assertEqual(command[command.index("--scenarios") + 1], scenario)
            self.assertEqual(command[command.index("--rounds") + 1], "10")
        import argparse
        for bad in ("ext4/home/s3", "ntfs/lan/s3", "ext4/unknown/s1", "lan/s3"):
            with self.assertRaises(argparse.ArgumentTypeError):
                island.case_choice(bad)

    def test_progress_updates_replace_a_complete_json_file(self):
        import island
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "ready.json"
            island.write_state(path, dict(iteration=1))
            island.write_state(path, dict(iteration=2))
            self.assertEqual(json.loads(path.read_text()), dict(iteration=2))
            self.assertEqual(list(Path(folder).iterdir()), [path])

    def test_build_worker_rejects_an_unowned_target_before_compiling(self):
        import island
        from types import SimpleNamespace
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(island.subprocess, "run") as compile:
            root = Path(folder)
            args = SimpleNamespace(target=root, ready=root / "ready", worktree=root, cpus=(0,))
            with self.assertRaises(FileNotFoundError):
                island.build_load(args)
            compile.assert_not_called()

    def test_noise_check_rejects_build_slowdown_and_added_noise(self):
        import island
        def result(scenario):
            return dict(case=f"ext4/lan/{scenario}", variant="fetchloom+aa", failed=dict(base=0, variant=0),
                        unmatched=dict(base=[], variant=[]), metrics=dict(wall=dict(n_log_pairs=20,
                        missing_metric_pairs=0, nonpositive_pairs=0, bootstrap_95_median_ratio=[.99, 1.01],
                        log_ratio_robust_sd=.01, plain_median=1, median_ratio=1)))
        quiet = [result(s) for s in island.SCENARIOS]
        loaded = [result(s) for s in island.SCENARIOS]
        noise = dict(results=[dict(case=f"ext4/lan/{s}", variant="fetchloom+aa",
                                   metrics=dict(wall=dict(log_ratio_robust_sd=.01, plain_median=1, median_ratio=1)))
                              for s in island.SCENARIOS])
        self.assertTrue(all(r["passed"] for r in island.compare_noise(quiet, loaded, noise)))
        loaded[0]["metrics"]["wall"]["plain_median"] = 1.2
        loaded[1]["metrics"]["wall"]["log_ratio_robust_sd"] = .1
        results = island.compare_noise(quiet, loaded, noise)
        self.assertEqual([r["passed"] for r in results], [False, False, True])

    def test_the_noise_file_is_ab_json_output_of_a_quiet_aa_campaign(self):
        import island
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            rows = [dict(kind="lab", tool=tool, filesystem="ext4", profile="lan", scenario="s2", round=n,
                         warmup=False, status="passed", quick=False, count=1000, file_size=102400,
                         wall_seconds=wall)
                    for n in range(1, 21) for tool, wall in (("fetchloom", 1 + n / 1000), ("fetchloom+aa", 1 + n % 3 / 1000))]
            (root / "runs.jsonl").write_text("".join(json.dumps(row) + "\n" for row in rows))
            subprocess.run([sys.executable, str(Path(island.__file__).with_name("ab.py")), str(root), "--resamples", "200",
                            "--json", str(root / "noise.json")], check=True, capture_output=True)
            noise = json.loads((root / "noise.json").read_text())
        quiet = loaded = noise["results"]
        [result] = island.compare_noise(quiet, loaded, noise, cases=("ext4/lan/s2",))
        self.assertTrue(result["passed"], result["reasons"])
        with self.assertRaisesRegex(ValueError, "ext4/lan/s3"):
            island.compare_noise(quiet, loaded, noise, cases=("ext4/lan/s3",))


class CheckRefusalTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name).resolve()
        (self.root / "lab/fs").mkdir(parents=True)
        (self.root / "source").mkdir()
        noise = self.root / "noise.json"
        noise.write_text(json.dumps(dict(results=[aa(case) for case in island.DEFAULT_CASES])))
        self.args = dict(cases=None, cpus=(0, 1), build_cpus=(2, 3), rounds=20, jobs=2, effect=.03, noise_multiple=2,
                         out=self.root / "lab/results/island", target=self.root / "target", worktree=self.root / "source",
                         fetchloom=self.root / "fetchloom", noise=noise, seed=1729, cargo="cargo")

    def refused(self, said, **changes):
        args = SimpleNamespace(**dict(self.args, **changes))
        with mock.patch.object(lab, "require_root"), mock.patch.object(lab, "ROOT", self.root / "lab"), \
             self.assertRaisesRegex(ValueError, said):
            island.check(args)
        self.assertFalse(args.out.exists())

    def test_an_island_check_that_cannot_hold_starts_nothing(self):
        self.refused("choose distinct cases", cases=["ext4/lan/s2", "ext4/lan/s2"])
        self.refused("measurement and build CPU lists must be nonempty and disjoint", build_cpus=(1, 2))
        self.refused("measurement and build CPU lists must be nonempty and disjoint", build_cpus=())
        for changes in (dict(rounds=9), dict(jobs=0), dict(jobs=3)):
            self.refused("use at least 10 rounds and 1 to the build CPU count jobs", **changes)
        for effect in (math.nan, 0, 1):
            self.refused("effect must be a finite fraction between 0 and 1", effect=effect)
        for multiple in (math.inf, .5):
            self.refused("noise-multiple must be finite and at least 1", noise_multiple=multiple)
        self.refused("write results under", out=self.root / "elsewhere")
        self.refused("the compiler target must be outside source and measured filesystems", target=self.root / "source/target")
        self.refused("the compiler target must be outside source and measured filesystems", target=self.root / "lab/fs/target")
        (self.root / "target-made").mkdir()
        self.refused("output and compiler target must be new paths", target=self.root / "target-made")
        self.refused("the noise file has no a/a result for ext4/lan/s4", cases=["ext4/lan/s4"])
        self.assertFalse((self.root / "lab/results").exists())

    def test_the_command_reports_a_refusal_as_a_usage_error(self):
        argv = ["island.py", "check", "--fetchloom", "fl", "--cpus", "0", "--build-cpus", "0", "--noise", "noise.json",
                "--out", "out", "--worktree", "source", "--target", "target"]
        with mock.patch.object(sys, "argv", argv), mock.patch.object(lab, "require_root"), \
             mock.patch.object(lab.os, "sched_getaffinity", return_value={0}), \
             contextlib.redirect_stderr(io.StringIO()) as said, self.assertRaises(SystemExit) as stopped:
            island.main()
        self.assertEqual(stopped.exception.code, 2)
        self.assertEqual(said.getvalue().splitlines()[-1],
                         "island.py: error: measurement and build CPU lists must be nonempty and disjoint")


class CampaignTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.args = SimpleNamespace(fetchloom=Path("/fl"), cpus=(0,), rounds=10, seed=1729, out=self.root)

    def campaign(self, program, load=None):
        output = self.root / "quiet-island" / "ext4-lan-s2"
        output.mkdir(parents=True)
        command = [sys.executable, "-c", program, str(output)]
        with mock.patch.object(island, "campaign_command", return_value=command):
            return island.campaign_case(self.args, output, load, "ext4/lan/s2")

    def test_a_campaign_returns_the_a_a_comparison_of_its_runs(self):
        rows = [dict(kind="lab", tool=tool, filesystem="ext4", profile="lan", scenario="s2", round=n, warmup=False,
                     status="passed", quick=False, count=1, file_size=1, wall_seconds=1) for n in range(1, 11)
                for tool in ("fetchloom", "fetchloom+aa")]
        program = f"import sys\nfrom pathlib import Path\nPath(sys.argv[1], 'runs.jsonl').write_text({''.join(json.dumps(r) + chr(10) for r in rows)!r})\n"
        [result] = self.campaign(program)
        self.assertEqual((result["case"], result["variant"], result["rounds"]), ("ext4/lan/s2", "fetchloom+aa", 10))

    def test_a_failed_campaign_or_a_load_that_ended_is_refused(self):
        with self.assertRaisesRegex(RuntimeError, f"benchmark command failed with exit 3; see {self.root / 'quiet-island-ext4-lan-s2.log'}"):
            self.campaign("raise SystemExit(3)")
        ended = subprocess.Popen([sys.executable, "-c", "raise SystemExit(1)"])
        ended.wait()
        (self.root / "quiet-island").rename(self.root / "first")
        finished = self.root / "finished"
        with self.assertRaisesRegex(RuntimeError, "compiler load ended during measurement; exit 1"):
            self.campaign(f"import time\ntime.sleep(1)\nopen({str(finished)!r}, 'w').close()", load=ended)
        # stopped as soon as the load was seen to end, not when the campaign was done
        self.assertFalse(finished.exists())

    def test_a_process_that_will_not_stop_is_killed(self):
        island.stop(None)
        process = mock.Mock(pid=4242)
        process.wait.side_effect = [subprocess.TimeoutExpired("campaign", 5), 0]
        with mock.patch.object(island.os, "killpg", side_effect=[ProcessLookupError, None]) as kill, \
             mock.patch.object(lab, "end_group") as ended:
            island.stop(process)
        self.assertEqual(kill.call_args_list, [mock.call(4242, signal.SIGTERM), mock.call(4242, signal.SIGKILL)])
        ended.assert_called_once_with(4242)

    def test_the_compiler_load_builds_until_a_build_fails(self):
        target, worktree = self.root / "target", self.root / "source"
        target.mkdir()
        worktree.mkdir()
        ready = self.root / "out/build-ready.json"
        ready.parent.mkdir()
        island.write_state(target / ".fl-bench-island.json", dict(output=str(ready.parent.resolve()), worktree=str(worktree.resolve())))
        builds = self.root / "builds"
        # the second build fails, as a compiler that ran out of room would
        cargo = self.root / "cargo"
        cargo.write_text(f"#!{sys.executable}\nimport sys\nfrom pathlib import Path\nif sys.argv[1] == 'build':\n"
                         f"    count = Path({str(builds)!r})\n    count.write_text(count.read_text() + 'x' if count.exists() else 'x')\n"
                         "    sys.exit(7 if len(count.read_text()) == 2 else 0)\n")
        cargo.chmod(0o755)
        args = SimpleNamespace(target=target, ready=ready, worktree=worktree, cpus=tuple(sorted(os.sched_getaffinity(0))),
                               cargo=str(cargo), jobs=2)
        self.assertEqual(island.build_load(args), 7)
        state = json.loads(ready.read_text())
        self.assertEqual((state["iteration"], state["cpu_affinity"], state["command"]),
                         (1, list(args.cpus), [str(cargo), "build", "--locked", "--release", "--jobs", "2", "--target-dir", str(target)]))


class NoiseTests(unittest.TestCase):
    def test_each_reason_an_island_fails_is_named(self):
        noise = dict(results=[aa("ext4/lan/s2", bootstrap_95_median_ratio=[.995, 1.005])])
        for case, quiet, loaded, said in [
            ("failed pairs", aa("ext4/lan/s2"), dict(aa("ext4/lan/s2"), failed=dict(base=1, variant=0)),
             "loaded: failed, missing or insufficient paired observations"),
            ("too few pairs", aa("ext4/lan/s2", n_log_pairs=9), aa("ext4/lan/s2"), "quiet: failed, missing or insufficient paired observations"),
            ("outside the band", aa("ext4/lan/s2"), aa("ext4/lan/s2", bootstrap_95_median_ratio=[1.02, 1.04]),
             "loaded: A/A interval is outside the chosen equivalence band"),
            ("no interval", aa("ext4/lan/s2", bootstrap_95_median_ratio=None), aa("ext4/lan/s2"),
             "quiet: A/A interval is outside the chosen equivalence band"),
            ("wider", aa("ext4/lan/s2", bootstrap_95_median_ratio=[.999, 1.001]), aa("ext4/lan/s2", bootstrap_95_median_ratio=[.975, 1.025]),
             "loaded A/A interval width exceeds the quiet/historical limit"),
            ("biased", aa("ext4/lan/s2"), aa("ext4/lan/s2", median_ratio=1.02), "loaded A/A bias shifted by more than one third"),
        ]:
            with self.subTest(case):
                [result] = island.compare_noise([quiet], [loaded], noise, cases=("ext4/lan/s2",))
                self.assertFalse(result["passed"])
                self.assertTrue(any(reason.startswith(said) for reason in result["reasons"]), result["reasons"])

    def test_noise_without_a_usable_spread_or_shape_is_refused(self):
        for sigma in (None, -1, math.inf):
            noise = dict(results=[aa("ext4/lan/s2", log_ratio_robust_sd=sigma)])
            with self.subTest(sigma=sigma), self.assertRaisesRegex(ValueError, "quiet noise has no usable paired wall spread for ext4/lan/s2"):
                island.compare_noise([aa("ext4/lan/s2")], [aa("ext4/lan/s2")], noise, cases=("ext4/lan/s2",))
        for noise in ([], dict(results={}), "results"):
            with self.subTest(noise=noise), self.assertRaisesRegex(ValueError, "the noise file must be ab.py --json output"):
                island.noise_reference(noise)


if __name__ == "__main__":
    unittest.main()
