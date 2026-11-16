"""the project ci.yml's action job syncs: data from a folder mirror, whose source is gone by then.

``prepare`` pins a file a loopback server serves once, pushes it to a folder mirror and stops the
server, so the action's first run can take the file only from the mirror. ``forget`` removes the
cache, the placed data and the mirror before the second run, which must then place the file from
the cache the action restored. ``check`` says whether the placed file holds the pinned bytes.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import subprocess

from ci_journey import Server, digest, environment, remove_tree, require


def prepare(binary, root):
    # the bytes differ in each run and on each system, so no earlier run's cache holds them
    data = f"run,system\n{os.environ.get('GITHUB_RUN_ID', 'local')}-{os.environ.get('GITHUB_RUN_ATTEMPT', '1')},{platform.system()}\n".encode()
    project, home = root / "project", root / "home"
    project.mkdir(parents=True)
    home.mkdir()
    with Server({"/fixture.csv": data}) as server:
        env = environment(home, root / "setup-cache", server.origin)
        for arguments in [["add", server.url("/fixture.csv"), "--as", "fixture"],
                          ["push", "--to", str(root / "mirror"), "--yes"]]:
            result = subprocess.run([str(binary), "--json", f"-C={project}", *arguments], env=env,
                                    stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=120)
            print("+ fetchloom", *arguments, "=>", result.returncode, flush=True)
            require(result.returncode == 0 and json.loads(result.stdout)["status"] == "ok", result.stdout + result.stderr)
    remove_tree(root / "setup-cache")
    remove_tree(project / "data")
    (root / "expected").write_text(hashlib.sha256(data).hexdigest(), encoding="ascii")


def forget(binary, root):
    # the action keeps fetchloom's own cache folder, as this job's environment gives it
    said = subprocess.run([str(binary), "--json", "settings"], stdin=subprocess.DEVNULL,
                          capture_output=True, text=True, timeout=60, check=True)
    cache = next(setting["value"] for setting in json.loads(said.stdout)["result"]["settings"]
                 if setting["name"] == "cache")
    require(Path(cache).is_dir(), f"the first run left no cache at {cache}")
    for path in [Path(cache), root / "project/data", root / "mirror"]:
        remove_tree(path)


def check(root):
    placed = root / "project/data/fixture/fixture.csv"
    require(placed.is_file(), f"{placed} was not placed")
    require(digest(placed) == (root / "expected").read_text(encoding="ascii"), f"{placed} holds other bytes")
    print(f"{placed} holds the pinned bytes")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("step", choices=["prepare", "forget", "check"])
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--binary", type=Path)
    args = parser.parse_args()
    if args.step == "prepare":
        prepare(args.binary.resolve(), args.root)
    elif args.step == "forget":
        forget(args.binary.resolve(), args.root)
    else:
        check(args.root)


if __name__ == "__main__":
    main()
