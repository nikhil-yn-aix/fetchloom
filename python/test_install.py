"""the shell installer against a private release served only on loopback."""

import functools
import hashlib
import http.server
import io
import os
import platform
import shutil
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class QuietHandler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, format, *args):
        pass


@unittest.skipUnless(sys.platform.startswith("linux"), "shell installer coverage runs on linux")
class LocalRelease(unittest.TestCase):
    def setUp(self):
        targets = {"x86_64": "x86_64", "amd64": "x86_64", "aarch64": "aarch64", "arm64": "aarch64"}
        arch = targets.get(platform.machine())
        if arch is None:
            self.skipTest("no release for this architecture")
        if not (shutil.which("curl") or shutil.which("wget")):
            self.skipTest("installer requires curl or wget")
        if not (shutil.which("sha256sum") or shutil.which("shasum")):
            self.skipTest("installer requires a SHA-256 command")
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.home = self.root / "home"
        self.release = self.root / "release"
        self.release.mkdir()
        self.bin = self.home / ".local/bin/fetchloom"
        self.bin.parent.mkdir(parents=True)
        self.bin.write_bytes(b"previous installation\n")
        self.profile = self.home / ".profile"
        self.profile.write_text("# existing profile\n", encoding="utf-8")
        self.target = f"{arch}-unknown-linux-musl"
        handler = functools.partial(QuietHandler, directory=str(self.release))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.url = f"http://127.0.0.1:{server.server_port}"
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(self.url + "/", timeout=5) as response:
            self.assertEqual(response.status, 200)

    def package(self, matching=True, exit_code=0):
        name = f"fetchloom-0.1.0-{self.target}"
        executable = (
            '#!/bin/sh\nprintf ran > "$HOME/executed"\n'
            "printf 'fetchloom 0.1.0\\n'\n"
            f"exit {exit_code}\n"
        ).encode()
        archive = self.release / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as packed:
            entry = tarfile.TarInfo(f"{name}/fetchloom")
            entry.mode = 0o755
            entry.size = len(executable)
            packed.addfile(entry, io.BytesIO(executable))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest() if matching else "0" * 64
        (self.release / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
        return executable

    def install(self, modify_path=False):
        env = {name: value for name, value in os.environ.items()
               if not name.upper().startswith("FETCHLOOM_") and not name.upper().endswith("_PROXY")}
        env.update(HOME=str(self.home), FETCHLOOM_DOWNLOAD_URL=self.url, NO_PROXY="127.0.0.1")
        if not modify_path:
            env["FETCHLOOM_NO_MODIFY_PATH"] = "1"
        return subprocess.run(["sh", str(ROOT / "install.sh"), "0.1.0"], env=env,
                              stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=30)



class InstallerChecks(LocalRelease):
    def test_matching_archive_is_installed(self):
        executable = self.package()
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.bin.read_bytes(), executable)
        self.assertEqual((self.home / "executed").read_text(), "ran")
        self.assertEqual(self.profile.read_text(), "# existing profile\n")
        self.assertIn("installed fetchloom 0.1.0", result.stdout)

    def test_wrong_digest_neither_executes_nor_replaces(self):
        self.package(matching=False)
        result = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("SHA-256", result.stderr)
        self.assertEqual(self.bin.read_bytes(), b"previous installation\n")
        self.assertFalse((self.home / "executed").exists())
        self.assertEqual(self.profile.read_text(), "# existing profile\n")

    def test_failed_version_check_preserves_the_installed_program(self):
        self.package(exit_code=19)
        result = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.bin.read_bytes(), b"previous installation\n")

    def test_profile_update_is_opt_in_for_this_test_and_idempotent(self):
        self.package()
        for _ in range(2):
            result = self.install(modify_path=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        profile = self.profile.read_text()
        self.assertTrue(profile.startswith("# existing profile\n"))
        self.assertEqual(profile.count('export PATH="$HOME/.local/bin:$PATH"'), 1)


class InstallerDestinationTests(LocalRelease):
    def test_directory_destination_is_refused_without_overwriting_its_contents(self):
        self.package()
        self.bin.unlink()
        self.bin.mkdir()
        member = self.bin / "fetchloom"
        member.write_bytes(b"existing data must stay\n")
        result = self.install()
        with self.subTest("exit status"):
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        with self.subTest("existing directory member"):
            self.assertEqual(member.read_bytes(), b"existing data must stay\n")
        with self.subTest("clear diagnostic"):
            self.assertIn("directory", result.stderr)
        self.assertFalse((self.home / "executed").exists())
        self.assertEqual(self.profile.read_text(), "# existing profile\n")

    def test_symlink_to_directory_is_refused_without_overwriting_its_target(self):
        self.package()
        self.bin.unlink()
        outside = self.root / "outside"
        outside.mkdir()
        member = outside / "fetchloom"
        member.write_bytes(b"existing data must stay\n")
        self.bin.symlink_to(outside, target_is_directory=True)
        result = self.install()
        with self.subTest("exit status"):
            self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        with self.subTest("file outside the install directory"):
            self.assertEqual(member.read_bytes(), b"existing data must stay\n")
        with self.subTest("clear diagnostic"):
            self.assertIn("directory", result.stderr)
        self.assertTrue(self.bin.is_symlink())
        self.assertFalse((self.home / "executed").exists())
        self.assertEqual(self.profile.read_text(), "# existing profile\n")
