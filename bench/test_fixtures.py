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


if __name__ == "__main__":
    unittest.main()
