"""dvc: a repository in the current folder without git, its cache in <home>, and import-url.

usage: dvc_import.py <out> <home> <url> [--name NAME] [--again]

a first run imports the url and keeps the .dvc file it wrote in <home>, as a project would commit
it. --again is the second project: a new repository on the same cache gets that .dvc file and
checks the data out, the workflow dvc documents for a shared cache.
"""

import argparse
import shutil
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("url")
    parser.add_argument("--name", help="the file name; the url's last segment by default")
    parser.add_argument("--again", action="store_true", help="the second project")
    args = parser.parse_args()
    name = args.name or wrapping.url_name(args.url)
    # dvc's own state and settings go to the new home too, and it sends no usage reports
    env = {
        "DVC_NO_ANALYTICS": "1",
        "DVC_SITE_CACHE_DIR": str(args.home / "site"),
        "DVC_GLOBAL_CONFIG_DIR": str(args.home / "config"),
    }
    dvc = wrapping.program("dvc")
    wrapping.run([dvc, "init", "--no-scm", "-q"], env)
    wrapping.run([dvc, "cache", "dir", str(args.home / "cache")], env)
    args.out.mkdir(parents=True, exist_ok=True)
    target = args.out / name
    kept = args.home / f"{name}.dvc"
    if args.again:
        shutil.copyfile(kept, f"{target}.dvc")
        wrapping.run([dvc, "checkout"], env)
    else:
        wrapping.run([dvc, "import-url", args.url, str(target)], env)
        shutil.copyfile(f"{target}.dvc", kept)


if __name__ == "__main__":
    main()
