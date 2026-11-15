"""release gates, checksums and provenance, using the step each workflow actually runs."""

import hashlib
import os
import shlex
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = [("publish.yml", ".whl", 6), ("pages.yml", ".deb", 2)]
REPOSITORY, TAG = "owner/project", "v0.1.0"
# a stand-in for gh that logs each command line and refuses the package GH_REFUSE names
GH = '#!/bin/sh\nprintf \'%s\\n\' "$*" >> "$GH_LOG"\n[ "${3##*/}" != "$GH_REFUSE" ]\n'


def step_script(workflow):
    """the run script of the workflow's step that downloads and checks the release's packages."""
    lines = (ROOT / ".github/workflows" / workflow).read_text(encoding="utf-8").splitlines()
    start = next(number for number, line in enumerate(lines) if line.startswith("      - name: download and check the "))
    body = next(number for number in range(start, len(lines)) if lines[number] == "        run: |") + 1
    script = []
    for line in lines[body:]:
        if line.strip() and not line.startswith(" " * 10):
            break
        script.append(line)
    return textwrap.dedent("\n".join(script)) + "\n"


@unittest.skipUnless(sys.platform.startswith("linux"), "runs the workflows' bash steps, which run on ubuntu")
class ReleaseChecks(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        tools = Path(folder.name)
        (tools / "gh").write_text(GH, encoding="utf-8")
        (tools / "gh").chmod(0o755)
        self.log = tools / "gh.log"
        self.env = dict(os.environ, PATH=os.pathsep.join([str(tools), os.environ.get("PATH", "")]),
                        GH_LOG=str(self.log), GH_REFUSE="", GITHUB_REPOSITORY=REPOSITORY, TAG=TAG)

    def run_check(self, workflow, suffix, count, change):
        """the workflow's step as github runs it, in a workspace whose dist/ already holds the
        release's downloads, which the stand-in gh leaves alone."""
        self.log.write_text("", encoding="utf-8")
        with tempfile.TemporaryDirectory() as folder:
            workspace = Path(folder)
            (workspace / "python").symlink_to(ROOT / "python", target_is_directory=True)
            assets = workspace / "dist"
            assets.mkdir()
            names = [f"fetchloom-{index}{suffix}" for index in range(count)]
            lines = []
            for name in names:
                payload = name.encode()
                (assets / name).write_bytes(payload)
                lines.append(f"{hashlib.sha256(payload).hexdigest()}  {name}")
            change(assets, names, lines)
            (assets / "SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="utf-8")
            return subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", step_script(workflow)],
                                  cwd=workspace, env=self.env, capture_output=True, text=True, timeout=60)

    def verified(self):
        """each package the step asked gh to verify, with the rest of that command line."""
        return [line.split(" ") for line in self.log.read_text(encoding="utf-8").splitlines()
                if line.startswith("attestation verify ")]

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

    def test_a_package_that_is_a_link_is_refused(self):
        def link(assets, names, lines):
            # the same bytes, so only the link itself is wrong
            target = assets.parent / "elsewhere"
            target.write_bytes((assets / names[0]).read_bytes())
            (assets / names[0]).unlink()
            (assets / names[0]).symlink_to(target)

        for workflow, suffix, count in WORKFLOWS:
            with self.subTest(workflow=workflow):
                result = self.run_check(workflow, suffix, count, link)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(f"fetchloom-0{suffix} is not a regular", result.stdout + result.stderr)
                self.assertEqual(self.verified(), [])

    def test_each_package_must_have_an_attestation_from_the_release_workflow(self):
        for workflow, suffix, count in WORKFLOWS:
            with self.subTest(workflow=workflow):
                result = self.run_check(workflow, suffix, count, lambda assets, names, lines: None)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual([words[:2] + [Path(words[2]).name] + words[3:] for words in self.verified()], [
                    ["attestation", "verify", f"fetchloom-{index}{suffix}", "--repo", REPOSITORY,
                     "--signer-workflow", f"{REPOSITORY}/.github/workflows/release.yml",
                     "--source-ref", f"refs/tags/{TAG}", "--deny-self-hosted-runners"]
                    for index in range(count)])

    def test_a_package_without_that_attestation_is_refused(self):
        for workflow, suffix, count in WORKFLOWS:
            with self.subTest(workflow=workflow):
                self.env["GH_REFUSE"] = f"fetchloom-1{suffix}"
                result = self.run_check(workflow, suffix, count, lambda assets, names, lines: None)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(Path(self.verified()[-1][2]).name, f"fetchloom-1{suffix}")


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

        allowed = tomllib.loads((ROOT / ".config/deny.toml").read_text(encoding="utf-8"))["licenses"]["allow"]
        accepted = tomllib.loads((ROOT / ".config/about.toml").read_text(encoding="utf-8"))["accepted"]
        self.assertEqual(sorted(accepted), sorted(allowed))


@unittest.skipUnless(shutil.which("sha256sum") and sys.platform != "win32", "runs the documented check with sh and sha256sum")
class ProvenanceDocumentation(unittest.TestCase):
    """the check releasing.md has the owner run on a draft, against a stand-in gh."""

    NAMES = ["fetchloom-0.1.0-x86_64-unknown-linux-musl.tar.gz", "fetchloom-0.1.0-x86_64-pc-windows-msvc.zip",
             "fetchloom_0.1.0_amd64.deb", "fetchloom-0.1.0-py3-none-win_amd64.whl", "fetchloom-0.1.0.cdx.json",
             "install.sh", "install.ps1"]

    def run_documented(self, refuse=""):
        text = (ROOT / "docs/dev/engineering/releasing.md").read_text(encoding="utf-8")
        blocks = [block.split("```", 1)[0] for block in text.split("```sh\n")[1:] if "gh attestation verify \"$file\"" in block]
        self.assertEqual(len(blocks), 1, "releasing.md must have one release provenance check")
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        root = Path(folder.name)
        (root / "check").mkdir()
        for name in self.NAMES:
            (root / "check" / name).write_bytes(name.encode())
        (root / "check/SHA256SUMS").write_text("".join(f"{hashlib.sha256(name.encode()).hexdigest()}  {name}\n"
                                                       for name in self.NAMES), encoding="utf-8")
        (root / "gh").write_text(GH, encoding="utf-8")
        (root / "gh").chmod(0o755)
        env = dict(os.environ, PATH=os.pathsep.join([folder.name, os.environ.get("PATH", "")]),
                   GH_LOG=str(root / "gh.log"), GH_REFUSE=refuse)
        result = subprocess.run(["sh", "-c", textwrap.dedent(blocks[0])], cwd=root, env=env,
                                capture_output=True, text=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout, (root / "gh.log").read_text(encoding="utf-8").splitlines()

    def test_every_file_of_the_draft_is_checked_against_the_release_workflow(self):
        printed, called = self.run_documented()
        self.assertEqual(called[0], "release download v0.1.0 --repo nikhil-yn-aix/fetchloom --dir check")
        self.assertEqual(sorted(called[1:]), sorted(
            f"attestation verify check/{name} --repo nikhil-yn-aix/fetchloom --source-ref refs/tags/v0.1.0 "
            "--signer-workflow nikhil-yn-aix/fetchloom/.github/workflows/release.yml --deny-self-hosted-runners"
            for name in self.NAMES))
        self.assertNotIn("not verified", printed)

    def test_a_file_without_that_attestation_is_named(self):
        printed, _ = self.run_documented(refuse="fetchloom_0.1.0_amd64.deb")
        self.assertIn("not verified: check/fetchloom_0.1.0_amd64.deb\n", printed)


class GateDocumentation(unittest.TestCase):
    def test_local_gate_lists_the_linux_release_and_python_checks(self):
        required = [
            "cargo clippy --release --locked -- -D warnings",
            "python3 -m unittest discover -s python",
            "python3 -m unittest discover -s bench",
        ]
        cases = [
            (".github/pull_request_template.md", "", None),
            ("docs/dev/engineering/README.md", "run the full gate:", "## Commands"),
            ("docs/dev/engineering/other-platforms.md", "## WSL gate", "## XFS loop mount"),
        ]
        for path, start, end in cases:
            text = (ROOT / path).read_text(encoding="utf-8")
            if start:
                text = text.split(start, 1)[1]
            if end:
                # a renamed heading would widen the span, so the bound heading must be there
                self.assertTrue(end in text, f"{path} lacks {end}")
                text = text.split(end, 1)[0]
            for command in required:
                with self.subTest(path=path, command=command):
                    self.assertTrue(command in text, f"{path} lacks {command}")
        ci = (ROOT / "docs/dev/engineering/ci.md").read_text(encoding="utf-8")
        self.assertTrue(required[0] in ci, "the CI table omits release clippy")
