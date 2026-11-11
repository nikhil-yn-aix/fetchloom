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
        self.assertEqual(raised.exception.message, "1 file in the data folder differs from data.lock")

    def test_strict_names_several_files_in_the_plural(self):
        files = [{"path": f"data/iris/{name}", "reason": "changed"} for name in ["a.csv", "b.csv"]]
        result = {"project": "p", "placement": {"kept": files}}
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": result, "warnings": []}), ""
        )
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom.subprocess, "run", return_value=response):
                with self.assertRaises(fetchloom.Error) as raised:
                    fetchloom.sync("p", strict=True)
        self.assertEqual(raised.exception.message, "2 files in the data folder differ from data.lock")


class GetOptions(unittest.TestCase):
    def run_get(self, result, *args, **kwargs):
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": result, "warnings": []}), ""
        )
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom.subprocess, "run", return_value=response) as run:
                actual = fetchloom.get(*args, **kwargs)
        return actual, run.call_args.args[0]

    def test_get_with_a_path_writes_that_file_and_returns_it(self):
        digest = "d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d"
        result = {"file": "/work/data.csv", "folder": "/work", "files": [], "items": []}
        actual, argv = self.run_get(
            result, "https://example.org/iris.csv", Path("data.csv"),
            sha256=digest, force=True, yes=True, offline=True,
        )
        self.assertEqual(actual, Path("/work/data.csv"))
        self.assertEqual(argv, [
            "fetchloom", "--json", "-C", ".", "get", "--output=data.csv", f"--sha256={digest}",
            "--force", "--yes", "--offline", "--", "https://example.org/iris.csv",
        ])

    def test_get_without_a_path_returns_the_folder(self):
        result = {"file": None, "folder": "/work/flowers", "files": [], "items": []}
        for folder, flags in [(None, []), ("flowers", ["--dir=flowers"])]:
            with self.subTest(folder=folder):
                actual, argv = self.run_get(result, "zenodo:3608990", folder=folder)
                self.assertEqual(actual, Path("/work/flowers"))
                self.assertEqual(argv, ["fetchloom", "--json", "-C", ".", "get", *flags, "--", "zenodo:3608990"])

    def test_get_takes_a_path_or_a_folder_not_both(self):
        with patch.object(fetchloom.subprocess, "run") as run:
            with self.assertRaises(fetchloom.Error) as raised:
                fetchloom.get("zenodo:3608990", "iris.csv", folder="iris")
        run.assert_not_called()
        self.assertEqual(raised.exception.kind, "usage")
        with self.assertRaises(TypeError):
            fetchloom.get("zenodo:3608990", "iris.csv", "iris")

    def test_a_refused_file_raises_the_kind_fetchloom_gave(self):
        answer = {
            "status": "error",
            "errors": [{
                "kind": "local.not_empty",
                "message": "data.csv already exists and holds other bytes than the link serves now",
                "details": [],
                "help": "add --force to replace it, or choose another name with -o",
            }],
            "warnings": [],
        }
        response = subprocess.CompletedProcess([], 50, json.dumps(answer), "")
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom.subprocess, "run", return_value=response):
                with self.assertRaises(fetchloom.Error) as raised:
                    fetchloom.get("https://example.org/iris.csv", "data.csv")
        self.assertEqual((raised.exception.kind, raised.exception.code), ("local.not_empty", 50))


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

    def test_hints_show_the_command_a_person_runs(self):
        typed = "unset FETCHLOOM_OFFLINE, then run `fetchloom --json -C {project} path iris`"
        for project, shown in [
            (".", "unset FETCHLOOM_OFFLINE, then run `fetchloom path iris`"),
            ("analysis", "unset FETCHLOOM_OFFLINE, then run `fetchloom -C analysis path iris`"),
        ]:
            answer = {
                "status": "error",
                "errors": [{
                    "kind": "offline.missing",
                    "message": "offline, and 1 file data.lock names is not in the cache",
                    "details": ["iris/iris.csv"],
                    "help": typed.format(project=project),
                }],
                "warnings": [],
            }
            response = subprocess.CompletedProcess([], 20, json.dumps(answer), "")
            with self.subTest(project=project):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom.subprocess, "run", return_value=response):
                        with self.assertRaises(fetchloom.Error) as raised:
                            fetchloom.path("iris", project)
                self.assertEqual(raised.exception.help, shown)

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
        self.assertEqual(dir(fetchloom), ["Error", "get", "path", "sync"])

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

    def test_missing_program_points_at_the_install_steps(self):
        with self.assertRaises(FileNotFoundError) as raised:
            self.locate("source/fetchloom", [])
        self.assertEqual(
            str(raised.exception),
            "the fetchloom program is missing: install it as "
            "https://github.com/nikhil-yn-aix/fetchloom#install says, or put it on PATH",
        )

    def test_user_scripts_follow_the_interpreters_preferred_scheme(self):
        with patch.object(fetchloom.sysconfig, "get_preferred_scheme", return_value="custom_user") as preferred:
            with patch.object(fetchloom.sysconfig, "get_path", return_value="") as get_path:
                with patch.object(fetchloom.shutil, "which", return_value="from-path"):
                    self.assertEqual(fetchloom._program(), "from-path")
        preferred.assert_called_once_with("user")
        get_path.assert_any_call("scripts", "custom_user")
