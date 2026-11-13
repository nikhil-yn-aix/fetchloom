"""deterministic fixtures, high-entropy or csv tables, and native prepared projects."""
from datetime import date, timedelta
import gzip
import hashlib
import itertools
import json
from pathlib import Path
import random
import shutil
import subprocess
import tarfile
import zipfile

import lab


ALL_SCENARIOS = tuple(f"s{i}" for i in range(1, 10))
# scenarios measured after an untimed run of another scenario's files, and that scenario
WARM = {"s5": "s2", "s6": "s2", "s9": "s3"}
# every member written on 2020-01-01, so an archive's bytes depend only on its members
EPOCH = 1577836800


def content(seed, name, size):
    return hashlib.shake_256(f"{seed}:{name}".encode()).digest(size)


def table(seed, name, size):
    """a csv table of at most `size` bytes, rows a field survey might log, so it compresses as
    research tables do."""
    rng = random.Random(f"{seed}:{name}")
    sites = ("north ridge", "creek", "meadow", "pine stand", "delta", "south slope")
    species = ("Abies alba", "Betula pendula", "Carpinus betulus", "Fagus sylvatica", "Picea abies", "Quercus robur")
    out = [b"id,date,site,species,count,mass_g,length_mm,flag\n"]
    total = len(out[0])
    for row in itertools.count():
        day = date(2019, 1, 1) + timedelta(days=row // 40)
        line = (f"{row},{day.isoformat()},{rng.choice(sites)},{rng.choice(species)},{rng.randint(0, 40)},"
                f"{rng.uniform(.5, 900):.2f},{rng.gauss(120, 30):.1f},{'check' if rng.random() < .05 else 'ok'}\n").encode()
        if total + len(line) > size:
            break
        out.append(line)
        total += len(line)
    return b"".join(out)


def sha256(path):
    with Path(path).open("rb") as f:
        return hashlib.file_digest(f, "sha256").hexdigest()


def case_specs(profiles, scenarios, quick):
    cases = []
    for profile in profiles:
        for scenario in scenarios:
            if scenario not in ("s1", "s2", "s7") and profile != "lan":
                continue
            if scenario == "s7" and profile != "home":
                continue
            base = "s1" if scenario == "s7" else WARM.get(scenario, scenario)
            size = (lab.PROFILES[profile][3] if base == "s1" else 102400 if base == "s2"
                    else 1024 * 1024 if base == "s8" else 4096)
            count = 1 if base == "s1" else lab.PROFILES[profile][4] if base == "s2" else 100 if base == "s8" else 10000
            if quick:
                size = 2 * 1024 * 1024 if base == "s1" else 64 * 1024 if base == "s8" else size
                count = min(count, 12)
                # keep the prescribed ten-second interruption meaningful at 100 mbit/s
                if scenario == "s7":
                    size = 256 * 1024 * 1024
            cases.append(dict(id=scenario, base=base, profile=profile, size=size,
                              count=count, quick=quick))
    return cases


def identity(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def read_json(path):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return None


def write_json(path, value):
    pending = path.with_suffix(path.suffix + ".new")
    pending.write_text(json.dumps(value, indent=2) + "\n")
    pending.replace(path)


def build(case, seed):
    key = f"{seed}-{'quick' if case['quick'] else 'full'}-{case['profile']}-{case['base']}"
    if case["id"] == "s7" and case["quick"]:
        key += "-interrupt"
    target = lab.ROOT / "www" / key
    manifest = target / "manifest.json"
    case = dict(case, key=key)
    signature = dict(seed=seed, generator=sha256(Path(__file__)),
                     **{name: case[name] for name in ("base", "profile", "quick", "size", "count")})
    previous = read_json(manifest)
    if (isinstance(previous, dict) and previous.get("fixture_signature") == signature
            and previous.get("server_dir") == str(target)):
        return dict(case, **previous)
    if target.exists():
        shutil.rmtree(target)
    target.mkdir(parents=True, exist_ok=True)
    files = []
    for index in range(case["count"]):
        name = f"table-{index:05}.csv" if case["base"] == "s8" else f"file-{index:05}.bin"
        path = target / name
        with path.open("wb") as f:
            if case["base"] == "s8":
                f.write(table(seed, f"{key}/{name}", case["size"]))
            remaining = 0 if case["base"] == "s8" else case["size"]
            chunk = 0
            while remaining:
                length = min(1024 * 1024, remaining)
                f.write(content(seed, f"{key}/{name}/{chunk}", length))
                remaining -= length
                chunk += 1
        files.append(dict(name=name, sha256=sha256(path), size=path.stat().st_size,
                          url=f"https://lab.flb/{key}/{name}"))
    members = []
    if case["base"] == "s8":
        members = files
        archive = target / "members.tar.gz"
        with archive.open("wb") as raw, gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as packed, \
                tarfile.open(fileobj=packed, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            for item in members:
                info = tarfile.TarInfo(item["name"])
                info.size, info.mtime, info.mode = item["size"], EPOCH, 0o644
                with (target / item["name"]).open("rb") as body:
                    tar.addfile(info, body)
        files = [dict(name=archive.name, sha256=sha256(archive), size=archive.stat().st_size,
                      url=f"https://lab.flb/{key}/{archive.name}")]
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
    data = dict(files=files, members=members, server_dir=str(target), fixture_signature=signature)
    write_json(manifest, data)
    return dict(case, **data)


def prepare_native(case, fetchloom, names):
    root = lab.ROOT / "prepared" / case["key"]
    root.mkdir(parents=True, exist_ok=True)
    env = lab.environment()
    for tool in names:
        target = root / tool
        signature = dict(tool=tool, fixture=identity({name: case.get(name) for name in
                                                     ("key", "files", "members", "fixture_signature")}),
                         harness={name: sha256(Path(__file__).with_name(name)) for name in
                                  ("fixtures.py", "lab.py", "tools.py", "uv.lock")})
        if tool == "fetchloom":
            signature["executable"] = sha256(fetchloom)
        if read_json(target / ".ready") == signature:
            continue
        if target.exists():
            shutil.rmtree(target)
        target.mkdir()
        log = root / f"{tool}-prepare.log"
        with log.open("w") as output:
            def run(argv, cwd=target, input=None):
                lab.run_group(argv, cwd=cwd, env=env, input=input, text=True, stdout=output,
                              stderr=subprocess.STDOUT, timeout=lab.PREPARE_TIMEOUT)
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
                run(["git", "config", "annex.security.allowed-ip-addresses", lab.SERVER_ADDRESS])
                (target / "data").mkdir(exist_ok=True)
                batch = "".join(f'{item["url"]} data/{item["name"]}\n' for item in case["files"])
                run(["git", "annex", "addurl", "--batch", "--with-files"], input=batch)
                run(["datalad", "save", "-m", "prepare benchmark inputs"])
                run(["git", "annex", "drop", "--force", "--all"])
            write_json(target / ".ready", signature)
    case["prepared"] = str(root)
    return case
