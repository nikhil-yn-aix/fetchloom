"""hugging face's own client: `hf download` of a repository at a commit into its cache in <home>,
with xet where the repository uses it. the snapshot's files are then linked into the output
folder, since the hub's users read them in its cache.

usage: hf_download.py <out> <home> <kind>/<owner>/<name>@<commit>

the reference is the one in bench/datasets.toml without its `hf:` prefix.
"""

import argparse
import sys
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("reference", help="such as datasets/openai/openai_humaneval@<commit>")
    args = parser.parse_args()
    repo, _, commit = args.reference.removeprefix("hf:").partition("@")
    kind, _, repo_id = repo.partition("/")
    if kind not in ("datasets", "models", "spaces") or not commit:
        wrapping.fail("give the reference as <kind>/<owner>/<name>@<commit>")
    env = {"HF_HOME": str(args.home / "hf"), "HF_HUB_DISABLE_TELEMETRY": "1"}
    wrapping.run([wrapping.program("hf"), "download", repo_id, "--repo-type", kind.removesuffix("s"),
                  "--revision", commit], env)
    snapshot = args.home / "hf" / "hub" / f"{kind}--{repo_id.replace('/', '--')}" / "snapshots" / commit
    if not snapshot.is_dir():
        wrapping.fail(f"no snapshot at {snapshot}")
    print(f"{wrapping.place(snapshot, args.out)} files", file=sys.stderr)


if __name__ == "__main__":
    main()
