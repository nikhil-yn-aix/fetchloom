"""action.yml keeps fetchloom's cache in github's cache only as its cache input allows.

github lets every pull request's run, a fork's too, restore what a run on the default branch
saved, so a saved cache is readable by anyone who can open a pull request on a public
repository. each test walks the action's own steps as github runs them on linux: a step runs
when its condition holds, a bash step runs with a stand-in fetchloom whose answers the test
fixes, and the cache actions are played by a store that records what was saved.
"""

import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
ACTION = ROOT / "action.yml"
PROJECT = "/work/project"

# the stand-in notes each command line and answers from the files the test wrote: push's from
# push-after.json once a sync ran, as the cache then holds what the sync downloaded
STAND_IN = """#!/bin/sh
printf '%s\\n' "$*" >> "$ANSWERS/asked"
case " $* " in
  *" settings "*) cat "$ANSWERS/settings.json" ;;
  *" push "*)
    if grep -q ' sync ' "$ANSWERS/asked"; then answer=push-after; else answer=push; fi
    cat "$ANSWERS/$answer.json"
    exit "$(cat "$ANSWERS/$answer.code")"
    ;;
  *" cache gc "*) ;;
  *" cache "*) cat "$ANSWERS/cache.json" ;;
  *" sync "*) ;;
  *) echo "the stand-in has no answer for: $*" >&2; exit 99 ;;
esac
"""


def document(command, result, status="ok", errors=()):
    """fetchloom's --json document, as compact as fetchloom writes it."""
    return json.dumps({"schema": 1, "command": command, "status": status, "result": result,
                       "warnings": [], "errors": list(errors), "work": {}}, separators=(",", ":"))


def asking(*asks):
    """push --dry-run's document, asking about each (dataset, access) of ``asks``."""
    return document("push", {"target": "/unwritten", "started": True, "datasets": ["iris", "vault"],
                             "asks": [{"dataset": name, "access": access} for name, access in asks],
                             "dry_run": True})


def serving(*projects):
    """the cache report's document, the cache serving ``projects``."""
    return document("cache", {"path": "/cache", "objects": 2, "bytes": 10, "projects": list(projects),
                              "waiting": []})


def action_steps():
    """each step of action.yml: its keys, with env and with as maps and run as its script."""
    lines = ACTION.read_text(encoding="utf-8").split("\n  steps:\n", 1)[1].splitlines()
    steps, at = [], 0
    while at < len(lines):
        line = lines[at]
        if line.startswith("    - "):
            steps.append({})
            line = "      " + line[6:]
        found = re.match(r"      ([\w-]+):(?: (.*))?$", line)
        if steps and found:
            key, value = found.group(1), found.group(2) or ""
            if key in ("env", "with"):
                mapping = {}
                while at + 1 < len(lines) and lines[at + 1].startswith("        "):
                    at += 1
                    name, _, given = lines[at].strip().partition(": ")
                    mapping[name] = given
                steps[-1][key] = mapping
            elif key == "run" and value.split(" #")[0].strip() == "|":
                script = []
                while at + 1 < len(lines) and (lines[at + 1].startswith("        ") or not lines[at + 1].strip()):
                    at += 1
                    script.append(lines[at][8:])
                steps[-1][key] = "\n".join(script) + "\n"
            else:
                steps[-1][key] = value
        at += 1
    return steps


def defaults():
    """each input action.yml declares, with its default."""
    text = ACTION.read_text(encoding="utf-8").split("\ninputs:\n", 1)[1].split("\noutputs:\n", 1)[0]
    return {name: given.strip("'") for name, given in re.findall(r"(?m)^  ([\w-]+):\n(?:    .*\n)*?    default: (.*)$", text)}


def lookup(reference, context):
    value = context
    for part in reference.split("."):
        value = value.get(part, "") if isinstance(value, dict) else ""
    return value if isinstance(value, str) else ""


def expanded(value, context):
    return re.sub(r"\$\{\{\s*([^}]*?)\s*\}\}", lambda found: lookup(found.group(1), context), value)


def holds(condition, context):
    """whether a step's condition holds: comparisons of a context value with a quoted word, joined
    by &&, the only form action.yml uses; any other form fails the test rather than guess."""
    if not condition:
        return True
    held = True
    for part in condition.split("&&"):
        found = re.fullmatch(r"\s*([\w.-]+) (==|!=) '([^']*)'\s*", part)
        if found is None:
            raise AssertionError(f"the test reads no condition like {condition!r}")
        equal = lookup(found.group(1), context).lower() == found.group(3).lower()
        held = held and equal == (found.group(2) == "==")
    return held


@unittest.skipIf(os.name == "nt", "github runs the action's bash steps in git bash; the windows action lane runs them")
class CacheTests(unittest.TestCase):
    def walk(self, cache=None, visibility="public", push=None, push_after=None, push_code=0,
             projects=(PROJECT,), hit=False):
        """the action's run with the ``cache`` input, in a repository of ``visibility``, fetchloom
        answering push --dry-run with ``push`` before a sync and ``push_after`` once one ran, and
        its cache serving ``projects``; with ``hit``, github's cache holds the run's exact key."""
        folder = Path(self.enterContext(tempfile.TemporaryDirectory()))
        answers, tools, temp = folder / "answers", folder / "bin", folder / "runner"
        for made in (answers, tools, temp):
            made.mkdir()
        nothing = asking()
        files = {"settings.json": document("settings", {"settings": [{"name": "cache", "value": str(folder / "cache"),
                                                                       "origin": "default"}]}),
                 "push.json": push or nothing, "push.code": str(push_code),
                 "push-after.json": push_after or push or nothing, "push-after.code": str(push_code),
                 "cache.json": serving(*projects), "asked": ""}
        for name, text in files.items():
            (answers / name).write_text(text, encoding="utf-8")
        (tools / "fetchloom").write_text(STAND_IN, encoding="utf-8")
        (tools / "fetchloom").chmod(0o755)
        (folder / "work/project").mkdir(parents=True)
        (folder / "work/project/data.lock").write_text("version = 1\n", encoding="utf-8")
        inputs = defaults()
        inputs["project"] = str(folder / "work/project")
        if cache is not None:
            inputs["cache"] = cache
        context = {"inputs": inputs, "steps": {}, "runner": {"os": "Linux"},
                   "github": {"action_path": str(ROOT), "event": {"repository": {"visibility": visibility}}}}
        system = [entry for entry in os.environ.get("PATH", "").split(os.pathsep) if os.path.isabs(entry)]
        run = {"ran": [], "saved": [], "said": "", "failed": None}
        for step in action_steps():
            if not holds(step.get("if", ""), context):
                continue
            run["ran"].append(step["name"])
            outputs = {}
            if step.get("uses", "").startswith("actions/cache/restore@"):
                outputs["cache-hit"] = "true" if hit else ""
            elif step.get("uses", "").startswith("actions/cache/save@"):
                run["saved"].append(expanded(step["with"]["key"], context))
            elif step["name"] == "install fetchloom":
                continue
            else:
                self.assertEqual(step.get("shell"), "bash", step["name"])
                said = folder / "output"
                said.write_text("", encoding="utf-8")
                env = {"PATH": os.pathsep.join([str(tools), *system]), "HOME": str(folder), "ANSWERS": str(answers),
                       "RUNNER_TEMP": str(temp), "RUNNER_OS": "Linux", "RUNNER_ARCH": "X64", "GITHUB_OUTPUT": str(said),
                       "LC_ALL": "C"}
                env.update({name: expanded(value, context) for name, value in step.get("env", {}).items()})
                script = folder / "step.sh"
                script.write_text(step["run"], encoding="utf-8")
                done = subprocess.run([shutil.which("bash"), "--noprofile", "--norc", "-eo", "pipefail", str(script)],
                                      env=env, cwd=folder / "work", capture_output=True, text=True, timeout=60)
                run["said"] += done.stdout
                if done.returncode != 0:
                    run["failed"] = (step["name"], done.stdout + done.stderr)
                    break
                outputs = dict(line.split("=", 1) for line in said.read_text(encoding="utf-8").splitlines() if "=" in line)
            if "id" in step:
                context["steps"][step["id"]] = {"outputs": outputs}
        run["asked"] = (answers / "asked").read_text(encoding="utf-8").splitlines()
        run["notices"] = [line for line in run["said"].splitlines() if line.startswith("::notice")]
        return run

    def assertKept(self, run):
        self.assertIsNone(run["failed"])
        self.assertIn("restore the cache", run["ran"])
        self.assertEqual(len(run["saved"]), 1, run["ran"])
        self.assertEqual(run["notices"], [])

    def test_by_default_a_public_repository_keeps_no_cache_of_data_not_public(self):
        run = self.walk(push=asking(("iris", "unknown"), ("vault", "login")))
        self.assertIsNone(run["failed"])
        self.assertNotIn("restore the cache", run["ran"])
        self.assertEqual(run["saved"], [])
        self.assertEqual(len(run["notices"]), 1, run["said"])
        for named in ("iris", "vault", "always", "never"):
            self.assertIn(named, run["notices"][0])

    def test_the_question_is_a_dry_run_that_writes_nothing(self):
        run = self.walk(cache="auto")
        pushes = [line for line in run["asked"] if " push " in f" {line} "]
        self.assertEqual(len(pushes), 2, run["asked"])
        for line in pushes:
            self.assertIn("--dry-run", line.split())
            self.assertIn("--offline", line.split())

    def test_a_cache_of_public_data_no_key_brought_is_kept(self):
        self.assertKept(self.walk(cache="auto"))

    def test_a_private_repository_keeps_its_cache_without_asking(self):
        run = self.walk(cache="auto", visibility="private", push=asking(("vault", "login")))
        self.assertKept(run)
        self.assertFalse([line for line in run["asked"] if " push " in f" {line} "], run["asked"])

    def test_an_internal_repository_or_an_event_without_one_counts_as_public(self):
        for visibility in ("internal", ""):
            with self.subTest(visibility=visibility):
                run = self.walk(visibility=visibility, push=asking(("vault", "login")))
                self.assertNotIn("restore the cache", run["ran"])
                self.assertEqual(run["saved"], [])
                self.assertEqual(len(run["notices"]), 1, run["said"])

    def test_files_a_key_brought_in_this_run_are_not_saved(self):
        run = self.walk(cache="auto", push_after=asking(("iris", "login")))
        self.assertIn("restore the cache", run["ran"])
        self.assertEqual(run["saved"], [])
        self.assertEqual(len(run["notices"]), 1, run["said"])
        self.assertIn("iris", run["notices"][0])

    def test_a_cache_other_projects_use_is_not_saved(self):
        for visibility in ("public", "private"):
            with self.subTest(visibility=visibility):
                run = self.walk(cache="auto", visibility=visibility, projects=(PROJECT, "/work/other"))
                self.assertEqual(run["saved"], [])
                self.assertEqual(len(run["notices"]), 1, run["said"])
                self.assertIn("other projects", run["notices"][0])

    def test_always_keeps_whatever_the_data(self):
        run = self.walk(cache="always", push=asking(("vault", "login")), projects=(PROJECT, "/work/other"))
        self.assertKept(run)
        self.assertFalse([line for line in run["asked"] if " push " in f" {line} "], run["asked"])

    def test_never_restores_and_saves_nothing_and_says_nothing(self):
        run = self.walk(cache="never", visibility="private")
        self.assertIsNone(run["failed"])
        self.assertNotIn("restore the cache", run["ran"])
        self.assertNotIn("keep only what this data.lock needs", run["ran"])
        self.assertEqual(run["saved"], [])
        self.assertEqual(run["notices"], [])
        self.assertFalse([line for line in run["asked"] if re.search(r" (push|cache) ", f" {line} ")], run["asked"])

    def test_a_cache_that_came_back_whole_is_neither_cleaned_nor_saved_again(self):
        run = self.walk(cache="auto", hit=True)
        self.assertIn("restore the cache", run["ran"])
        self.assertNotIn("keep only what this data.lock needs", run["ran"])
        self.assertEqual(run["saved"], [])

    def test_a_question_fetchloom_cannot_answer_keeps_no_cache(self):
        failed = document("push", None, status="error",
                          errors=[{"kind": "lock.outdated", "message": "data.lock is out of date"}])
        run = self.walk(cache="auto", push=failed, push_code=1)
        self.assertNotIn("restore the cache", run["ran"])
        self.assertEqual(run["saved"], [])
        self.assertEqual(len(run["notices"]), 1, run["said"])

    def test_a_cache_input_other_than_the_three_fails_the_run(self):
        run = self.walk(cache="sometimes")
        self.assertIsNotNone(run["failed"])
        self.assertIn("::error::", run["failed"][1])
        self.assertIn("auto, always or never", run["failed"][1])
        self.assertEqual(run["saved"], [])

    def test_each_project_keeps_a_cache_of_its_own(self):
        first, second = self.walk(cache="auto"), self.walk(cache="auto")
        self.assertNotEqual(first["saved"], second["saved"], "two project folders share a key")


if __name__ == "__main__":
    unittest.main()
