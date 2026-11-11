"""run the fetchloom program this package installs.

each function runs ``fetchloom --json`` and returns its answer, or raises :class:`Error`
with the kind and message of the JSON error. the kinds and their exit codes are listed in
https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/scripting.md.
"""

import json
import os
import shutil
import subprocess
import sys
import sysconfig
import warnings
from pathlib import Path
from typing import Any

__all__ = ["Error", "get", "path", "sync"]


def __dir__() -> list[str]:
    # tab completion and dir() show the helpers, not the modules they use
    return list(__all__)


def __getattr__(name: str) -> str:
    # the installed package's version, read only when asked, so importing stays quick
    if name == "__version__":
        from importlib.metadata import version

        return version("fetchloom")
    raise AttributeError(f"module 'fetchloom' has no attribute {name!r}")


class Error(Exception):
    """a fetchloom run that failed.

    ``kind`` is the JSON error's kind, such as ``usage`` or ``network.unreachable``, and
    ``message`` its summary. ``details`` lists supporting lines, ``help`` names the command
    or flag that fixes the error or is None, and ``code`` is the exit code. ``kind`` is None
    when fetchloom stopped without a JSON answer. ``str(error)`` reads like the command
    line's error: the message, then each detail and the help on a line of its own.
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
        lines = [self.message if self.kind is None else f"{self.kind}: {self.message}"]
        lines.extend(f"  {detail}" for detail in self.details)
        if self.help:
            lines.append(f"  help: {self.help}")
        return "\n".join(lines)


def path(
    name: str,
    project: str | os.PathLike[str] = ".",
    *,
    yes: bool = False,
    offline: bool = False,
    locked: bool = False,
) -> Path:
    """return the absolute path of a dataset, or of a file or folder inside one.

    ``name`` is a dataset name such as ``iris``, or a path inside a dataset such as
    ``iris/Iris.csv``. ``project`` is the project folder or any folder inside it. when the
    dataset is not in place yet, fetchloom syncs the project first and may download data.
    ``yes=True`` approves large downloads; ``offline=True`` prevents network requests.
    ``locked=True`` never changes data.lock: when that sync would, it raises :class:`Error`
    with kind ``lock.outdated``.
    """
    return Path(_run(_options(["path", name], yes, offline, locked), project)["path"])


def sync(
    project: str | os.PathLike[str] = ".",
    *,
    yes: bool = False,
    offline: bool = False,
    strict: bool = False,
    locked: bool = False,
) -> Path:
    """make the project's data folder match data.lock and return the project's path.

    ``project`` is the project folder or any folder inside it. files edited by hand stay,
    and each one fetchloom keeps is reported as a python warning; ``strict=True`` also raises
    :class:`Error` with kind ``conflict.kept``, listing them in ``details``, so a notebook
    never runs on inputs that differ from the lock. a file fetchloom put back as pinned, such
    as one linked to the cache's copy that was edited in place, matches the lock again: its
    warning says so and names the file that keeps the changed copy, and nothing is raised. a
    download above 1 GiB raises :class:`Error` with kind ``unconfirmed`` unless ``yes=True``
    approves it. ``offline=True`` prevents network requests. ``locked=True`` raises
    :class:`Error` with kind ``lock.outdated`` instead of updating a data.lock that is
    missing or out of date with data.toml.
    """
    result = _run(_options(["sync"], yes, offline, locked), project)
    placement = result.get("placement") or {}
    kept = [file["path"] for file in placement.get("kept", [])]
    if strict and kept:
        files = "1 file in the data folder differs" if len(kept) == 1 else f"{len(kept)} files in the data folder differ"
        raise Error(
            "conflict.kept",
            f"{files} from data.lock",
            kept,
            "save any changes you need, then run `fetchloom sync --force`",
        )
    return Path(result["project"])


def get(
    link: str,
    path: str | os.PathLike[str] | None = None,
    *,
    folder: str | os.PathLike[str] | None = None,
    sha256: str | None = None,
    force: bool = False,
    yes: bool = False,
    offline: bool = False,
) -> Path:
    """download a link once, without a project, and return where it went.

    ``link`` is a link, a DOI or an id such as ``zenodo:3608990``. with ``path``, the one
    file the link resolves to is written at exactly that path, writable, and that path is
    returned, so ``pd.read_csv(fetchloom.get(url, "data.csv"))`` works. a file already
    there that holds the same bytes stays as it is; one holding other bytes raises
    :class:`Error` with kind ``local.not_empty`` unless ``force=True`` replaces it. without
    ``path``, the download goes into a new folder named after it, or into ``folder``, and
    the folder is returned. ``sha256`` is the hash the one file must have; with ``path``, a
    file already holding it is kept without a download. ``yes=True`` approves large
    downloads; ``offline=True`` prevents network requests, and keeps a file at ``path`` that
    still holds what the link served last time.
    """
    if path is not None and folder is not None:
        raise Error(
            "usage",
            "get writes one file at path or fills a folder, so it takes path or folder, not both",
        )
    arguments = ["get"]
    if path is not None:
        arguments.append(f"--output={os.fspath(path)}")
    if folder is not None:
        arguments.append(f"--dir={os.fspath(folder)}")
    if sha256 is not None:
        arguments.append(f"--sha256={sha256}")
    if force:
        arguments.append("--force")
    _options(arguments, yes, offline, False)
    result = _run([*arguments, "--", link], ".")
    return Path(result["file"] if path is not None else result["folder"])


def _options(arguments: list[str], yes: bool, offline: bool, locked: bool) -> list[str]:
    for flag, wanted in [("--yes", yes), ("--offline", offline), ("--locked", locked)]:
        if wanted:
            arguments.append(flag)
    return arguments


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
        help = error["help"]
        if help:
            # the program quotes its own command line, which holds the flags this helper adds
            help = help.replace("`fetchloom --json ", "`fetchloom ")
            if os.fspath(project) == ".":
                help = help.replace("`fetchloom -C . ", "`fetchloom ")
        raise Error(error["kind"], error["message"], error["details"], help, finished.returncode)
    return answer["result"]


def _program() -> str:
    windows = sys.platform == "win32"
    name = "fetchloom.exe" if windows else "fetchloom"
    package = Path(__file__).resolve().parent
    # --target keeps scripts beside the package; --prefix puts them above site-packages.
    folders = [package.parent / "bin"]
    site = package.parent
    if site.name in {"site-packages", "dist-packages"}:
        prefix = site.parent
        if windows:
            if prefix.name.lower() == "lib":
                prefix = prefix.parent
            folders.append(prefix / "Scripts")
        else:
            if prefix.name.startswith("python"):
                prefix = prefix.parent
            if prefix.name in {"lib", "lib64"}:
                folders.append(prefix.parent / "bin")
    folders.extend([
        sysconfig.get_path("scripts"),
        sysconfig.get_path("scripts", vars={"base": sys.base_prefix}),
    ])
    try:
        folders.append(sysconfig.get_path("scripts", sysconfig.get_preferred_scheme("user")))
    except KeyError:
        pass
    for folder in folders:
        if folder and Path(folder, name).is_file():
            return str(Path(folder, name))
    found = shutil.which("fetchloom")
    if found is None:
        raise FileNotFoundError(
            "the fetchloom program is missing: install it as "
            "https://github.com/nikhil-yn-aix/fetchloom#install says, or put it on PATH"
        )
    return found
