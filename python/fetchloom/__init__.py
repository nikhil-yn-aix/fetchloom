"""run the fetchloom program this package installs.

each function runs ``fetchloom --json`` and returns its answer, or raises :class:`Error`
with the kind and message of the JSON error. the kinds and their exit codes are listed in
https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/agents.md.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import sysconfig
import warnings
from pathlib import Path
from typing import Any

__all__ = ["Error", "path", "sync"]


class Error(Exception):
    """a fetchloom run that failed.

    ``kind`` is the JSON error's kind, such as ``usage`` or ``network.unreachable``, and
    ``message`` its summary. ``details`` lists supporting lines, ``help`` names the command
    or flag that fixes the error or is None, and ``code`` is the exit code. ``kind`` is None
    when fetchloom stopped without a JSON answer.
    """

    def __init__(
        self,
        kind: str | None,
        message: str,
        details: list[str] | None = None,
        help: str | None = None,
        code: int | None = None,
    ) -> None:
        # every field goes to args, so the error survives pickling between processes
        super().__init__(kind, message, details, help, code)
        self.kind = kind
        self.message = message
        self.details = details or []
        self.help = help
        self.code = code

    def __str__(self) -> str:
        return self.message if self.kind is None else f"{self.kind}: {self.message}"


def path(
    name: str,
    project: str | os.PathLike[str] = ".",
    *,
    yes: bool = False,
    offline: bool = False,
) -> Path:
    """return the absolute path of a dataset, or of a file or folder inside one.

    ``name`` is a dataset name such as ``iris``, or a path inside a dataset such as
    ``iris/iris.csv``. ``project`` is the project folder or any folder inside it. when the
    dataset is not in place yet, fetchloom syncs the project first and may download data.
    ``yes=True`` approves large downloads; ``offline=True`` prevents network requests.
    """
    arguments = ["path", name]
    if yes:
        arguments.append("--yes")
    if offline:
        arguments.append("--offline")
    return Path(_run(arguments, project))


def sync(
    project: str | os.PathLike[str] = ".",
    *,
    yes: bool = False,
    offline: bool = False,
) -> Path:
    """make the project's data folder match data.lock and return the project's path.

    ``project`` is the project folder or any folder inside it. files edited by hand stay,
    and each one fetchloom keeps is reported as a python warning. a download above 1 GiB
    raises :class:`Error` with kind ``unconfirmed`` unless ``yes=True`` approves it.
    ``offline=True`` prevents network requests.
    """
    arguments = ["sync"]
    if yes:
        arguments.append("--yes")
    if offline:
        arguments.append("--offline")
    return Path(_run(arguments, project)["project"])


def _run(arguments: list[str], project: str | os.PathLike[str]) -> Any:
    finished = subprocess.run(
        [_program(), "--json", "-C", os.fspath(project), *arguments],
        stdin=subprocess.DEVNULL,
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        check=False,
    )
    try:
        answer = json.loads(finished.stdout)
    except ValueError:
        said = finished.stderr.strip() or "nothing"
        raise Error(
            None,
            f"fetchloom exited with code {finished.returncode} without a JSON answer. it said: {said}",
            code=finished.returncode,
        ) from None
    for warning in answer["warnings"]:
        warnings.warn(warning, stacklevel=3)
    if answer["status"] != "ok":
        error = answer["errors"][0]
        raise Error(error["kind"], error["message"], error["details"], error["help"], finished.returncode)
    return answer["result"]


def _program() -> str:
    name = "fetchloom.exe" if sys.platform == "win32" else "fetchloom"
    folders = [sysconfig.get_path("scripts")]
    try:
        folders.append(sysconfig.get_path("scripts", f"{os.name}_user"))
    except KeyError:
        pass
    # pip install --target puts the program in bin beside the package folder
    folders.append(str(Path(__file__).resolve().parent.parent / "bin"))
    for folder in folders:
        if folder and Path(folder, name).is_file():
            return str(Path(folder, name))
    found = shutil.which("fetchloom")
    if found is None:
        raise FileNotFoundError("the fetchloom program is missing. reinstall it with `pip install fetchloom`")
    return found
