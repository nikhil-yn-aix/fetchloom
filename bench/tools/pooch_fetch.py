"""pooch: retrieve with a known hash, unpacking with pooch's own processors, from the cache in
<home>. the files are then linked into the output folder, since pooch's users read them in its
cache.

usage:
  pooch_fetch.py <out> <home> <url> <sha256 or expect file> [--unpack zip|tar] [--name NAME]
  pooch_fetch.py <out> <home> doi:<doi>

the second form fetches every file of a zenodo, figshare or dataverse record, with the hashes
the repository lists, through pooch's registry from a DOI.
"""

import argparse
import sys
from pathlib import Path

import pooch

import wrapping


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("home", type=Path)
    parser.add_argument("source", help="a url, or doi:<doi> for a whole record")
    parser.add_argument("sha256", nargs="?", help="64 hex digits, or an expect file naming one file")
    parser.add_argument("--unpack", choices=["", "zip", "tar"], default="",
                        help="unpack with pooch.Unzip or pooch.Untar; empty for a plain file")
    parser.add_argument("--name", help="the cached file's name; pooch's default by default")
    args = parser.parse_args()
    cache = args.home / "pooch"
    if args.source.startswith("doi:"):
        if args.sha256 or args.unpack:
            wrapping.fail("a doi takes its hashes from the repository and is not unpacked here")
        record = pooch.create(path=cache, base_url=args.source.rstrip("/") + "/", registry=None)
        record.load_registry_from_doi()
        paths = [record.fetch(name) for name in record.registry]
        for path in paths:
            wrapping.place(path, args.out / Path(path).relative_to(cache).parent)
        print(f"{len(paths)} files", file=sys.stderr)
        return
    if not args.sha256:
        wrapping.fail("a url needs its sha256")
    processor = {"zip": pooch.Unzip, "tar": pooch.Untar, "": None}[args.unpack]
    result = pooch.retrieve(
        args.source,
        known_hash=f"sha256:{wrapping.pin(args.sha256)}",
        fname=args.name,
        path=cache,
        processor=processor() if processor else None,
    )
    if args.unpack:
        # the processor unpacks into <archive>.unzip or <archive>.untar beside the archive
        folder = next(p for p in Path(result[0]).parents if p.suffix in (".unzip", ".untar"))
        count = wrapping.place(folder, args.out)
    else:
        count = wrapping.place(result, args.out)
    print(f"{count} files", file=sys.stderr)


if __name__ == "__main__":
    main()
