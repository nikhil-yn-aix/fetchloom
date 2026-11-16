"""run the fetchloom program this package installs.

each helper runs ``fetchloom --json --progress`` and returns a :class:`pathlib.Path`, or raises
:class:`Error` with the kind and message of the JSON error. each warning goes on as a python
warning, and the progress lines a long download prints go to ``sys.stderr`` as they come.
https://nikhil-yn-aix.github.io/fetchloom/user/reference/python-helper/ describes the helpers, and
https://nikhil-yn-aix.github.io/fetchloom/user/reference/exit-codes/ lists the kinds with their exit
codes.
"""

import collections
import json
import ntpath
import os
import posixpath
import re
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
    "--strict": "strict",
}
_VALUES = {"-o": "path", "--output": "path", "--dir": "folder", "--sha256": "sha256", "-C": "project"}
# flags no helper takes, as help names them, each with what a python caller does instead
_UNTAKEN = {
    "add --web-page": "run `fetchloom get --web-page` at a terminal, as get() has no keyword for it",
    "`--mirror <folder>`": "the FETCHLOOM_MIRRORS variable set to <folder>",
}
# an example or command in backticks, or input fetchloom quotes, as in `found no dataset matching "x"`
_QUOTED = re.compile(r'`[^`]*`|"[^"]*"')
# a fetchloom command in backticks, which fetchloom words with the arguments the helper gave it
_COMMAND = re.compile(r"`fetchloom ([^`]*)`")
# how fetchloom says that no folder from -C upward holds a project
_NO_PROJECT = "there is no data.toml in this folder or any folder above it"
# how many of the last lines fetchloom said on stderr an error without a JSON answer repeats
_TAIL = 20
# whether fetchloom words the commands it suggests for powershell and cmd, as on windows
_WINDOWS = sys.platform == "win32"
# the marks fetchloom puts around a word it withholds from a command, as src/suggested.rs does
_MARKS = frozenset("\uf8e0\uf8e1")
# what fetchloom withholds from a command on windows, as src/suggested.rs lists it: what cmd acts
# on even inside quotes, and what powershell's quoting cannot keep, a double quote straight or curly
_UNSAFE_ON_WINDOWS = frozenset('&|<>^%!$`"\u201c\u201d\u201e') | _MARKS
# the line fetchloom adds for each word it withheld, naming it by the placeholder it shows instead
_WITHHELD = re.compile(r"(<(?:file|folder|name)(?: \d+)?>) is (.*), which no command here can hold safely")
# control characters and the bidi controls, as src/name.rs lists them
_CONTROL = re.compile("[\x00-\x1f\x7f-\x9f\u061c\u200e\u200f\u202a-\u202e\u2066-\u2069]")


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
    ``message`` its summary. ``details`` lists supporting lines, ``help`` is advice for a person
    or None, and ``code`` is the exit code. ``kind`` is None when fetchloom stopped without a JSON
    answer, and ``message`` then ends with the last lines it said on stderr. ``code`` is None when
    the helper raised the error itself, with no process to exit. ``result`` is the JSON result
    fetchloom gave with the error, as a sync that placed some datasets lists them in
    ``datasets`` and those that wait in ``waiting``, or None. ``str(error)`` reads like the
    command line's error: the message, then each detail and the help on a line of its own.
    """

    def __init__(
        self,
        kind: str | None,
        message: str,
        details: list[str] | None = None,
        help: str | None = None,
        code: int | None = None,
        result: Any = None,
    ) -> None:
        # every field goes to args, so the error survives pickling between processes
        super().__init__(kind, message, details, help, code, result)
        self.kind = kind
        self.message = message
        self.details = details or []
        self.help = help
        self.code = code
        self.result = result

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
    ``iris/Iris.csv``. ``project`` is the project folder or any folder inside it. a dataset
    not in place yet is synced alone first, which may download its data. ``yes=True``
    approves large downloads, and ``offline=True`` prevents network requests.
    ``locked=True`` never changes data.lock: when that sync would, it raises :class:`Error`
    with kind ``lock.outdated``.
    """
    arguments = _options(["path"], yes, offline, locked)
    keywords = {"project", "yes", "offline", "locked"}
    try:
        answer = _run([*arguments, "--", name], project, keywords)
    except Error as error:
        meant = _meant(error.help, project, _placeholders(error.details))
        if meant is None:
            raise
        call = _path_call(meant, project, {"yes": yes, "offline": offline, "locked": locked})
        help = _escaped(f"did you mean `{call}`?")
        details = _still_named(error.details, error.message, help)
        raise Error(error.kind, error.message, details, help, error.code, error.result) from None
    return Path(answer["path"])


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
    """make the project's data folder match data.lock and return the project's absolute root.

    ``project`` is the project folder or any folder inside it. ``names``, such as ``["iris"]``
    or ``"iris"``, syncs those datasets alone; the others keep their pins and files, and the
    next sync without names checks them all. a file edited by hand stays, reported as a python
    warning. a link to the cache's copy that was edited in place is put back as pinned, and its
    warning names the file beside it that keeps the changed bytes.

    ``strict=True`` raises :class:`Error` with kind ``data.not_ready`` and code 1, as
    ``sync --strict`` does, when the data is not ready to use afterwards: a file it kept, a
    recipe's result not on this machine, or a dataset data.lock does not pin as data.toml
    asks, each named in ``details`` with what makes it ready. ``yes=True`` approves a
    download above 10 GiB, or the size the ``ask_above`` setting gives, which otherwise raises
    kind ``unconfirmed``. ``offline=True`` prevents network requests. ``locked=True`` raises kind
    ``lock.outdated`` instead of updating a missing or out-of-date data.lock, for the named
    datasets alone when ``names`` names some, and kind ``build.needed`` for a recipe data.lock
    does not pin as building it now would make, which only ``fetchloom build`` changes.
    ``verify=True`` checks the SHA-256 of every placed file and of each cached file it places, as
    ``sync --verify`` does, and puts back or downloads again one whose bytes changed though its
    size and time did not.
    """
    arguments = _options(["sync"], yes, offline, locked)
    if verify:
        arguments.append("--verify")
    if strict:
        arguments.append("--strict")
    named = [names] if isinstance(names, str) else list(names)
    if named:
        arguments += ["--", *named]
    switches = {"yes": yes, "offline": offline, "locked": locked, "verify": verify, "strict": strict}
    try:
        result = _run(arguments, project, {"project", *switches})
    except Error as error:
        meant = _meant_names(error.help, project, _placeholders(error.details))
        if meant is None:
            raise
        help = _escaped(f"did you mean `{_sync_call(meant, project, switches)}`?")
        details = _still_named(error.details, error.message, help)
        raise Error(error.kind, error.message, details, help, error.code, error.result) from None
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
    returned, so ``pd.read_csv(fetchloom.get(url, "data.csv"))`` works. a file already there
    that holds the same bytes stays as it is; one holding other bytes raises :class:`Error`
    with kind ``local.not_empty`` unless ``force=True``, which needs ``path``, replaces it.
    without ``path``, the download goes into a new folder named after it, or into
    ``folder``, and the folder is returned; giving both raises kind ``usage``. a leading ``~``
    in ``path`` or ``folder`` is your home folder, as pandas reads it. ``sha256`` is the hash
    the one file must have, and with ``path`` a file already holding it is kept without a
    download. ``yes=True`` approves large downloads; ``offline=True`` prevents network
    requests, and keeps a file at ``path`` that still holds what the link served last time.
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
    try:
        result = _run([*arguments, "--", link], ".", keywords)
    except Error as error:
        meant = _meant_link(error.help)
        if meant is None:
            raise
        guessed, after = meant
        given = {"path": path, "folder": folder, "sha256": sha256}
        call = _get_call(guessed, given, {"force": force, "yes": yes, "offline": offline})
        help = _escaped(f"did you mean `{call}`?{after}")
        details = _still_named(error.details, error.message, help)
        raise Error(error.kind, error.message, details, help, error.code, error.result) from None
    return Path(result["file"] if path is not None else result["folder"])


def _local(path: str | os.PathLike[str]) -> str:
    # pandas expands ~ in the paths it is given, and no shell does it for a python call
    return os.path.expanduser(os.fspath(path))


def _escaped(text: str) -> str:
    """``text`` with each control character, bidi controls among them, written as an escape, as
    the command line writes it, so a name from a cloned project or a server cannot recolor the
    terminal, set its title or reorder its words: a tab as ``\\t``, an escape as ``\\u{1b}``."""
    short = {"\t": "\\t", "\n": "\\n", "\r": "\\r"}
    return _CONTROL.sub(lambda found: short.get(found[0], f"\\u{{{ord(found[0]):x}}}"), text)


def _shell_word(word: str, windows: bool | None = None) -> str:
    """``word`` as fetchloom shows it in the commands it suggests, so a shell reads it as typed.

    a word starting with ``~`` or ``=``, which zsh expands, ``@``, which powershell splats, or
    ``%``, as fish's ``%self``, is quoted, as is one holding ``~`` after an ``=``, which bash
    expands. a word holding the marks fetchloom puts around a word it withholds is withheld
    everywhere, and on ``windows``, this machine's system unless given, so is a word holding what
    cmd acts on even inside quotes, ``& | < > ^ % !``, or what powershell's quoting cannot keep,
    ``$``, a backtick or a double quote, straight or curly: fetchloom shows ``"<file>"`` for a path
    to a file and ``"<name>"`` for any other, and names the word on a line of its own. any other
    word there is double-quoted, which powershell and cmd both read as written, as is one holding a
    comma or starting with one ``-`` and holding a dot, which powershell would change, and a
    backslash ending it is doubled inside the quotes. elsewhere double quotes keep a word literal
    unless it holds ``$``, a backtick, a double quote, ``!``, which bash and zsh expand from their
    history, or a backslash; then it is single-quoted, and a single quote or a backslash goes
    outside the quotes, escaped, since fish reads a backslash inside them as an escape too.
    """
    if windows is None:
        windows = _WINDOWS
    if _MARKS.intersection(word) or (windows and _UNSAFE_ON_WINDOWS.intersection(word)):
        # a path to a file, not a host or a link, which only look like one
        file = ("/" in word or "\\" in word) and "://" not in word and ntpath.splitext(word)[1] != ""
        return '"<file>"' if file else '"<name>"'
    quoted_anyway = (
        word[:1] in ("", "~", "=", "@", "%")
        or "~" in word.partition("=")[2]
        or (windows and word.startswith("-") and not word.startswith("--") and "." in word)
    )
    bare = "-_./:@%+=~" if windows else "-_./:@%+=,~"
    if not quoted_anyway and all(c.isascii() and (c.isalnum() or c in bare) for c in word):
        return word
    if windows:
        doubled = word + "\\" * (len(word) - len(word.rstrip("\\")))
        return f'"{doubled}"'
    if not any(c in '$`"!\\' for c in word):
        return f'"{word}"'
    quoted, inside = [], False
    for c in word:
        if c in "'\\":
            if inside:
                quoted.append("'")
                inside = False
            quoted.append("\\")
        elif not inside:
            quoted.append("'")
            inside = True
        quoted.append(c)
    if inside:
        quoted.append("'")
    return "".join(quoted)


def _options(arguments: list[str], yes: bool, offline: bool, locked: bool) -> list[str]:
    """append ``--yes``, ``--offline`` and ``--locked`` to ``arguments`` for each one that is on, and
    return the same list."""
    for flag, wanted in [("--yes", yes), ("--offline", offline), ("--locked", locked)]:
        if wanted:
            arguments.append(flag)
    return arguments


def _run(arguments: list[str], project: str | os.PathLike[str], keywords: set[str]) -> Any:
    """run fetchloom with ``arguments`` in ``project`` and return the result of its JSON answer.

    each warning goes on as a python warning, and an error raises :class:`Error`. their texts
    name the ``keywords`` the caller takes in place of the flags those set.
    """
    # one word, so a folder such as -proj or --help is never read as options
    finished = _call([_program(), "--json", "--progress", f"-C={_local(project)}", *arguments])
    try:
        answer = json.loads(finished.stdout)
    except ValueError:
        said = finished.stderr.strip() or "nothing"
        raise Error(
            None,
            f"fetchloom exited with code {finished.returncode} without a JSON answer. it said: {said}",
            code=finished.returncode,
        ) from None

    def told(text: str, placeholders: dict[str, str]) -> str:
        # a line naming a withheld word gives it as fetchloom wrote it, flags and all
        if _WITHHELD.fullmatch(text):
            return text
        return _spoken(_as_typed(text, project, placeholders), keywords)

    for warning in answer["warnings"]:
        placeholders = _placeholders(warning["details"])
        message = told(warning["message"], placeholders)
        help = told(warning["help"], placeholders) if warning["help"] else None
        # as fetchloom prints a warning: what a placeholder in it stands for comes before the help
        said = [message, *_named(warning["details"], message, help), help]
        warnings.warn(_escaped("; ".join(filter(None, said))), stacklevel=3)
    if answer["status"] != "ok":
        error = answer["errors"][0]
        placeholders = _placeholders(error["details"])
        help = error["help"]
        if error["kind"] == "project.invalid" and error["message"].startswith(_NO_PROJECT):
            # a script or notebook often runs in another folder than its project's
            help = _no_project(project)
        elif help:
            help = told(help, placeholders)
        message = told(error["message"], placeholders)
        details = _still_named([told(detail, placeholders) for detail in error["details"]], message, help)
        raise Error(error["kind"], _escaped(message), [_escaped(detail) for detail in details],
                    None if help is None else _escaped(help), finished.returncode, answer.get("result"))
    return answer["result"]


def _placeholders(details: list[str]) -> dict[str, str]:
    """each word fetchloom withheld from a command on windows, by the lines of ``details`` naming
    it, with the placeholder it shows instead."""
    return {match.group(2): match.group(1) for match in map(_WITHHELD.fullmatch, details) if match}


def _named(details: list[str], *texts: str | None) -> list[str]:
    """the lines of ``details`` naming a withheld word whose placeholder ``texts`` still show."""
    shown = " ".join(filter(None, texts))
    return [detail for detail in details if (named := _WITHHELD.fullmatch(detail)) and named.group(1) in shown]


def _still_named(details: list[str], *texts: str | None) -> list[str]:
    """``details`` without a line naming a withheld word whose placeholder neither ``texts`` nor the
    other details still show, as once the helper takes ``-C=<folder>`` out of a command."""
    kept = [detail for detail in details if not _WITHHELD.fullmatch(detail)]
    named = _named(details, *texts, *kept)
    return [detail for detail in details if detail in kept or detail in named]


def _folder_shown(folder: str, placeholders: dict[str, str]) -> str | None:
    """``folder`` as fetchloom shows it after ``-C``: quoted as :func:`_shell_word` quotes it, or,
    where it withholds a folder no command can hold, the placeholder ``placeholders`` gives it, in
    double quotes. none for one withheld without one."""
    if _MARKS.intersection(folder) or (_WINDOWS and _UNSAFE_ON_WINDOWS.intersection(folder)):
        # the line naming a folder holding the marks writes them out
        written = folder.replace("\uf8e0", "\\u{f8e0}").replace("\uf8e1", "\\u{f8e1}")
        placeholder = placeholders.get(written)
        return None if placeholder is None else f'"{placeholder}"'
    return _shell_word(folder)


def _given(project: str | os.PathLike[str], placeholders: dict[str, str]) -> str | None:
    """the ``-C=<folder>`` word the helper gives fetchloom for ``project``, as fetchloom repeats it in
    a command line it retypes: quoted as :func:`_shell_word` quotes it, or by the placeholder
    ``placeholders`` gives it where fetchloom withholds it, or None for one withheld without one."""
    return _folder_shown(f"-C={_local(project)}", placeholders)


def _as_typed(text: str, project: str | os.PathLike[str], placeholders: dict[str, str]) -> str:
    """``text`` with each ``fetchloom`` command in backticks as a person types it at a terminal.

    fetchloom words them with the arguments the helper gave it, so each leaves out the
    ``--json`` and ``--progress`` the helper passes, the ``-C`` that names the working folder
    when ``project`` is that folder, as the helper gave it, as ``-C=.``, or as fetchloom names the
    folder a run worked in before each command it suggests, spelled out or by the placeholder
    ``placeholders`` gives it, and the ``--`` the helper puts before names, unless a name after it
    starts with ``-``.
    """
    given = _given(project, placeholders)
    # how fetchloom names the folder a run worked in: this one, or the project given whole
    named, whole = [
        None if shown is None else f"-C {shown}"
        for shown in (_folder_shown(folder, placeholders) for folder in [os.getcwd(), _local(project)])
    ]

    def typed(match: re.Match[str]) -> str:
        words = match.group(1)
        for own in ["--json --progress ", "--json ", "--progress "]:
            words = words.removeprefix(own)
        if os.fspath(project) == ".":
            for folder in [given, named]:
                if folder is not None:
                    words = words.removeprefix(f"{hidden.get(folder, folder)} ")
        # the folder after -C= may hold " -- " itself, so the helper's own comes after it
        folder = "" if given is None else f"{hidden.get(given, given)} "
        if not words.startswith(folder):
            folder = ""
        before, separator, names = words[len(folder):].partition(" -- ")
        if separator and not any(name.lstrip("'\"").startswith("-") for name in names.split()):
            words = f"{folder}{before} {names}"
        return f"`fetchloom {words}`"

    # a folder holding a backtick would end its command's backticks early, so a mark without one
    # stands in for it while the commands are read
    hidden: dict[str, str] = {}
    for folder in [given, named, whole]:
        if folder is not None and "`" in folder and folder not in hidden:
            hidden[folder] = f"\0{len(hidden)}\0"
    for folder, mark in hidden.items():
        text = text.replace(f"{folder} ", f"{mark} ")
    text = _COMMAND.sub(typed, text)
    for folder, mark in hidden.items():
        text = text.replace(mark, folder)
    return text


def _meant(help: str | None, project: str | os.PathLike[str], placeholders: dict[str, str]) -> str | None:
    """the name fetchloom guessed was meant in ``help``, its did-you-mean for a name given to
    ``path`` in ``project``, whose folder ``placeholders`` may name, that names nothing, or None
    for any other help and for a name a shell needs quoted. that help repeats the command line as
    typed, so the folder after ``-C`` is ``project`` as the helper gave it."""
    folder = _typed_folder(project, placeholders)
    if folder is None:
        return None
    flags = "(?: --(?:yes|offline|locked))*"
    guess = re.fullmatch(
        rf"did you mean `fetchloom {re.escape(folder)}path{flags} ([A-Za-z0-9_./:@%+=,~-]+)`\?", help or ""
    )
    return guess.group(1) if guess else None


def _meant_names(help: str | None, project: str | os.PathLike[str], placeholders: dict[str, str]) -> list[str] | None:
    """the names fetchloom guessed were meant in ``help``, its did-you-mean for names given to
    ``sync``, as :func:`_meant` reads one given to ``path``."""
    folder = _typed_folder(project, placeholders)
    if folder is None:
        return None
    flags = "(?: --(?:yes|offline|locked|verify|strict))*"
    name = "[A-Za-z0-9_./:@%+=,~][A-Za-z0-9_./:@%+=,~-]*"
    guess = re.fullmatch(rf"did you mean `fetchloom {re.escape(folder)}sync{flags}((?: {name})+)`\?", help or "")
    return guess.group(1).split() if guess else None


def _meant_link(help: str | None) -> tuple[str, str] | None:
    """the link fetchloom guessed was meant in ``help``, its did-you-mean for a link given to
    ``get``, with what the help says after it, or None for any other help. fetchloom shows a link
    with no character a shell reads as it is, and puts any other in double quotes."""
    guess = re.fullmatch(
        r'did you mean `fetchloom get(?: [^`]*)? (?:"([^"$`!\\]*)"|([A-Za-z0-9_./:@%+=,~-]+))`\?(.*)',
        help or "",
        re.DOTALL,
    )
    if guess is None:
        return None
    return guess.group(1) or guess.group(2), guess.group(3)


def _typed_folder(project: str | os.PathLike[str], placeholders: dict[str, str]) -> str | None:
    """the ``-C=<folder>`` word for ``project`` as fetchloom repeats it in a command line typed with
    it, with the space after, nothing for the working folder, or None for a folder it withholds."""
    if os.fspath(project) == ".":
        return ""
    given = _given(project, placeholders)
    return None if given is None else f"{given} "


def _path_call(name: str, project: str | os.PathLike[str], switches: dict[str, bool]) -> str:
    """the call of :func:`path` for ``name`` in ``project`` with the keywords in ``switches``
    that are on, as a python caller types it."""
    words = [json.dumps(name, ensure_ascii=False)]
    if os.fspath(project) != ".":
        words.append(f"project={json.dumps(os.fspath(project), ensure_ascii=False)}")
    words += [f"{keyword}=True" for keyword, on in switches.items() if on]
    return f"fetchloom.path({', '.join(words)})"


def _sync_call(names: list[str], project: str | os.PathLike[str], switches: dict[str, bool]) -> str:
    """the call of :func:`sync` for ``names`` in ``project`` with the keywords in ``switches``
    that are on, as a python caller types it."""
    words = []
    if os.fspath(project) != ".":
        words.append(json.dumps(os.fspath(project), ensure_ascii=False))
    named = names[0] if len(names) == 1 else names
    words.append(f"names={json.dumps(named, ensure_ascii=False)}")
    words += [f"{keyword}=True" for keyword, on in switches.items() if on]
    return f"fetchloom.sync({', '.join(words)})"


def _get_call(link: str, given: dict[str, Any], switches: dict[str, bool]) -> str:
    """the call of :func:`get` for ``link`` with the ``path``, ``folder`` and ``sha256`` in
    ``given`` that were given and the keywords in ``switches`` that are on, as a python caller
    types it."""
    words = [json.dumps(link, ensure_ascii=False)]
    if given["path"] is not None:
        words.append(json.dumps(os.fspath(given["path"]), ensure_ascii=False))
    words += [
        f"{keyword}={json.dumps(os.fspath(value), ensure_ascii=False)}"
        for keyword, value in given.items()
        if keyword != "path" and value is not None
    ]
    words += [f"{keyword}=True" for keyword, on in switches.items() if on]
    return f"fetchloom.get({', '.join(words)})"


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
    """pass each line of ``stream`` on to ``sys.stderr`` as it comes, keeping the last in ``tail``."""
    for line in stream:
        tail.append(line)
        try:
            sys.stderr.write(line)
            sys.stderr.flush()
        except (AttributeError, OSError, ValueError):
            # no stderr, as under pythonw, or a closed one: the program must still be read
            pass


def _no_project(project: str | os.PathLike[str]) -> str:
    """the help for a ``project`` that holds no data.toml, and has none above it. a folder windows
    commands cannot hold is shown as ``"<folder>"``, which the end of the help names."""
    folder = _local(project)
    given = _given(project, {}) or '-C "<folder>"'
    add = "fetchloom add <link>" if os.fspath(project) == "." else f"fetchloom {given} add <link>"
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
    longest = sorted([*switches, *values], key=len, reverse=True)
    found = re.compile(rf"(add )?(?<![\w-])({'|'.join(map(re.escape, longest))})(?![\w-])")
    for flag, instead in _UNTAKEN.items():
        text = text.replace(flag, instead)

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


def _program(windows: bool = sys.platform == "win32") -> str:
    """the fetchloom program to run: the one installed with this package, else the one in the
    interpreter's scripts folder, its base interpreter's or the user's, else the first on PATH
    that :func:`_on_path` accepts.

    raises FileNotFoundError, pointing at the install steps, when there is none.
    """
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
            # under lib sits a folder each interpreter names for itself, as python3.12 or pypy3.10
            if prefix.parent.name in {"lib", "lib64"}:
                prefix = prefix.parent
            if prefix.name in {"lib", "lib64"}:
                folders.append(prefix.parent / "bin")
    folders.extend([
        sysconfig.get_path("scripts"),
        sysconfig.get_path("scripts", vars={"base": sys.base_prefix}),
        sysconfig.get_path("scripts", sysconfig.get_preferred_scheme("user")),
    ])
    for folder in folders:
        if folder and Path(folder, name).is_file():
            return str(Path(folder, name))
    found = _on_path(windows)
    if found is None:
        raise FileNotFoundError(
            "the fetchloom program is missing: install it as "
            "https://github.com/nikhil-yn-aix/fetchloom#install says, or put it on PATH"
        )
    return found


def _on_path(windows: bool) -> str | None:
    """the fetchloom program in the first absolute folder of PATH that holds it as a file this
    user may run, with each extension PATHEXT names on ``windows``. a relative folder, an empty
    entry among them, is passed over, and so is the working folder, which python's own search
    tries first on windows: a cloned project could hold a program of that name."""
    paths = ntpath if windows else posixpath
    names = ["fetchloom"]
    if windows:
        extensions = os.environ.get("PATHEXT", ".COM;.EXE;.BAT;.CMD").split(";")
        names = [f"fetchloom{extension}" for extension in extensions if extension]
    for folder in os.environ.get("PATH", paths.defpath).split(paths.pathsep):
        if not _absolute(folder, windows):
            continue
        for name in names:
            program = paths.join(folder, name)
            if os.path.isfile(program) and os.access(program, os.X_OK):
                return program
    return None


def _absolute(folder: str, windows: bool) -> bool:
    """whether ``folder`` is one place wherever the program runs, as rust's ``Path::is_absolute``
    reads it. on ``windows`` that is a drive with a root, as ``C:\\tools``, or a share, as
    ``\\\\lab\\share``: ``\\tools`` and ``C:tools`` depend on the working drive or its folder, though
    python before 3.13 counts the first as absolute."""
    if not windows:
        return posixpath.isabs(folder)
    drive, rest = ntpath.splitdrive(folder)
    # a share or a device, as \\lab\share, roots itself, while a drive letter needs its own root
    return bool(drive) and (drive[0] in "\\/" or rest[:1] in ("\\", "/"))
