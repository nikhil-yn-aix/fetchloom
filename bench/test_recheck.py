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

import hashlib

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

    def test_the_exclude_names_one_file_of_every_dataset(self):
        with tempfile.TemporaryDirectory() as folder:
            project = Path(folder)
            (project / "data.toml").write_text('[datasets.p00001]\nref = "local"\n\n[datasets.p00000]\nref = "local"\n')
            self.assertEqual(recheck.exclude_paths(project), ["data/p00000/remove.bin", "data/p00001/remove.bin"])

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
            # each arm's binary has a name of one length, as its folders have
            self.assertEqual([path.name for path in copies.values()], ["arm0", "arm1", "arm2"])


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
        self.assertIn("| status | candidate | 6 | 0/0 | 1000.0 → 1100.0 ms | 1.100 [1.100, 1.100] | 1.000 [1.000, 1.000] | 1.0 → 2.0 | 2.000", text)
        self.assertIn("| status | aa | 6 | 0/0 | 1000.0 → 1000.0 ms | 1.000 [1.000, 1.000] | 1.000 [1.000, 1.000] | 1.0 → 1.0 | 1.000", text)
        # a failed run leaves its comparison without any performance, and says how many failed
        broken = [dict(row, status="failed") if (row["tool"], row["round"]) == ("fetchloom+candidate", 3) else row for row in rows]
        text = recheck.summary(broken)
        self.assertIn("| status | candidate | 5 | 0/1 | failed | - | - | - | - |", text)
        self.assertIn("| status | aa | 6 | 0/0 | 1000.0 → 1000.0 ms | 1.000 [1.000, 1.000]", text)

    def test_a_link_case_names_its_link_in_the_summary(self):
        rows = [dict(kind="lab", tool=arm, scenario="s1", filesystem="ext4", profile=profile, round=n, warmup=False,
                     status="passed", quick=False, count=1, file_size=8, wall_seconds=wall * (2 if profile == "poor" else 1),
                     user_seconds=1, sys_seconds=0, maxrss_bytes=1 << 20)
                for profile in ("home", "poor") for n in range(1, 6)
                for arm, wall in (("fetchloom", 1.0), ("fetchloom+candidate", .9), ("fetchloom+aa", 1.0))]
        text = recheck.summary(rows)
        self.assertIn("| s1 home | candidate | 5 | 0/0 | 1000.0 → 900.0 ms | 0.900 [0.900, 0.900]", text)
        self.assertIn("| s1 poor | candidate | 5 | 0/0 | 2000.0 → 1800.0 ms | 0.900 [0.900, 0.900]", text)

    def test_what_a_run_downloads_or_places_is_checked_after_the_clock_and_before_it_is_cleared(self):
        with tempfile.TemporaryDirectory() as folder:
            out = Path(folder)
            placed = out / "placed"
            good, bad = b"right bytes", b"wrong bytes"
            done = []
            for leaves, status in ((good, "passed"), (bad, "failed")):
                def measured(argv, cwd, env, logfile, timeout, *, cpus=None, leaves=leaves):
                    (placed / "data" / "d").mkdir(parents=True)
                    (placed / "data" / "d" / "file.bin").write_bytes(leaves)
                    done.append("run")
                    return dict(wall_seconds=1, exit_code=0, timeout=False)

                def cleared():
                    done.append("post")
                    recheck.remove(placed)

                def outputs():
                    done.append("check")
                    return placed, {"data/d/file.bin": hashlib.sha256(good).hexdigest()}

                specs = {arm: dict(argv=[arm], cwd=out, env={}, post=cleared, outputs=outputs) for arm in recheck.ARMS[:1]}
                (out / "runs.jsonl").unlink(missing_ok=True)
                with mock.patch.object(recheck.run, "measure", side_effect=measured):
                    recheck.measure("place", specs, 1, 1729, out, None, 5)
                rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines()]
                self.assertEqual({row["status"] for row in rows}, {status}, rows)
                self.assertEqual(rows[-1]["checks"], [] if status == "passed" else
                                 [f"1 of 1 expected files are missing or wrong under {placed}"])
            self.assertEqual(done, ["run", "check", "post"] * 4)

    def test_every_file_must_hold_its_own_bytes_at_its_own_path(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            (root / "data/a").mkdir(parents=True)
            (root / "data/b").mkdir()
            (root / "data/a/a.bin").write_bytes(b"1")
            (root / "data/b/b.bin").write_bytes(b"2")
            digest = {data: hashlib.sha256(data).hexdigest() for data in (b"1", b"2")}
            expect = {"data/a/a.bin": digest[b"1"], "data/b/b.bin": digest[b"2"]}
            self.assertEqual(recheck.placed(root, expect), [])
            # the same bytes swapped between the two files are two wrong files, though all the bytes are there
            (root / "data/a/a.bin").write_bytes(b"2")
            (root / "data/b/b.bin").write_bytes(b"1")
            self.assertEqual(recheck.placed(root, expect), [f"2 of 2 expected files are missing or wrong under {root}"])
            self.assertEqual(recheck.placed(root, {"data/c/c.bin": digest[b"1"], "data/a/a.bin": None}),
                             [f"2 of 2 expected files are missing or wrong under {root}"])

    def test_each_dataset_of_a_project_expects_its_files_where_fetchloom_places_them(self):
        sources = dict(links={"http://h/links/l00000.bin": "1" * 64, "http://h/many/m00001.bin": "2" * 64},
                       members={"m00000.bin": "3" * 64, "m00001.bin": "4" * 64}, big="5" * 64)
        with tempfile.TemporaryDirectory() as folder:
            project = Path(folder)
            self.assertEqual(recheck.layout(project, sources), {"data.toml": None})
            (project / "data.toml").write_text('[datasets.l00000]\nref = "http://h/links/l00000.bin"\n\n'
                                                '[datasets.second]\nref = "http://h/many/m00001.bin"\n\n'
                                                '[datasets.members]\nref = "http://h/members.zip"\n\n'
                                                '[datasets.stray]\nref = "http://h/links/l00009.bin"\n')
            self.assertEqual(recheck.layout(project, sources),
                             {"data/l00000/l00000.bin": "1" * 64, "data/second/m00001.bin": "2" * 64,
                              "data/members/m00000.bin": "3" * 64, "data/members/m00001.bin": "4" * 64,
                              "data/stray/l00009.bin": None})


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
        self.assertEqual(folder, self.root / "fx/arm1")
        self.assertEqual(recheck.exclude_paths(folder / "exclude"), [f"data/p0000{index}/remove.bin" for index in range(3)])
        merged = (folder / "merge/data.toml").read_text()
        self.assertTrue(merged.startswith("<<<<<<< ours\n[datasets.o00000]") and "=======\n[datasets.t00000]" in merged, merged)
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
        served = recheck.expected(self.root, "http://127.0.0.1:9")
        # at this scale: 40,000 datasets make 4 files and 1,000 links the least, 3, as 10,000 members do
        self.assertEqual((len(served["links"]), len(served["members"])), (7, 3))
        specs = recheck.cases(self.root, "fetchloom", "/bin/fetchloom", folder, "http://127.0.0.1:9", served)
        self.assertEqual(sorted(specs), sorted(recheck.ROUNDS))
        work = self.root / "work/arm0"
        # the cases that download or place files say where, and which file goes where once they ran
        self.assertEqual({name: spec["outputs"]()[0].name for name, spec in specs.items() if "outputs" in spec},
                         dict(place="place", add_links="links", zip="zip", get="get"))
        specs["place"]["prep"]()
        self.assertEqual(sorted(specs["place"]["outputs"]()[1]), [f"data/m0000{index}/m0000{index}.bin" for index in range(4)])
        self.assertEqual(specs["get"]["outputs"]()[1], {"big.bin": served["big"]})
        specs["place"]["post"]()
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


class LinkTests(unittest.TestCase):
    def test_each_link_case_runs_every_arm_on_a_project_its_own_build_locked(self):
        prepared, ran, shaped = [], [], []

        def built(spec, seed):
            return dict(spec, key=f"{spec['profile']}-{spec['id']}", files=[], members=[])

        def prepare(case, binary, names):
            prepared.append((case["key"], Path(binary).name, names))
            return case

        def one_run(tool, case, workdir, out, number, warmup, timeout, *, cpus=None, run_id=None):
            ran.append((case["profile"], case["id"], number, warmup, tool.name, tool.prepared_as, Path(tool.fetchloom).name, run_id))
            workdir.mkdir(parents=True, exist_ok=True)

        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            binaries = {arm: root / "bin" / recheck.FOLDERS[arm] for arm in recheck.ARMS}
            with mock.patch.object(recheck.lab, "ROOT", root), mock.patch.object(recheck.lab, "profile", side_effect=shaped.append), \
                 mock.patch.object(recheck.fixtures, "build", side_effect=built), \
                 mock.patch.object(recheck.fixtures, "prepare_native", side_effect=prepare), \
                 mock.patch.object(recheck.run, "one_run", side_effect=one_run), contextlib.redirect_stderr(io.StringIO()):
                recheck.links(binaries, root / "out", seed=1729, rounds=2, cpus=None, timeout=60)
            self.assertFalse((root / "fs/ext4/work").exists())
        cases = [(profile, scenario) for profile in ("home", "poor") for scenario in ("s1", "s2r", "s7")]
        self.assertEqual(shaped, [step for profile, _ in cases for step in ("lan", profile)])
        self.assertEqual(prepared, [(f"{profile}-{scenario}", recheck.FOLDERS[arm], [f"fetchloom-{recheck.FOLDERS[arm]}"])
                                    for profile, scenario in cases for arm in recheck.ARMS])
        self.assertEqual(len(ran), 6 * 3 * 3)
        for profile, scenario in cases:
            runs = [entry for entry in ran if entry[:2] == (profile, scenario)]
            for number in range(3):
                self.assertEqual(sorted(entry[4] for entry in runs if entry[2] == number), sorted(recheck.ARMS))
            self.assertEqual([entry[3] for entry in runs], [entry[2] == 0 for entry in runs])
        # each arm syncs with its own binary, under its own name of one length
        self.assertEqual({entry[4:7] for entry in ran},
                         {(arm, f"fetchloom-{recheck.FOLDERS[arm]}", recheck.FOLDERS[arm]) for arm in recheck.ARMS})
        self.assertEqual([entry[7] for entry in ran], [f"{n:06}" for n in range(1, len(ran) + 1)])


class SettingTests(unittest.TestCase):
    def test_link_cases_need_root_and_the_lab_before_anything_is_made(self):
        with tempfile.TemporaryDirectory() as folder:
            root = Path(folder)
            argv = ["--baseline", "a", "--candidate", "b", "--root", str(root / "recheck"), "--links"]
            with mock.patch.object(recheck.lab.os, "geteuid", return_value=1000), self.assertRaisesRegex(SystemExit, "the lab needs root"):
                recheck.main(argv)
            with mock.patch.object(recheck.lab.os, "geteuid", return_value=0), mock.patch.object(recheck.lab, "ROOT", root / "lab"), \
                 contextlib.redirect_stderr(io.StringIO()) as said, self.assertRaises(SystemExit):
                recheck.main(argv)
            self.assertIn("--links needs the lab: run lab.py setup first", said.getvalue())
            with contextlib.redirect_stderr(io.StringIO()) as said, self.assertRaises(SystemExit):
                recheck.main(["--baseline", "a", "--candidate", "b", "--root", str(root / "recheck"), "--cases", ""])
            self.assertIn("choose at least one case, or --links", said.getvalue())
            self.assertFalse((root / "recheck").exists())

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

    def test_a_failed_run_fails_the_recheck_and_its_case_shows_no_ratio(self):
        baseline = build(self.root / "old")
        candidate = build(self.root / "new", BUILD + "if words == ['--version']:\n    sys.exit(4)\n")
        code, said, shown = self.recheck(baseline, candidate, "--cases", "version,help", "--rounds", "2")
        self.assertEqual(code, 1, said)
        # the measured rounds' failures, warmup aside
        self.assertIn("| version | candidate | 0 | 0/2 | failed | - | - | - | - |", shown)
        self.assertNotIn("failed", [line for line in shown.splitlines() if line.startswith("| help | candidate")][0])
        self.assertIn("recheck: version fetchloom+candidate round 0 failed: exit 4", said)
        self.assertIn("recheck: 3 of 18 runs failed, so their cases show no ratio", said)

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
