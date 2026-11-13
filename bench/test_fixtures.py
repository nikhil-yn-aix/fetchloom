import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import fixtures
import lab


class CacheIdentityTests(unittest.TestCase):
    def test_existing_manifest_matches_the_requested_dimensions(self):
        spec = dict(id="s2", base="s2", profile="lan", quick=True, count=1, size=8)
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            first = fixtures.build(spec, 17)
            second = fixtures.build(dict(spec, count=2, size=16), 17)
            self.assertEqual(len(second["files"]), 2)
            self.assertEqual([item["size"] for item in second["files"]], [16, 16])
            self.assertEqual([Path(second["server_dir"], item["name"]).stat().st_size for item in second["files"]], [16, 16])
            again = fixtures.build(dict(spec, count=2, size=16), 17)
            self.assertEqual(second, again)
            self.assertNotEqual(first["files"][0]["sha256"], second["files"][0]["sha256"])

    def test_prepared_project_matches_executable_bytes(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            root = Path(folder)
            executable = root / "client"
            case = dict(key="case", files=[])
            for version in ("first", "second"):
                executable.write_text(f"#!{sys.executable}\nfrom pathlib import Path\nPath('data.lock').write_text({version!r})\n")
                executable.chmod(0o755)
                prepared = fixtures.prepare_native(case, executable, ["fetchloom"])
                self.assertEqual((Path(prepared["prepared"]) / "fetchloom" / "data.lock").read_text(), version)

    def test_malformed_manifest_is_not_reused(self):
        spec = dict(id="s2", base="s2", profile="lan", quick=True, count=1, size=8)
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            first = fixtures.build(spec, 17)
            manifest = Path(first["server_dir"]) / "manifest.json"
            manifest.write_text("{")
            self.assertEqual(fixtures.build(spec, 17)["files"], first["files"])

    def test_the_compressed_archive_holds_csv_tables_that_shrink_and_repeat(self):
        import hashlib
        import tarfile
        [spec] = fixtures.case_specs(["lan"], ["s8"], True)
        archives = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
                case = fixtures.build(spec, 17)
                [archive] = case["files"]
                with tarfile.open(Path(case["server_dir"]) / archive["name"], "r:gz") as tar:
                    members = {info.name: tar.extractfile(info).read() for info in tar.getmembers()}
            archives.append(archive)
        self.assertEqual(archive["name"], "members.tar.gz")
        self.assertEqual(len(members), spec["count"])
        self.assertEqual({item["name"]: item["sha256"] for item in case["members"]},
                         {name: hashlib.sha256(data).hexdigest() for name, data in members.items()})
        self.assertTrue(all(data.startswith(b"id,date,site,") for data in members.values()))
        self.assertLess(archive["size"], sum(map(len, members.values())) / 2)
        self.assertEqual(archives[0]["sha256"], archives[1]["sha256"])


class CaseTests(unittest.TestCase):
    def test_a_smoke_run_keeps_a_large_enough_file_to_interrupt(self):
        [resume] = fixtures.case_specs(["home"], ["s7"], True)
        [large] = fixtures.case_specs(["home"], ["s1"], True)
        # at 100 mbit/s, 2 MiB would be done long before the stop at 10 s
        self.assertEqual((resume["base"], resume["size"], large["size"]), ("s1", 256 * 1024 * 1024, 2 * 1024 * 1024))
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            built = fixtures.build(dict(resume, size=8), 17)
        self.assertEqual(built["key"], "17-quick-home-s1-interrupt")

    def test_the_zip_holds_each_file_stored_and_dated_alike_so_its_bytes_repeat(self):
        import zipfile
        [spec] = fixtures.case_specs(["lan"], ["s4"], True)
        spec = dict(spec, count=3, size=16)
        built = []
        for _ in range(2):
            with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
                case = fixtures.build(spec, 17)
                [archive] = case["files"]
                with zipfile.ZipFile(Path(case["server_dir"]) / archive["name"]) as packed:
                    entries = [(info.filename, info.compress_type, info.date_time, info.external_attr >> 16)
                               for info in packed.infolist()]
            built.append(archive)
        self.assertEqual(archive["name"], "members.zip")
        self.assertEqual(entries, [(f"file-0000{index}.bin", zipfile.ZIP_STORED, (2020, 1, 1, 0, 0, 0), 0o644) for index in range(3)])
        self.assertEqual([item["name"] for item in case["members"]], [name for name, *_ in entries])
        self.assertEqual(built[0]["sha256"], built[1]["sha256"])


class PreparationTests(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        server = self.root / "www" / "case"
        server.mkdir(parents=True)
        files = []
        for name in ["a.bin", "b.bin"]:
            (server / name).write_bytes(name.encode())
            files.append(dict(name=name, sha256=fixtures.sha256(server / name), size=5, url=f"https://lab.flb/case/{name}"))
        self.case = dict(key="case", files=files, members=[], server_dir=str(server), fixture_signature={})

    def prepare(self, tool):
        with mock.patch.object(lab, "ROOT", self.root), mock.patch.object(lab, "run_group") as run:
            fixtures.prepare_native(dict(self.case), self.root / "fetchloom", [tool])
        return run

    def test_a_prepared_project_is_kept_while_its_inputs_and_harness_stay(self):
        (self.root / "fetchloom").write_bytes(b"build")
        self.assertTrue(self.prepare("fetchloom").called)
        self.assertFalse(self.prepare("fetchloom").called)
        (self.root / "fetchloom").write_bytes(b"another build")
        self.assertTrue(self.prepare("fetchloom").called)

    def test_a_tool_with_nothing_to_prepare_is_only_marked_ready(self):
        self.assertFalse(self.prepare("curl").called)
        target = self.root / "prepared/case/curl"
        self.assertEqual([path.name for path in target.iterdir()], [".ready"])
        self.assertEqual(json.loads((target / ".ready").read_text())["tool"], "curl")

    def test_dvc_and_datalad_must_download_what_they_were_prepared_with(self):
        dvc = [call.args[0] for call in self.prepare("dvc").call_args_list]
        self.assertLess(dvc.index(["dvc", "push", "-r", "stage"]), dvc.index(["dvc", "remote", "add", "-d", "lab", "https://lab.flb/dvc/case"]))
        self.assertIn(["dvc", "remote", "modify", "lab", "verify", "true"], dvc)
        # the copy dvc pushed is gone, so a measured pull fetches every file
        self.assertFalse((self.root / "prepared/case/dvc/data").exists())
        datalad = self.prepare("datalad").call_args_list
        addurl = next(call for call in datalad if call.args[0][:3] == ["git", "annex", "addurl"])
        self.assertEqual(addurl.kwargs["input"], "https://lab.flb/case/a.bin data/a.bin\nhttps://lab.flb/case/b.bin data/b.bin\n")
        self.assertEqual(datalad[-1].args[0], ["git", "annex", "drop", "--force", "--all"])
        self.assertFalse(self.prepare("dvc").called)


if __name__ == "__main__":
    unittest.main()
