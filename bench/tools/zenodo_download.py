"""zenodo_get: every file of a zenodo record into the output folder, checked against the md5
the record lists. it keeps no cache, so <home> is unused.

usage: zenodo_download.py <out> <home> <record id or doi>
"""

import argparse
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("record")
    args = parser.parse_args()
    record = args.record.removeprefix("zenodo:")
    wrapping.run([wrapping.program("zenodo_get"), record, "-o", str(args.out)])


if __name__ == "__main__":
    main()
