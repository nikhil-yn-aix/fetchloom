"""check every downloaded package before publication: its bytes against SHA256SUMS, then its
provenance attestation with gh. pages.yml runs the same checks on the debian packages written
out, since the job that signs them runs no repository file."""

from __future__ import annotations

import argparse
import hashlib
import re
import shutil
import subprocess
import sys
from pathlib import Path


def check_release(folder: Path, suffix: str, count: int) -> list[str]:
    """the names of the ``count`` packages ending in ``suffix`` that SHA256SUMS in ``folder``
    lists, in order; raise ValueError unless each was downloaded as a regular file with that
    SHA-256, and no other."""
    entries = {}
    for number, line in enumerate((folder / "SHA256SUMS").read_text(encoding="utf-8").splitlines(), 1):
        match = re.fullmatch(r"([0-9a-f]{64}) [ *]([A-Za-z0-9][A-Za-z0-9._+-]*)", line)
        if match is None:
            raise ValueError(f"invalid checksum entry on line {number}")
        digest, name = match.groups()
        if name in entries:
            raise ValueError(f"duplicate checksum entry for {name}")
        entries[name] = digest
    selected = {name: digest for name, digest in entries.items() if name.endswith(suffix)}
    if count < 1 or len(selected) != count:
        raise ValueError(f"expected {count} {suffix} packages in SHA256SUMS, found {len(selected)}")
    downloaded = {path.name for path in folder.iterdir() if path.name.endswith(suffix)}
    missing = selected.keys() - downloaded
    unlisted = downloaded - selected.keys()
    if missing or unlisted:
        raise ValueError(f"package inventory differs: missing={sorted(missing)}, unlisted={sorted(unlisted)}")
    for name, expected in sorted(selected.items()):
        path = folder / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"{name} is not a regular package file")
        digest = hashlib.sha256()
        with path.open("rb") as file:
            for block in iter(lambda: file.read(1024 * 1024), b""):
                digest.update(block)
        if digest.hexdigest() != expected:
            raise ValueError(f"SHA-256 mismatch for {name}")
    print(f"checked all {count} {suffix} packages")
    return sorted(selected)


def check_provenance(folder: Path, names: list[str], repository: str, tag: str) -> None:
    """raise ValueError unless gh verifies, for each package, an attestation that release.yml of
    ``repository`` made for its bytes on a github-hosted runner from the tag ``tag``. a release's
    assets can be replaced after it is built, SHA256SUMS along with them."""
    gh = shutil.which("gh")
    if gh is None:
        raise ValueError("gh is needed to verify where each package was built")
    for name in names:
        verify = [gh, "attestation", "verify", str(folder / name), "--repo", repository,
                  "--signer-workflow", f"{repository}/.github/workflows/release.yml",
                  "--source-ref", f"refs/tags/{tag}", "--deny-self-hosted-runners"]
        if subprocess.run(verify, check=False).returncode != 0:
            raise ValueError(f"{name} has no attestation from release.yml at {tag}")
    print(f"release.yml built all {len(names)} packages from {tag}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("folder", type=Path)
    parser.add_argument("--suffix", choices=[".whl", ".deb"], required=True)
    parser.add_argument("--count", type=int, required=True)
    parser.add_argument("--repo", required=True, help="the repository that built the release, owner/name")
    parser.add_argument("--tag", required=True, help="the release's tag, such as v0.1.0")
    args = parser.parse_args()
    try:
        names = check_release(args.folder, args.suffix, args.count)
        check_provenance(args.folder, names, args.repo, args.tag)
    except (OSError, ValueError) as error:
        print(f"release verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
