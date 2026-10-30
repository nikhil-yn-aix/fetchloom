"""gdown: a google drive file, or a folder with --folder, into the output folder. its command
keeps no cache; <home> becomes its home, so the cookies it saves start empty.

usage: gdown_download.py <out> <home> <file id | folders/<folder id>>

the reference is the one in bench/datasets.toml without its `drive:` prefix.
"""

import argparse
import os
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("reference", help="a file id, or folders/<folder id>")
    args = parser.parse_args()
    reference = args.reference.removeprefix("drive:")
    env = {"HOME": str(args.home), "USERPROFILE": str(args.home)}
    gdown = wrapping.program("gdown")
    args.out.mkdir(parents=True, exist_ok=True)
    if reference.startswith("folders/"):
        url = f"https://drive.google.com/drive/{reference}"
        wrapping.run([gdown, "--folder", url, "-O", str(args.out)], env)
    else:
        # a trailing separator makes gdown keep the file's own name inside the folder
        wrapping.run([gdown, reference, "-O", str(args.out) + os.sep], env)


if __name__ == "__main__":
    main()
