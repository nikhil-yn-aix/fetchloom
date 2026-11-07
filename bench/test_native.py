import os
from pathlib import Path
import stat
import tempfile
import unittest
import urllib.request
import zipfile

import native


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


if __name__ == "__main__":
    unittest.main()
