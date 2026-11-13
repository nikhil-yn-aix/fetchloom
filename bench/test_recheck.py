import argparse
import contextlib
import fcntl
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import recheck

# stands in for a fetchloom build: add names a dataset after what it adds, and add, lock and sync
# leave a data.lock, all the preparation reads
BUILD = """import sys
from pathlib import Path
words = sys.argv[1:]
if "add" in words:
    with open("data.toml", "a") as manifest:
        manifest.write(f'[datasets.{Path(words[-1]).stem}]\\nref = "{words[-1]}"\\n\\n')
if {"add", "lock", "sync"} & set(words):
    Path("data.lock").write_text("version = 1\\n")
"""


class FixtureTests(unittest.TestCase):
    def test_the_shared_files_depend_only_on_scale_and_seed(self):
        made = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as folder:
                root = Path(folder)
                recheck.sources(root, 0.0001, 7, "http://127.0.0.1:9")
                made.append({str(path.relative_to(root)): path.read_bytes() for path in sorted(root.rglob("*"))
                             if path.is_file() and path.name != "sources.json"})
        self.assertEqual(made[0], made[1])
        self.assertEqual(len([name for name in made[0] if name.startswith("src/files/")]), 10)
        self.assertEqual(made[0]["src/links.txt"].decode().splitlines()[0], "http://127.0.0.1:9/links/l00000.bin")

    def test_the_shared_files_are_made_again_only_for_another_scale_seed_or_origin(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            made = recheck.sources(root, 0.0001, 7, "http://127.0.0.1:9")
            kept = root / "www" / "kept"
            kept.write_text("")
            self.assertEqual(recheck.sources(root, 0.0001, 7, "http://127.0.0.1:9"), made)
            self.assertTrue(kept.exists())
            recheck.sources(root, 0.0001, 7, "http://127.0.0.1:10")
            self.assertFalse(kept.exists())

    def test_the_drop_names_one_file_of_every_dataset(self):
        with tempfile.TemporaryDirectory() as folder:
            project = Path(folder)
            (project / "data.toml").write_text('[datasets.p00001]\nref = "local"\n\n[datasets.p00000]\nref = "local"\n')
            self.assertEqual(recheck.drop_paths(project), ["data/p00000/remove.bin", "data/p00001/remove.bin"])

    def test_every_arm_runs_a_fresh_copy_and_the_aa_arm_the_baseline(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "old").write_bytes(b"baseline")
            (root / "new").write_bytes(b"candidate")
            copies = recheck.arms(root / "old", root / "new", root)
            self.assertEqual(list(copies), list(recheck.ARMS))
            self.assertEqual([path.read_bytes() for path in copies.values()], [b"baseline", b"candidate", b"baseline"])
            self.assertTrue(all(path.parent == root / "bin" for path in copies.values()))
            self.assertTrue(all(path.stat().st_mode & 0o100 for path in copies.values()))


class MeasureTests(unittest.TestCase):
    def test_every_arm_runs_once_a_round_and_the_rows_feed_the_summary(self):
        walls = {"fetchloom": 1.0, "fetchloom+candidate": 1.1, "fetchloom+aa": 1.0}

        def measured(argv, cwd, env, logfile, timeout, *, cpus=None):
            return dict(wall_seconds=walls[argv[0]], user_seconds=.5, sys_seconds=.1, exit_code=0, timeout=False,
                        maxrss_bytes=(2 if argv[0] == "fetchloom+candidate" else 1) << 20, cpu_affinity=[0])

        orders = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as folder:
                out = Path(folder)
                specs = {arm: dict(argv=[arm], cwd=out, env={}) for arm in recheck.ARMS}
                with mock.patch.object(recheck.run, "measure", side_effect=measured):
                    recheck.measure("status", specs, 6, 1729, out, None, 5)
                rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
            rounds = [[row["tool"] for row in rows if row["round"] == number] for number in range(7)]
            self.assertTrue(all(sorted(order) == sorted(recheck.ARMS) for order in rounds))
            self.assertEqual([row["warmup"] for row in rows].count(True), 3)
            orders.append(rounds)
        self.assertEqual(*orders)
        self.assertGreater(len({tuple(order) for order in orders[0]}), 1)
        text = recheck.summary(rows)
        self.assertIn("| status | candidate | 6 | 1000.0 → 1100.0 ms | 1.100 [1.100, 1.100] | 1.0 → 2.0 | 2.000", text)
        self.assertIn("| status | aa | 6 | 1000.0 → 1000.0 ms | 1.000 [1.000, 1.000] | 1.0 → 1.0 | 1.000", text)


def build(path, program=BUILD):
    path.write_text(f"#!{sys.executable}\n{program}")
    path.chmod(0o755)
    return path


class PreparationTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.signature = recheck.sources(self.root, 0.0001, 7, "http://127.0.0.1:9")

    def test_each_arm_prepares_once_with_its_own_binary(self):
        binary = build(self.root / "fetchloom")
        folder = recheck.prepare(self.root, "fetchloom+candidate", binary, self.signature, "http://127.0.0.1:9")
        self.assertEqual(folder, self.root / "fx/fetchloom-candidate")
        self.assertEqual(recheck.drop_paths(folder / "drop"), [f"data/p0000{index}/remove.bin" for index in range(3)])
        merged = (folder / "merge/data.toml").read_text()
        self.assertTrue(merged.startswith("<<<<<<< ours\n[datasets.ours]") and "=======\n[datasets.theirs]" in merged, merged)
        marker = (folder / "ready.json").stat().st_mtime_ns
        recheck.prepare(self.root, "fetchloom+candidate", binary, self.signature, "http://127.0.0.1:9")
        self.assertEqual((folder / "ready.json").stat().st_mtime_ns, marker)
        build(binary, BUILD + "# a new build\n")
        recheck.prepare(self.root, "fetchloom+candidate", binary, self.signature, "http://127.0.0.1:9")
        self.assertNotEqual((folder / "ready.json").stat().st_mtime_ns, marker)

    def test_a_preparation_that_fails_names_the_arm_and_the_command(self):
        failing = build(self.root / "fetchloom", "import sys\nprint('no space left on device', file=sys.stderr)\nsys.exit(50)\n")
        with self.assertRaises(RuntimeError) as raised:
            recheck.prepare(self.root, "fetchloom", failing, self.signature, "http://127.0.0.1:9")
        self.assertEqual(str(raised.exception),
                         f"fetchloom: fetchloom add {self.root / 'src/files'} exited 50: no space left on device")

    def test_a_case_that_changes_its_project_works_on_a_fresh_copy_and_clears_it_after(self):
        folder = recheck.prepare(self.root, "fetchloom", build(self.root / "fetchloom"), self.signature, "http://127.0.0.1:9")
        specs = recheck.cases(self.root, "fetchloom", "/bin/fetchloom", folder, "http://127.0.0.1:9")
        self.assertEqual(sorted(specs), sorted(recheck.ROUNDS))
        work = self.root / "work/fetchloom"
        (folder / "many/data/stray.bin").parent.mkdir(parents=True)
        (folder / "many/data/stray.bin").write_bytes(b"")
        specs["place"]["prep"]()
        self.assertEqual(sorted(path.name for path in (work / "place").iterdir()), ["data.lock", "data.toml"])
        specs["place"]["post"]()
        self.assertFalse((work / "place").exists())
        specs["get"]["prep"]()
        (work / "get-cache").mkdir()
        specs["get"]["post"]()
        self.assertEqual(list(work.iterdir()), [])
        self.assertEqual(specs["get"]["argv"], ["/bin/fetchloom", "-q", "--yes", "get", "http://127.0.0.1:9/big.bin", "-o",
                                                str(work / "get/big.bin")])

    def test_a_tree_with_read_only_folders_is_removed(self):
        tree = self.root / "tree/data"
        tree.mkdir(parents=True)
        (tree / "file.bin").write_bytes(b"x")
        tree.chmod(0o555)
        recheck.remove(self.root / "tree")
        self.assertFalse((self.root / "tree").exists())
        recheck.remove(self.root / "tree")


class ServerTests(unittest.TestCase):
    def test_the_server_sends_each_file_whole(self):
        import urllib.request
        with tempfile.TemporaryDirectory() as folder:
            body = os.urandom(3 << 20)
            Path(folder, "big.bin").write_bytes(body)
            server = recheck.serve(Path(folder), 0)
            try:
                opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
                with opener.open(f"http://127.0.0.1:{server.server_address[1]}/big.bin", timeout=30) as response:
                    self.assertEqual(response.read(), body)
            finally:
                server.shutdown()
                server.server_close()

    def test_a_case_that_needs_a_fresh_copy_makes_it_before_and_clears_it_after_each_run(self):
        done = []
        specs = {arm: dict(argv=[arm], cwd=Path("."), env={}, prep=lambda arm=arm: done.append(("prep", arm)),
                           post=lambda arm=arm: done.append(("post", arm))) for arm in recheck.ARMS}

        def measured(argv, *args, **kwargs):
            done.append(("run", argv[0]))
            return dict(wall_seconds=1, exit_code=0, timeout=False)

        with tempfile.TemporaryDirectory() as folder, mock.patch.object(recheck.run, "measure", side_effect=measured):
            recheck.measure("place", specs, 1, 1729, Path(folder), None, 5)
        self.assertEqual(len(done), 2 * 3 * 3)
        self.assertTrue(all([step for step, _ in done[index:index + 3]] == ["prep", "run", "post"] for index in range(0, 18, 3)))


class SettingTests(unittest.TestCase):
    def test_the_measured_environment_has_no_setting_of_the_shell(self):
        with mock.patch.dict(os.environ, {"FETCHLOOM_JOBS": "2", "NO_COLOR": "1", "LANG": "C.UTF-8"}):
            env = recheck.environment(Path("/r"), Path("/r/cache"), "http://127.0.0.1:9")
        self.assertEqual({key: env[key] for key in env if key.startswith("FETCHLOOM_")},
                         dict(FETCHLOOM_CACHE="/r/cache", FETCHLOOM_ALLOWED_ORIGINS="http://127.0.0.1:9"))
        self.assertEqual((env["HOME"], env["TMPDIR"], env["LANG"], "NO_COLOR" in env), ("/r/home", "/r/tmp", "C.UTF-8", False))

    def test_cpus_may_lie_outside_the_scripts_own(self):
        with mock.patch.object(recheck.os, "cpu_count", return_value=4):
            self.assertEqual(recheck.cpus("2-3"), (2, 3))
            with self.assertRaisesRegex(argparse.ArgumentTypeError, "not available"):
                recheck.cpus("4")

    def test_a_case_holds_the_measuring_lock_while_it_runs(self):
        with recheck.turn(None):
            pass
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder, "measure.lock")
            with recheck.turn(path), open(path, "a") as other:
                with self.assertRaises(BlockingIOError):
                    fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with open(path, "a") as other:
                fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)


@unittest.skipUnless(os.access(recheck.run.TIME, os.X_OK), "GNU time")
class CommandTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)

    def recheck(self, baseline, candidate, *more):
        said, shown = io.StringIO(), io.StringIO()
        # the stand-ins never fetch, so the server may take any port
        argv = ["--baseline", str(baseline), "--candidate", str(candidate), "--root", str(self.root / "recheck"),
                "--scale", "0.0001", "--port", "0", *more]
        with contextlib.redirect_stderr(said), contextlib.redirect_stdout(shown):
            code = recheck.main(argv)
        return code, said.getvalue(), shown.getvalue()

    def test_the_command_times_every_arm_on_each_case_asked_for(self):
        baseline, candidate = build(self.root / "old"), build(self.root / "new", BUILD + "# a new build\n")
        code, said, shown = self.recheck(baseline, candidate, "--cases", "version,help", "--rounds", "2")
        self.assertEqual(code, 0, said)
        [results] = (self.root / "recheck/results").iterdir()
        rows = [json.loads(line) for line in (results / "runs.jsonl").read_text().splitlines()]
        self.assertEqual(len(rows), 2 * 3 * 3)
        self.assertEqual({row["status"] for row in rows}, {"passed"})
        meta = json.loads((results / "meta.json").read_text())
        self.assertEqual(meta["binaries"]["fetchloom+aa"]["sha256"], meta["binaries"]["fetchloom"]["sha256"])
        self.assertEqual(meta["binaries"]["fetchloom+candidate"]["source"], str(candidate))
        self.assertEqual(shown, (results / "summary.md").read_text())
        self.assertEqual(sorted(tuple(line.split(" | ")[:2]) for line in shown.splitlines()[2:]),
                         [("| help", "aa"), ("| help", "candidate"), ("| version", "aa"), ("| version", "candidate")])
        self.assertEqual(said.splitlines()[:3], [f"preparing {arm}" for arm in recheck.ARMS])
        self.assertEqual(said.splitlines()[3:5], ["version: 2 rounds", "help: 2 rounds"])

    def test_a_build_that_cannot_prepare_ends_the_recheck(self):
        failing = build(self.root / "old", "raise SystemExit(1)\n")
        code, said, _ = self.recheck(failing, build(self.root / "new"), "--cases", "version")
        self.assertEqual(code, 1)
        self.assertTrue(said.splitlines()[-1].startswith("recheck: fetchloom: fetchloom add "), said)

    def test_the_command_refuses_unknown_cases_and_impossible_sizes(self):
        for given in (["--cases", "sync,fly"], ["--rounds", "0"], ["--scale", "0"], ["--scale", "2"]):
            with self.subTest(given), self.assertRaises(SystemExit):
                self.recheck("a", "b", *given)
        self.assertFalse((self.root / "recheck").exists())


if __name__ == "__main__":
    unittest.main()
