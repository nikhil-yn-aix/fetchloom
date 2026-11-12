"""require all six named targets and the exact assets checked by their native jobs."""

import argparse
import json
from pathlib import Path
import re

from ci_journey import digest, require, wheel_version

TARGETS = {
    "x86_64-unknown-linux-musl": {"wheel": {"manylinux2014_x86_64", "manylinux_2_17_x86_64", "musllinux_1_2_x86_64"}, "deb": "amd64"},
    "aarch64-unknown-linux-musl": {"wheel": {"manylinux2014_aarch64", "manylinux_2_17_aarch64", "musllinux_1_2_aarch64"}, "deb": "arm64"},
    "x86_64-pc-windows-msvc": {"wheel": {"win_amd64"}},
    "aarch64-pc-windows-msvc": {"wheel": {"win_arm64"}},
    # the intel image has no 3.15, which takes about 90 s to install there
    "x86_64-apple-darwin": {"wheel": {"macosx_11_0_x86_64"}, "python": "3.14."},
    "aarch64-apple-darwin": {"wheel": {"macosx_11_0_arm64"}},
}


def archive_name(target, version):
    extension = ".zip" if "windows" in target else ".tar.gz"
    return f"fetchloom-{version}-{target}{extension}"


def stages(target):
    expected = {"archive", "wheel-current"}
    if target in {"x86_64-unknown-linux-musl", "x86_64-pc-windows-msvc"}:
        expected.add("wheel-minimum")
    if "windows" not in target:
        expected.add("installer-sh")
    elif target.startswith("x86_64"):
        expected.update({"installer-powershell", "installer-pwsh"})
    if target in {"x86_64-unknown-linux-musl", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"}:
        expected.add("rar")
    if "linux" in target:
        expected.add("deb")
    return expected


def inventory(folder, proofs, version, commit):
    documents = [json.loads(path.read_text(encoding="utf-8")) for path in proofs.rglob("verified.json")]
    names = [document["target"] for document in documents]
    require(len(names) == len(set(names)) and set(names) == set(TARGETS), f"target inventory differs: {sorted(names)}")
    expected = {}
    for document in documents:
        target = document["target"]
        require(document["version"] == version and document["commit"] == commit, f"wrong revision in {target}")
        require(set(document["checks"]) == stages(target), f"missing or unexpected check in {target}")
        hashes = set()
        for stage, proof in document["checks"].items():
            require(proof["ok"] is True, f"{target}: {stage} did not pass")
            require(proof["version"] == version and proof["commit"] == commit, f"{target}: {stage} used another revision")
            require(re.fullmatch(r"[0-9a-f]{64}", proof["binary_sha256"]) is not None, "invalid executable hash")
            hashes.add(proof["binary_sha256"])
            if stage.startswith("wheel-"):
                require(proof.get("helper") is True, f"{target}: the installed helper was not exercised")
            if stage == "wheel-current":
                python = TARGETS[target].get("python", "3.15.")
                require(proof["python"].startswith(python), f"{target}: the wheel was not exercised on Python {python}x")
            if stage == "wheel-minimum":
                require(proof["python"].startswith("3.10."), "Python 3.10 wheel was not exercised")
        require(len(hashes) == 1, f"{target}: checks did not use the same executable")
        files = document["files"]
        archive = archive_name(target, version)
        wheels = [name for name in files if name.endswith(".whl")]
        require(len(wheels) == 1, f"{target}: expected exactly one wheel")
        match = re.fullmatch(rf"fetchloom-{re.escape(wheel_version(version))}-py3-none-(.+)\.whl", wheels[0])
        require(match is not None and set(match[1].split(".")) == TARGETS[target]["wheel"], f"wrong wheel target: {wheels[0]}")
        wanted = {archive, wheels[0]}
        if "deb" in TARGETS[target]:
            wanted.add(f"fetchloom_{version}_{TARGETS[target]['deb']}.deb")
        require(set(files) == wanted, f"{target}: unexpected package files {sorted(files)}")
        for name, checksum in files.items():
            require(name not in expected, f"duplicate asset {name}")
            path = folder / name
            require(path.is_file() and not path.is_symlink() and digest(path) == checksum, f"missing or changed asset: {name}")
            expected[name] = checksum
    require({path.name for path in folder.iterdir()} == set(expected), "unlisted or missing release assets")
    source = list(proofs.rglob("source-package.json"))
    require(len(source) == 1, "missing source-package install proof")
    source = json.loads(source[0].read_text(encoding="utf-8"))
    require(source["ok"] is True and source["version"] == version and source["commit"] == commit, "source package did not pass")
    print(f"verified all {len(TARGETS)} targets and {len(expected)} assets for {commit}")
    return expected


def record(folder, proof_dir, target, version, commit):
    checks = {}
    for name in sorted(stages(target)):
        path = proof_dir / f"{name}.json"
        require(path.is_file(), f"missing executed check: {target}/{name}")
        checks[name] = json.loads(path.read_text(encoding="utf-8"))
    document = {"target": target, "version": version, "commit": commit, "checks": checks,
                "files": {path.name: digest(path) for path in folder.iterdir() if path.is_file()}}
    (proof_dir / "verified.json").write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["record", "inventory"])
    parser.add_argument("folder", type=Path)
    parser.add_argument("proofs", type=Path)
    parser.add_argument("--target", choices=sorted(TARGETS))
    parser.add_argument("--version", required=True)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    if args.command == "record":
        require(args.target is not None, "record requires a target")
        record(args.folder, args.proofs, args.target, args.version, args.commit)
    else:
        inventory(args.folder, args.proofs, args.version, args.commit)


if __name__ == "__main__":
    main()
