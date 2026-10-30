"""datahugger: every file of a record behind a DOI or url into the output folder, with its
defaults, which unpack zip files. it keeps no cache, so <home> is unused.

usage: datahugger_download.py <out> <home> <doi or url>
"""

import argparse
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("source", help="a DOI such as 10.5281/zenodo.3684992, or a record's url")
    args = parser.parse_args()
    wrapping.run([wrapping.program("datahugger"), args.source.removeprefix("doi:"), str(args.out)])


if __name__ == "__main__":
    main()
