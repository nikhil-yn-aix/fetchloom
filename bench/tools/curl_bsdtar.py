"""curl, then bsdtar to unpack the archive it wrote into the output folder.

usage: curl_bsdtar.py <out> <url> [--name NAME]

the archive stays beside the output folder, in the current one. windows' own tar.exe is bsdtar.
"""

import argparse
import os
import sys
from pathlib import Path

import wrapping


def bsdtar():
    if os.name == "nt":
        return str(Path(os.environ.get("SystemRoot", r"C:\Windows")) / "System32" / "tar.exe")
    return wrapping.program("bsdtar")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("url")
    parser.add_argument("--name", help="the archive's file name; the url's last segment by default")
    args = parser.parse_args()
    archive = Path.cwd() / (args.name or wrapping.url_name(args.url))
    args.out.mkdir(parents=True, exist_ok=True)
    wrapping.run([wrapping.program("curl"), "-q", "--fail", "-L", "-o", str(archive), args.url])
    wrapping.run([bsdtar(), "-xf", str(archive), "-C", str(args.out)])
    print(f"unpacked {archive.name}", file=sys.stderr)


if __name__ == "__main__":
    main()
