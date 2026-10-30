"""datahugger: every file of a record behind a DOI or url into the output folder. it keeps no
cache, so <home> is unused.

usage: datahugger_download.py <out> <home> <doi or url> [--no-unzip]

datahugger unpacks zip files by default; --no-unzip keeps them as the source serves them.
"""

import argparse
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("source", help="a DOI such as 10.5281/zenodo.3684992, or a record's url")
    parser.add_argument("--no-unzip", action="store_true", help="passed to datahugger")
    args = parser.parse_args()
    options = ["--no-unzip"] if args.no_unzip else []
    wrapping.run([wrapping.program("datahugger"), *options, args.source.removeprefix("doi:"), str(args.out)])


if __name__ == "__main__":
    main()
