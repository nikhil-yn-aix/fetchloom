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


class ReleaseChecksums(unittest.TestCase):
    def test_sha256sums_lists_every_kind_of_release_file(self):
        import fnmatch

        text = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        lines = [line.strip() for line in text.splitlines() if line.strip().startswith("sha256sum -- ")]
        self.assertEqual(len(lines), 1, "release.yml must write SHA256SUMS once")
        operands = shlex.split(lines[0].split(">", 1)[0])[2:]
        for name in ["fetchloom-0.1.0-x86_64-pc-windows-msvc.zip", "fetchloom-0.1.0-aarch64-apple-darwin.tar.gz",
                     "fetchloom_0.1.0_amd64.deb", "fetchloom-0.1.0-py3-none-win_amd64.whl",
                     "fetchloom-0.1.0.cdx.json", "install.sh", "install.ps1"]:
            with self.subTest(name=name):
                self.assertTrue(any(fnmatch.fnmatchcase(name, operand) for operand in operands), f"SHA256SUMS leaves out {name}")


class ThirdPartyLicenses(unittest.TestCase):
    def test_the_notices_accept_exactly_the_licenses_deny_allows(self):
        import tomllib

        allowed = tomllib.loads((ROOT / "deny.toml").read_text(encoding="utf-8"))["licenses"]["allow"]
        accepted = tomllib.loads((ROOT / "about.toml").read_text(encoding="utf-8"))["accepted"]
        self.assertEqual(sorted(accepted), sorted(allowed))


class GateDocumentation(unittest.TestCase):
    def test_local_gate_lists_the_linux_release_and_python_checks(self):
        required = [
            "cargo clippy --release --locked -- -D warnings",
            "python3 -m unittest discover -s python",
            "python3 -m unittest discover -s bench",
        ]
        cases = [
            (".github/pull_request_template.md", "", None),
            ("docs/dev/engineering/README.md", "run the full gate:", "## commands"),
            ("docs/dev/engineering/building-and-testing.md", "## WSL gate", "## xfs loop mount"),
        ]
        for path, start, end in cases:
            text = (ROOT / path).read_text(encoding="utf-8")
            if start:
                text = text.split(start, 1)[1]
            if end:
                text = text.split(end, 1)[0]
            for command in required:
                with self.subTest(path=path, command=command):
                    self.assertTrue(command in text, f"{path} lacks {command}")
        ci = (ROOT / "docs/dev/engineering/ci.md").read_text(encoding="utf-8")
        self.assertTrue(required[0] in ci, "the CI table omits release clippy")
