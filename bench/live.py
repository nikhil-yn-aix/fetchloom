"""Interleaved live comparisons against each service's own client."""
import argparse
from collections import Counter
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
from urllib.parse import quote
import urllib.request

import fixtures
import lab
import run
import tools

SERVICES = ("zenodo", "huggingface", "kaggle", "figshare", "dataverse")
PYTHON = "/opt/fl-bench/venv/bin/python"


def compare(first, second):
    if not first or not second:
        status = "empty"
    elif first == second:
        status = "identical"
    elif Counter(first.values()) == Counter(second.values()):
        status = "layout-difference"
    else:
        status = "content-difference"
    return dict(status=status, only_first=sorted(set(first) - set(second)),
                only_second=sorted(set(second) - set(first)),
                changed=sorted(name for name in first.keys() & second.keys() if first[name] != second[name]))


def request(url):
    return urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "fetchloom-benchmark/1"}), timeout=45)


def get_json(url):
    with request(url) as response:
        return json.load(response)


def catalog(service, quick):
    result = dict(service=service, select=[], quick=quick)
    files = []
    if service == "zenodo":
        record = "3608990" if quick else "8164711"
        data = get_json(f"https://zenodo.org/api/records/{record}")
        result.update(ref=f"zenodo:{record}", native=record, tool="zenodo_get",
                      select=[] if quick else ["pbmc.RData"])
        for item in data["files"]:
            if not result["select"] or item["key"] in result["select"]:
                files.append(dict(name=item["key"], size=item["size"], url=item["links"]["self"], checksum=item["checksum"]))
    elif service == "huggingface":
        repo = "scikit-learn/iris" if quick else "stanfordnlp/imdb"
        revision = get_json(f"https://huggingface.co/api/datasets/{repo}")["sha"] if quick else "e6281661ce1c48d982bc483cf8a173c1bbeb5d31"
        selection = "Iris.csv" if quick else "plain_text/*.parquet"
        data = get_json(f"https://huggingface.co/api/datasets/{repo}/tree/{revision}?recursive=true&expand=true")
        result.update(ref=f"hf:datasets/{repo}@{revision}", native=repo, revision=revision,
                      tool="hf", select=[selection])
        for item in data:
            if item["type"] == "file" and fnmatch.fnmatch(item["path"], selection):
                files.append(dict(name=item["path"], size=item["size"],
                                  url=f"https://huggingface.co/datasets/{repo}/resolve/{revision}/{item['path']}",
                                  checksum="sha256:" + item["lfs"]["oid"] if "lfs" in item else None))
    elif service == "kaggle":
        repo, version = ("uciml/iris", 2) if quick else ("zynicide/wine-reviews", 4)
        data = get_json(f"https://www.kaggle.com/api/v1/datasets/list/{repo}?datasetVersionNumber={version}")
        result.update(ref=f"kaggle:{repo}@{version}", native=f"{repo}/versions/{version}", tool="kagglehub",
                      select=["Iris.csv" if quick else "winemag-data-130k-v2.csv"])
        for item in data["datasetFiles"]:
            if item["name"] not in result["select"]:
                continue
            files.append(dict(name=item["name"], size=item["totalBytes"], checksum=None,
                              url=f"https://api.kaggle.com/v1/datasets/download/{repo}/{quote(item['name'])}?datasetVersionNumber={version}&raw=true"))
    elif service == "figshare":
        data = get_json("https://api.figshare.com/v2/articles/5616403")
        result.update(ref="figshare:5616403", native="10.6084/m9.figshare.5616403", tool="datahugger",
                      max_file_size=1024 if quick else None)
        for item in data["files"]:
            if not quick or item["size"] <= 1024:
                files.append(dict(name=item["name"], size=item["size"], url=item["download_url"],
                                  checksum="md5:" + item["computed_md5"]))
        if quick:
            result["select"] = [item["name"] for item in files]
    else:
        base = "https://dataverse.harvard.edu"
        data = get_json(base + "/api/datasets/:persistentId/?persistentId=doi:10.7910/DVN/TJCLKP")["data"]
        version = data["latestVersion"]
        result.update(ref="dataverse:dataverse.harvard.edu/doi:10.7910/DVN/TJCLKP",
                      native=base + "/dataset.xhtml?persistentId=doi:10.7910/DVN/TJCLKP", tool="datahugger", max_file_size=1024 if quick else None,
                      version=f"{version['versionNumber']}.{version['versionMinorNumber']}")
        for entry in data.get("files", version.get("files", [])):
            item = entry["dataFile"]
            if entry.get("restricted"):
                continue
            if quick and item["filesize"] > 1024:
                continue
            name = item.get("originalFileName", item["filename"])
            folder = entry.get("directoryLabel", "")
            files.append(dict(name=f"{folder}/{name}" if folder else name,
                              size=item.get("originalFileSize", item["filesize"]),
                              url=base + f"/api/access/datafile/{item['id']}?format=original", checksum=None))
        if quick:
            result["select"] = ["**/" + Path(item["name"]).name for item in files]
    if not files:
        raise ValueError("the public API returned no matching files")
    result["files"] = files
    result["bytes"] = sum(item["size"] for item in files)
    return result


def reference(dataset, root):
    root.mkdir(parents=True)
    hashes = {}
    for index, item in enumerate(dataset["files"]):
        path = root / str(index)
        with request(item["url"]) as response, path.open("wb") as f:
            shutil.copyfileobj(response, f, 1024 * 1024)
        if path.stat().st_size != item["size"]:
            raise ValueError(f"reference size differs from API for {item['name']}")
        if item["checksum"]:
            algorithm, expected = item["checksum"].split(":", 1)
            with path.open("rb") as f:
                actual = hashlib.file_digest(f, algorithm).hexdigest()
            if actual != expected:
                raise ValueError(f"reference checksum mismatch for {item['name']}")
        hashes[item["name"]] = fixtures.sha256(path)
    return hashes


def fingerprint(root, client=None):
    if root.is_file():
        return {root.name: fixtures.sha256(root)}
    result = {}
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        relative = path.relative_to(root)
        if any(part in (".cache", ".fetchloom") for part in relative.parts):
            continue
        if (client == "datahugger" and relative.parts[0] == "generated"
                and relative.name.startswith("checksums") and relative.suffix == ".json"):
            continue
        result[str(relative)] = fixtures.sha256(path)
    return result

def command(name, dataset, work, fetchloom):
    dest = work / "output"
    if name == "fetchloom":
        argv = [str(fetchloom), "-q", "--yes", "get", dataset["ref"], "--no-extract", "-o", str(dest)]
        for pattern in dataset["select"]:
            argv += ["--select", pattern]
        return argv
    if name == "zenodo_get":
        argv = ["/opt/fl-bench/venv/bin/zenodo_get", dataset["native"], "-o", str(dest)]
        for pattern in dataset["select"]:
            argv += ["--glob", pattern]
        return argv
    if name == "hf":
        argv = ["/opt/fl-bench/venv/bin/hf", "download", dataset["native"], "--repo-type", "dataset",
                "--revision", dataset["revision"], "--include", *dataset["select"], "--local-dir", str(dest)]
        checks = work / "SHA256SUMS"
        checks.write_text("".join(f"{digest}  {dest / filename}\n"
                                  for filename, digest in dataset["expected_sha256"].items()))
        return tools.script(work / "command.sh", [tools.shell(argv), tools.shell(["sha256sum", "-c", checks])])
    script = work / "download.py"
    if name == "kagglehub":
        script.write_text("import kagglehub, hashlib\nfrom pathlib import Path\n"
                          f"path = kagglehub.dataset_download({dataset['native']!r}, path={dataset['select'][0]!r})\n"
                          "with Path(path).open('rb') as source:\n"
                          "    actual = hashlib.file_digest(source, 'sha256').hexdigest()\n"
                          f"if actual != {dataset['expected_sha256'][dataset['select'][0]]!r}:\n"
                          "    raise ValueError('downloaded file has wrong SHA-256')\n"
                          f"Path({str(work / 'output-path')!r}).write_text(path)\n")
    else:
        prelude = ""
        if dataset["service"] == "dataverse":
            prelude = ("import functools, requests\n"
                       "requests.get = functools.partial(requests.get, headers={\n"
                       "    'User-Agent': 'fetchloom-benchmark/1 datahugger/0.13'})\n")
        script.write_text(prelude + "import datahugger\n"
                          f"datahugger.get({dataset['native']!r}, {str(dest)!r}, "
                          f"checksum=True, unzip=False, max_file_size={dataset.get('max_file_size')!r})\n")
    return [PYTHON, str(script)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetchloom", required=True, type=Path)
    parser.add_argument("--quick", action="store_true")
    parser.add_argument("--services", type=lambda x: run.csv_choices(x, SERVICES), default=list(SERVICES))
    parser.add_argument("--seed", type=int, default=1729)
    parser.add_argument("--out", type=Path, default=lab.ROOT / "results" / "live")
    parser.add_argument("--timeout", type=float, default=1200)
    args = parser.parse_args()
    lab.require_root()
    if not args.out.resolve().is_relative_to(lab.ROOT):
        parser.error("write results under /srv/fl-bench")
    if not (lab.ROOT / "owned.json").exists():
        parser.error("run lab.py setup first")
    lock = lab.exclusive()
    if (args.out / "runs.jsonl").exists():
        parser.error("output already contains runs")
    args.fs = ["ext4"]
    args.profiles = ["live"]
    args.fetchloom = args.fetchloom.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    meta = run.metadata(args, {"fetchloom": tools.Fetchloom(args.fetchloom)})
    meta.update(kind="live", datasets=[], rounds=1 if args.quick else 5)
    for name in ("zenodo_get", "huggingface_hub", "hf_xet", "kagglehub", "datahugger"):
        meta["tool_versions"][name] = subprocess.check_output(
            [PYTHON, "-c", f"import importlib.metadata; print(importlib.metadata.version({name!r}))"], text=True).strip()
    rng = random.Random(args.seed)
    failures = 0
    for service in args.services:
        reference_root = args.out / "references" / service
        try:
            dataset = catalog(service, args.quick)
            expected = reference(dataset, reference_root)
            dataset["expected_sha256"] = expected
            meta["datasets"].append(dataset)
        except Exception as error:
            reason = f"public metadata or reference unavailable: {type(error).__name__}: {error}"
            run.append(args.out / "runs.jsonl", dict(kind="live", service=service, status="skipped", reason=reason, quick=args.quick))
            print(f"{service}: skipped: {reason}", flush=True)
            continue
        for number in range(1, meta["rounds"] + 1):
            order = ["fetchloom", dataset["tool"]]
            rng.shuffle(order)
            meta["orders"].append(dict(service=service, round=number, tools=order))
            pair = {}
            for name in order:
                work = lab.ROOT / "fs" / "ext4" / "live-work"
                tools.reset(work)
                for folder in ("home", "cache", "scratch"):
                    (work / folder).mkdir()
                env = tools.Tool(args.fetchloom).env(work)
                env.update(HF_HOME=str(work / "cache" / "hf"), HF_XET_CACHE=str(work / "cache" / "xet"),
                           HF_HUB_CACHE=str(work / "cache" / "hf" / "hub"),
                           HUGGINGFACE_HUB_CACHE=str(work / "cache" / "hf" / "hub"),
                           HF_TOKEN_PATH=str(work / "home" / "hf-token"), HF_HUB_DISABLE_IMPLICIT_TOKEN="1",
                           KAGGLEHUB_CACHE=str(work / "cache" / "kaggle"), HF_HUB_DISABLE_TELEMETRY="1")
                # do not inherit credentials: the live comparison is public and reproducible.
                for key in ("HF_TOKEN", "HUGGING_FACE_HUB_TOKEN", "KAGGLE_KEY", "KAGGLE_USERNAME", "KAGGLE_API_TOKEN"):
                    env.pop(key, None)
                argv = command(name, dataset, work, args.fetchloom)
                meta["commands"][f"{service}/{name}"] = run.exact_command(argv, work)
                if (work / "download.py").exists():
                    meta["commands"][f"{service}/{name}"]["download.py"] = (work / "download.py").read_text()
                logfile = args.out / f"{service}-{number}-{name}.log"
                lab.drop_caches([])
                before = run.free_bytes(work)
                row = dict(kind="live", service=service, scenario=service, tool=name, round=number, warmup=False,
                           quick=args.quick, filesystem="ext4", profile="live", status="failed",
                           command=meta["commands"][f"{service}/{name}"], count=len(expected), file_size=dataset["bytes"])
                row["environment"] = {key: value for key, value in env.items()
                                      if key in ("HOME", "XDG_CACHE_HOME", "FETCHLOOM_CACHE", "TMPDIR",
                                                 "HF_HOME", "HF_HUB_CACHE", "HUGGINGFACE_HUB_CACHE", "HF_XET_CACHE",
                                                 "KAGGLEHUB_CACHE", "HF_TOKEN_PATH", "HF_HUB_DISABLE_IMPLICIT_TOKEN",
                                                 "REQUESTS_CA_BUNDLE", "SSL_CERT_FILE", "HF_HUB_DISABLE_TELEMETRY")}
                row.update(run.measure(argv, work, env, logfile, args.timeout))
                os.sync()
                row["free_before_bytes"] = before
                row["free_after_bytes"] = run.free_bytes(work)
                row["disk_bytes"] = before - row["free_after_bytes"]
                output_root = Path((work / "output-path").read_text()) if (work / "output-path").exists() else work / "output"
                actual = fingerprint(output_root, client=name)
                row["files"] = actual
                row["comparison_to_reference"] = compare(expected, actual)
                if row["exit_code"] == 0 and not row["timeout"] and row["comparison_to_reference"]["status"] in ("identical", "layout-difference"):
                    row["status"] = "passed"
                elif name == "kagglehub" and any(word in logfile.read_text(errors="replace").lower()
                                                for word in ("credential", "unauthorized", "401", "403", "authenticate", "captcha")):
                    row.update(status="skipped", reason="kagglehub refused anonymous access; see command log")
                failures += row["status"] == "failed"
                pair[name] = actual
                run.append(args.out / "runs.jsonl", row)
                print(f"{service} round {number}/{meta['rounds']} {name}: {row['status']} {row['wall_seconds']:.3f}s", flush=True)
                shutil.rmtree(work)
            run.append(args.out / "runs.jsonl", dict(kind="comparison", service=service, round=number,
                                                     comparison=compare(pair["fetchloom"], pair[dataset["tool"]])))
        (args.out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    (args.out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
    lock.close()
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
