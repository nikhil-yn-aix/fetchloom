"""Tests that the ``fl`` executable ships with the package and runs."""

from __future__ import annotations

import subprocess
from importlib.metadata import version
from pathlib import Path

import pytest

import fetchloom


def run_version(executable: str) -> str:
    """Return what ``executable --version`` prints."""
    done = subprocess.run(
        [executable, "--version"], capture_output=True, text=True, check=True
    )
    return done.stdout


def test_find_fl_bin_returns_a_working_fl() -> None:
    assert run_version(fetchloom.find_fl_bin()) == f"fl {version('fetchloom')}\n"


def test_fetchloom_executable_sits_beside_fl() -> None:
    fl = Path(fetchloom.find_fl_bin())
    alias = fl.with_name("fetchloom" + fl.suffix)
    assert run_version(str(alias)) == f"fl {version('fetchloom')}\n"


def test_missing_fl_names_every_directory_searched(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    empty = [tmp_path / "scripts", tmp_path / "bin"]
    monkeypatch.setattr(fetchloom, "_search_dirs", lambda: empty)
    with pytest.raises(fetchloom.FlNotFoundError) as raised:
        fetchloom.find_fl_bin()
    for directory in empty:
        assert str(directory) in str(raised.value)
