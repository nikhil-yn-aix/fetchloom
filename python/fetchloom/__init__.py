"""fetchloom, uv for research data: find the ``fl`` executable of this package."""

from __future__ import annotations

import sysconfig
from pathlib import Path

__all__ = ["FlNotFoundError", "find_fl_bin"]


class FlNotFoundError(FileNotFoundError):
    """Raised when ``fl`` is in none of the places a wheel installs it."""


def find_fl_bin() -> str:
    """Return the path of the ``fl`` executable installed with this package.

    The scripts directory of the running interpreter is searched first, then the user
    scripts directory, then the ``bin`` directory next to the package, where
    ``pip install --target`` puts it.

    Raises:
        FlNotFoundError: when no directory holds the executable. The message lists
            every directory searched.

    """
    name = "fl" + (sysconfig.get_config_var("EXE") or "")
    searched = _search_dirs()
    for directory in searched:
        candidate = directory / name
        if candidate.is_file():
            return str(candidate)
    places = ", ".join(str(directory) for directory in searched)
    message = f"could not find {name}, searched: {places}"
    raise FlNotFoundError(message)


def _search_dirs() -> list[Path]:
    """Return the directories a wheel may install ``fl`` into, without repeats."""
    candidates = [
        Path(sysconfig.get_path("scripts")),
        Path(sysconfig.get_path("scripts", sysconfig.get_preferred_scheme("user"))),
        Path(__file__).resolve().parent.parent / "bin",
    ]
    return list(dict.fromkeys(candidates))
