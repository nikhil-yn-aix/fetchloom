"""kagglehub: dataset_download of one dataset version into its cache in <home>, which unpacks
kaggle's archive. the files are then linked into the output folder, since kagglehub's users
read them in its cache.

usage: kagglehub_download.py <out> <home> <owner>/<dataset>@<version>

the reference is the one in bench/datasets.toml without its `kaggle:` prefix. credentials come
from where kagglehub reads them, `~/.kaggle` or the KAGGLE_* variables, when a dataset needs them.
"""

import argparse
import os
import sys
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("reference", help="such as uciml/breast-cancer-wisconsin-data@2")
    args = parser.parse_args()
    dataset, _, version = args.reference.removeprefix("kaggle:").partition("@")
    if dataset.count("/") != 1 or not version.isdigit():
        wrapping.fail("give the reference as <owner>/<dataset>@<version>")
    # kagglehub reads its cache folder when it is imported
    os.environ["KAGGLEHUB_CACHE"] = str(args.home / "kaggle")
    import kagglehub

    path = kagglehub.dataset_download(f"{dataset}/versions/{version}")
    print(f"{wrapping.place(path, args.out)} files", file=sys.stderr)


if __name__ == "__main__":
    main()
