import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

import lab
import run


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


if __name__ == "__main__":
    unittest.main()
