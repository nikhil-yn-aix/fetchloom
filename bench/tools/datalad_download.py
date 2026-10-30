"""datalad: a new dataset in the output folder and download-url, which hands the file to
git-annex. linux only.

usage: datalad_download.py <out> <home> <url> [--name NAME]

<home> becomes HOME for datalad and git, so their settings start empty; git still needs a name.
"""

import argparse
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("url")
    parser.add_argument("--name", help="the file name; the url's last segment by default")
    args = parser.parse_args()
    env = {
        "HOME": str(args.home),
        "GIT_AUTHOR_NAME": "bench", "GIT_AUTHOR_EMAIL": "bench@localhost",
        "GIT_COMMITTER_NAME": "bench", "GIT_COMMITTER_EMAIL": "bench@localhost",
    }
    datalad = wrapping.program("datalad")
    args.out.mkdir(parents=True, exist_ok=True)
    wrapping.run([datalad, "create", str(args.out)], env)
    name = args.name or wrapping.url_name(args.url)
    wrapping.run([datalad, "download-url", "-d", str(args.out), "-O", str(args.out / name), args.url], env)
    # git-annex makes its object folders read-only, which would stop the harness removing the run
    for folder in (args.out / ".git" / "annex" / "objects").rglob("*"):
        if folder.is_dir():
            folder.chmod(0o755)


if __name__ == "__main__":
    main()
