"""Deterministic high-entropy fixtures and native prepared projects."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import zipfile

import lab


ALL_SCENARIOS = tuple(f"s{i}" for i in range(1, 8))


def content(seed, name, size):
    return hashlib.shake_256(f"{seed}:{name}".encode()).digest(size)


def sha256(path):
    with Path(path).open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def case_specs(profiles, scenarios, quick):
    cases = []
    for profile in profiles:
        for scenario in scenarios:
            if scenario in ("s3", "s4", "s5", "s6") and profile != "lan":
                continue
            if scenario == "s7" and profile != "home":
                continue
            base = "s1" if scenario == "s7" else "s2" if scenario in ("s5", "s6") else scenario
            size = lab.PROFILES[profile][3] if base == "s1" else 102400 if base == "s2" else 4096
            count = 1 if base == "s1" else lab.PROFILES[profile][4] if base == "s2" else 10000
            if quick:
                size = 2 * 1024 * 1024 if base == "s1" else size
                count = min(count, 12)
                # keep the prescribed ten-second interruption meaningful at 100 mbit/s.
                if scenario == "s7":
                    size = 256 * 1024 * 1024
            cases.append(dict(id=scenario, base=base, profile=profile, size=size,
                              count=count, quick=quick))
    return cases


def build(case, seed):
    key = f"{seed}-{'quick' if case['quick'] else 'full'}-{case['profile']}-{case['base']}"
    if case["id"] == "s7" and case["quick"]:
        key += "-interrupt"
    target = lab.ROOT / "www" / key
    manifest = target / "manifest.json"
    case = dict(case, key=key)
    if manifest.exists():
        return dict(case, **json.loads(manifest.read_text()))
    target.mkdir(parents=True, exist_ok=True)
    files = []
    for index in range(case["count"]):
        name = f"file-{index:05}.bin"
        path = target / name
        with path.open("wb") as f:
            remaining = case["size"]
            chunk = 0
            while remaining:
                length = min(1024 * 1024, remaining)
                f.write(content(seed, f"{key}/{name}/{chunk}", length))
                remaining -= length
                chunk += 1
        files.append(dict(name=name, sha256=sha256(path), size=case["size"],
                          url=f"https://lab.flb/{key}/{name}"))
    members = []
    if case["base"] == "s4":
        members = files
        archive = target / "members.zip"
        with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_STORED) as z:
            for item in members:
                info = zipfile.ZipInfo(item["name"], date_time=(2020, 1, 1, 0, 0, 0))
                info.external_attr = 0o644 << 16
                z.writestr(info, (target / item["name"]).read_bytes())
        files = [dict(name=archive.name, sha256=sha256(archive), size=archive.stat().st_size,
                      url=f"https://lab.flb/{key}/{archive.name}")]
    data = dict(files=files, members=members, server_dir=str(target))
    manifest.write_text(json.dumps(data, indent=2) + "\n")
    return dict(case, **data)


def prepare_native(case, fetchloom, names):
    root = lab.ROOT / "prepared" / case["key"]
    root.mkdir(parents=True, exist_ok=True)
    env = lab.environment()
    for tool in names:
        target = root / tool
        if (target / ".ready").exists():
            continue
        if target.exists():
            shutil.rmtree(target)
        target.mkdir()
        log = root / f"{tool}-prepare.log"
        with log.open("w") as output:
            def run(argv, cwd=target, input=None):
                subprocess.run([str(x) for x in argv], cwd=cwd, env=env, input=input,
                               text=True, stdout=output, stderr=subprocess.STDOUT, check=True, timeout=1200)
            if tool == "fetchloom":
                text = "\n".join(f'[datasets.f{i:05}]\nref = "{item["url"]}"\n'
                                 for i, item in enumerate(case["files"]))
                (target / "data.toml").write_text(text)
                env["FETCHLOOM_CACHE"] = str(root / "lock-cache")
                run([fetchloom, "-q", "--yes", "lock"])
                shutil.rmtree(root / "lock-cache", ignore_errors=True)
                env.pop("FETCHLOOM_CACHE", None)
            elif tool == "dvc":
                run(["git", "init", "-q"])
                run(["dvc", "init", "-q"])
                data = target / "data"
                data.mkdir()
                for item in case["files"]:
                    shutil.copyfile(Path(case["server_dir"]) / item["name"], data / item["name"])
                run(["dvc", "add", "data"])
                remote = lab.ROOT / "www" / "dvc" / case["key"]
                remote.mkdir(parents=True, exist_ok=True)
                run(["dvc", "remote", "add", "stage", remote])
                run(["dvc", "push", "-r", "stage"])
                run(["dvc", "remote", "remove", "stage"])
                run(["dvc", "remote", "add", "-d", "lab", f"https://lab.flb/dvc/{case['key']}"])
                run(["dvc", "remote", "modify", "lab", "verify", "true"])
                run(["dvc", "config", "cache.type", "reflink,hardlink,copy"])
                shutil.rmtree(data)
                shutil.rmtree(target / ".dvc" / "cache", ignore_errors=True)
                shutil.rmtree(target / ".dvc" / "tmp", ignore_errors=True)
            elif tool == "datalad":
                run(["datalad", "create", "-f", "."])
                run(["git", "config", "annex.backend", "SHA256E"])
                run(["git", "config", "annex.security.allowed-ip-addresses", "10.77.0.2"])
                (target / "data").mkdir(exist_ok=True)
                batch = "".join(f'{item["url"]} data/{item["name"]}\n' for item in case["files"])
                run(["git", "annex", "addurl", "--batch", "--with-files"], input=batch)
                run(["datalad", "save", "-m", "prepare benchmark inputs"])
                run(["git", "annex", "drop", "--force", "--all"])
            else:
                (target / "registry.json").write_text(json.dumps(case["files"]))
            (target / ".ready").write_text("1\n")
    case["prepared"] = str(root)
    return case
