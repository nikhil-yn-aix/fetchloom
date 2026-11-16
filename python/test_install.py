"""the installers against a private release served only on loopback."""

import functools
import hashlib
import http.server
import io
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import unittest
import urllib.request
import zipfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
REFUSED = "FETCHLOOM_DOWNLOAD_URL must be https, or plain http to this machine (127.0.0.1, localhost or [::1])"
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


def serve(test, folder):
    """the http address of a server on loopback that serves ``folder`` until ``test`` ends."""
    handler = functools.partial(QuietHandler, directory=str(folder))
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    # shutdown waits out one poll, which is half a second by default
    thread = threading.Thread(
        target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True
    )
    thread.start()
    test.addCleanup(thread.join)
    test.addCleanup(server.server_close)
    test.addCleanup(server.shutdown)
    return f"http://127.0.0.1:{server.server_port}"


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
        self.url = serve(self, self.release)
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open(self.url + "/", timeout=5) as response:
            self.assertEqual(response.status, 200)

    def package(self, matching=True, exit_code=0, build="", said="fetchloom 0.1.0", flat=False):
        name = f"fetchloom-0.1.0-{self.target}"
        executable = (
            '#!/bin/sh\nprintf ran > "$HOME/executed"\n'
            f"printf '{said}\\n'\n"
            f"exit {exit_code}\n# {build}\n"
        ).encode()
        archive = self.release / f"{name}.tar.gz"
        with tarfile.open(archive, "w:gz") as packed:
            entry = tarfile.TarInfo("fetchloom" if flat else f"{name}/fetchloom")
            entry.mode = 0o755
            entry.size = len(executable)
            packed.addfile(entry, io.BytesIO(executable))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest() if matching else "0" * 64
        (self.release / "SHA256SUMS").write_text(f"{digest}  {archive.name}\n", encoding="ascii")
        return executable

    def install(self, modify_path=False, path=None, url=None, version="0.1.0"):
        env = {name: value for name, value in os.environ.items()
               if not name.upper().startswith("FETCHLOOM_") and not name.upper().endswith("_PROXY")}
        env.update(HOME=str(self.home), FETCHLOOM_DOWNLOAD_URL=url or self.url, NO_PROXY="127.0.0.1",
                   PATH=os.pathsep.join(self.tools if path is None else path))
        if not modify_path:
            env["FETCHLOOM_NO_MODIFY_PATH"] = "1"
        return subprocess.run(["sh", str(ROOT / "install.sh"), *filter(None, [version])], env=env,
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

    def test_a_program_that_is_not_the_version_named_is_refused(self):
        self.package(said="fetchloom 0.0.9")
        result = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("error: the downloaded fetchloom says it is fetchloom 0.0.9, not fetchloom 0.1.0. "
                      "nothing was installed", result.stderr)
        self.assertEqual(self.bin.read_bytes(), b"previous installation\n")

    def test_an_archive_without_its_folder_names_where_the_program_belongs(self):
        self.package(flat=True)
        result = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"holds no fetchloom-0.1.0-{self.target}/fetchloom. nothing was installed", result.stderr)

    def test_an_archive_name_that_is_no_plain_version_is_refused(self):
        archive = self.release / f"fetchloom-0.1.0-{self.target}.tar.gz"
        self.package()
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        (self.release / "SHA256SUMS").write_text(
            f"{digest}  fetchloom-0.1.0/../../trav-{self.target}.tar.gz\n", encoding="ascii")
        result = self.install(version=None)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"SHA256SUMS lists no fetchloom archive for {self.target}", result.stderr)
        self.assertEqual(self.bin.read_bytes(), b"previous installation\n")

    def test_a_staging_folder_a_killed_run_left_is_removed(self):
        self.package()
        folder = self.bin.parent
        # pid_max is at most 4194304, so no process has these
        stale = folder / ".fetchloom-install-999999999"
        (stale / "fetchloom-0.1.0").mkdir(parents=True)
        outside = self.root / "outside"
        outside.mkdir()
        (outside / "kept").write_text("kept\n", encoding="utf-8")
        linked = folder / ".fetchloom-install-999999998"
        linked.symlink_to(outside, target_is_directory=True)
        running = folder / f".fetchloom-install-{os.getpid()}"
        running.mkdir()
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(os.path.lexists(stale))
        self.assertFalse(os.path.lexists(linked))
        self.assertEqual((outside / "kept").read_text(encoding="utf-8"), "kept\n")
        self.assertTrue(running.is_dir())

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

    def test_a_profile_it_cannot_write_is_named_and_the_others_still_change(self):
        # a read-only ~/.bashrc, as a home-manager link into /nix/store is, stopped dash after
        # fetchloom was installed, so ~/.zshrc and the next steps were left out
        executable = self.package()
        bashrc, zshrc = self.home / ".bashrc", self.home / ".zshrc"
        bashrc.write_text("# managed elsewhere\n", encoding="utf-8")
        bashrc.chmod(0o444)
        self.addCleanup(bashrc.chmod, 0o644)
        zshrc.write_text("# zsh\n", encoding="utf-8")
        result = self.install(modify_path=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.bin.read_bytes(), executable)
        self.assertIn("warning: could not add ~/.local/bin to PATH in ~/.bashrc", result.stderr)
        self.assertEqual(bashrc.read_text(encoding="utf-8"), "# managed elsewhere\n")
        for profile in [self.profile, zshrc]:
            self.assertEqual(profile.read_text(encoding="utf-8").count('export PATH="$HOME/.local/bin:$PATH"'), 1)
        self.assertIn("added", result.stdout)
        self.assertIn("next: open a new terminal and run fetchloom to see the first steps", result.stdout)

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


    def test_plain_http_is_taken_only_from_this_machine(self):
        executable = self.package()
        port = self.url.rsplit(":", 1)[1]
        for url in ["http://example.invalid", f"http://localhost:{port}@example.invalid",
                    f"http://127.0.0.1.example.invalid:{port}", f"ftp://127.0.0.1:{port}"]:
            with self.subTest(url=url):
                result = self.install(url=url)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn(REFUSED, result.stderr)
                self.assertNotIn("downloading", result.stdout)
                self.assertEqual(self.bin.read_bytes(), b"previous installation\n")
        result = self.install(url=f"http://localhost:{port}")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.bin.read_bytes(), executable)

    def test_an_https_folder_passes_to_the_download(self):
        self.package()
        url = self.url.replace("http://", "https://")
        result = self.install(url=url)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"downloading {url}/SHA256SUMS", result.stdout)
        self.assertIn(f"could not download {url}/SHA256SUMS", result.stderr)


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


@unittest.skipUnless(shutil.which("pwsh"), "install.ps1's address check runs in powershell 7")
class PowerShellDownloadUrl(unittest.TestCase):
    """install.ps1 refuses a FETCHLOOM_DOWNLOAD_URL that is neither https nor plain http to this
    machine before it downloads anything. the others reach a loopback server holding no release."""

    def run_installer(self, url):
        home = tempfile.TemporaryDirectory()
        self.addCleanup(home.cleanup)
        env = {name: value for name, value in os.environ.items()
               if not name.upper().startswith("FETCHLOOM_") and not name.upper().endswith("_PROXY")}
        env.update(LOCALAPPDATA=home.name, FETCHLOOM_DOWNLOAD_URL=url, FETCHLOOM_NO_MODIFY_PATH="1")
        result = subprocess.run([shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                                 "-File", str(ROOT / "install.ps1")], env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=60)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        # powershell may color an error and wraps a long one across lines behind a | gutter
        return result.stdout, " ".join(re.sub(r"\x1b\[[0-9;]*m", "", result.stderr).replace("|", " ").split())

    def test_plain_http_is_taken_only_from_this_machine(self):
        empty = tempfile.TemporaryDirectory()
        self.addCleanup(empty.cleanup)
        port = serve(self, empty.name).rsplit(":", 1)[1]
        for url in ["http://example.invalid", f"http://127.0.0.1:{port}@example.invalid", f"ftp://127.0.0.1:{port}"]:
            with self.subTest(url=url):
                printed, said = self.run_installer(url)
                self.assertIn(REFUSED, said)
                self.assertNotIn("downloading", printed)
        for url in [f"http://127.0.0.1:{port}", f"http://localhost:{port}", f"https://127.0.0.1:{port}"]:
            with self.subTest(url=url):
                printed, said = self.run_installer(url)
                self.assertIn(f"downloading {url}/SHA256SUMS", printed)
                self.assertIn(f"could not download {url}/SHA256SUMS", said)
                self.assertNotIn(REFUSED, said)


@unittest.skipUnless(shutil.which("pwsh"), "install.ps1 runs in powershell 7 here")
class PowerShellRelease(unittest.TestCase):
    """install.ps1 against a private release on loopback, stopping before it runs anything it
    downloaded unless a test says otherwise."""

    # install.ps1 asks for the arm64 build on an arm64 system, as on an apple silicon mac
    TARGET = ("aarch64" if platform.machine().lower() in ("arm64", "aarch64") else "x86_64") + "-pc-windows-msvc"

    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        self.local = self.root / "local"
        self.release = self.root / "release"
        self.release.mkdir()
        self.programs = self.local / "Programs/fetchloom"
        self.url = serve(self, self.release)

    def package(self, program=b"not a program\n", inside=None):
        """a release archive whose fetchloom.exe is ``program``, kept at ``inside``, and the
        SHA256SUMS line naming it; returns the archive's SHA-256."""
        name = f"fetchloom-0.1.0-{self.TARGET}"
        data = io.BytesIO()
        with zipfile.ZipFile(data, "w") as packed:
            packed.writestr(inside or f"{name}/fetchloom.exe", program)
        (self.release / f"{name}.zip").write_bytes(data.getvalue())
        digest = hashlib.sha256(data.getvalue()).hexdigest()
        (self.release / "SHA256SUMS").write_text(f"{digest}  {name}.zip\n", encoding="ascii")
        return digest

    def installer(self, version="0.1.0"):
        """the command that runs install.ps1 against this release, and its environment."""
        env = {name: value for name, value in os.environ.items()
               if not name.upper().startswith("FETCHLOOM_") and not name.upper().endswith("_PROXY")}
        env.update(LOCALAPPDATA=str(self.local), FETCHLOOM_DOWNLOAD_URL=self.url, FETCHLOOM_NO_MODIFY_PATH="1",
                   NO_PROXY="127.0.0.1")
        command = [shutil.which("pwsh"), "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass",
                   "-File", str(ROOT / "install.ps1"), *(["-Version", version] if version else [])]
        return command, env

    def install(self, version="0.1.0"):
        command, env = self.installer(version)
        result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                timeout=120)
        # powershell may color an error and wraps a long one across lines behind a | gutter
        said = " ".join(re.sub(r"\x1b\[[0-9;]*m", "", result.stderr).replace("|", " ").split())
        return result, said

    def test_an_archive_name_that_is_no_plain_version_is_refused_before_any_download(self):
        digest = self.package()
        # windows and .net read each .. against the folder before it, so this name led two
        # folders above the staging folder
        (self.release / f"trav-{self.TARGET}.zip").write_bytes((self.release / f"fetchloom-0.1.0-{self.TARGET}.zip").read_bytes())
        (self.release / "SHA256SUMS").write_text(f"{digest}  fetchloom-../../../../trav-{self.TARGET}.zip\n",
                                                 encoding="ascii")
        result, said = self.install(version=None)
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn(f"SHA256SUMS lists no fetchloom archive for {self.TARGET}", said)
        self.assertEqual(sorted(path.name for path in self.root.rglob(f"trav-{self.TARGET}.zip")),
                         [f"trav-{self.TARGET}.zip"])

    def test_an_archive_without_its_folder_names_where_the_program_belongs(self):
        self.package(inside="fetchloom.exe")
        result, said = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        expected = f"fetchloom-0.1.0-{self.TARGET}.zip holds no fetchloom-0.1.0-{self.TARGET}\\fetchloom.exe. nothing was installed"
        self.assertIn(expected, said)

    def test_a_folder_in_the_programs_place_is_refused_before_any_download(self):
        self.package()
        destination = self.programs / "bin/fetchloom.exe"
        destination.mkdir(parents=True)
        result, said = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("fetchloom.exe is a folder or a link to one. nothing was installed", said)
        self.assertNotIn("downloading", result.stdout)
        self.assertEqual(list(destination.iterdir()), [])

    def test_a_staging_folder_a_killed_run_left_is_removed(self):
        stale = self.programs / "install-999999999"
        (stale / "fetchloom-0.1.0").mkdir(parents=True)
        aside = self.programs / "old-fl-12.exe"
        aside.write_bytes(b"an old fl.exe a running program held\n")
        running = self.programs / f"install-{os.getpid()}"
        running.mkdir()
        # the release holds nothing, so the run stops at its first download
        result, said = self.install()
        self.assertIn("could not download", said)
        self.assertFalse(stale.exists())
        self.assertFalse(aside.exists())
        self.assertTrue(running.is_dir())

    @unittest.skipUnless(os.path.isfile(r"C:\Windows\System32\curl.exe"), "needs windows' own curl.exe")
    def test_a_program_that_is_not_the_version_named_is_refused(self):
        installed = self.programs / "bin/fetchloom.exe"
        installed.parent.mkdir(parents=True)
        installed.write_bytes(b"previous installation\n")
        self.package(program=Path(r"C:\Windows\System32\curl.exe").read_bytes())
        result, said = self.install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertRegex(said, r"the downloaded fetchloom\.exe says it is curl \S+ .*, not fetchloom 0\.1\.0\. nothing was installed")
        self.assertIn("checking that the downloaded fetchloom.exe runs on this machine", result.stdout)
        self.assertEqual(installed.read_bytes(), b"previous installation\n")

    @unittest.skipUnless(sys.platform == "win32" and (ROOT / "target/debug/fetchloom.exe").is_file(),
                         "needs windows and a fetchloom.exe built here")
    def test_an_upgrade_waits_while_another_program_holds_fetchloom_exe(self):
        self.package(program=(ROOT / "target/debug/fetchloom.exe").read_bytes())
        result, said = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        # python opens a file without letting others delete it, as security software checking a
        # program may, so windows cannot replace fetchloom.exe until this lets go
        held = open(self.programs / "bin/fetchloom.exe", "rb")
        self.addCleanup(held.close)
        command, env = self.installer()
        lines = []
        with subprocess.Popen(command, env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True) as upgrade:
            for line in upgrade.stdout:
                lines.append(line)
                if line.startswith("waiting for another program"):
                    held.close()
            upgrade.wait(timeout=120)
        said = "".join(lines)
        self.assertTrue(held.closed, f"the installer never waited for fetchloom.exe:\n{said}")
        self.assertEqual(upgrade.returncode, 0, said)
        self.assertIn("installed fetchloom 0.1.0", said)
        self.assertEqual(list(self.programs.glob("old-fetchloom-*.exe")), [])

    @unittest.skipUnless(sys.platform == "win32" and (ROOT / "target/debug/fetchloom.exe").is_file(),
                         "needs windows and a fetchloom.exe built here")
    def test_a_running_fl_exe_goes_aside_so_an_upgrade_makes_it_again(self):
        self.package(program=(ROOT / "target/debug/fetchloom.exe").read_bytes())
        result, said = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        bin = self.programs / "bin"
        # a listener that never answers keeps fl.exe waiting on its download, running
        with socket.create_server(("127.0.0.1", 0)) as silent:
            origin = f"http://127.0.0.1:{silent.getsockname()[1]}"
            env = {**os.environ, "LOCALAPPDATA": str(self.local), "FETCHLOOM_ALLOWED_ORIGINS": origin,
                   "FETCHLOOM_KEYRING": "0", "NO_PROXY": "127.0.0.1"}
            running = subprocess.Popen([str(bin / "fl.exe"), "get", "--dir", str(self.root / "out"), f"{origin}/slow"],
                                       env=env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            self.addCleanup(running.wait)
            self.addCleanup(running.kill)
            silent.settimeout(60)
            held, _ = silent.accept()
            with held:
                result, said = self.install()
                self.assertIsNone(running.poll())
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("fl.exe was not made", said)
        self.assertTrue(os.path.samefile(bin / "fl.exe", bin / "fetchloom.exe"))
        self.assertEqual(len(list(self.programs.glob("old-fl-*.exe"))), 1)
