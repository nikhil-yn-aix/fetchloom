"""snapshot coverage is the union of native and capability-specific runs, over the snapshots git
tracks."""

import contextlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import check_snapshots
from check_snapshots import require_referenced


class SnapshotTests(unittest.TestCase):
    def test_a_reflink_only_snapshot_is_kept_when_another_run_references_it(self):
        require_referenced({"ordinary.snap", "reflink.snap"}, [{"ordinary.snap"}, {"reflink.snap"}])

    def test_unreferenced_unknown_and_missing_reports_are_rejected(self):
        for expected, reports in [({"unused.snap"}, [set()]), ({"known.snap"}, [{"other.snap"}]), ({"known.snap"}, []), (set(), [set()])]:
            with self.subTest(expected=expected, reports=reports), self.assertRaises(ValueError):
                require_referenced(expected, reports)


@unittest.skipUnless(shutil.which("git"), "lists the tracked snapshots with git")
class TrackedSnapshots(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.root = Path(folder.name)
        subprocess.run(["git", "init", "-q", str(self.root)], check=True)
        self.reports = self.root / "reports"

    def write(self, *names, track=True):
        for name in names:
            (self.root / name).parent.mkdir(parents=True, exist_ok=True)
            (self.root / name).write_text("", encoding="utf-8")
        if track:
            subprocess.run(["git", "add", "--", *names], cwd=self.root, check=True)

    def report(self, run, *names):
        folder = self.reports / run
        folder.mkdir(parents=True)
        (folder / "snapshots.json").write_text(json.dumps(names), encoding="utf-8")

    def check(self):
        printed = io.StringIO()
        with patch.object(check_snapshots, "ROOT", self.root), \
             patch.object(sys, "argv", ["check_snapshots.py", str(self.reports)]), contextlib.redirect_stdout(printed):
            check_snapshots.main()
        return printed.getvalue()

    def test_every_tracked_snapshot_is_referenced_by_some_run(self):
        self.write("tests/cli/snapshots/basics__help.snap", "tests/cli/snapshots/placing__reflink.snap", "README.md")
        # a snapshot a run left behind is not in the repository, so no run needs to reference it
        self.write("tests/cli/snapshots/scratch.snap", track=False)
        self.report("tests-ubuntu-24.04-1/native", "tests/cli/snapshots/basics__help.snap")
        self.report("tests-ubuntu-24.04-1/xfs", "tests/cli/snapshots/placing__reflink.snap")
        self.assertEqual(self.check(), "all 2 snapshots were referenced across 2 test runs\n")

    def test_a_tracked_snapshot_no_run_referenced_fails(self):
        self.write("tests/cli/snapshots/basics__help.snap", "tests/cli/snapshots/placing__reflink.snap")
        self.report("tests-macos-15-1/native", "tests/cli/snapshots/basics__help.snap")
        with self.assertRaisesRegex(ValueError, r"unreferenced snapshots: \['tests/cli/snapshots/placing__reflink.snap'\]"):
            self.check()

    def test_a_pending_snapshot_must_not_be_committed(self):
        for pending in ["tests/cli/snapshots/basics__help.snap.new", "tests/cli/snapshots/.basics__help.pending-snap"]:
            with self.subTest(pending=pending):
                self.write("tests/cli/snapshots/basics__help.snap", pending)
                with self.assertRaisesRegex(ValueError, "pending snapshots must not be committed"):
                    self.check()
                subprocess.run(["git", "rm", "-q", "--cached", "--", pending], cwd=self.root, check=True)
