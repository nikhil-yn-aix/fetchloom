"""what the benchmark wrappers share: running a tool, reading pinned hashes and placing what a
tool keeps in its cache into the run's output folder.

each wrapper takes the output folder first and exits nonzero when its tool fails. expect files
hold `<sha256>  <path>` lines; a relative path to one is read from the repository's root, as the
harness reads a workload's `expect`.
"""

import hashlib
import os
import shutil
import subprocess
import sys
from pathlib import Path
from urllib.parse import unquote, urlsplit

REPO = Path(__file__).resolve().parents[2]


def fail(message, code=1):
    print(message, file=sys.stderr, flush=True)
    sys.exit(code)


def program(name):
    """a console script installed beside this python, else a program on PATH."""
    folder = Path(sys.executable).parent
    for candidate in (folder / name, folder / f"{name}.exe"):
        if candidate.is_file():
            return str(candidate)
    found = shutil.which(name)
    if not found:
        fail(f"{name} is not installed")
    return found


def run(argv, env=None, cwd=None):
    """runs a tool on this process's streams and exits with the tool's code when it fails."""
    code = subprocess.call(argv, env=None if env is None else {**os.environ, **env}, cwd=cwd)
    if code:
        fail(f"{Path(argv[0]).name} exited with {code}", code)


def pins(spec):
    """(sha256, path) pairs from an expect file, or one pair from 64 hex digits."""
    if len(spec) == 64 and all(c in "0123456789abcdefABCDEF" for c in spec):
        return [(spec.lower(), "")]
    path = Path(spec)
    if not path.is_absolute():
        path = REPO / path
    if not path.is_file():
        fail(f"no expect file at {path}; write it first, for live data with bench/expect-live.py")
    found = []
    for line in path.read_text(encoding="utf-8-sig").splitlines():
        if line.strip():
            digest, _, name = line.strip().partition("  ")
            found.append((digest.lower(), name))
    return found


def pin(spec):
    """the one sha256 a single-file expect file or argument names."""
    found = pins(spec)
    if len(found) != 1:
        fail(f"{spec} lists {len(found)} files; this wrapper takes one")
    return found[0][0]


def sha256(path):
    digest = hashlib.sha256()
    with open(path, "rb") as file:
        while chunk := file.read(1 << 20):
            digest.update(chunk)
    return digest.hexdigest()


def url_name(url):
    return unquote(urlsplit(url).path.rstrip("/").rsplit("/", 1)[-1]) or "download"


def place(source, target):
    """hardlinks a cached file, or every file under a cached folder, to target, copying where a
    link cannot be made. tools that keep data in their cache use it in place, so a link is the
    cheapest way to give the harness the files under the run's folder."""
    source = Path(source)
    files = [source] if source.is_file() else [p for p in sorted(source.rglob("*")) if p.is_file()]
    for path in files:
        destination = Path(target) / (path.name if path == source else path.relative_to(source))
        destination.parent.mkdir(parents=True, exist_ok=True)
        real = path.resolve()
        try:
            os.link(real, destination)
        except OSError:
            shutil.copyfile(real, destination)
    return len(files)
