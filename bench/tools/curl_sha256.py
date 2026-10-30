"""curl, then a sha256 check of the file it wrote: one stream, verified, the floor for a pinned
download.

usage: curl_sha256.py <out> <url> <sha256 or expect file> [--name NAME]
"""

import argparse
import sys
from pathlib import Path

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("url")
    parser.add_argument("sha256", help="64 hex digits, or an expect file naming one file")
    parser.add_argument("--name", help="the file name; the url's last segment by default")
    args = parser.parse_args()
    want = wrapping.pin(args.sha256)
    target = args.out / (args.name or wrapping.url_name(args.url))
    args.out.mkdir(parents=True, exist_ok=True)
    # -q first skips any .curlrc, so curl runs with its defaults
    wrapping.run([wrapping.program("curl"), "-q", "--fail", "-L", "-o", str(target), args.url])
    got = wrapping.sha256(target)
    if got != want:
        wrapping.fail(f"{target.name}: sha256 {got}, expected {want}")
    print(f"{target.name}: sha256 matches", file=sys.stderr)


if __name__ == "__main__":
    main()
