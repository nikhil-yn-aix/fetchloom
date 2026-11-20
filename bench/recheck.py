"""compare a fetchloom build with the last release on the release recheck's cases.

the recheck times the commands a release must not slow down: a status, a settled sync and a cache
check of 100,000 files, `path` beside them, verifying 512 MiB, placing 40,000 datasets, adding
1,000 links, a zip of 10,000 files, a 1 GiB `get`, a merge and an exclude across 1,000 datasets, and
the commands of a few milliseconds. with --links, the lab's s1, s2r and s7 too, at home and on the
poor link. each arm makes its own projects and caches from the same generated files with its own
binary, so no arm reads another's formats, and every file a run downloads or places is checked by
its SHA-256 after the clock stops. a byte-identical copy of the baseline runs as a third arm, an a/a
control for the machine's noise. linux only.
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
from collections import Counter
import hashlib
import zipfile

import ab
import fixtures
import lab
import native
import run
import tools

ARMS = ("fetchloom", "fetchloom+candidate", "fetchloom+aa")
# each arm's binary and folders go by an id of one length, so no arm's paths are longer than another's
FOLDERS = {arm: f"arm{index}" for index, arm in enumerate(ARMS)}
# the lab's link cases, measured with --links, each on these links
LINKS = ("s1", "s2r", "s7")
LINK_PROFILES = ("home", "poor")
LINK_ROUNDS = 10
# measured rounds per case after a warmup round; the shortest commands need many pairs
ROUNDS = dict(status=20, sync=20, cache_verify=20, path=100, verify=20, place=10, add_links=25, zip=20,
              get=15, merge=15, exclude=15, version=100, help=100, tiny_sync=100)


def sizes(scale):
    """the fixture dimensions at a scale, 1 for the release recheck and less for a smoke run."""
    def count(n):
        return max(3, round(n * scale))

    return dict(files=count(100_000), large=8, large_bytes=max(1 << 20, round((64 << 20) * scale)),
                many=count(40_000), links=count(1000), members=count(10_000), get=max(1 << 20, round((1 << 30) * scale)),
                side=count(1000), pairs=count(1000))


def write(path, data):
    """writes data to path, making its folders first."""
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
    folder = root / "fx" / FOLDERS[arm]
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

    def one_each(project, cache, listing):
        """a project with a dataset for each link of the list, pinned by `lock`, as several links
        typed for one add make them."""
        project.mkdir(parents=True, exist_ok=True)
        links = (src / listing).read_text().split()
        (project / "data.toml").write_text("".join(f'[datasets.{Path(link).stem}]\nref = "{link}"\n\n' for link in links))
        fetchloom(project, cache, "lock")

    for name in ("files", "tiny"):
        fetchloom(folder / "big", "big-cache", "add", src / name)
    fetchloom(folder / "tiny", "tiny-cache", "add", src / "tiny")
    fetchloom(folder / "tiny", "tiny-cache", "sync")
    fetchloom(folder / "large", "large-cache", "add", src / "large")
    one_each(folder / "many", "many-cache", "many.txt")
    (folder / "zip").mkdir()
    (folder / "zip" / "data.toml").write_text(f'[datasets.members]\nref = "{origin}/members.zip"\n')
    fetchloom(folder / "zip", "zip-cache", "lock")
    for side in ("ours", "theirs"):
        one_each(folder / side, "merge-cache", f"{side}.txt")
    for name in ("data.toml", "data.lock"):
        write(folder / "merge" / name, conflict((folder / "ours" / name).read_text(),
                                                (folder / "theirs" / name).read_text()).encode())
    for pair in sorted((src / "pairs").iterdir()):
        fetchloom(folder / "exclude", "exclude-cache", "add", pair)
    marker.write_text(json.dumps(ready) + "\n")
    return folder


def exclude_paths(project):
    """one file of every dataset in the project, which the exclude case names."""
    names = tomllib.loads((project / "data.toml").read_text())["datasets"]
    return [f"data/{name}/remove.bin" for name in sorted(names)]


def fresh(target, template):
    """an emptied target holding copies of the template's data.toml, data.lock and data.lock.d."""
    remove(target)
    target.mkdir(parents=True)
    for item in template.iterdir():
        if item.name in ("data.toml", "data.lock", "data.lock.d"):
            (shutil.copytree if item.is_dir() else shutil.copyfile)(item, target / item.name)


def expected(root):
    """the SHA-256 of every file each case that downloads or places files must leave: the files the
    server serves for it and the zip's members."""
    www = root / "www"

    def digests(paths):
        return Counter(fixtures.sha256(path) for path in paths)

    with zipfile.ZipFile(www / "members.zip") as archive:
        members = Counter(hashlib.sha256(archive.read(info)).hexdigest() for info in archive.infolist())
    return dict(place=digests(sorted((www / "many").iterdir())), add_links=digests(sorted((www / "links").iterdir())),
                zip=members, get=digests([www / "big.bin"]))


def placed(folder, digests):
    """what is wrong with the files a run left under folder, against the SHA-256 it must leave, each
    as often as given: one line counting those missing or wrong, or none. hidden files are
    fetchloom's own, so they are left out."""
    folder = Path(folder)
    found = Counter(fixtures.sha256(path) for path in (folder.rglob("*") if folder.is_dir() else [])
                    if path.is_file() and not any(part.startswith(".") for part in path.relative_to(folder).parts))
    missing = digests - found
    if not missing:
        return []
    return [f"{sum(missing.values())} of {sum(digests.values())} expected files are missing or wrong under {folder}"]


def cases(root, arm, binary, folder, origin, digests):
    """each case's command, folder and environment for one arm, and for a case that downloads or
    places files, the folder it leaves them in and their SHA-256; prep and post run outside the clock."""
    work = root / "work" / FOLDERS[arm]
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
                   prep=lambda: fresh(work / "place", folder / "many"), post=emptied(work / "place"),
                   outputs=(work / "place", digests["place"])),
        add_links=dict(argv=[fl, "-q", "--yes", "add", *(root / "src" / "links.txt").read_text().split()], cwd=work / "links",
                       env=env(work / "links-cache"), post=emptied(work / "links", work / "links-cache"),
                       prep=lambda: (work / "links").mkdir(parents=True, exist_ok=True),
                       outputs=(work / "links", digests["add_links"])),
        zip=dict(argv=[fl, "-q", "--yes", "sync"], cwd=work / "zip", env=env(work / "zip-cache"),
                 prep=lambda: fresh(work / "zip", folder / "zip"), post=emptied(work / "zip", work / "zip-cache"),
                 outputs=(work / "zip", digests["zip"])),
        get=dict(argv=[fl, "-q", "--yes", "get", f"{origin}/big.bin", "-o", str(work / "get" / "big.bin")],
                 cwd=root, env=env(work / "get-cache"), post=emptied(work / "get", work / "get-cache"),
                 prep=lambda: (work / "get").mkdir(parents=True, exist_ok=True), outputs=(work / "get", digests["get"])),
        merge=dict(argv=[fl, "--json", "--offline", "lock", "--dry-run"], cwd=folder / "merge",
                   env=env(folder / "merge-cache")),
        exclude=dict(argv=[fl, "--json", "--offline", "exclude", "--dry-run", *exclude_paths(folder / "exclude")],
                     cwd=folder / "exclude", env=env(folder / "exclude-cache")),
        version=dict(argv=[fl, "--version"], cwd=root, env=env(root / "home" / "cache")),
        help=dict(argv=[fl, "help"], cwd=root, env=env(root / "home" / "cache")),
        tiny_sync=dict(argv=[fl, "-q", "--yes", "sync"], cwd=folder / "tiny", env=env(folder / "tiny-cache")),
    )


class Files(native.Quiet):
    """serves files, handing each body to the kernel."""
    def copyfile(self, source, outputfile):
        # the kernel copies the body, so the server is not what a 1 GiB get waits on; the headers
        # are already out, as wfile holds no buffer
        self.connection.sendfile(source)


def serve(www, port):
    """serves www on port, 0 for a free one, from a background thread; returns the server."""
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
    """every arm once a round in a seeded shuffled order, round 0 a warmup; one row per run. what a
    run downloads or places is checked after the clock and before it is cleared, and a wrong or
    missing file fails the run."""
    rng = random.Random(f"{seed}/{name}")
    for number in range(rounds + 1):
        order = list(specs)
        rng.shuffle(order)
        for arm in order:
            spec = specs[arm]
            if spec.get("prep"):
                spec["prep"]()
            result = run.measure(spec["argv"], spec["cwd"], spec["env"], out / "logs" / f"{name}-{FOLDERS[arm]}.log",
                                 timeout, cpus=cpus)
            checks = placed(*spec["outputs"]) if spec.get("outputs") else []
            if spec.get("post"):
                spec["post"]()
            status = "passed" if result["exit_code"] == 0 and not result["timeout"] and not checks else "failed"
            row = dict(result, kind="recheck", tool=arm, scenario=name, filesystem="local", profile="recheck",
                       round=number, warmup=number == 0, status=status, checks=checks, quick=False, count=None,
                       file_size=None, load=os.getloadavg()[0])
            run.append(out / "runs.jsonl", row)


def links(binaries, out, *, seed, rounds, cpus, timeout, lock=None):
    """the lab's link cases with every arm: s1, s2r and s7 at home and on the poor link, each arm
    syncing a project its own build locked, once a round in a seeded shuffled order after a warmup.
    run.one_run checks every placed file's SHA-256 after the clock and before the work folder goes."""
    arms = {arm: tools.Build(binary, arm, FOLDERS[arm]) for arm, binary in binaries.items()}
    workdir = lab.ROOT / "fs" / "ext4" / "work"
    count = 0
    for profile in LINK_PROFILES:
        for scenario in LINKS:
            [spec] = fixtures.case_specs([profile], [scenario], False)
            print(f"{scenario} {profile}: {rounds} rounds", file=sys.stderr, flush=True)
            with turn(lock):
                lab.profile("lan")
                case = fixtures.build(spec, seed)
                for arm in arms.values():
                    case = fixtures.prepare_native(case, arm.fetchloom, [arm.prepared_as])
                lab.profile(profile)
                rng = random.Random(f"{seed}/{scenario}/{profile}")
                for number in range(rounds + 1):
                    order = list(arms)
                    rng.shuffle(order)
                    for name in order:
                        count += 1
                        run.one_run(arms[name], case, workdir, out, number, number == 0, timeout, cpus=cpus,
                                    run_id=f"{count:06}")
                        if workdir.exists():
                            shutil.rmtree(workdir)


def summary(rows):
    """per case and arm: pairs, medians, and the time, cpu and memory ratios with their intervals."""
    lines = ["| case | arm | pairs | wall, baseline → arm | time ratio [95%] | cpu ratio [95%] | peak MiB, baseline → arm | memory ratio [95%] |",
             "|---|---|---:|---|---|---|---|---|"]
    measured = [row for row in rows if not row["warmup"] and row["status"] == "passed"]
    for result in ab.compare(rows, threshold=.02, min_pairs=5):
        key = tuple(result["case"].split("/"))
        # a link case names its link, since it runs on two
        case = key[2] if key[1] == "recheck" else f"{key[2]} {key[1]}"

        def median(tool, field):
            values = [row[field] for row in measured if (row["filesystem"], row["profile"], row["scenario"]) == key
                      and row["tool"] == tool and row.get(field)]
            return statistics.median(values) if values else float("nan")

        def ratio(metric):
            data = result["metrics"][metric]
            bounds = data["bootstrap_95_median_ratio"]
            return f"{data['median_ratio']:.3f} [{bounds[0]:.3f}, {bounds[1]:.3f}]" if bounds else "-"

        arm = result["variant"]
        lines.append(f"| {case} | {arm.removeprefix('fetchloom+')} | {result['rounds']} | "
                     f"{median('fetchloom', 'wall_seconds') * 1000:.1f} → {median(arm, 'wall_seconds') * 1000:.1f} ms | "
                     f"{ratio('wall')} | {ratio('cpu')} | {median('fetchloom', 'maxrss_bytes') / 2**20:.1f} → "
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
        copy = folder / FOLDERS[arm]
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
                        help="a new or empty folder, or one recheck.py made, for fixtures, projects and results, "
                             "on the filesystem to measure")
    parser.add_argument("--cases", type=lambda text: [name for name in text.split(",") if name], default=list(ROUNDS),
                        help=f"comma-separated, from {','.join(ROUNDS)}; empty for the link cases alone")
    parser.add_argument("--links", action="store_true",
                        help="also time the lab's s1, s2r and s7 at home and on the poor link; needs root and lab.py setup")
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
    if not args.cases and not args.links:
        parser.error("choose at least one case, or --links")
    if args.links:
        lab.require_root()
        if not (lab.ROOT / "owned.json").exists() or not os.path.ismount(lab.ROOT / "fs" / "ext4"):
            parser.error("--links needs the lab: run lab.py setup first")
    root = args.root.resolve()
    native.claim(root, "recheck.py")
    for folder in ("home", "tmp"):
        (root / folder).mkdir(parents=True, exist_ok=True)
    out = root / "results" / datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    (out / "logs").mkdir(parents=True)
    origin = f"http://127.0.0.1:{args.port}"
    binaries = arms(args.baseline, args.candidate, root)
    server = serve(root / "www", args.port)
    try:
        signature = sources(root, args.scale, args.seed, origin)
        digests = expected(root)
        specs = {}
        for arm, binary in binaries.items():
            print(f"preparing {arm}", file=sys.stderr, flush=True)
            folder = prepare(root, arm, binary, signature, origin)
            for name, spec in cases(root, arm, binary, folder, origin, digests).items():
                specs.setdefault(name, {})[arm] = spec
        given = dict(zip(ARMS, (args.baseline, args.candidate, args.baseline)))
        meta = dict(date=datetime.now(timezone.utc).isoformat(), seed=args.seed, scale=args.scale,
                    cpus=args.cpus, folders=FOLDERS,
                    binaries={arm: dict(source=str(given[arm].resolve()), copy=str(path), sha256=fixtures.sha256(path))
                              for arm, path in binaries.items()},
                    links=[f"{scenario} {profile}" for profile in LINK_PROFILES for scenario in LINKS] if args.links else [],
                    network_profiles=lab.network_profiles() if args.links else None)
        (out / "meta.json").write_text(json.dumps(meta, indent=2) + "\n")
        for name in args.cases:
            rounds = args.rounds or ROUNDS[name]
            print(f"{name}: {rounds} rounds", file=sys.stderr, flush=True)
            with turn(args.lock):
                measure(name, specs[name], rounds, args.seed, out, args.cpus, args.timeout)
        if args.links:
            with lab.exclusive():
                lab.pin_server(args.cpus or lab.cpu_list(""))
                links(binaries, out, seed=args.seed, rounds=args.rounds or LINK_ROUNDS, cpus=args.cpus,
                      timeout=args.timeout, lock=args.lock)
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
