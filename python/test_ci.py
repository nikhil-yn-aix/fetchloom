"""ci_tests.py against stand-ins for cargo, nextest, the test binaries and git: libtest and
nextest must select the same tests, and a run must execute every test it selected."""

import contextlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import ci_tests
from ci_tests import require_same, selected

CHMOD = "place::tests::a_writable_copy_works_where_the_mount_refuses_chmod"
LIB_ALIAS = "cli::sync::disk::tests::a_name_listed_in_another_case_stays_apart_until_a_record_joins_them"
CLI_ALIAS = "placing::a_file_renamed_in_another_case_still_matches_its_pin"
# each test binary's tests, with whether each is ignored. the chmod and alias runs take only
# their own tests, not others whose names begin with theirs
SUITES = {
    "fetchloom": {CHMOD: False, LIB_ALIAS: False, "place::tests::copies_bytes": False,
                  "net::tests::a_reset_is_tried_again": False, f"{CHMOD}_through_a_link": False,
                  "source::zenodo::tests::live_zenodo": True},
    "fetchloom::cli": {CLI_ALIAS: False, f"{CLI_ALIAS}_offline": False, "basics::get_has_its_own_help": False,
                       **{f"placing::places_file_{number}": False for number in range(8)}},
}
NATIVE = sorted(f"{binary}::{name}" for binary, tests in SUITES.items() for name, ignored in tests.items()
                if not ignored)
# a disk run leaves out the tests of modules that touch no disk, here net and basics
SELECTIONS = {
    "native": NATIVE,
    "disk": [name for name in NATIVE if not name.startswith(("fetchloom::net::", "fetchloom::cli::basics::"))],
    "chmod": [f"fetchloom::{CHMOD}"],
    "aliases": sorted([f"fetchloom::{LIB_ALIAS}", f"fetchloom::cli::{CLI_ALIAS}"]),
}


def matches(expression, name):
    """whether nextest's filterset ``expression``, of the shapes ci_tests.py writes, takes the test
    ``name``: ``test(x)`` takes a name holding x, and ``test(=x)`` only x itself."""
    if expression is None:
        return True
    if expression.startswith("not (") and expression.endswith(")"):
        return not matches(expression[len("not ("):-1], name)
    return any(name == term[len("test(="):-1] if term.startswith("test(=") else term[len("test("):-1] in name
               for term in expression.split(" | "))


def listed(tests, arguments):
    """what a libtest binary holding ``tests`` prints for ``arguments``: the tests whose names hold
    a filter, or are one with ``--exact``, but hold none ``--skip`` names, and with ``--ignored``
    the ignored ones alone."""
    assert arguments[:3] == ["--list", "--format", "terse"], arguments
    filters, skips, exact, ignored = [], [], False, False
    words = iter(arguments[3:])
    for word in words:
        if word == "--skip":
            skips.append(next(words))
        elif word in ("--exact", "--ignored"):
            exact, ignored = exact or word == "--exact", ignored or word == "--ignored"
        else:
            filters.append(word)

    def hit(name, pattern):
        return name == pattern if exact else pattern in name

    lines = [f"{name}: test" for name, is_ignored in tests.items()
             if (is_ignored or not ignored) and (not filters or any(hit(name, f) for f in filters))
             and not any(hit(name, skip) for skip in skips)]
    return "\n".join([*lines, *([] if ignored else ["throughput: bench"]), ""])


class Workspace:
    """answers each command ci_tests.py runs as cargo, nextest, libtest and git would for
    ``SUITES``, and keeps the commands."""

    def __init__(self, root):
        self.root = root
        self.commands = []
        self.binaries = {binary: root / "target/debug/deps" / binary.replace("::", "-") for binary in SUITES}
        self.listed = dict(self.binaries)
        self.hidden = set()
        self.code = 0
        self.unrun = set()
        self.report = True
        self.lock_changed = False
        self.snapshots = []
        self.env = None

    def __call__(self, command, cwd=None, env=None, check=False, **_):
        command = [str(word) for word in command]
        self.commands.append(command)
        code, out = 0, ""
        if command[:2] == ["cargo", "test"]:
            artifacts = [{"reason": "compiler-artifact", "executable": None, "profile": {"test": False}},
                         {"reason": "compiler-artifact", "executable": str(self.root / "target/debug/fetchloom"),
                          "profile": {"test": False}},
                         *({"reason": "compiler-artifact", "executable": str(path), "profile": {"test": True}}
                           for path in self.binaries.values()),
                         {"reason": "build-finished", "success": True}]
            out = "\n".join(json.dumps(artifact) for artifact in artifacts) + "\n"
        elif command[:3] == ["cargo-nextest", "nextest", "list"]:
            out = json.dumps({"rust-suites": {binary: {"binary-path": str(path), "testcases": {
                name: {"ignored": ignored, "filter-match": {
                    "status": "matches" if matches(self.expression(command), name)
                    and f"{binary}::{name}" not in self.hidden else "mismatch"}}
                for name, ignored in SUITES.get(binary, {"extra::test": False}).items()}}
                for binary, path in self.listed.items()}})
        elif command[:3] == ["cargo-nextest", "nextest", "run"]:
            code = self.run(command, env)
        elif command[0] == "git":
            code = int(self.lock_changed)
        else:
            binary = next(binary for binary, path in self.binaries.items() if str(path) == command[0])
            out = listed(SUITES[binary], command[1:])
        if check and code:
            raise subprocess.CalledProcessError(code, command)
        return subprocess.CompletedProcess(command, code, out, "")

    def expression(self, command):
        return command[command.index("-E") + 1] if "-E" in command else None

    def run(self, command, env):
        self.env = env
        Path(env["INSTA_SNAPSHOT_REFERENCES_FILE"]).write_text("".join(line + "\n" for line in self.snapshots),
                                                              encoding="utf-8")
        if self.report:
            ran = [(binary, name) for binary, tests in SUITES.items() for name, ignored in tests.items()
                   if not ignored and matches(self.expression(command), name) and f"{binary}::{name}" not in self.unrun]
            cases = "".join(f'<testcase classname="{binary}" name="{name}" time="{number}.5"/>'
                            for number, (binary, name) in enumerate(ran))
            junit = self.root / "target/nextest/ci/junit.xml"
            junit.parent.mkdir(parents=True, exist_ok=True)
            junit.write_text(f'<testsuites><testsuite name="ci">{cases}</testsuite></testsuites>', encoding="utf-8")
        return self.code


class SelectionTests(unittest.TestCase):
    def test_only_matching_nonignored_cases_count(self):
        def case(ignored=False, status="matches"):
            return {"ignored": ignored, "filter-match": {"status": status}}
        document = {"rust-suites": {"cli": {"testcases": {
            "kept": case(), "live": case(ignored=True), "filtered": case(status="mismatch")}}}}
        self.assertEqual(selected(document), {"cli::kept"})

    def test_empty_missing_and_extra_selections_fail(self):
        for expected, actual in [(set(), set()), ({"one"}, set()), ({"one"}, {"one", "two"})]:
            with self.subTest(expected=expected, actual=actual), self.assertRaises(RuntimeError):
                require_same(expected, actual, "filesystem")
        require_same({"one"}, {"one"}, "filesystem")


class RecordAndRun(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name).resolve()
        self.workspace = Workspace(self.root)

    def main(self, *arguments):
        """``ci_tests.py`` run with ``arguments`` in the workspace, and what it printed."""
        printed = io.StringIO()
        with patch.object(ci_tests, "ROOT", self.root), patch.object(ci_tests, "RESULTS", self.root / "target/ci"), \
             patch.object(ci_tests.subprocess, "run", self.workspace), \
             patch.object(sys, "argv", ["ci_tests.py", *arguments]), contextlib.redirect_stdout(printed):
            code = ci_tests.main()
        return code, printed.getvalue()

    def recorded(self):
        return json.loads((self.root / "target/ci/selection.json").read_text(encoding="utf-8"))

    def test_record_keeps_what_libtest_selects_for_each_run(self):
        self.assertEqual(self.main("record"), (0, "libtest selections: {'native': 16, 'disk': 14, 'chmod': 1, 'aliases': 2}\n"))
        self.assertEqual(self.recorded(), SELECTIONS)

    def test_record_refuses_test_binaries_cargo_and_nextest_list_apart(self):
        self.workspace.listed["fetchloom::extra"] = self.root / "target/debug/deps/fetchloom-extra"
        with self.assertRaisesRegex(RuntimeError, "cargo and nextest binaries differ"):
            self.main("record")
        self.assertFalse((self.root / "target/ci/selection.json").exists())

    def test_record_refuses_a_selection_nextest_makes_apart(self):
        self.workspace.hidden.add("fetchloom::cli::placing::places_file_3")
        with self.assertRaisesRegex(RuntimeError, r"native: 15 tests; missing \['fetchloom::cli::placing::places_file_3'\]"):
            self.main("record")

    def test_each_run_selects_what_record_kept_and_keeps_its_reports(self):
        self.main("record")
        snapshot = self.root / "tests/cli/snapshots/basics__help.snap"
        # windows names a snapshot with its long-path prefix
        self.workspace.snapshots = [str(snapshot), "\\\\?\\" + str(self.root / "tests/cli/snapshots/get__one.snap")]
        for scope, flags in [("native", []), ("xfs", ["--disk"]), ("chmod", ["--chmod"]), ("aliases", ["--aliases"])]:
            with self.subTest(scope=scope):
                code, printed = self.main("run", scope, *flags)
                kind = flags[0][2:] if flags else "native"
                self.assertEqual((code, printed), (0, f"{scope}: {len(SELECTIONS[kind])} selected tests match cargo/libtest\n"))
                folder = self.root / "target/ci" / scope
                self.assertEqual(json.loads((folder / "selected.json").read_text(encoding="utf-8")), SELECTIONS[kind])
                self.assertEqual(json.loads((folder / "snapshots.json").read_text(encoding="utf-8")),
                                 ["tests/cli/snapshots/basics__help.snap", "tests/cli/snapshots/get__one.snap"])
                self.assertEqual(sorted(path.name for path in folder.iterdir()), ["junit.xml", "selected.json", "snapshots.json"])
                # snapshots must match as committed, never be written or passed regardless
                self.assertEqual((self.workspace.env["INSTA_UPDATE"], self.workspace.env["INSTA_FORCE_PASS"]), ("no", "0"))
                self.assertEqual(self.workspace.commands[-1], ["git", "diff", "--exit-code", "--", "Cargo.lock"])

    def test_the_step_summary_lists_the_ten_slowest_tests(self):
        self.main("record")
        summary = self.root / "summary.md"
        summary.write_text("earlier steps\n", encoding="utf-8")
        with patch.dict(os.environ, {"GITHUB_STEP_SUMMARY": str(summary)}):
            self.main("run", "native")
        lines = summary.read_text(encoding="utf-8").splitlines()
        self.assertEqual(lines[:5], ["earlier steps", "### native: 16 tests match libtest", "", "| test | seconds |", "|---|---:|"])
        rows = [line.split(" | ") for line in lines[5:]]
        self.assertEqual(len(rows), 10)
        # the stand-in's last test takes longest
        self.assertEqual(rows[0], ["| placing::places_file_7", "15.5 |"])
        seconds = [float(row[1].removesuffix(" |")) for row in rows]
        self.assertEqual(seconds, sorted(seconds, reverse=True))

    def test_a_run_refuses_a_selection_other_than_the_recorded_one(self):
        self.main("record")
        self.workspace.hidden.add(f"fetchloom::{CHMOD}")
        with self.assertRaisesRegex(RuntimeError, "chmod: 0 tests"):
            self.main("run", "chmod", "--chmod")
        self.assertFalse(any(command[:3] == ["cargo-nextest", "nextest", "run"] for command in self.workspace.commands))

    def test_a_passing_run_must_have_executed_every_selected_test(self):
        self.main("record")
        self.workspace.unrun.add("fetchloom::cli::placing::places_file_0")
        with self.assertRaisesRegex(RuntimeError, r"xfs executed: 13 tests; missing \['fetchloom::cli::placing::places_file_0'\]"):
            self.main("run", "xfs", "--disk")
        # a failed run reports its failure through its exit code
        self.workspace.code = 100
        self.assertEqual(self.main("run", "xfs", "--disk")[0], 100)

    def test_a_passing_run_without_a_report_fails(self):
        self.main("record")
        self.workspace.report = False
        with self.assertRaisesRegex(RuntimeError, "a successful run produced no JUnit report"):
            self.main("run", "native")
        self.workspace.code = 100
        self.assertEqual(self.main("run", "native")[0], 100)

    def test_a_run_that_changed_cargo_lock_fails(self):
        self.main("record")
        self.workspace.lock_changed = True
        with self.assertRaises(subprocess.CalledProcessError):
            self.main("run", "native")

    def test_an_archive_run_lists_and_runs_the_archived_build(self):
        self.main("record")
        archive = self.root / "target/ci/windows-tests.tar.zst"
        self.main("run", "refs", "--disk", "--archive", str(archive))
        nextest = [command for command in self.workspace.commands[-2:] if command[0] == "cargo-nextest"]
        for command in nextest:
            with self.subTest(command=command[2]):
                at = command.index("--archive-file")
                self.assertEqual(command[at:at + 7], ["--archive-file", str(archive), "--extract-to", str(self.root),
                                                     "--extract-overwrite", "--workspace-remap", str(self.root)])
                # nothing is built again from source
                self.assertNotIn("--tests", command)


@unittest.skipIf(os.name == "nt", "stands in for nextest and git with scripts on PATH")
class Program(unittest.TestCase):
    def test_a_run_exits_with_nextests_own_code(self):
        # the only way failed tests fail the nextest step
        listing = {"rust-suites": {"fetchloom": {"binary-path": "fetchloom", "testcases": {
            CHMOD: {"ignored": False, "filter-match": {"status": "matches"}}}}}}
        with tempfile.TemporaryDirectory() as folder:
            # a copy in a checkout of its own, so its reports stay out of this one
            root = Path(folder)
            (root / "python").mkdir()
            shutil.copyfile(ci_tests.__file__, root / "python/ci_tests.py")
            (root / "target/ci").mkdir(parents=True)
            (root / "target/ci/selection.json").write_text(json.dumps({"chmod": [f"fetchloom::{CHMOD}"]}),
                                                         encoding="utf-8")
            tools = root / "tools"
            tools.mkdir()
            for name, program in [
                ("cargo-nextest", f"import sys\nif sys.argv[2] != 'list':\n    sys.exit(100)\nprint({json.dumps(listing)!r})\n"),
                ("git", ""),
            ]:
                (tools / name).write_text(f"#!{sys.executable}\n{program}", encoding="utf-8")
                (tools / name).chmod(0o755)
            result = subprocess.run([sys.executable, str(root / "python/ci_tests.py"), "run", "chmod", "--chmod"],
                                    env={**os.environ, "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}"},
                                    capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 100, result.stdout + result.stderr)
        self.assertEqual(result.stdout, "chmod: 1 selected tests match cargo/libtest\n")
