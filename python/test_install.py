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
# the programs install.sh runs. a test's PATH holds their folders and nothing else, so a
# program named fl elsewhere on this machine cannot change what the installer sees
TOOLS = ["sh", "curl", "wget", "sha256sum", "shasum", "tar", "gzip", "grep", "head", "cut",
         "uname", "mkdir", "mv", "rm", "ln"]


def tool_folders():
    folders = []
    for tool in TOOLS:
        found = shutil.which(tool)
        if found and os.path.dirname(found) not in folders:
            folders.append(os.path.dirname(found))
    return folders


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
        self.tools = tool_folders()
        handler = functools.partial(QuietHandler, directory=str(self.release))
        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
        # shutdown waits out one poll, which is half a second by default
        thread = threading.Thread(
            target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
        )
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        self.url = f"http://127.0.0.1:{server.server_port}"
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(self.url + "/", timeout=5) as response:
            self.assertEqual(response.status, 200)

    def package(self, matching=True, exit_code=0, build=""):
        name = f"fetchloom-0.1.0-{self.target}"
        executable = (
            '#!/bin/sh\nprintf ran > "$HOME/executed"\n'
            "printf 'fetchloom 0.1.0\\n'\n"
            f"exit {exit_code}\n# {build}\n"
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

    def install(self, modify_path=False, path=None):
        env = {name: value for name, value in os.environ.items()
               if not name.upper().startswith("FETCHLOOM_") and not name.upper().endswith("_PROXY")}
        env.update(HOME=str(self.home), FETCHLOOM_DOWNLOAD_URL=self.url, NO_PROXY="127.0.0.1",
                   PATH=os.pathsep.join(self.tools if path is None else path))
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

    def test_a_wget_that_cannot_hold_to_https_is_refused_before_any_download(self):
        self.package()
        tools = self.root / "tools"
        tools.mkdir()
        for tool in TOOLS:
            found = shutil.which(tool)
            if found and tool not in ("curl", "wget"):
                (tools / tool).symlink_to(found)
        # busybox's wget, which knows neither --https-only nor --secure-protocol
        wget = tools / "wget"
        wget.write_text('#!/bin/sh\n[ "$1" = --help ] && { echo "Usage: wget [-cq] [-O FILE] URL"; exit 0; }\n'
                        'printf ran > "$HOME/downloaded"\nexit 1\n', encoding="utf-8")
        wget.chmod(0o755)
        result = self.install(path=[str(tools)])
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("install curl", result.stderr)
        self.assertFalse((self.home / "downloaded").exists())
        self.assertEqual(self.bin.read_bytes(), b"previous installation\n")


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


class ShortNameTests(LocalRelease):
    """fl is a link to fetchloom beside it, made only where it hides no other fl."""

    def setUp(self):
        super().setUp()
        for folder in self.tools:
            if os.access(os.path.join(folder, "fl"), os.X_OK):
                self.skipTest(f"{folder} holds a program named fl, and the installer needs that folder")
        self.link = self.bin.parent / "fl"

    def other_fl(self):
        folder = self.root / "elsewhere"
        folder.mkdir()
        program = folder / "fl"
        program.write_bytes(b"#!/bin/sh\necho another fl\n")
        program.chmod(0o755)
        return program

    def test_fl_is_a_link_to_fetchloom_beside_it(self):
        self.package()
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(os.readlink(self.link), "fetchloom")
        ran = subprocess.run([str(self.link)], env={"HOME": str(self.home)},
                             capture_output=True, text=True, timeout=30)
        self.assertEqual(ran.stdout, "fetchloom 0.1.0\n")
        self.assertIn("fl runs fetchloom too\n", result.stdout)

    def test_a_reinstall_keeps_the_link_and_puts_back_a_missing_one(self):
        on_path = [str(self.bin.parent), *self.tools]
        self.package(build="first")
        self.assertEqual(self.install(path=on_path).returncode, 0)
        upgrade = self.package(build="second")
        for case in ["kept", "put back"]:
            with self.subTest(case):
                if case == "put back":
                    self.link.unlink()
                result = self.install(path=on_path)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(os.readlink(self.link), "fetchloom")
                self.assertEqual(self.link.read_bytes(), upgrade)
                self.assertIn("fl runs fetchloom too\n", result.stdout)

    def test_an_fl_already_in_the_folder_stays_as_it_is(self):
        executable = self.package()
        elsewhere = self.other_fl()
        for case in ["file", "link elsewhere"]:
            with self.subTest(case):
                if case == "file":
                    self.link.write_bytes(b"someone else's fl\n")
                else:
                    self.link.unlink()
                    self.link.symlink_to(elsewhere)
                before = os.readlink(self.link) if self.link.is_symlink() else self.link.read_bytes()
                result = self.install()
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                after = os.readlink(self.link) if self.link.is_symlink() else self.link.read_bytes()
                self.assertEqual(after, before)
                self.assertEqual(self.bin.read_bytes(), executable)
                self.assertIn("left ~/.local/bin/fl alone: it is not a link to fetchloom. "
                              "to replace it with one: ln -sf fetchloom ~/.local/bin/fl\n", result.stdout)
                self.assertNotIn("fl runs fetchloom too", result.stdout)

    def test_another_fl_on_path_is_never_hidden(self):
        executable = self.package()
        other = self.other_fl()
        for case, path in [
            ("earlier on PATH", [str(other.parent), str(self.bin.parent), *self.tools]),
            ("later on PATH", [str(self.bin.parent), *self.tools, str(other.parent)]),
        ]:
            with self.subTest(case):
                result = self.install(path=path)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertFalse(os.path.lexists(self.link))
                self.assertEqual(self.bin.read_bytes(), executable)
                self.assertIn(f"left fl alone: it runs {other} here. to use fl for fetchloom in your "
                              "shell, add alias fl=fetchloom to its startup file\n", result.stdout)
