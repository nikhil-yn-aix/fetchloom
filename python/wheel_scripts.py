"""add the console scripts maturin cannot declare to the wheels it built.

maturin refuses ``[project.scripts]`` in a wheel that carries an executable, so the release
adds ``fl`` afterwards: an entry_points.txt naming the launcher, which pip and uv turn into a
command, and its line in RECORD. every other file in the wheel keeps its bytes and mode.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import os
import sys
import zipfile
from pathlib import Path

SCRIPTS = {"fl": "fetchloom.__main__:main"}


def add_scripts(wheel: Path) -> None:
    """write ``SCRIPTS`` into ``wheel``'s entry_points.txt and RECORD, replacing the wheel.

    raises ValueError when the wheel has no single RECORD or already has entry points, which
    would mean maturin now writes them itself.
    """
    with zipfile.ZipFile(wheel) as source:
        members = [(info, source.read(info)) for info in source.infolist()]
    records = [info for info, _ in members if info.filename.endswith(".dist-info/RECORD")]
    if len(records) != 1:
        raise ValueError(f"{wheel.name} has no single .dist-info/RECORD")
    record = records[0]
    points = record.filename.removesuffix("RECORD") + "entry_points.txt"
    if any(info.filename == points for info, _ in members):
        raise ValueError(f"{wheel.name} already has {points}, so maturin may now write scripts")
    text = "[console_scripts]\n" + "".join(f"{name}={target}\n" for name, target in SCRIPTS.items())
    data = text.encode("utf-8")
    digest = base64.urlsafe_b64encode(hashlib.sha256(data).digest()).rstrip(b"=").decode("ascii")
    partial = wheel.with_name(wheel.name + ".part")
    try:
        with zipfile.ZipFile(partial, "w") as target:
            for info, content in members:
                if info is record:
                    added = zipfile.ZipInfo(points, date_time=info.date_time)
                    added.create_system = info.create_system
                    added.external_attr = info.external_attr
                    added.compress_type = info.compress_type
                    target.writestr(added, data)
                    lines = content.decode("utf-8").splitlines(keepends=True)
                    at = next((index for index, line in enumerate(lines)
                               if line.startswith(record.filename + ",")), None)
                    if at is None:
                        raise ValueError(f"{record.filename} does not list itself")
                    lines.insert(at, f"{points},sha256={digest},{len(data)}\n")
                    content = "".join(lines).encode("utf-8")
                target.writestr(info, content)
        os.replace(partial, wheel)
    except BaseException:
        partial.unlink(missing_ok=True)
        raise


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("wheels", nargs="+", type=Path)
    args = parser.parse_args()
    try:
        for wheel in args.wheels:
            add_scripts(wheel)
            print(f"added {', '.join(SCRIPTS)} to {wheel.name}")
    except (OSError, ValueError, zipfile.BadZipFile) as error:
        print(f"adding scripts failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
