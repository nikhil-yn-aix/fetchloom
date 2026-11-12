"""the python helper's command arguments and returned paths."""

import inspect
import io
import itertools
import json
import os
import pickle
import socket
import subprocess
import sys
import tempfile
import traceback
import unittest
from pathlib import Path
from unittest.mock import patch

import fetchloom
import wheel_scripts


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
                        with patch.object(fetchloom, "_call", return_value=response) as run:
                            if command == "path":
                                actual = fetchloom.path("iris/data.csv", Path("project"), **flags)
                            else:
                                actual = fetchloom.sync(Path("project"), **flags)
                    arguments = [command]
                    if yes:
                        arguments.append("--yes")
                    if offline:
                        arguments.append("--offline")
                    if locked:
                        arguments.append("--locked")
                    if command == "path":
                        arguments += ["--", "iris/data.csv"]
                    self.assertEqual(actual, Path("resolved"))
                    self.assertEqual(run.call_args.args[0], ["fetchloom", "--json", "--progress", "-C", "project", *arguments])

    def test_options_are_keyword_only(self):
        with self.assertRaises(TypeError):
            fetchloom.path("iris", ".", True)
        with self.assertRaises(TypeError):
            fetchloom.sync(".", True)
        with self.assertRaises(TypeError):
            fetchloom.sync(".", ["iris"])

    def test_a_name_like_an_option_stays_a_name(self):
        def program(argv, **_):
            # as fetchloom does, it reads -h before any -- as a request for help, which has no path
            options = argv[: argv.index("--")] if "--" in argv else argv
            if "-h" in options:
                document = {"status": "ok", "command": "help", "result": {"commands": [], "options": []}, "warnings": []}
                return subprocess.CompletedProcess(argv, 0, json.dumps(document), "")
            error = {"kind": "usage", "message": f'data.toml has no dataset called "{argv[-1]}"', "details": [], "help": None}
            return subprocess.CompletedProcess(argv, 2, json.dumps({"status": "error", "errors": [error], "warnings": []}), "")

        # a quoted name stays as typed, even one spelled as a flag the helper names for python
        for name in ["-h", "--yes"]:
            with self.subTest(name=name):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", side_effect=program) as run:
                        with self.assertRaises(fetchloom.Error) as raised:
                            fetchloom.path(name, "p", yes=True)
                self.assertEqual(raised.exception.message, f'data.toml has no dataset called "{name}"')
                self.assertEqual(run.call_args.args[0], ["fetchloom", "--json", "--progress", "-C", "p", "path", "--yes", "--", name])

    def test_sync_verify_hashes_every_placed_file_as_the_cli_does(self):
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": {"project": "p"}, "warnings": []}), ""
        )
        for verify, flags in [(False, ["--locked"]), (True, ["--locked", "--verify"])]:
            with self.subTest(verify=verify):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response) as run:
                        fetchloom.sync("p", names="iris", locked=True, verify=verify)
                self.assertEqual(
                    run.call_args.args[0],
                    ["fetchloom", "--json", "--progress", "-C", "p", "sync", *flags, "--", "iris"],
                )

    def test_sync_names_the_datasets_it_asks_for_after_its_flags(self):
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": {"project": "p"}, "warnings": []}), ""
        )
        for names, named in [
            ((), []),
            (["small", "large"], ["--", "small", "large"]),
            ("small", ["--", "small"]),
            (iter(["-small"]), ["--", "-small"]),
        ]:
            with self.subTest(names=names):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response) as run:
                        self.assertEqual(fetchloom.sync("p", names=names, yes=True), Path("p"))
                self.assertEqual(
                    run.call_args.args[0],
                    ["fetchloom", "--json", "--progress", "-C", "p", "sync", "--yes", *named],
                )

    def test_strict_sync_fails_as_sync_strict_does(self):
        # fetchloom's answers in a project whose recipe clean was never built: sync succeeds,
        # and only sync --strict fails
        result = {
            "project": "/work/p",
            "selected": None,
            "datasets": [],
            "lock_changed": False,
            "placement": {"placed": 0, "replaced": 0, "removed": 0, "unchanged": 1, "kept": [], "unbuilt": []},
        }
        warning = {
            "kind": "build.unbuilt",
            "message": "clean is not built yet",
            "details": [],
            "help": "run `fetchloom build clean` to make it, then commit data.lock and data.lock.d",
        }
        error = {
            "kind": "data.not_ready",
            "message": "the data is not ready to use",
            "details": ["clean needs a build (not built yet): `fetchloom build clean` makes it"],
            "help": "each line says what makes it ready",
            "fix": None,
        }

        def program(argv):
            if "--strict" in argv:
                answer = {"status": "error", "result": result, "warnings": [warning], "errors": [error]}
                return subprocess.CompletedProcess(argv, 1, json.dumps(answer), "")
            answer = {"status": "ok", "result": result, "warnings": [warning], "errors": []}
            return subprocess.CompletedProcess(argv, 0, json.dumps(answer), "")

        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom, "_call", side_effect=program) as run:
                with self.assertWarns(UserWarning):
                    self.assertEqual(fetchloom.sync("p"), Path("/work/p"))
                with self.assertWarns(UserWarning), self.assertRaises(fetchloom.Error) as raised:
                    fetchloom.sync("p", strict=True)
        self.assertEqual(run.call_args.args[0], ["fetchloom", "--json", "--progress", "-C", "p", "sync", "--strict"])
        self.assertEqual((raised.exception.kind, raised.exception.code), ("data.not_ready", 1))
        self.assertEqual(raised.exception.details, error["details"])

    # the words of the cli's own case table, quoted as it quotes them
    QUOTED = [
        ("plain", "plain", "plain"),
        ("", '""', '""'),
        ("hi!.csv", "'hi!.csv'", '"hi!.csv"'),
        ("$notes", "'$notes'", "'$notes'"),
        ("a`b`", "'a`b`'", "'a`b`'"),
        ("ada's data", '"ada\'s data"', '"ada\'s data"'),
        ("it's $5!", "'it'\\''s $5!'", "'it''s $5!'"),
        ("my data", '"my data"', '"my data"'),
        ("données é.csv", '"données é.csv"', '"données é.csv"'),
        ("data\\notes", "'data'\\\\'notes'", '"data\\notes"'),
        ('a"b', "'a\"b'", "'a\"b'"),
        ("~notes.csv", '"~notes.csv"', '"~notes.csv"'),
        ("=notes.csv", '"=notes.csv"', '"=notes.csv"'),
        ("@notes.csv", '"@notes.csv"', '"@notes.csv"'),
        ("ada’s “best” data", '"ada’s “best” data"', "'ada’’s “best” data'"),
    ]

    def test_suggested_words_are_quoted_as_fetchloom_quotes_them(self):
        for word, posix, windows in self.QUOTED:
            with self.subTest(word=word):
                self.assertEqual(fetchloom._shell_word(word, windows=False), posix)
                self.assertEqual(fetchloom._shell_word(word, windows=True), windows)

    @unittest.skipUnless(os.name == "posix", "runs the quoted words in posix shells")
    def test_a_quoted_word_reads_back_whole_in_interactive_bash(self):
        # interactive bash expands `!` inside double quotes from its history
        words = [word for word, _, _ in self.QUOTED]
        line = "printf '%s\\n' " + " ".join(fetchloom._shell_word(word, windows=False) for word in words) + "\n"
        for shell in [["bash", "--norc", "--noprofile", "-i", "-s"], ["sh", "-s"]]:
            with self.subTest(shell=shell[0]):
                try:
                    told = subprocess.run(
                        shell, input=line.encode(), capture_output=True,
                        env={**os.environ, "HISTFILE": "/dev/null"},
                    )
                except FileNotFoundError:
                    continue
                self.assertEqual(told.stdout.decode().split("\n")[:-1], words)


class GetOptions(unittest.TestCase):
    def run_get(self, result, *args, **kwargs):
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": result, "warnings": []}), ""
        )
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom, "_call", return_value=response) as run:
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
            "fetchloom", "--json", "--progress", "-C", ".", "get", "--output=data.csv", f"--sha256={digest}",
            "--force", "--yes", "--offline", "--", "https://example.org/iris.csv",
        ])

    def test_get_without_a_path_returns_the_folder(self):
        result = {"file": None, "folder": "/work/flowers", "files": [], "items": []}
        for folder, flags in [(None, []), ("flowers", ["--dir=flowers"])]:
            with self.subTest(folder=folder):
                actual, argv = self.run_get(result, "zenodo:3608990", folder=folder)
                self.assertEqual(actual, Path("/work/flowers"))
                self.assertEqual(argv, ["fetchloom", "--json", "--progress", "-C", ".", "get", *flags, "--", "zenodo:3608990"])

    def test_a_home_folder_in_a_path_is_expanded_as_pandas_does(self):
        home = str(Path.home())
        result = {"file": "/f", "folder": "/d", "files": [], "items": []}
        _, argv = self.run_get(result, "https://example.org/~iris.csv", "~/data/iris.csv")
        self.assertEqual(argv[6], f"--output={home}/data/iris.csv")
        self.assertEqual(argv[-1], "https://example.org/~iris.csv")
        _, argv = self.run_get(result, "zenodo:3608990", folder=Path("~", "flowers"))
        self.assertEqual(argv[6], f"--dir={Path(home, 'flowers')}")
        response = subprocess.CompletedProcess(
            [], 0, json.dumps({"status": "ok", "result": {"path": "p"}, "warnings": []}), ""
        )
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom, "_call", return_value=response) as run:
                fetchloom.path("~iris", "~/project")
        self.assertEqual(run.call_args.args[0][4:], [f"{home}/project", "path", "--", "~iris"])

    def test_get_takes_a_path_or_a_folder_not_both(self):
        with patch.object(fetchloom, "_call") as run:
            with self.assertRaises(fetchloom.Error) as raised:
                fetchloom.get("zenodo:3608990", "iris.csv", folder="iris")
        run.assert_not_called()
        self.assertEqual(raised.exception.kind, "usage")
        with self.assertRaises(TypeError):
            fetchloom.get("zenodo:3608990", "iris.csv", "iris")

    def test_force_needs_a_path_to_replace(self):
        with patch.object(fetchloom, "_call") as run:
            for folder in [None, "iris"]:
                with self.subTest(folder=folder):
                    with self.assertRaises(fetchloom.Error) as raised:
                        fetchloom.get("zenodo:3608990", folder=folder, force=True)
                    self.assertEqual(
                        str(raised.exception),
                        "usage: force=True replaces only the file at path, so it needs path",
                    )
        run.assert_not_called()

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
            with patch.object(fetchloom, "_call", return_value=response):
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
            with patch.object(fetchloom, "_call", return_value=response):
                with self.assertRaises(fetchloom.Error) as raised:
                    fetchloom.path("iris/iris.csv")
        self.assertEqual(
            traceback.format_exception_only(type(raised.exception), raised.exception)[-1],
            "fetchloom.Error: usage: iris has no file or folder called iris.csv\n"
            "  iris holds Iris.csv and README.md\n"
            "  help: did you mean `fetchloom.path(\"iris/Iris.csv\")`?\n",
        )
        self.assertEqual(raised.exception.code, 2)

    def test_hints_show_the_command_a_person_runs(self):
        typed = "unset FETCHLOOM_OFFLINE, then run `fetchloom --json --progress -C {project} path iris`"
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
                    with patch.object(fetchloom, "_call", return_value=response):
                        with self.assertRaises(fetchloom.Error) as raised:
                            fetchloom.path("iris", project)
                self.assertEqual(raised.exception.help, shown)

    def test_commands_leave_out_the_folder_and_separator_the_helper_added(self):
        # what fetchloom says with the -C and the -- the helper gives it, and how python shows it
        cases = [
            (
                lambda: fetchloom.sync(strict=True),
                "`fetchloom -C /work/study build clean` makes it",
                "`fetchloom build clean` makes it",
            ),
            (
                lambda: fetchloom.sync("/work/study", strict=True),
                "`fetchloom -C /work/study build clean` makes it",
                "`fetchloom -C /work/study build clean` makes it",
            ),
            (
                lambda: fetchloom.sync(names=["irs"]),
                "did you mean `fetchloom --json --progress -C . sync -- iris`?",
                "did you mean `fetchloom sync iris`?",
            ),
            (
                lambda: fetchloom.sync(names=["-irs"]),
                "did you mean `fetchloom --json --progress -C . sync -- -iris`?",
                "did you mean `fetchloom sync -- -iris`?",
            ),
        ]
        for call, said, shown in cases:
            answer = {
                "status": "error",
                "errors": [{"kind": "usage", "message": "refused", "details": [said], "help": said}],
                "warnings": [{"kind": "build.unbuilt", "message": "refused", "details": [], "help": said}],
            }
            response = subprocess.CompletedProcess([], 2, json.dumps(answer), "")
            with self.subTest(said=said, shown=shown):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response):
                        with patch.object(fetchloom.os, "getcwd", return_value="/work/study"):
                            with self.assertWarns(UserWarning) as warned:
                                with self.assertRaises(fetchloom.Error) as raised:
                                    call()
                self.assertEqual((raised.exception.details, raised.exception.help), ([shown], shown))
                self.assertEqual(str(warned.warning), f"refused; {shown}")

    def test_a_mistyped_name_is_suggested_as_the_call_that_finds_it(self):
        for call, typed, shown in [
            (
                lambda: fetchloom.path("irs"),
                "fetchloom --json --progress -C . path -- iris",
                'fetchloom.path("iris")',
            ),
            (
                lambda: fetchloom.path("irs", "study", offline=True),
                "fetchloom --json --progress -C study path --offline -- iris",
                'fetchloom.path("iris", project="study", offline=True)',
            ),
        ]:
            answer = {
                "status": "error",
                "errors": [{
                    "kind": "usage",
                    "message": 'data.toml has no dataset called "irs"',
                    "details": [],
                    "help": f"did you mean `{typed}`?",
                }],
                "warnings": [],
            }
            response = subprocess.CompletedProcess([], 2, json.dumps(answer), "")
            with self.subTest(typed=typed):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response):
                        with self.assertRaises(fetchloom.Error) as raised:
                            call()
                self.assertEqual(raised.exception.help, f"did you mean `{shown}`?")
                self.assertEqual(raised.exception.args[3], f"did you mean `{shown}`?")

    def test_a_script_outside_its_project_is_told_to_pass_project(self):
        missing = "there is no data.toml in this folder or any folder above it"
        start = "run `fetchloom add <link>` to start a project here"
        elsewhere = os.path.abspath("elsewhere")
        for call, message, help, shown in [
            (
                lambda: fetchloom.path("processed"),
                missing,
                start,
                "give project= the folder that holds data.toml, "
                f"or run `fetchloom add <link>` to start a project in {os.getcwd()}",
            ),
            (
                lambda: fetchloom.sync("elsewhere"),
                f"{missing} inside the git repository at /work",
                f"{start}, or give -C the folder of a project outside this repository, "
                "as in `fetchloom -C <folder> <command>`",
                "give project= the folder that holds data.toml, "
                f"or run `fetchloom -C elsewhere add <link>` to start a project in {elsewhere}",
            ),
            (
                lambda: fetchloom.path("processed", "elsewhere"),
                "data.toml is not valid TOML: unclosed table, expected `]`",
                "fix that line, then run `fetchloom --json --progress -C elsewhere path -- processed` again",
                "fix that line, then run `fetchloom -C elsewhere path processed` again",
            ),
        ]:
            answer = {
                "status": "error",
                "errors": [{"kind": "project.invalid", "message": message, "details": [], "help": help}],
                "warnings": [],
            }
            response = subprocess.CompletedProcess([], 50, json.dumps(answer), "")
            with self.subTest(message=message):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response):
                        with self.assertRaises(fetchloom.Error) as raised:
                            call()
                self.assertEqual((raised.exception.message, raised.exception.help), (message, shown))

    def test_help_names_the_keywords_a_python_caller_sets(self):
        # help is written for a person at a terminal; a command in backticks stays one they type
        for helper, help, shown in [
            (
                "get",
                "add --force to replace it, or choose another name with -o",
                "pass force=True to replace it, or choose another name with the path argument",
            ),
            (
                "get",
                "choose another folder with --dir, or empty this one",
                "choose another folder with the folder argument, or empty this one",
            ),
            (
                "get",
                "check the hash given to --sha256, or ask the file's owner which bytes are right",
                "check the hash given to the sha256 argument, or ask the file's owner which bytes are right",
            ),
            (
                "get",
                "name the file inside it, as in `-o results/iris.csv`",
                "name the file inside it, as in `path='results/iris.csv'`",
            ),
            (
                "get",
                "name the file inside it, as in `--output=results/iris.csv`",
                "name the file inside it, as in `path='results/iris.csv'`",
            ),
            (
                "get",
                "unpack it into a folder with `--dir=flowers`",
                "unpack it into a folder with `folder='flowers'`",
            ),
            (
                "get",
                "check the connection and run `fetchloom get --output=u.csv -- https://example.org/u.csv` again",
                "check the connection and run `fetchloom get --output=u.csv https://example.org/u.csv` again",
            ),
            (
                "get",
                "run it again without --offline to download it once. "
                "after that, `fetchloom get <link> --offline` reuses it",
                "run it again without offline=True to download it once. "
                "after that, `fetchloom get <link> --offline` reuses it",
            ),
            (
                "sync",
                "add --yes to download it without being asked, or --locked-ish words stay",
                "pass yes=True to download it without being asked, or --locked-ish words stay",
            ),
            (
                "sync",
                "give -C a folder that exists and that you may open",
                "give the project argument a folder that exists and that you may open",
            ),
            (
                "path",
                "give -C a folder that exists and that you may open",
                "give the project argument a folder that exists and that you may open",
            ),
            ("path", "add --force to replace it", "add --force to replace it"),
        ]:
            answer = {
                "status": "error",
                "errors": [{"kind": "usage", "message": "refused", "details": [], "help": help}],
                "warnings": [],
            }
            response = subprocess.CompletedProcess([], 2, json.dumps(answer), "")
            with self.subTest(helper=helper, help=help):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response):
                        with self.assertRaises(fetchloom.Error) as raised:
                            if helper == "get":
                                fetchloom.get("https://example.org/iris.csv", "data.csv")
                            elif helper == "sync":
                                fetchloom.sync()
                            else:
                                fetchloom.path("iris")
                self.assertEqual(raised.exception.help, shown)

    def test_quoted_input_stays_as_typed(self):
        # fetchloom quotes what it was given, which a keyword's name would misquote
        for call, message, shown in [
            (
                lambda: fetchloom.get("--dir=zzz"),
                'found no dataset matching "--dir=zzz"',
                'found no dataset matching "--dir=zzz"',
            ),
            (
                lambda: fetchloom.get("--yes", offline=True),
                'searching for "--yes" needs the internet',
                'searching for "--yes" needs the internet',
            ),
            (lambda: fetchloom.sync(""), "-C needs a value", "the project argument needs a value"),
        ]:
            answer = {
                "status": "error",
                "errors": [{"kind": "usage", "message": message, "details": [], "help": None}],
                "warnings": [],
            }
            response = subprocess.CompletedProcess([], 2, json.dumps(answer), "")
            with self.subTest(message=message):
                with patch.object(fetchloom, "_program", return_value="fetchloom"):
                    with patch.object(fetchloom, "_call", return_value=response):
                        with self.assertRaises(fetchloom.Error) as raised:
                            call()
                self.assertEqual(raised.exception.message, shown)

    def test_messages_details_and_warnings_name_the_keywords_too(self):
        answer = {
            "status": "error",
            "errors": [{
                "kind": "local.not_empty",
                "message": "data.csv already exists, and its bytes are not the ones --sha256 names",
                "details": ["--sha256 names 000000000000, data.csv holds d144ff83495e"],
                "help": "check the hash first, or add --force to replace the file",
            }],
            "warnings": [{
                "kind": "archive.packed",
                "message": "x.csv holds the archive x.zip",
                "details": [],
                "help": "use `--dir x` to unpack it into a folder",
            }],
        }
        response = subprocess.CompletedProcess([], 50, json.dumps(answer), "")
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom, "_call", return_value=response):
                with self.assertWarns(UserWarning) as warned:
                    with self.assertRaises(fetchloom.Error) as raised:
                        fetchloom.get("https://example.org/x.zip", "x.csv", sha256="0" * 64)
        self.assertEqual(
            str(warned.warning),
            "x.csv holds the archive x.zip; use `folder='x'` to unpack it into a folder",
        )
        self.assertEqual(
            str(raised.exception),
            "local.not_empty: data.csv already exists, and its bytes are not the ones the sha256 argument names\n"
            "  the sha256 argument names 000000000000, data.csv holds d144ff83495e\n"
            "  help: check the hash first, or pass force=True to replace the file",
        )

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


def standing_in(child):
    """``subprocess.Popen`` running python with the code ``child`` in place of fetchloom, with
    the arguments the helper gives it."""
    popen = subprocess.Popen

    def start(argv, **options):
        return popen([sys.executable, "-c", child, *argv[1:]], **options)

    return start


class Progress(unittest.TestCase):
    def test_progress_reaches_python_stderr_while_fetchloom_runs(self):
        with socket.create_server(("127.0.0.1", 0)) as listener:
            # it says a progress line, then waits until the test has read that line from
            # sys.stderr before it answers, which it could not if the line came at the end
            child = (
                "import json, socket, sys\n"
                f"told = socket.create_connection(('127.0.0.1', {listener.getsockname()[1]}), timeout=30)\n"
                "print('Downloading big.bin - 1.9 MiB', file=sys.stderr, flush=True)\n"
                "told.recv(1)\n"
                "result = {'path': ' '.join(sys.argv[1:])}\n"
                "print(json.dumps({'status': 'ok', 'result': result, 'warnings': []}))\n"
            )

            class Watched(io.StringIO):
                def write(self, text):
                    if "Downloading" in text:
                        connection, _ = listener.accept()
                        with connection:
                            connection.sendall(b"!")
                    return super().write(text)

            with patch.object(fetchloom, "_program", return_value="fetchloom"):
                with patch.object(fetchloom.subprocess, "Popen", side_effect=standing_in(child)) as popen:
                    with patch.object(fetchloom.sys, "stderr", Watched()) as shown:
                        answer = fetchloom.path("big", "p")
        self.assertEqual(answer, Path("--json --progress -C p path -- big"))
        self.assertEqual(shown.getvalue(), "Downloading big.bin - 1.9 MiB\n")
        self.assertEqual(popen.call_args.kwargs["stdin"], subprocess.DEVNULL)

    def test_a_crash_says_only_the_last_lines_fetchloom_said(self):
        child = (
            "import sys\n"
            "for number in range(1000):\n"
            "    print(f'line {number}', file=sys.stderr)\n"
            "sys.exit(101)\n"
        )
        with patch.object(fetchloom, "_program", return_value="fetchloom"):
            with patch.object(fetchloom.subprocess, "Popen", side_effect=standing_in(child)):
                with patch.object(fetchloom.sys, "stderr", io.StringIO()) as shown:
                    with self.assertRaises(fetchloom.Error) as raised:
                        fetchloom.sync("p")
        self.assertEqual(shown.getvalue().splitlines(), [f"line {number}" for number in range(1000)])
        self.assertEqual((raised.exception.kind, raised.exception.code), (None, 101))
        message = raised.exception.message
        self.assertTrue(message.startswith("fetchloom exited with code 101 without a JSON answer. it said: "), message)
        self.assertTrue(message.endswith("line 999"), message)
        self.assertNotIn("line 0\n", message)


class PublicNames(unittest.TestCase):
    def test_dir_shows_the_helpers_but_not_the_modules_they_use(self):
        shown = dir(fetchloom)
        self.assertIn("sync", shown)
        hidden = [name for name in shown if name.startswith("_") or inspect.ismodule(getattr(fetchloom, name))]
        self.assertEqual(hidden, [])

    def test_the_version_is_the_installed_packages(self):
        with tempfile.TemporaryDirectory() as directory:
            installed = Path(directory) / "fetchloom-9.9.9.dist-info"
            installed.mkdir()
            (installed / "METADATA").write_text("Metadata-Version: 2.1\nName: fetchloom\nVersion: 9.9.9\n", encoding="utf-8")
            with patch.object(sys, "path", [directory, *sys.path]):
                self.assertEqual(fetchloom.__version__, "9.9.9")


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
                    with patch.object(fetchloom, "_on_path", return_value=fallback):
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

    def test_path_is_searched_in_its_absolute_folders_alone(self):
        # a cloned project may hold a program called fetchloom, which a search of the working
        # folder, a relative entry or an empty one would run
        name = "fetchloom.exe" if fetchloom.sys.platform == "win32" else "fetchloom"
        with tempfile.TemporaryDirectory() as root:
            clone, installed = Path(root, "clone"), Path(root, "installed")
            for folder in [clone, installed]:
                folder.mkdir()
                (folder / name).write_text("")
                (folder / name).chmod(0o755)
            # a helper with no program beside it, as from a source tree, so only PATH is left
            # even where an installed wheel bundles one
            unbundled = str(Path(root, "source", "fetchloom", "__init__.py"))
            here = os.getcwd()
            os.chdir(clone)
            try:
                for entries, found in [([".", "", str(installed)], str(installed / name)), (["."], None)]:
                    environment = {"PATH": os.pathsep.join(entries), "PATHEXT": ".exe"}
                    with self.subTest(entries=entries):
                        with patch.dict(os.environ, environment), \
                             patch.object(fetchloom, "__file__", unbundled), \
                             patch.object(fetchloom.sysconfig, "get_path", return_value=""):
                            if found is None:
                                with self.assertRaises(FileNotFoundError):
                                    fetchloom._program()
                            else:
                                self.assertEqual(fetchloom._program(), found)
            finally:
                os.chdir(here)

    def test_windows_counts_a_path_folder_absolute_only_with_a_drive_or_share_and_a_root(self):
        # as rust's Path::is_absolute reads windows paths, which python 3.12 and 3.13 read apart
        folders = ["\\tools", "C:tools", "tools", "", "C:\\tools", "\\\\lab\\share\\tools", "D:/tools"]
        tried = []

        def is_file(program):
            tried.append(program)
            return False

        with patch.dict(os.environ, {"PATH": ";".join(folders), "PATHEXT": ".EXE"}), \
             patch.object(fetchloom.os.path, "isfile", side_effect=is_file):
            self.assertIsNone(fetchloom._on_path(windows=True))
        self.assertEqual(
            tried,
            ["C:\\tools\\fetchloom.EXE", "\\\\lab\\share\\tools\\fetchloom.EXE", "D:/tools\\fetchloom.EXE"],
        )

    def test_user_scripts_follow_the_interpreters_preferred_scheme(self):
        with patch.object(fetchloom.sysconfig, "get_preferred_scheme", return_value="custom_user") as preferred:
            with patch.object(fetchloom.sysconfig, "get_path", return_value="") as get_path:
                with patch.object(Path, "is_file", return_value=False), \
                     patch.object(fetchloom, "_on_path", return_value="from-path"):
                    self.assertEqual(fetchloom._program(), "from-path")
        preferred.assert_called_once_with("user")
        get_path.assert_any_call("scripts", "custom_user")


class Launcher(unittest.TestCase):
    """``fl`` and ``python -m fetchloom`` hand everything to the program and return its exit code."""

    # stands in for fetchloom: it reports what it was given, writes to stderr and exits 3
    PROGRAM = (
        "import json, os, sys\n"
        "given = sys.stdin.read()\n"
        "print(json.dumps({'arguments': sys.argv[1:], 'stdin': given, 'process': os.getpid()}))\n"
        "print('said on stderr', file=sys.stderr)\n"
        "sys.exit(3)\n"
    )

    def starters(self):
        module, function = wheel_scripts.SCRIPTS["fl"].split(":")
        # the program found is this python, so no fetchloom build is needed
        found = "import runpy, sys, fetchloom\nfetchloom._program = lambda: sys.executable\n"
        return {
            "fl": f"{found}from {module} import {function}\nsys.exit({function}())\n",
            "python -m fetchloom": f"{found}runpy.run_module('fetchloom', run_name='__main__', alter_sys=True)\n",
        }

    def launch(self, starter, *arguments, stdin=b""):
        environment = dict(os.environ, PYTHONPATH=str(Path(fetchloom.__file__).resolve().parents[1]))
        process = subprocess.Popen(
            [sys.executable, "-c", starter, *arguments],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=environment,
        )
        stdout, stderr = process.communicate(stdin, timeout=60)
        return process, stdout, stderr.decode("utf-8")

    def test_arguments_stdin_and_exit_code_pass_through(self):
        arguments = ["two words", "", "--json", "\u00fc", "-"]
        for name, starter in self.starters().items():
            with self.subTest(name):
                process, stdout, stderr = self.launch(
                    starter, "-c", self.PROGRAM, *arguments, stdin=b"piped input\n"
                )
                self.assertEqual(process.returncode, 3, stderr)
                report = json.loads(stdout)
                self.assertEqual(report["arguments"], arguments)
                self.assertEqual(report["stdin"], "piped input\n")
                self.assertEqual(stderr.splitlines(), ["said on stderr"])
                if os.name == "posix":
                    # the program took over the launcher's process rather than running under it
                    self.assertEqual(report["process"], process.pid)

    def test_a_missing_program_exits_127_with_the_install_hint(self):
        starter = self.starters()["fl"].replace(
            "fetchloom._program = lambda: sys.executable",
            "def missing():\n    raise FileNotFoundError('the fetchloom program is missing')\n"
            "fetchloom._program = missing",
        )
        process, stdout, stderr = self.launch(starter)
        self.assertEqual(process.returncode, 127)
        self.assertEqual(stdout, b"")
        self.assertEqual(stderr.splitlines(), ["error: the fetchloom program is missing"])
