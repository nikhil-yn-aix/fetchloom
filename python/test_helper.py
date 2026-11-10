"""the python helper's command arguments and returned paths."""

import itertools
import json
import pickle
import subprocess
import traceback
import unittest
from pathlib import Path
from unittest.mock import patch

import fetchloom


class HelperOptions(unittest.TestCase):
    def test_path_and_sync_forward_approval_offline_and_locked_flags(self):
        for command in ["path", "sync"]:
            for yes, offline, locked in itertools.product([False, True], repeat=3):
                with self.subTest(command=command, yes=yes, offline=offline, locked=locked):
                    result = {"path": "resolved"} if command == "path" else {"project": "resolved"}
                    response = subprocess.CompletedProcess(
                        [], 0, json.dumps({"status": "ok", "result": result, "warnings": []}), ""
                    )
                    flags = {"yes": yes, "offline": offline, "locked": locked}
                    with patch.object(fetchloom, "_program", return_value="fetchloom"):
                        with patch.object(fetchloom.subprocess, "run", return_value=response) as run:
                            if command == "path":
                                actual = fetchloom.path("iris/data.csv", Path("project"), **flags)
                                arguments = ["path", "iris/data.csv"]
                            else:
                                actual = fetchloom.sync(Path("project"), **flags)
                                arguments = ["sync"]
                    if yes:
                        arguments.append("--yes")
                    if offline:
                        arguments.append("--offline")
                    if locked:
                        arguments.append("--locked")
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


class Errors(unittest.TestCase):
    def test_a_traceback_shows_the_details_and_the_help(self):
        answer = {
            "status": "error",
            "errors": [{
                "kind": "usage",
                "message": "iris has no file or folder called iris.csv",
                "details": ["iris holds Iris.csv and README.md"],
                "help": "did you mean `fetchloom path iris/Iris.csv`?",
            }],
            "warnings": [],
        }
        response = subprocess.CompletedProcess([], 2, json.dumps(answer), "")
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom.subprocess, "run", return_value=response):
                with self.assertRaises(fetchloom.Error) as raised:
                    fetchloom.path("iris/iris.csv")
        self.assertEqual(
            traceback.format_exception_only(type(raised.exception), raised.exception)[-1],
            "fetchloom.Error: usage: iris has no file or folder called iris.csv\n"
            "  iris holds Iris.csv and README.md\n"
            "  help: did you mean `fetchloom path iris/Iris.csv`?\n",
        )
        self.assertEqual(raised.exception.code, 2)

    def test_an_error_without_details_or_help_is_one_line(self):
        self.assertEqual(str(fetchloom.Error("usage", "no data.toml here")), "usage: no data.toml here")
        self.assertEqual(
            str(fetchloom.Error(None, "fetchloom exited with code 9 without a JSON answer. it said: nothing", code=9)),
            "fetchloom exited with code 9 without a JSON answer. it said: nothing",
        )

    def test_an_error_survives_pickling(self):
        error = fetchloom.Error("usage", "no data.toml here", ["looked in /p"], "run `fetchloom add`", 2)
        copy = pickle.loads(pickle.dumps(error))
        self.assertEqual((copy.kind, copy.message, copy.details, copy.help, copy.code),
                         ("usage", "no data.toml here", ["looked in /p"], "run `fetchloom add`", 2))
        self.assertEqual(str(copy), str(error))


class PublicNames(unittest.TestCase):
    def test_dir_lists_only_the_helpers(self):
        self.assertEqual(dir(fetchloom), ["Error", "path", "sync"])

    def test_the_version_is_the_installed_packages(self):
        with patch("importlib.metadata.version", return_value="9.9.9") as version:
            self.assertEqual(fetchloom.__version__, "9.9.9")
        version.assert_called_once_with("fetchloom")

    def test_strict_names_what_it_raises_on_and_what_it_does_not(self):
        documented = " ".join(fetchloom.sync.__doc__.split())
        self.assertIn("``strict=True`` also raises", documented)
        self.assertIn("put back as pinned", documented)

    def test_the_path_example_names_a_real_file(self):
        self.assertIn("``iris/Iris.csv``", fetchloom.path.__doc__)


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
