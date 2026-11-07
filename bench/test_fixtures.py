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


if __name__ == "__main__":
    unittest.main()
