"""the python helper's command arguments and returned paths."""

import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import patch

import fetchloom


class HelperOptions(unittest.TestCase):
    def test_path_and_sync_forward_approval_and_offline_flags(self):
        for command in ["path", "sync"]:
            for yes, offline in [(False, False), (True, False), (False, True), (True, True)]:
                with self.subTest(command=command, yes=yes, offline=offline):
                    result = "resolved" if command == "path" else {"project": "resolved"}
                    response = subprocess.CompletedProcess(
                        [], 0, json.dumps({"status": "ok", "result": result, "warnings": []}), ""
                    )
                    with patch.object(fetchloom, "_program", return_value="fetchloom"):
                        with patch.object(fetchloom.subprocess, "run", return_value=response) as run:
                            if command == "path":
                                actual = fetchloom.path("iris/data.csv", Path("project"), yes=yes, offline=offline)
                                arguments = ["path", "iris/data.csv"]
                            else:
                                actual = fetchloom.sync(Path("project"), yes=yes, offline=offline)
                                arguments = ["sync"]
                    if yes:
                        arguments.append("--yes")
                    if offline:
                        arguments.append("--offline")
                    self.assertEqual(actual, Path("resolved"))
                    self.assertEqual(run.call_args.args[0], ["fetchloom", "--json", "-C", "project", *arguments])
                    self.assertEqual(run.call_args.kwargs["stdin"], subprocess.DEVNULL)

    def test_options_are_keyword_only(self):
        with self.assertRaises(TypeError):
            fetchloom.path("iris", ".", True)
        with self.assertRaises(TypeError):
            fetchloom.sync(".", True)

    def test_strict_sync_raises_on_kept_files(self):
        kept = {"project": "p", "placement": {"kept": [{"path": "data/iris/iris.csv", "reason": "changed"}]}}
        warning = "data/iris/iris.csv has changed, so it stays"
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": kept, "warnings": [warning]}), ""
        )
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom.subprocess, "run", return_value=response):
                with self.assertWarns(UserWarning):
                    self.assertEqual(fetchloom.sync("p"), Path("p"))
                with self.assertWarns(UserWarning), self.assertRaises(fetchloom.Error) as raised:
                    fetchloom.sync("p", strict=True)
        self.assertEqual(raised.exception.kind, "conflict.kept")
        self.assertEqual(raised.exception.details, ["data/iris/iris.csv"])


class ProgramDiscovery(unittest.TestCase):
    def locate(self, package, binaries, fallback=None):
        root = Path.cwd() / "discovery"
        name = "fetchloom.exe" if fetchloom.sys.platform == "win32" else "fetchloom"
        available = {root / folder / name for folder in binaries}

        def scripts(key, scheme=None, vars=None):
            self.assertEqual(key, "scripts")
            folder = "user" if scheme else "base" if vars else "interpreter"
            return str(root / folder)

        with patch.object(fetchloom, "__file__", str(root / package / "__init__.py")):
            with patch.object(fetchloom.sysconfig, "get_path", side_effect=scripts):
                with patch.object(Path, "is_file", lambda path: path in available):
                    with patch.object(fetchloom.shutil, "which", return_value=fallback):
                        return fetchloom._program()

    def executable(self, folder):
        name = "fetchloom.exe" if fetchloom.sys.platform == "win32" else "fetchloom"
        return str(Path.cwd() / "discovery" / folder / name)

    def test_target_binary_precedes_unrelated_interpreter_and_user_scripts(self):
        self.assertEqual(
            self.locate("target/fetchloom", ["target/bin", "interpreter", "user"]),
            self.executable("target/bin"),
        )

    def test_prefix_binary_is_found_without_path(self):
        windows = fetchloom.sys.platform == "win32"
        package = "prefix/Lib/site-packages/fetchloom" if windows else "prefix/lib/python3.12/site-packages/fetchloom"
        scripts = "prefix/Scripts" if windows else "prefix/bin"
        self.assertEqual(self.locate(package, [scripts]), self.executable(scripts))

    def test_prefix_binary_precedes_unrelated_interpreter(self):
        windows = fetchloom.sys.platform == "win32"
        package = "prefix/Lib/site-packages/fetchloom" if windows else "prefix/lib64/python3.12/site-packages/fetchloom"
        scripts = "prefix/Scripts" if windows else "prefix/bin"
        self.assertEqual(self.locate(package, [scripts, "interpreter"]), self.executable(scripts))

    def test_base_interpreter_scripts_are_considered(self):
        self.assertEqual(self.locate("source/fetchloom", ["base"]), self.executable("base"))

    def test_path_fallback_remains_available(self):
        self.assertEqual(self.locate("source/fetchloom", [], "from-path"), "from-path")

    def test_missing_program_names_the_reinstall_command(self):
        with self.assertRaisesRegex(FileNotFoundError, "pip install fetchloom"):
            self.locate("source/fetchloom", [])

    def test_user_scheme_follows_interpreter_policy(self):
        with patch.object(fetchloom.sysconfig, "get_preferred_scheme", return_value="custom_user", create=True):
            self.assertEqual(fetchloom._user_scheme(), "custom_user")

    def test_python39_framework_user_scheme(self):
        with patch.object(fetchloom.sysconfig, "get_preferred_scheme", None, create=True):
            with patch.object(fetchloom.sys, "platform", "darwin"):
                with patch.object(fetchloom.sys, "_framework", "Python", create=True):
                    self.assertEqual(fetchloom._user_scheme(), "osx_framework_user")
