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
