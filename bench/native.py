"""Compare two fetchloom builds on this machine's own filesystem.

the lab in run.py needs linux namespaces, loop mounts and tc. this runner needs none of them, so it
also runs on windows (ntfs or a dev drive, with defender as it is) and macos (apfs, or an exfat
drive). it generates fixed files, serves them from a local http server, and times cold syncs, warm
placement, settled syncs and `path` for each build in alternating order, round by round.
"""
import argparse
import functools
import hashlib
import http.server
import json
import math
import os
from pathlib import Path
import platform
import random
import shutil
import stat
import statistics
import subprocess
import sys
import threading
import time
import zipfile

import ab

SCENARIOS = {
    "small": dict(count=10000, size=4096, archive=False),
    "zip": dict(count=10000, size=4096, archive=True),
    "large": dict(count=1, size=1 << 30, archive=False),
}
RESAMPLES = 20000


def content(seed, name, size):
    return hashlib.shake_256(f"{seed}:{name}".encode()).digest(size)


def write_file(path, seed, name, size):
    with path.open("wb") as output:
        written, chunk = 0, 0
        while written < size:
            length = min(1 << 20, size - written)
            output.write(content(seed, f"{name}/{chunk}", length))
            written += length
            chunk += 1


def build_fixture(www, name, spec, seed):
    """writes the scenario's files once; returns the file names fetchloom is asked for."""
    folder = www / name
    marker = folder / ".complete"
    expected = json.dumps(dict(spec, seed=seed), sort_keys=True)
    names = [f"file-{index:05}.bin" for index in range(spec["count"])]
    if spec["archive"]:
        names_served = ["members.zip"]
    else:
        names_served = names
    if marker.exists() and marker.read_text() == expected:
        return names_served
    remove(folder)
    folder.mkdir(parents=True)
    if spec["archive"]:
        with zipfile.ZipFile(folder / "members.zip", "w", compression=zipfile.ZIP_STORED) as archive:
            for member in names:
                info = zipfile.ZipInfo(member, date_time=(2020, 1, 1, 0, 0, 0))
                info.external_attr = 0o644 << 16
                archive.writestr(info, content(seed, f"{name}/{member}/0", spec["size"]))
    else:
        for member in names:
            write_file(folder / member, seed, f"{name}/{member}", spec["size"])
    marker.write_text(expected)
    return names_served


def manifest(base_url, scenario, names):
    return "".join(f'[datasets.f{index:05}]\nref = "{base_url}/{scenario}/{name}"\n\n'
                   for index, name in enumerate(names))


class Quiet(http.server.SimpleHTTPRequestHandler):
    # keep-alive, as real file servers do; http/1.0 would cost a new connection per file
    protocol_version = "HTTP/1.1"
    # headers and body go out in separate writes; with nagle on, the client's delayed ack holds
    # the body back about 40 ms on every response
    disable_nagle_algorithm = True

    def log_message(self, *_):
        pass


class Server(http.server.ThreadingHTTPServer):
    # the default backlog of 5 drops connections when many downloads start together
    request_queue_size = 1024


def serve(www):
    handler = functools.partial(Quiet, directory=str(www))
    server = Server(("127.0.0.1", 0), handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def run(argv, cwd, env):
    start = time.perf_counter()
    done = subprocess.run(argv, cwd=cwd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    seconds = time.perf_counter() - start
    if done.returncode != 0:
        raise RuntimeError(f"{' '.join(map(str, argv))} in {cwd} exited {done.returncode}: {done.stderr.strip()}")
    return seconds


def environment(cache):
    env = dict(os.environ)
    env["FETCHLOOM_CACHE"] = str(cache)
    env.pop("NO_COLOR", None)
    return env


def prepare(binary, label, scenario, text, work):
    """locks the scenario once per build, so each build reads a lock it wrote."""
    template = work / "templates" / label / scenario
    if (template / "data.lock").exists():
        return template
    remove(template)
    template.mkdir(parents=True)
    (template / "data.toml").write_text(text)
    cache = work / "lock-cache" / label
    run([binary, "-q", "--yes", "lock"], template, environment(cache))
    remove(cache)
    return template


def fresh_project(template, path):
    remove(path)
    path.mkdir(parents=True)
    for name in ("data.toml", "data.lock"):
        shutil.copyfile(template / name, path / name)
    return path


def measure_round(binary, template, work):
    """one build's four phases from an empty cache; returns seconds per phase."""
    cache = work / "cache"
    remove(cache)
    env = environment(cache)
    times = {}
    first = fresh_project(template, work / "cold")
    times["cold"] = run([binary, "-q", "--yes", "sync"], first, env)
    second = fresh_project(template, work / "warm")
    times["warm"] = run([binary, "-q", "--yes", "sync"], second, env)
    times["settled"] = run([binary, "-q", "--yes", "sync"], second, env)
    times["path"] = run([binary, "-q", "path", "f00000"], second, env)
    for folder in (first, second, cache):
        remove(folder)
    return times


def summarize(baseline, candidate, seed):
    """paired log ratios of candidate over baseline, matched by round."""
    logs = [math.log(c) - math.log(b) for b, c in zip(baseline, candidate) if b > 0 and c > 0]
    interval = ab.bootstrap(logs, seed, RESAMPLES)
    ratio = math.exp(statistics.median(logs)) if logs else None
    bounds = [math.exp(bound) for bound in interval] if interval else None
    if bounds is None or len(logs) < 3:
        verdict = "too few rounds"
    elif bounds[1] < 1:
        verdict = "candidate faster"
    elif bounds[0] > 1:
        verdict = "candidate slower"
    else:
        verdict = "no clear difference"
    return dict(baseline_median=statistics.median(baseline), candidate_median=statistics.median(candidate),
                ratio=ratio, interval=bounds, pairs=len(logs), verdict=verdict)


def probe(argv):
    try:
        done = subprocess.run(argv, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    return done.stdout.strip() if done.returncode == 0 else None


def filesystem(root):
    system = platform.system()
    if system == "Windows":
        drive = os.path.splitdrive(str(root.resolve()))[0]
        info = probe(["fsutil", "fsinfo", "volumeinfo", drive]) or ""
        name = next((line.split(":", 1)[1].strip() for line in info.splitlines()
                     if line.strip().startswith("File System Name")), None)
        dev_drive = probe(["fsutil", "devdrv", "query", drive])
        return dict(type=name, dev_drive=dev_drive)
    if system == "Darwin":
        mounts = probe(["mount"]) or ""
        best = None
        for line in mounts.splitlines():
            # "/dev/disk3s5 on /System/Volumes/Data (apfs, local, journaled)"
            if " on " not in line or " (" not in line:
                continue
            point, kind = line.split(" on ", 1)[1].rsplit(" (", 1)
            if str(root.resolve()).startswith(point) and (best is None or len(point) > len(best[0])):
                best = (point, kind.split(",")[0])
        return dict(type=best[1] if best else None, mount=best[0] if best else None)
    return dict(type=probe(["stat", "-f", "-c", "%T", str(root)]))


def defender():
    if platform.system() != "Windows":
        return None
    state = probe(["powershell", "-NoProfile", "-Command",
                   "(Get-MpComputerStatus).RealTimeProtectionEnabled"])
    return dict(real_time_protection=state)


def digest(path):
    hasher = hashlib.sha256()
    with Path(path).open("rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            hasher.update(block)
    return hasher.hexdigest()


def remove(path):
    """deletes a tree that fetchloom made read-only; windows refuses to delete read-only files."""
    path = Path(path)
    if not path.exists():
        return
    for folder, _, files in os.walk(path):
        for name in files:
            os.chmod(os.path.join(folder, name), stat.S_IWRITE | stat.S_IREAD)
    shutil.rmtree(path)


def table(summary):
    lines = ["scenario | phase | baseline s | candidate s | candidate/baseline | 95% interval | verdict",
             "--- | --- | ---: | ---: | ---: | --- | ---"]
    for key, item in summary.items():
        scenario, phase = key.split("/")
        bounds = item["interval"]
        interval = f"[{bounds[0]:.3f}, {bounds[1]:.3f}]" if bounds else "-"
        ratio = f"{item['ratio']:.3f}" if item["ratio"] is not None else "-"
        lines.append(f"{scenario} | {phase} | {item['baseline_median']:.3f} | {item['candidate_median']:.3f} | "
                     f"{ratio} | {interval} | {item['verdict']}")
    return "\n".join(lines)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--baseline", required=True, type=Path, help="the fetchloom build to compare against")
    parser.add_argument("--candidate", required=True, type=Path, help="the fetchloom build being measured")
    parser.add_argument("--root", required=True, type=Path,
                        help="an empty folder on the filesystem to measure; fixtures, caches and projects go here")
    parser.add_argument("--scenarios", default="small,zip,large",
                        type=lambda text: [name for name in text.split(",") if name])
    parser.add_argument("--rounds", type=int, default=5)
    parser.add_argument("--large-mib", type=int, default=1024, help="size of the large scenario's file")
    parser.add_argument("--seed", type=int, default=1729)
    parser.add_argument("--out", type=Path, help="folder for native.json and native.md; defaults to ROOT/results")
    args = parser.parse_args(argv)
    unknown = [name for name in args.scenarios if name not in SCENARIOS]
    if unknown or args.rounds < 1 or args.large_mib < 1:
        parser.error(f"scenarios are {', '.join(SCENARIOS)}; rounds and --large-mib must be positive")
    root = args.root.resolve()
    out = (args.out or root / "results").resolve()
    out.mkdir(parents=True, exist_ok=True)
    binaries = {"baseline": args.baseline.resolve(strict=True), "candidate": args.candidate.resolve(strict=True)}
    specs = dict(SCENARIOS, large=dict(SCENARIOS["large"], size=args.large_mib << 20))
    server = serve(root / "www")
    base_url = f"http://127.0.0.1:{server.server_address[1]}"
    order = random.Random(args.seed)
    raw = {}
    try:
        for scenario in args.scenarios:
            names = build_fixture(root / "www", scenario, specs[scenario], args.seed)
            text = manifest(base_url, scenario, names)
            templates = {label: prepare(binary, label, scenario, text, root / "work")
                         for label, binary in binaries.items()}
            for number in range(1, args.rounds + 1):
                labels = list(binaries)
                order.shuffle(labels)
                for label in labels:
                    times = measure_round(binaries[label], templates[label], root / "work" / "run")
                    for phase, seconds in times.items():
                        raw.setdefault(f"{scenario}/{phase}", {}).setdefault(label, []).append(seconds)
                print(f"{scenario}: round {number} of {args.rounds} done", file=sys.stderr)
    finally:
        server.shutdown()
        server.server_close()
    summary = {key: summarize(sides["baseline"], sides["candidate"], f"{args.seed}/{key}")
               for key, sides in raw.items()}
    report = dict(machine=dict(platform=platform.platform(), python=sys.version.split()[0],
                               cpus=os.cpu_count(), filesystem=filesystem(root), defender=defender()),
                  binaries={label: dict(path=str(path), sha256=digest(path)) for label, path in binaries.items()},
                  settings=dict(scenarios=args.scenarios, rounds=args.rounds, seed=args.seed,
                                sizes={name: specs[name] for name in args.scenarios}),
                  seconds=raw, summary=summary)
    (out / "native.json").write_text(json.dumps(report, indent=2) + "\n")
    (out / "native.md").write_text(table(summary) + "\n")
    print(table(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
