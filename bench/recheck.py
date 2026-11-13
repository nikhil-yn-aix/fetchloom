"""Compare a fetchloom build with the last release on the release recheck's cases.

the recheck times the commands a release must not slow down: a status, a settled sync and a cache
check of 100,000 files, `path` beside them, verifying 512 MiB, placing 40,000 datasets, adding
1,000 links, a zip of 10,000 files, a 1 GiB `get`, a merge and a drop across 1,000 datasets, and
the commands of a few milliseconds. each arm makes its own projects and caches from the same
generated files with its own binary, so no arm reads another's formats. a byte-identical copy of
the baseline runs as a third arm, an a/a control for the machine's noise. linux only.
"""
import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import fcntl
import functools
import json
import os
from pathlib import Path
import random
import shutil
import statistics
import subprocess
import sys
import threading
import tomllib
import zipfile

import ab
import fixtures
import lab
import native
import run

ARMS = ("fetchloom", "fetchloom+candidate", "fetchloom+aa")
# measured rounds per case after a warmup round; the shortest commands need many pairs
ROUNDS = dict(status=20, sync=20, cache_verify=20, path=100, verify=20, place=10, add_links=25, zip=20,
              get=15, merge=15, drop=15, version=100, help=100, tiny_sync=100)


def sizes(scale):
    """the fixture dimensions at a scale, 1 for the release recheck and less for a smoke run."""
    def count(n):
        return max(3, round(n * scale))

    return dict(files=count(100_000), large=8, large_bytes=max(1 << 20, round((64 << 20) * scale)),
                many=count(40_000), links=count(1000), members=count(10_000), get=max(1 << 20, round((1 << 30) * scale)),
                side=count(1000), pairs=count(1000))


def write(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def remove(path):
    """deletes a tree; linux unlinks read-only files, so only folders need to be writable."""
    path = Path(path)
    if not path.exists():
        return
    for folder, _, _ in os.walk(path):
        os.chmod(folder, 0o755)
    shutil.rmtree(path)


def sources(root, scale, seed, origin):
    """the files every arm adds and the files the local server serves, made once per signature."""
    marker = root / "sources.json"
    signature = dict(scale=scale, seed=seed, origin=origin, generator=fixtures.sha256(__file__))
    if fixtures.read_json(marker) == signature:
        return signature
    remove(root / "src")
    remove(root / "www")
    rng = random.Random(seed)
    dims = sizes(scale)
    src, www = root / "src", root / "www"
    for index in range(dims["files"]):
        write(src / "files" / f"d{index // 1000:03}" / f"f{index:06}.bin",
              fixtures.content(seed, f"files/{index}", rng.randint(1024, 8192)))
    for index in range(3):
        write(src / "tiny" / f"t{index}.bin", fixtures.content(seed, f"tiny/{index}", rng.randint(1024, 8192)))
    for index in range(dims["large"]):
        path = src / "large" / f"part-{index}.bin"
        path.parent.mkdir(parents=True, exist_ok=True)
        with path.open("wb") as out:
            for chunk in range(0, dims["large_bytes"], 1 << 20):
                out.write(fixtures.content(seed, f"large/{index}/{chunk}", min(1 << 20, dims["large_bytes"] - chunk)))
    for index in range(dims["pairs"]):
        for name in ("keep.bin", "remove.bin"):
            write(src / "pairs" / f"p{index:05}" / name, fixtures.content(seed, f"pairs/{index}/{name}", 64))

    def links(name, folder, count, size, suffix=".bin"):
        lines = []
        for index in range(count):
            write(www / folder / f"{folder[0]}{index:05}{suffix}", fixtures.content(seed, f"{folder}/{index}", size()))
            lines.append(f"{origin}/{folder}/{folder[0]}{index:05}{suffix}\n")
        (src / name).write_text("".join(lines))

    links("links.txt", "links", dims["links"], lambda: rng.randint(90_000, 110_000))
    links("many.txt", "many", dims["many"], lambda: 16)
    links("ours.txt", "ours", dims["side"], lambda: 64)
    links("theirs.txt", "theirs", dims["side"], lambda: 64)
    with zipfile.ZipFile(www / "members.zip", "w", compression=zipfile.ZIP_STORED) as archive:
        for index in range(dims["members"]):
            info = zipfile.ZipInfo(f"m{index:05}.bin", date_time=(2020, 1, 1, 0, 0, 0))
            info.external_attr = 0o644 << 16
            archive.writestr(info, fixtures.content(seed, f"members/{index}", 4096))
    with (www / "big.bin").open("wb") as out:
        for chunk in range(0, dims["get"], 1 << 20):
            out.write(fixtures.content(seed, f"get/{chunk}", min(1 << 20, dims["get"] - chunk)))
    marker.write_text(json.dumps(signature) + "\n")
    return signature


def environment(root, cache, origin):
    """the measured environment: no fetchloom setting of the shell, a home of the recheck's own."""
    env = {key: value for key, value in os.environ.items() if not key.startswith("FETCHLOOM_") and key != "NO_COLOR"}
    home = root / "home"
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / ".config"), XDG_CACHE_HOME=str(home / ".cache"),
               XDG_DATA_HOME=str(home / ".local" / "share"), XDG_STATE_HOME=str(home / ".local" / "state"),
               TMPDIR=str(root / "tmp"), FETCHLOOM_CACHE=str(cache), FETCHLOOM_ALLOWED_ORIGINS=origin,
               NO_PROXY="127.0.0.1", no_proxy="127.0.0.1")
    return env


def conflict(ours, theirs):
    """two sides of a file as git leaves them after a merge in diff3 style, with an empty base."""
    return f"<<<<<<< ours\n{ours}||||||| base\n=======\n{theirs}>>>>>>> theirs\n"


def prepare(root, arm, binary, signature, origin):
    """the arm's own projects and caches, made by its own binary; rebuilt when the binary changes."""
    folder = root / "fx" / arm.replace("+", "-")
    marker = folder / "ready.json"
    ready = dict(binary=fixtures.sha256(binary), sources=signature)
    if fixtures.read_json(marker) == ready:
        return folder
    remove(folder)
    src = root / "src"

    def fetchloom(project, cache, *args):
        project.mkdir(parents=True, exist_ok=True)
        done = subprocess.run([str(binary), "-q", "--yes", *map(str, args)], cwd=project,
                              env=environment(root, folder / cache, origin), stdin=subprocess.DEVNULL,
                              stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        if done.returncode:
            raise RuntimeError(f"{arm}: fetchloom {' '.join(map(str, args))} exited {done.returncode}: {done.stderr.strip()}")

    for name in ("files", "tiny"):
        fetchloom(folder / "big", "big-cache", "add", src / name)
    fetchloom(folder / "tiny", "tiny-cache", "add", src / "tiny")
    fetchloom(folder / "tiny", "tiny-cache", "sync")
    fetchloom(folder / "large", "large-cache", "add", src / "large")
    fetchloom(folder / "many", "many-cache", "add", "-i", src / "many.txt")
    (folder / "zip").mkdir()
    (folder / "zip" / "data.toml").write_text(f'[datasets.members]\nref = "{origin}/members.zip"\n')
    fetchloom(folder / "zip", "zip-cache", "lock")
    for side in ("ours", "theirs"):
        fetchloom(folder / side, "merge-cache", "add", "-i", src / f"{side}.txt")
    for name in ("data.toml", "data.lock"):
        write(folder / "merge" / name, conflict((folder / "ours" / name).read_text(),
                                                (folder / "theirs" / name).read_text()).encode())
    for pair in sorted((src / "pairs").iterdir()):
        fetchloom(folder / "drop", "drop-cache", "add", pair)
    marker.write_text(json.dumps(ready) + "\n")
    return folder


def drop_paths(project):
    """one file of every dataset in the project, which the drop case names."""
    names = tomllib.loads((project / "data.toml").read_text())["datasets"]
    return [f"data/{name}/remove.bin" for name in sorted(names)]


def fresh(target, template):
    remove(target)
    target.mkdir(parents=True)
    for item in template.iterdir():
        if item.name in ("data.toml", "data.lock", "data.lock.d"):
            (shutil.copytree if item.is_dir() else shutil.copyfile)(item, target / item.name)


def cases(root, arm, binary, folder, origin):
    """each case's command, folder and environment for one arm; prep and post run outside the clock."""
    work = root / "work" / arm.replace("+", "-")
    env = functools.partial(environment, root, origin=origin)
    fl = str(binary)

    def emptied(*paths):
        return lambda: [remove(path) for path in paths]

    return dict(
        status=dict(argv=[fl, "--json", "status"], cwd=folder / "big", env=env(folder / "big-cache")),
        sync=dict(argv=[fl, "-q", "--yes", "sync"], cwd=folder / "big", env=env(folder / "big-cache")),
        cache_verify=dict(argv=[fl, "--json", "cache", "verify"], cwd=folder / "big", env=env(folder / "big-cache")),
        path=dict(argv=[fl, "path", "tiny"], cwd=folder / "big", env=env(folder / "big-cache")),
        verify=dict(argv=[fl, "--json", "status", "--verify"], cwd=folder / "large", env=env(folder / "large-cache")),
        place=dict(argv=[fl, "-q", "--yes", "sync"], cwd=work / "place", env=env(folder / "many-cache"),
                   prep=lambda: fresh(work / "place", folder / "many"), post=emptied(work / "place")),
        add_links=dict(argv=[fl, "-q", "--yes", "add", "-i", str(root / "src" / "links.txt")], cwd=work / "links",
                       env=env(work / "links-cache"), post=emptied(work / "links", work / "links-cache"),
                       prep=lambda: (work / "links").mkdir(parents=True, exist_ok=True)),
        zip=dict(argv=[fl, "-q", "--yes", "sync"], cwd=work / "zip", env=env(work / "zip-cache"),
                 prep=lambda: fresh(work / "zip", folder / "zip"), post=emptied(work / "zip", work / "zip-cache")),
        get=dict(argv=[fl, "-q", "--yes", "get", f"{origin}/big.bin", "-o", str(work / "get" / "big.bin")],
                 cwd=root, env=env(work / "get-cache"), post=emptied(work / "get", work / "get-cache"),
                 prep=lambda: (work / "get").mkdir(parents=True, exist_ok=True)),
        merge=dict(argv=[fl, "--json", "--offline", "lock", "--dry-run"], cwd=folder / "merge",
                   env=env(folder / "merge-cache")),
        drop=dict(argv=[fl, "--json", "--offline", "drop", "--dry-run", *drop_paths(folder / "drop")],
                  cwd=folder / "drop", env=env(folder / "drop-cache")),
        version=dict(argv=[fl, "--version"], cwd=root, env=env(root / "home" / "cache")),
        help=dict(argv=[fl, "help"], cwd=root, env=env(root / "home" / "cache")),
        tiny_sync=dict(argv=[fl, "-q", "--yes", "sync"], cwd=folder / "tiny", env=env(folder / "tiny-cache")),
    )


class Files(native.Quiet):
    def copyfile(self, source, outputfile):
        # the kernel copies the body, so the server is not what a 1 GiB get waits on; the headers
        # are already out, as wfile holds no buffer
        self.connection.sendfile(source)


def serve(www, port):
    server = native.Server(("127.0.0.1", port), functools.partial(Files, directory=str(www)))
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True).start()
    return server


@contextmanager
def turn(path):
    """the machine's measuring lock, held for one case at a time, so other measurers get turns."""
    if path is None:
        yield
        return
    # closing the file releases the lock, as the case ends or fails
    with open(path, "a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        yield


def measure(name, specs, rounds, seed, out, cpus, timeout):
    """every arm once a round in a seeded shuffled order, round 0 a warmup; one row per run."""
    rng = random.Random(f"{seed}/{name}")
    for number in range(rounds + 1):
        order = list(specs)
        rng.shuffle(order)
        for arm in order:
            spec = specs[arm]
            if spec.get("prep"):
                spec["prep"]()
            result = run.measure(spec["argv"], spec["cwd"], spec["env"], out / "logs" / f"{name}-{arm}.log",
                                 timeout, cpus=cpus)
            if spec.get("post"):
                spec["post"]()
            status = "passed" if result["exit_code"] == 0 and not result["timeout"] else "failed"
            row = dict(result, kind="recheck", tool=arm, scenario=name, filesystem="local", profile="recheck",
                       round=number, warmup=number == 0, status=status, quick=False, count=None, file_size=None,
                       load=os.getloadavg()[0])
            run.append(out / "runs.jsonl", row)


def summary(rows):
    """per case and arm: pairs, medians, and the time and memory ratios with their intervals."""
    lines = ["| case | arm | pairs | wall, baseline → arm | time ratio [95%] | peak MiB, baseline → arm | memory ratio [95%] |",
             "|---|---|---:|---|---|---|---|"]
    measured = [row for row in rows if not row["warmup"] and row["status"] == "passed"]
    for result in ab.compare(rows, threshold=.02, min_pairs=5):
        case = result["case"].split("/")[-1]

        def median(tool, key):
            values = [row[key] for row in measured if row["scenario"] == case and row["tool"] == tool and row.get(key)]
            return statistics.median(values) if values else float("nan")

        def ratio(metric):
            data = result["metrics"][metric]
            bounds = data["bootstrap_95_median_ratio"]
            return f"{data['median_ratio']:.3f} [{bounds[0]:.3f}, {bounds[1]:.3f}]" if bounds else "-"

        arm = result["variant"]
        lines.append(f"| {case} | {arm.removeprefix('fetchloom+')} | {result['rounds']} | "
                     f"{median('fetchloom', 'wall_seconds') * 1000:.1f} → {median(arm, 'wall_seconds') * 1000:.1f} ms | "
                     f"{ratio('wall')} | {median('fetchloom', 'maxrss_bytes') / 2**20:.1f} → "
                     f"{median(arm, 'maxrss_bytes') / 2**20:.1f} | {ratio('memory')} |")
    return "\n".join(lines)


def arms(baseline, candidate, root):
    """the three arms' binaries, each a fresh copy, the a/a arm the baseline's bytes once more.
    the same bytes in an older file can peak a mebibyte higher on a short command, as the page
    cache holds them in larger pieces, so no arm runs the file it was given."""
    folder = root / "bin"
    folder.mkdir(parents=True, exist_ok=True)
    copies = {}
    for arm, source in zip(ARMS, (baseline, candidate, baseline)):
        copy = folder / arm.replace("+", "-")
        copy.unlink(missing_ok=True)
        shutil.copyfile(source, copy)
        copy.chmod(0o755)
        copies[arm] = copy
    return copies


def cpus(value):
    """a CPU list of this machine; the script itself may be pinned elsewhere, as its preparation and
    file server should be."""
    try:
        return lab.cpu_list(value, range(os.cpu_count()))
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from None


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--baseline", required=True, type=Path, help="the last release's fetchloom")
    parser.add_argument("--candidate", required=True, type=Path, help="the build being checked")
    parser.add_argument("--root", required=True, type=Path,
                        help="a folder for fixtures, projects and results, on the filesystem to measure")
    parser.add_argument("--cases", type=lambda text: text.split(","), default=list(ROUNDS),
                        help=f"comma-separated, from {','.join(ROUNDS)}")
    parser.add_argument("--rounds", type=int, help="measured rounds for every case, instead of each case's own")
    parser.add_argument("--scale", type=float, default=1.0, help="fixture size; below 1 for a smoke run")
    parser.add_argument("--cpus", type=cpus, default=None,
                        help="CPU list for the measured commands, which may lie outside the script's own")
    parser.add_argument("--lock", type=Path, help="a lock file held while each case runs")
    parser.add_argument("--port", type=int, default=19400, help="the local file server's port")
    parser.add_argument("--seed", type=int, default=1729)
    parser.add_argument("--timeout", type=run.seconds_choice, default=600)
    args = parser.parse_args(argv)
    unknown = [name for name in args.cases if name not in ROUNDS]
    if unknown or (args.rounds is not None and args.rounds < 1) or not 0 < args.scale <= 1:
        parser.error(f"cases are {', '.join(ROUNDS)}; rounds must be positive and scale in (0, 1]")
    root = args.root.resolve()
    for folder in ("home", "tmp"):
        (root / folder).mkdir(parents=True, exist_ok=True)
    out = root / "results" / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    (out / "logs").mkdir(parents=True)
    origin = f"http://127.0.0.1:{args.port}"
    binaries = arms(args.baseline, args.candidate, root)
    server = serve(root / "www", args.port)
    try:
        signature = sources(root, args.scale, args.seed, origin)
        specs = {}
        for arm, binary in binaries.items():
            print(f"preparing {arm}", file=sys.stderr, flush=True)
            folder = prepare(root, arm, binary, signature, origin)
            for name, spec in cases(root, arm, binary, folder, origin).items():
                specs.setdefault(name, {})[arm] = spec
        given = dict(zip(ARMS, (args.baseline, args.candidate, args.baseline)))
        meta = dict(date=datetime.now(timezone.utc).isoformat(), seed=args.seed, scale=args.scale,
                    cpus=args.cpus, binaries={arm: dict(source=str(given[arm].resolve()), copy=str(path),
                                                        sha256=fixtures.sha256(path))
                                              for arm, path in binaries.items()})
        (out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
        for name in args.cases:
            rounds = args.rounds or ROUNDS[name]
            print(f"{name}: {rounds} rounds", file=sys.stderr, flush=True)
            with turn(args.lock):
                measure(name, specs[name], rounds, args.seed, out, args.cpus, args.timeout)
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"recheck: {error}", file=sys.stderr)
        return 1
    finally:
        server.shutdown()
        server.server_close()
    rows = [json.loads(line) for line in (out / "runs.jsonl").read_text().splitlines() if line.strip()]
    text = summary(rows)
    (out / "summary.md").write_text(text + "\n")
    print(text)
    print(f"\nruns and logs: {out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
