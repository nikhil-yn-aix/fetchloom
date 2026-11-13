import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import unittest
from unittest import mock
import urllib.request
import zipfile

import native

# stands in for a fetchloom build: lock writes data.lock, every command keeps a file in the cache,
# and every command succeeds
BUILD = ("import os, sys\nfrom pathlib import Path\nif 'lock' in sys.argv:\n    Path('data.lock').write_text('version = 1\\n')\n"
         "cache = Path(os.environ['FETCHLOOM_CACHE'])\ncache.mkdir(parents=True, exist_ok=True)\n(cache / 'objects').write_text('')\n")


class FixtureTests(unittest.TestCase):
    def test_files_are_deterministic_and_rebuilt_only_when_the_spec_changes(self):
        with tempfile.TemporaryDirectory() as folder:
            www = Path(folder)
            spec = dict(count=3, size=5000, archive=False)
            names = native.build_fixture(www, "small", spec, 7)
            self.assertEqual(names, ["file-00000.bin", "file-00001.bin", "file-00002.bin"])
            first = (www / "small" / names[0]).read_bytes()
            self.assertEqual(len(first), 5000)
            self.assertEqual(first, native.content(7, f"small/{names[0]}/0", 5000))
            marker = (www / "small" / ".complete").stat().st_mtime_ns
            native.build_fixture(www, "small", spec, 7)
            self.assertEqual((www / "small" / ".complete").stat().st_mtime_ns, marker)
            native.build_fixture(www, "small", dict(spec, count=2), 7)
            self.assertFalse((www / "small" / names[2]).exists())

    def test_an_archive_scenario_serves_one_stored_zip(self):
        with tempfile.TemporaryDirectory() as folder:
            names = native.build_fixture(Path(folder), "zip", dict(count=4, size=16, archive=True), 1)
            self.assertEqual(names, ["members.zip"])
            with zipfile.ZipFile(Path(folder) / "zip" / "members.zip") as archive:
                infos = archive.infolist()
            self.assertEqual([info.filename for info in infos], [f"file-{i:05}.bin" for i in range(4)])
            self.assertTrue(all(info.compress_type == zipfile.ZIP_STORED for info in infos))

    def test_the_manifest_names_one_dataset_per_served_file(self):
        text = native.manifest("http://127.0.0.1:9", "small", ["a.bin", "b.bin"])
        self.assertEqual(text, '[datasets.f00000]\nref = "http://127.0.0.1:9/small/a.bin"\n\n'
                               '[datasets.f00001]\nref = "http://127.0.0.1:9/small/b.bin"\n\n')


class ServerTests(unittest.TestCase):
    def test_the_local_server_returns_the_fixture_bytes(self):
        with tempfile.TemporaryDirectory() as folder:
            www = Path(folder)
            native.build_fixture(www, "small", dict(count=1, size=100, archive=False), 3)
            server = native.serve(www)
            try:
                url = f"http://127.0.0.1:{server.server_address[1]}/small/file-00000.bin"
                with urllib.request.urlopen(url) as response:
                    self.assertEqual(response.read(), (www / "small" / "file-00000.bin").read_bytes())
            finally:
                server.shutdown()
                server.server_close()


class SummaryTests(unittest.TestCase):
    def test_ratios_pair_rounds_and_name_a_clear_win(self):
        result = native.summarize([2.0, 2.2, 1.8, 2.1, 1.9], [1.0, 1.1, 0.9, 1.05, 0.95], "seed")
        self.assertAlmostEqual(result["ratio"], 0.5)
        self.assertEqual(result["verdict"], "candidate faster")
        self.assertEqual(result["pairs"], 5)

    def test_equal_builds_show_no_clear_difference(self):
        result = native.summarize([1.0, 1.1, 0.9, 1.0, 1.05], [1.05, 1.0, 0.95, 1.0, 1.1], "seed")
        self.assertEqual(result["verdict"], "no clear difference")

    def test_two_rounds_are_too_few(self):
        self.assertEqual(native.summarize([1, 1], [2, 2], "seed")["verdict"], "too few rounds")


class RemoveTests(unittest.TestCase):
    def test_read_only_files_are_removed(self):
        with tempfile.TemporaryDirectory() as folder:
            tree = Path(folder) / "project" / "data"
            tree.mkdir(parents=True)
            placed = tree / "file.bin"
            placed.write_bytes(b"x")
            os.chmod(placed, stat.S_IREAD)
            native.remove(Path(folder) / "project")
            self.assertFalse((Path(folder) / "project").exists())


def build(path, program=BUILD):
    path.write_text(f"#!{sys.executable}\n{program}")
    path.chmod(0o755)
    return path


class ProbeTests(unittest.TestCase):
    def test_a_probe_gives_what_a_command_said_or_nothing(self):
        self.assertEqual(native.probe([sys.executable, "-c", "print(' ntfs ')"]), "ntfs")
        self.assertIsNone(native.probe([sys.executable, "-c", "raise SystemExit(1)"]))
        self.assertIsNone(native.probe(["no-such-probe-program"]))

    def test_each_system_names_the_filesystem_under_the_root(self):
        volume = "Volume Name : \nVolume Serial Number : 0x1234abcd\nMax Component Length : 255\nFile System Name : ReFS\n"
        said = {"volumeinfo": volume, "devdrv": "This is a trusted developer volume."}
        with mock.patch.object(native.platform, "system", return_value="Windows"), \
             mock.patch.object(native, "probe", side_effect=lambda argv: said[argv[2] if argv[1] == "fsinfo" else argv[1]]):
            self.assertEqual(native.filesystem(Path("D:/bench")), dict(type="ReFS", dev_drive="This is a trusted developer volume."))
        mounts = ("/dev/disk3s1s1 on / (apfs, sealed, local, read-only, journaled)\n"
                  "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled, nobrowse)\n"
                  "map auto_home on /System/Volumes/Data/home (autofs, automounted, nobrowse)\n"
                  "/dev/disk5s1 on /Volumes/Scratch (exfat, local, nodev, nosuid, noowners)\n")
        with mock.patch.object(native.platform, "system", return_value="Darwin"), mock.patch.object(native, "probe", return_value=mounts):
            self.assertEqual(native.filesystem(Path("/System/Volumes/Data/Users/fl/bench")), dict(type="apfs", mount="/System/Volumes/Data"))
            self.assertEqual(native.filesystem(Path("/Volumes/Scratch/bench")), dict(type="exfat", mount="/Volumes/Scratch"))
        with mock.patch.object(native.platform, "system", return_value="Linux"), \
             mock.patch.object(native, "probe", return_value="ext2/ext3") as probe:
            self.assertEqual(native.filesystem(Path("/srv/bench")), dict(type="ext2/ext3"))
        probe.assert_called_once_with(["stat", "-f", "-c", "%T", "/srv/bench"])

    def test_defender_is_read_on_windows_alone(self):
        with mock.patch.object(native.platform, "system", return_value="Linux"):
            self.assertIsNone(native.defender())
        with mock.patch.object(native.platform, "system", return_value="Windows"), mock.patch.object(native, "probe", return_value="True"):
            self.assertEqual(native.defender(), dict(real_time_protection="True"))


@unittest.skipIf(os.name == "nt", "stands in for fetchloom with a python script")
class RoundTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)

    def test_a_failed_command_says_where_it_ran_and_what_it_said(self):
        failing = build(self.root / "failing", "import sys\nprint('no route to host', file=sys.stderr)\nsys.exit(3)\n")
        with self.assertRaises(RuntimeError) as raised:
            native.run([failing, "-q", "sync"], self.root, dict(os.environ))
        self.assertEqual(str(raised.exception), f"{failing} -q sync in {self.root} exited 3: no route to host")

    def test_the_environment_names_the_cache_and_leaves_colour_to_fetchloom(self):
        with mock.patch.dict(os.environ, {"NO_COLOR": "1", "FETCHLOOM_CACHE": "/home/me/.cache", "LANG": "C.UTF-8"}):
            env = native.environment(self.root / "cache")
        self.assertEqual((env["FETCHLOOM_CACHE"], env["LANG"], "NO_COLOR" in env), (str(self.root / "cache"), "C.UTF-8", False))

    def test_each_build_locks_once_and_each_round_starts_from_an_empty_cache(self):
        runs = self.root / "runs.txt"
        counted = build(self.root / "fetchloom", BUILD + f"with open({str(runs)!r}, 'a') as runs:\n    runs.write(' '.join(sys.argv[1:]) + '\\n')\n")
        template = native.prepare(counted, "baseline", "small", "[datasets.f00000]\n", self.root / "work")
        self.assertEqual(native.prepare(counted, "baseline", "small", "[datasets.f00000]\n", self.root / "work"), template)
        self.assertEqual(runs.read_text(), "-q --yes lock\n")
        self.assertEqual(sorted(path.name for path in template.iterdir()), ["data.lock", "data.toml"])
        self.assertEqual(list((self.root / "work/lock-cache").iterdir()), [])
        times = native.measure_round(counted, template, self.root / "work/run")
        self.assertEqual(list(times), ["cold", "warm", "settled", "path"])
        self.assertEqual(runs.read_text().splitlines()[1:], ["-q --yes sync", "-q --yes sync", "-q --yes sync", "-q path f00000"])
        self.assertEqual(list((self.root / "work/run").iterdir()), [])

    def test_the_command_compares_two_builds_round_by_round(self):
        baseline, candidate = build(self.root / "baseline"), build(self.root / "candidate", BUILD + "# another build\n")
        said, shown = io.StringIO(), io.StringIO()
        with contextlib.redirect_stderr(said), contextlib.redirect_stdout(shown):
            code = native.main(["--baseline", str(baseline), "--candidate", str(candidate), "--root", str(self.root / "bench"),
                                "--scenarios", "large", "--large-mib", "1", "--rounds", "3"])
        self.assertEqual(code, 0)
        self.assertEqual(said.getvalue().splitlines(), [f"large: round {number} of 3 done" for number in (1, 2, 3)])
        report = json.loads((self.root / "bench/results/native.json").read_text())
        self.assertEqual(sorted(report["summary"]), ["large/cold", "large/path", "large/settled", "large/warm"])
        self.assertTrue(all(len(sides["baseline"]) == len(sides["candidate"]) == 3 for sides in report["seconds"].values()))
        self.assertEqual(report["binaries"]["candidate"], dict(path=str(candidate), sha256=hashlib.sha256(candidate.read_bytes()).hexdigest()))
        self.assertEqual(report["settings"], dict(scenarios=["large"], rounds=3, seed=1729,
                                                  sizes={"large": dict(count=1, size=1 << 20, archive=False)}))
        self.assertEqual((self.root / "bench/www/large/file-00000.bin").stat().st_size, 1 << 20)
        table = (self.root / "bench/results/native.md").read_text()
        self.assertEqual(shown.getvalue(), table)
        self.assertEqual(table.splitlines()[:2], ["scenario | phase | baseline s | candidate s | candidate/baseline | 95% interval | verdict",
                                                  "--- | --- | ---: | ---: | ---: | --- | ---"])

    def test_the_command_refuses_unknown_scenarios_and_sizes_that_are_not_positive(self):
        for given in (["--scenarios", "huge"], ["--rounds", "0"], ["--large-mib", "0"]):
            with self.subTest(given), contextlib.redirect_stderr(io.StringIO()) as said, self.assertRaises(SystemExit):
                native.main(["--baseline", "a", "--candidate", "b", "--root", str(self.root), *given])
            self.assertIn("scenarios are small, zip, large; rounds and --large-mib must be positive", said.getvalue())


class TableTests(unittest.TestCase):
    def test_a_phase_without_pairs_shows_dashes(self):
        summary = {"small/cold": dict(baseline_median=2, candidate_median=1, ratio=.5, interval=[.4, .6], verdict="candidate faster"),
                   "zip/path": dict(baseline_median=1, candidate_median=1, ratio=None, interval=None, verdict="too few rounds")}
        self.assertEqual(native.table(summary).splitlines()[2:],
                         ["small | cold | 2.000 | 1.000 | 0.500 | [0.400, 0.600] | candidate faster",
                          "zip | path | 1.000 | 1.000 | - | - | too few rounds"])


if __name__ == "__main__":
    unittest.main()
