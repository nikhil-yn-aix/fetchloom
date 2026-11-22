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
        [spec] = fixtures.case_specs(["home"], ["s8"], True)
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
        # the dropped link breaks into the same file, at the same size
        [dropped] = fixtures.case_specs(["home"], ["s10"], True)
        self.assertEqual((dropped["base"], dropped["size"]), ("s1", resume["size"]))
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            self.assertEqual(fixtures.build(dict(dropped, size=8), 17)["key"], built["key"])

    def test_the_zip_holds_each_file_stored_and_dated_alike_so_its_bytes_repeat(self):
        import zipfile
        [spec] = fixtures.case_specs(["home"], ["s4"], True)
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


class NextVersionTests(unittest.TestCase):
    def test_the_next_version_changes_one_file_in_ten_and_keeps_the_rest_byte_for_byte(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            case = fixtures.build(dict(id="s12", base="s12", profile="home", size=3000, count=20, quick=False), 7)
            previous, files = case["previous"], case["files"]
            self.assertEqual([item["name"] for item in previous], [item["name"] for item in files])
            changed = [item["name"] for item, before in zip(files, previous) if item["sha256"] != before["sha256"]]
            self.assertEqual(changed, ["file-00000.bin", "file-00010.bin"])
            self.assertEqual((files[3]["url"], previous[3]["url"]), (f"https://lab.flb/{case['key']}/next/file-00003.bin",
                                                                     f"https://lab.flb/{case['key']}/previous/file-00003.bin"))
            for item in files + previous:
                self.assertEqual(fixtures.sha256(fixtures.served_file(case, item)), item["sha256"])
                self.assertEqual(item["size"], 3000)


class RedirectTests(unittest.TestCase):
    def test_each_file_of_the_many_files_case_goes_through_the_redirect(self):
        with tempfile.TemporaryDirectory() as folder, mock.patch.object(lab, "ROOT", Path(folder)):
            many = fixtures.build(dict(id="s2r", base="s2r", profile="home", quick=True, count=2, size=8), 17)
            large = fixtures.build(dict(id="s1", base="s1", profile="home", quick=True, count=1, size=8), 17)
        self.assertEqual([item["url"] for item in many["files"]],
                         [f"https://lab.flb/r/{many['key']}/file-0000{index}.bin" for index in range(2)])
        self.assertEqual(large["files"][0]["url"], f"https://lab.flb/{large['key']}/file-00000.bin")
        # the storage host serves each body at the path without /r
        self.assertEqual([fixtures.served(item["url"]) for item in many["files"]],
                         [f"/{many['key']}/file-0000{index}.bin" for index in range(2)])


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

    def test_dvc_pulls_a_redirected_case_through_the_redirect_too(self):
        with mock.patch.object(lab, "ROOT", self.root), mock.patch.object(lab, "run_group") as run:
            fixtures.prepare_native(dict(self.case, base="s2r"), self.root / "fetchloom", ["dvc"])
        self.assertIn(["dvc", "remote", "add", "-d", "lab", "https://lab.flb/r/dvc/case"], [call.args[0] for call in run.call_args_list])

    def test_each_version_of_the_next_version_case_is_prepared_as_its_tool_keeps_versions(self):
        server = self.root / "www" / "case"
        previous = [dict(item, url=item["url"].replace("/case/", "/case/previous/")) for item in self.case["files"]]
        files = [dict(previous[0], url=previous[0]["url"].replace("previous", "next")),
                 dict(previous[1], sha256="1" * 64, url=previous[1]["url"].replace("previous", "next"))]
        for version, items in (("previous", previous), ("next", files)):
            (server / version).mkdir()
            for item in items:
                (server / version / item["name"]).write_bytes(b"x")
        case = dict(self.case, id="s12", base="s12", files=files, previous=previous)
        with mock.patch.object(lab, "ROOT", self.root), mock.patch.object(lab, "run_group") as run:
            for tool in ("fetchloom", "dvc", "datalad"):
                if tool == "dvc":
                    # dvc add writes the record each version's commit keeps
                    run.side_effect = lambda argv, cwd=None, **kwargs: (Path(cwd) / "data.dvc").write_text(
                        "".join(sorted(path.read_text() for path in (Path(cwd) / "data").iterdir()))) if argv[:2] == ["dvc", "add"] else None
                else:
                    run.side_effect = None
                (self.root / "fetchloom").write_bytes(b"build")
                fixtures.prepare_native(dict(case), self.root / "fetchloom", [tool])
        calls = [(call.args[0], call.kwargs.get("cwd")) for call in run.call_args_list]
        prepared = self.root / "prepared/case"
        # fetchloom locks the next version, and the version before in a folder of its own
        self.assertEqual([cwd for argv, cwd in calls if argv[-1] == "lock"], [prepared / "fetchloom", prepared / "fetchloom/previous"])
        self.assertIn("/case/previous/b.bin", (prepared / "fetchloom/previous/data.toml").read_text())
        self.assertIn("/case/next/b.bin", (prepared / "fetchloom/data.toml").read_text())
        # dvc pushes both versions and commits each one's record under a tag of its name
        dvc = [argv for argv, cwd in calls if cwd == prepared / "dvc"]
        self.assertEqual([argv for argv in dvc if argv[0] == "git" and argv[1] in ("commit", "tag")],
                         [["git", "commit", "-q", "-m", "previous"], ["git", "tag", "previous"],
                          ["git", "commit", "-q", "-m", "next"], ["git", "tag", "next"]])
        self.assertEqual(dvc.count(["dvc", "push", "-r", "stage"]), 2)
        # datalad replaces only the file that changed
        datalad = [call for call in run.call_args_list if call.kwargs.get("cwd") == prepared / "datalad"]
        added = [call.kwargs["input"] for call in datalad if call.args[0][:3] == ["git", "annex", "addurl"]]
        self.assertEqual(added, ["https://lab.flb/case/previous/a.bin data/a.bin\nhttps://lab.flb/case/previous/b.bin data/b.bin\n",
                                 "https://lab.flb/case/next/b.bin data/b.bin\n"])
        self.assertEqual([call.args[0] for call in datalad if call.args[0][:2] in (["git", "tag"], ["git", "rm"])],
                         [["git", "tag", "previous"], ["git", "rm", "-q", "data/b.bin"], ["git", "tag", "next"]])

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
