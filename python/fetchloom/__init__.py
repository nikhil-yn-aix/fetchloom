"""run the fetchloom program this package installs.

each function runs ``fetchloom --json --progress`` and returns its answer, or raises
:class:`Error` with the kind and message of the JSON error. the kinds and their exit codes are
listed in https://github.com/nikhil-yn-aix/fetchloom/blob/main/docs/user/scripting.md. the
progress lines a long download prints go to ``sys.stderr`` as they come.
"""

import collections
import json
import os
import re
import shutil
import subprocess
import sys
import sysconfig
import threading
import warnings
from collections.abc import Iterable
from pathlib import Path
from typing import IO, Any

__all__ = ["Error", "get", "path", "sync"]

# the flags the helpers pass for their keywords: a switch, as --force for force=True, and a flag
# that takes a value, as -o for path
_SWITCHES = {
    "--force": "force",
    "--yes": "yes",
    "--offline": "offline",
    "--locked": "locked",
    "--verify": "verify",
}
_VALUES = {"-o": "path", "--output": "path", "--dir": "folder", "--sha256": "sha256", "-C": "project"}
# an example or command in backticks, or input fetchloom quotes, as in `found no dataset matching "x"`
_QUOTED = re.compile(r'`[^`]*`|"[^"]*"')
# how fetchloom says that no folder from -C upward holds a project
_NO_PROJECT = "there is no data.toml in this folder or any folder above it"
# how many of the last lines fetchloom said on stderr an error without a JSON answer repeats
_TAIL = 20


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
    or flag that fixes the error or is None, and ``code`` is the exit code, or None when the
    helper raised the error itself, with no process to exit. ``kind`` is None
    when fetchloom stopped without a JSON answer, and ``message`` then ends with the last
    lines it said on stderr. ``str(error)`` reads like the command
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
    dataset is not in place yet, fetchloom syncs that dataset alone first and may download its
    data.
    ``yes=True`` approves large downloads; ``offline=True`` prevents network requests.
    ``locked=True`` never changes data.lock: when that sync would, it raises :class:`Error`
    with kind ``lock.outdated``.
    """
    arguments = _options(["path"], yes, offline, locked)
    return Path(_run([*arguments, "--", name], project, {"project", "yes", "offline", "locked"})["path"])


def sync(
    project: str | os.PathLike[str] = ".",
    *,
    names: str | Iterable[str] = (),
    yes: bool = False,
    offline: bool = False,
    strict: bool = False,
    locked: bool = False,
    verify: bool = False,
) -> Path:
    """make the project's data folder match data.lock and return the project's path.

    ``project`` is the project folder or any folder inside it. ``names``, such as ``["iris"]``
    or ``"iris"``, syncs those datasets alone: the others keep their entries in data.lock and
    their files as they are, and the next sync without names still checks them all. files
    edited by hand stay, and each one fetchloom keeps is reported as a python warning;
    ``strict=True`` also raises :class:`Error` with kind ``conflict.kept``, listing them in
    ``details``, so a notebook never runs on inputs that differ from the lock. a file
    fetchloom put back as pinned, such as one linked to the cache's copy that was edited in
    place, matches the lock again: its warning says so and names the file that keeps the
    changed copy, and nothing is raised. a download above 1 GiB raises :class:`Error` with
    kind ``unconfirmed`` unless ``yes=True`` approves it. ``offline=True`` prevents network
    requests. ``locked=True`` raises :class:`Error` with kind ``lock.outdated`` instead of
    updating a data.lock that is missing or out of date with data.toml, for the named
    datasets alone when ``names`` names some, and with kind ``build.needed`` when data.lock
    does not pin a recipe as building it now would make, which only ``fetchloom build``
    changes. ``verify=True`` checks the SHA-256 of every placed file, as ``sync --verify``
    does, and puts back one whose bytes changed though its size and time did not.
    """
    arguments = _options(["sync"], yes, offline, locked)
    if verify:
        arguments.append("--verify")
    named = [names] if isinstance(names, str) else list(names)
    if named:
        arguments += ["--", *named]
    result = _run(arguments, project, {"project", "yes", "offline", "locked", "verify"})
    placement = result.get("placement") or {}
    kept = [file["path"] for file in placement.get("kept", [])]
    if strict and kept:
        files = "1 file in the data folder differs" if len(kept) == 1 else f"{len(kept)} files in the data folder differ"
        # a person may run it from another folder, such as a notebook's own project
        words = ["fetchloom"] if os.fspath(project) == "." else ["fetchloom", "-C", result["project"]]
        words += ["sync", "--force", *(["--", *named] if named else [])]
        force = " ".join(_shell_word(word) for word in words)
        raise Error(
            "conflict.kept",
            f"{files} from data.lock",
            kept,
            f"save any changes you need, then run `{force}`",
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
    returned, so ``pd.read_csv(fetchloom.get(url, "data.csv"))`` works. a leading ``~`` in
    ``path`` or ``folder`` is your home folder, as pandas reads it. a file already there that
    holds the same bytes stays as it is; one holding other bytes raises :class:`Error` with
    kind ``local.not_empty`` unless ``force=True``, which needs ``path``, replaces it.
    without ``path``, the download goes into a new folder named after it, or into
    ``folder``, and the folder is returned. ``sha256`` is the hash the one file must have; with ``path``, a
    file already holding it is kept without a download. ``yes=True`` approves large
    downloads; ``offline=True`` prevents network requests, and keeps a file at ``path`` that
    still holds what the link served last time.
    """
    if path is not None and folder is not None:
        raise Error(
            "usage",
            "get writes one file at path or fills a folder, so it takes path or folder, not both",
        )
    if force and path is None:
        raise Error("usage", "force=True replaces only the file at path, so it needs path")
    arguments = ["get"]
    if path is not None:
        arguments.append(f"--output={_local(path)}")
    if folder is not None:
        arguments.append(f"--dir={_local(folder)}")
    if sha256 is not None:
        arguments.append(f"--sha256={sha256}")
    if force:
        arguments.append("--force")
    _options(arguments, yes, offline, False)
    keywords = {"path", "folder", "sha256", "force", "yes", "offline"}
    result = _run([*arguments, "--", link], ".", keywords)
    return Path(result["file"] if path is not None else result["folder"])


def _local(path: str | os.PathLike[str]) -> str:
    # pandas expands ~ in the paths it is given, and no shell does it for a python call
    return os.path.expanduser(os.fspath(path))


def _shell_word(word: str, windows: bool = sys.platform == "win32") -> str:
    """``word`` quoted as fetchloom quotes the commands it suggests, so a shell reads it as typed."""
    if word and all(c.isascii() and (c.isalnum() or c in "-_./:@%+=,~") for c in word):
        return word
    if any(c in word for c in "'\"$`\\"):
        quoted = word.replace("'", "''") if windows else word.replace("'", "'\"'\"'")
        return f"'{quoted}'"
    return f'"{word}"'


def _options(arguments: list[str], yes: bool, offline: bool, locked: bool) -> list[str]:
    for flag, wanted in [("--yes", yes), ("--offline", offline), ("--locked", locked)]:
        if wanted:
            arguments.append(flag)
    return arguments


def _run(arguments: list[str], project: str | os.PathLike[str], keywords: set[str]) -> Any:
    finished = _call([_program(), "--json", "--progress", "-C", _local(project), *arguments])
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
        warnings.warn(_spoken(warning, keywords), stacklevel=3)
    if answer["status"] != "ok":
        error = answer["errors"][0]
        help = error["help"]
        if error["kind"] == "project.invalid" and error["message"].startswith(_NO_PROJECT):
            # a script or notebook often runs in another folder than its project's
            help = _no_project(project)
        elif help:
            # the program quotes its own command line, which holds the flags this helper adds
            for own in ["--json --progress ", "--json ", "--progress "]:
                help = help.replace(f"`fetchloom {own}", "`fetchloom ")
            if os.fspath(project) == ".":
                help = help.replace("`fetchloom -C . ", "`fetchloom ")
            help = _spoken(help, keywords)
        message = _spoken(error["message"], keywords)
        details = [_spoken(detail, keywords) for detail in error["details"]]
        raise Error(error["kind"], message, details, help, finished.returncode)
    return answer["result"]


def _call(argv: list[str]) -> subprocess.CompletedProcess[str]:
    """run ``argv`` and return its whole stdout and the last :data:`_TAIL` lines of its stderr.

    each line it writes on stderr, such as a progress line, goes on to ``sys.stderr`` as it
    comes. as with :func:`subprocess.run`, an interrupt or any other exception while it runs
    stops the program before the exception goes on.
    """
    tail: collections.deque[str] = collections.deque(maxlen=_TAIL)
    answer: list[str] = []
    with subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        encoding="utf-8",
        errors="replace",
    ) as process:
        readers = [
            threading.Thread(target=lambda: answer.append(process.stdout.read()), daemon=True),
            threading.Thread(target=_forward, args=(process.stderr, tail), daemon=True),
        ]
        for reader in readers:
            reader.start()
        try:
            process.wait()
        except BaseException:
            process.kill()
            raise
        finally:
            for reader in readers:
                reader.join()
    return subprocess.CompletedProcess(argv, process.returncode, "".join(answer), "".join(tail))


def _forward(stream: IO[str], tail: collections.deque[str]) -> None:
    """passes each line of ``stream`` to ``sys.stderr`` as it comes, keeping the last in ``tail``."""
    for line in stream:
        tail.append(line)
        try:
            sys.stderr.write(line)
            sys.stderr.flush()
        except (AttributeError, OSError, ValueError):
            # no stderr, as under pythonw, or a closed one: the program must still be read
            pass


def _no_project(project: str | os.PathLike[str]) -> str:
    """the help for a ``project`` that holds no data.toml, and has none above it."""
    folder = _local(project)
    add = "fetchloom add <link>" if os.fspath(project) == "." else f"fetchloom -C {_shell_word(folder)} add <link>"
    return (
        "give project= the folder that holds data.toml, "
        f"or run `{add}` to start a project in {os.path.abspath(folder)}"
    )


def _spoken(text: str, keywords: set[str]) -> str:
    """``text`` with each flag a helper sets for one of ``keywords`` named as a python caller
    sets it: ``add --force`` reads ``pass force=True``, ``-o`` reads ``the path argument``, and
    an example in backticks, as `` `-o results/iris.csv` `` or `` `--output=results/iris.csv` ``,
    reads `` `path='results/iris.csv'` ``. a command in backticks stays as a person types it at
    a terminal, and input fetchloom quotes, as ``"--dir=zzz"``, stays as it was given.
    """
    switches = {flag: name for flag, name in _SWITCHES.items() if name in keywords}
    values = {flag: name for flag, name in _VALUES.items() if name in keywords}
    if not switches and not values:
        return text
    longest = sorted([*switches, *values], key=len, reverse=True)
    found = re.compile(rf"(add )?(?<![\w-])({'|'.join(map(re.escape, longest))})(?![\w-])")

    def named(match: re.Match[str]) -> str:
        added, flag = match.group(1), match.group(2)
        if flag in switches:
            return f"{'pass ' if added else ''}{switches[flag]}=True"
        return f"{added or ''}the {values[flag]} argument"

    spoken = []
    prose = 0
    for quoted in _QUOTED.finditer(text):
        spoken.append(found.sub(named, text[prose:quoted.start()]))
        part = quoted.group()
        spoken.append(f"`{_example(part[1:-1], switches, values)}`" if part.startswith("`") else part)
        prose = quoted.end()
    spoken.append(found.sub(named, text[prose:]))
    return "".join(spoken)


def _example(code: str, switches: dict[str, str], values: dict[str, str]) -> str:
    """``code`` from backticks as keyword arguments when it is flags alone, else as it is."""
    words = code.split()
    spoken = []
    while words:
        word = words.pop(0)
        flag, equals, value = word.partition("=")
        if word in switches:
            spoken.append(f"{switches[word]}=True")
        elif equals and flag in values:
            spoken.append(f"{values[flag]}={value!r}")
        elif word in values and words:
            spoken.append(f"{values[word]}={words.pop(0)!r}")
        else:
            return code
    return ", ".join(spoken) if spoken else code


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
