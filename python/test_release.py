"""release checksum gates, using the command each workflow actually runs."""

import hashlib
import shlex
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = [("publish.yml", ".whl", 6), ("pages.yml", ".deb", 2)]


class ReleaseChecks(unittest.TestCase):
    def run_check(self, workflow, suffix, count, change):
        with tempfile.TemporaryDirectory() as folder:
            assets = Path(folder)
            names = [f"fetchloom-{index}{suffix}" for index in range(count)]
            lines = []
            for name in names:
                payload = name.encode()
                (assets / name).write_bytes(payload)
                lines.append(f"{hashlib.sha256(payload).hexdigest()}  {name}")
            change(assets, names, lines)
            (assets / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
            text = (ROOT / ".github/workflows" / workflow).read_text(encoding="utf-8")
            checks = [line.strip() for line in text.splitlines() if line.strip().startswith((
                "sha256sum --check --ignore-missing SHA256SUMS",
                "python3 python/check_release.py ",
            ))]
            self.assertEqual(len(checks), 1, "the workflow must have one asset checksum gate")
            command = shlex.split(checks[0])
            if command[0] == "python3":
                command[0] = sys.executable
                command[1] = str(ROOT / command[1])
                command[2] = str(assets)
            return subprocess.run(command, cwd=assets, capture_output=True, text=True, timeout=30)

    def check_all(self, change, success):
        for workflow, suffix, count in WORKFLOWS:
            with self.subTest(workflow=workflow):
                result = self.run_check(workflow, suffix, count, change)
                self.assertEqual(result.returncode == 0, success, result.stdout + result.stderr)

    def test_matching_assets_are_accepted(self):
        self.check_all(lambda assets, names, lines: None, True)

    def test_changed_bytes_are_refused(self):
        self.check_all(lambda assets, names, lines: (assets / names[0]).write_bytes(b"changed"), False)

    def test_every_download_requires_a_checksum(self):
        self.check_all(lambda assets, names, lines: lines.pop(), False)

    def test_every_listed_asset_must_be_downloaded(self):
        self.check_all(lambda assets, names, lines: (assets / names[-1]).unlink(), False)

    def test_duplicate_checksum_entries_are_refused(self):
        self.check_all(lambda assets, names, lines: lines.append(lines[0]), False)

    def test_empty_manifest_is_refused(self):
        self.check_all(lambda assets, names, lines: lines.clear(), False)

    def test_incomplete_platform_set_is_refused(self):
        def remove(assets, names, lines):
            (assets / names[-1]).unlink()
            lines.pop()
        self.check_all(remove, False)

    def test_other_release_artifact_types_need_not_be_downloaded(self):
        self.check_all(lambda assets, names, lines: lines.append("0" * 64 + "  fetchloom-linux.tar.gz"), True)

    def test_malformed_checksum_rows_are_refused(self):
        self.check_all(lambda assets, names, lines: lines.append("not a checksum"), False)
