"""many separate urls, listed for each tool the way it takes them, then fetched.

usage: many_urls.py <tool> <out> <home> <base url> <expect file> --size BYTES [-- tool options]

each line of the expect file, `<sha256>  <path>`, names <base url>/<path>. the tools:

- fetchloom: a project in the current folder whose data.toml lists one url dataset per file and
  whose data.lock pins each file's sha256 and size, as a committed project would, then
  `fetchloom sync` with its cache in <home>. its data folder is <out>.
- curl: a config file of url and output pairs for `curl --parallel -K`.
- aria2c: an input file of urls with their names for `aria2c -i`.

every tool pays the same cost for its list: this wrapper's start and a file of 1,000 lines.
"""

import argparse
import hashlib
import os
import sys
from pathlib import Path, PurePosixPath

import wrapping


def fetchloom(entries, out, home, extra):
    data_dir = Path(os.path.relpath(out, Path.cwd())).as_posix()
    manifest = [f'[project]\ndata_dir = "{data_dir}"\n']
    lock = ["version = 1\n"]
    for name, url, digest, size in sorted(entries):
        manifest.append(f'[datasets.{name}]\nref = "{url}"\n')
        spec = hashlib.sha256(f"ref={url}\nextract=true\n".encode()).hexdigest()
        lock.append(
            f'[[dataset]]\nname = "{name}"\nref = "{url}"\nspec = "sha256:{spec}"\nresolved = "{url}"\n'
            f'locked = "2026-10-30T00:00:00Z"\nfiles = [\n'
            f'  {{ path = "{PurePosixPath(url).name}", size = {size}, sha256 = "{digest}", url = "{url}" }},\n]\n'
        )
    Path("data.toml").write_text("\n".join(manifest), encoding="utf-8", newline="\n")
    Path("data.lock").write_text("\n".join(lock), encoding="utf-8", newline="\n")
    env = {"FETCHLOOM_CACHE": str(home / "cache")}
    wrapping.run([wrapping.program("fetchloom"), "-C", ".", "sync", "--yes", *extra], env)


def curl(entries, out, extra):
    lines = [f'url = "{url}"\noutput = "{(out / PurePosixPath(url).name).as_posix()}"\n' for _, url, _, _ in entries]
    Path("urls.curl").write_text("".join(lines), encoding="utf-8", newline="\n")
    # -q first skips any .curlrc, so curl runs with its defaults
    wrapping.run([wrapping.program("curl"), "-q", "--parallel", "--fail", *extra, "-K", "urls.curl"])


def aria2c(entries, out, extra):
    lines = [f"{url}\n  out={PurePosixPath(url).name}\n" for _, url, _, _ in entries]
    Path("urls.aria2").write_text("".join(lines), encoding="utf-8", newline="\n")
    # --no-conf skips any aria2.conf, so aria2c runs with its defaults
    wrapping.run([wrapping.program("aria2c"), "--no-conf", "-d", str(out), *extra, "-i", "urls.aria2"])


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("tool", choices=["fetchloom", "curl", "aria2c"])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("base")
    parser.add_argument("expect")
    parser.add_argument("--size", type=int, required=True, help="every file's size in bytes")
    parser.add_argument("extra", nargs=argparse.REMAINDER, help="options for the tool, after --")
    args = parser.parse_args()
    extra = args.extra[1:] if args.extra[:1] == ["--"] else args.extra
    entries = []
    for digest, path in wrapping.pins(args.expect):
        url = f"{args.base.rstrip('/')}/{path}"
        name = "f" + PurePosixPath(path).stem.lower()
        entries.append((name, url, digest, args.size))
    args.out.mkdir(parents=True, exist_ok=True)
    out = args.out.resolve()
    if args.tool == "fetchloom":
        fetchloom(entries, out, args.home.resolve(), extra)
    elif args.tool == "curl":
        curl(entries, out, extra)
    else:
        aria2c(entries, out, extra)
    print(f"{len(entries)} urls", file=sys.stderr)


if __name__ == "__main__":
    main()
