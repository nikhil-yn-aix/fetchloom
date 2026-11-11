"""unpack only the expected release files, then run the packaged executable."""

import argparse
import json
from pathlib import Path
import tarfile
import zipfile

from check_artifacts import archive_name
from ci_journey import ROOT, digest, require


def unpack(archive, target, version, destination):
    program = "fetchloom.exe" if "windows" in target else "fetchloom"
    folder = f"fetchloom-{version}-{target}"
    expected = {f"{folder}/{name}" for name in [program, "LICENSE", "NOTICE", "README.md"]}
    destination.mkdir(parents=True, exist_ok=True)
    files = {}
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as packed:
            for member in packed.infolist():
                if member.is_dir():
                    require(member.filename.rstrip("/") == folder, "unexpected archive directory")
                    continue
                require(member.filename in expected and member.filename not in files, "unexpected or duplicate archive entry")
                require(member.file_size < 256 * 1024 * 1024, "oversized release file")
                files[member.filename] = packed.read(member)
    else:
        with tarfile.open(archive, "r:gz") as packed:
            for member in packed:
                if member.isdir():
                    require(member.name.rstrip("/") == folder, "unexpected archive directory")
                    continue
                require(member.isfile() and member.name in expected and member.name not in files, "unexpected or duplicate archive entry")
                require(member.size < 256 * 1024 * 1024, "oversized release file")
                if member.name.endswith("/fetchloom"):
                    require(member.mode & 0o111 != 0, "archive lost executable permissions")
                files[member.name] = packed.extractfile(member).read()
    require(set(files) == expected, "release archive is incomplete")
    for name, body in files.items():
        (destination / Path(name).name).write_bytes(body)
    binary = destination / program
    binary.chmod(0o755)
    return binary


def check_machine(binary, target):
    body = binary.read_bytes()
    arm = target.startswith("aarch64")
    if "linux" in target:
        require(body[:6] == b"\x7fELF\x02\x01", "not a 64-bit little-endian ELF")
        require(int.from_bytes(body[18:20], "little") == (183 if arm else 62), "wrong ELF architecture")
    elif "windows" in target:
        require(body[:2] == b"MZ", "not a Windows executable")
        offset = int.from_bytes(body[60:64], "little")
        require(body[offset:offset + 4] == b"PE\0\0", "invalid PE signature")
        require(int.from_bytes(body[offset + 4:offset + 6], "little") == (0xaa64 if arm else 0x8664), "wrong PE architecture")
    else:
        require(body[:4] == b"\xcf\xfa\xed\xfe", "not a 64-bit Mach-O")
        require(int.from_bytes(body[4:8], "little") == (0x100000c if arm else 0x1000007), "wrong Mach-O architecture")


def main():
    import subprocess
    import sys
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--built", type=Path, required=True)
    parser.add_argument("--dist", type=Path, default=Path("dist"))
    parser.add_argument("--proofs", type=Path, required=True)
    args = parser.parse_args()
    destination = Path(".release/artifact-bin") / args.target
    binary = unpack(args.dist / archive_name(args.target, args.version), args.target, args.version, destination)
    check_machine(binary, args.target)
    for name in ["LICENSE", "NOTICE", "README.md"]:
        require(digest(destination / name) == digest(ROOT / name), f"archive changed {name}")
    require(digest(binary) == digest(args.built), "archive binary differs from the checked release build")
    subprocess.run([sys.executable, "python/ci_journey.py", "--binary", str(binary.resolve()),
                    "--version", args.version, "--commit", args.commit,
                    "--report", str(args.proofs / "archive.json")], check=True)


if __name__ == "__main__":
    main()
