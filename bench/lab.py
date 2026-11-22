"""a root-owned, disposable linux network and filesystem lab."""
import argparse
from contextlib import contextmanager
import fcntl
import json
import os
import re
from pathlib import Path
import shutil
import signal
import subprocess
import threading
import time
import urllib.request

ROOT = Path(os.environ.get("FL_BENCH_ROOT", "/srv/fl-bench")).resolve()
VENV = Path(os.environ.get("FL_BENCH_VENV", "/opt/fl-bench/venv")).resolve()
NS = "flb-srv"
CLIENT = "flb-client"
SERVER = "flb-server"
BUNDLE = "/etc/ssl/certs/ca-certificates.crt"
CA = Path("/usr/local/share/ca-certificates/fl-bench.crt")
HOSTS = Path("/etc/hosts")
SERVER_ADDRESS = "10.77.0.2"
# the server's own name, and the storage host each file of a redirected case is sent on to, as a
# repository sends its downloads elsewhere
HOST, STORE = "lab.flb", "store.flb"
HOST_LINE = f"{SERVER_ADDRESS} {HOST} {STORE} # fl-bench\n"
# the lab server's origins, on a private address
ORIGINS = (f"https://{HOST}", f"https://{STORE}")
# one json line per request: which host and connection it came on, and when it arrived and ended,
# in milliseconds, so the budget of requests in flight is read from the server's side
ACCESS_FORMAT = ('{"request":"$request","status":$status,"bytes":$body_bytes_sent,"host":"$host",'
                 '"protocol":"$server_protocol","connection":$connection,"connection_requests":$connection_requests,'
                 '"request_time":$request_time,"msec":$msec}')
# runs are public and reproducible, so no key, account file or fetchloom setting is inherited
INHERITED = re.compile(r"(FETCHLOOM|HF|HUGGING_FACE|KAGGLE|KAGGLEHUB|ZENODO|FIGSHARE|OSF|DATAVERSE|AWS|GOOGLE|AZURE|GIT|CURL|DVC|DATALAD|POOCH)_"
                       r"|(NETRC|XDG_CONFIG_HOME|XDG_DATA_HOME|DBUS_SESSION_BUS_ADDRESS|PYTHONPATH|PYTHONHOME|PYTHONSTARTUP|PYTHONUSERBASE|PYTHONPYCACHEPREFIX)$")
# each link's round trip, loss each way, the server's rate and how long a full queue at that rate
# holds; lan has no rate, so netem never queues there. local is this machine's own work, as lan
PROFILES = {
    "lan": dict(delay_ms=0, loss_percent=0, rate_mbit=None, queue_ms=None),
    "local": dict(delay_ms=0, loss_percent=0, rate_mbit=None, queue_ms=None),
    "home": dict(delay_ms=20, loss_percent=0, rate_mbit=100, queue_ms=500),
    "poor": dict(delay_ms=300, loss_percent=3, rate_mbit=10, queue_ms=1000),
}
PACKET = 1500
PREPARE_TIMEOUT = 1200


def cpu_list(value, available=None):
    """the cpus in a list such as 0-3,8, or every available cpu for an empty one; ValueError for a
    malformed list or a cpu this process may not use."""
    available = set(os.sched_getaffinity(0) if available is None else available)
    if not value or not value.strip():
        if not available:
            raise ValueError("no CPUs are available")
        return tuple(sorted(available))
    chosen = set()
    for part in value.split(","):
        match = re.fullmatch(r"(\d+)(?:-(\d+))?", part.strip())
        if not match:
            raise ValueError("CPUs must be a comma-separated list such as 0-3,8")
        first = int(match[1])
        last = int(match[2]) if match[2] is not None else first
        if last < first or last - first >= len(available):
            raise ValueError("CPU range is reversed or exceeds the available CPUs")
        chosen.update(range(first, last + 1))
    if not chosen <= available:
        raise ValueError(f"CPUs are not available: {sorted(chosen - available)}")
    return tuple(sorted(chosen))


def cpu_choice(value):
    try:
        return cpu_list(value)
    except (ValueError, AttributeError, OSError) as error:
        raise argparse.ArgumentTypeError(str(error)) from None


@contextmanager
def affinity(cpus):
    """runs the block on these cpus, then restores the set it had; None leaves affinity alone."""
    if cpus is None:
        yield
        return
    previous = os.sched_getaffinity(0)
    os.sched_setaffinity(0, cpus)
    try:
        yield
    finally:
        os.sched_setaffinity(0, previous)


def pin_server(cpus, proc=Path("/proc")):
    """pin the lab's existing nginx threads; future workers inherit the master's mask."""
    cpus = tuple(sorted(cpus))
    pids = subprocess.check_output(["ip", "netns", "pids", NS], text=True).split()
    masks = {}
    for pid in pids:
        try:
            root = proc / pid
            if (root / "comm").read_text().strip() != "nginx":
                continue
            for thread in sorted((root / "task").iterdir(), key=lambda path: int(path.name)):
                tid = int(thread.name)
                os.sched_setaffinity(tid, cpus)
                actual = sorted(os.sched_getaffinity(tid))
                if actual != list(cpus):
                    raise RuntimeError(f"nginx thread {tid} has CPU mask {actual}, expected {list(cpus)}")
                masks[str(tid)] = actual
        except (FileNotFoundError, ProcessLookupError):
            continue
    if not masks:
        raise RuntimeError("no lab nginx processes could be pinned; run lab.py setup first")
    return masks


def end_group(pgid, grace=0):
    """wait up to grace seconds for a process group to empty, then kill what is left."""
    deadline = time.monotonic() + grace
    killed = None
    while True:
        try:
            os.killpg(pgid, 0)
        except ProcessLookupError:
            return
        now = time.monotonic()
        if killed is None and now >= deadline:
            try:
                os.killpg(pgid, signal.SIGKILL)
            except ProcessLookupError:
                return
            killed = now
        # killed orphans are reaped by init; a process stuck in the kernel is left after a while
        if killed is not None and now - killed > 5:
            return
        time.sleep(0.02)


def run_group(argv, input=None, timeout=None, **kwargs):
    """subprocess.run with check, in a private process group that a timeout kills whole."""
    if input is not None:
        kwargs["stdin"] = subprocess.PIPE
    with subprocess.Popen([str(x) for x in argv], start_new_session=True, **kwargs) as process:
        try:
            process.communicate(input, timeout=timeout)
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
            end_group(process.pid)
            raise
    if process.returncode:
        raise subprocess.CalledProcessError(process.returncode, process.args)


def call(argv, **kwargs):
    """subprocess.run with check; TMPDIR is the lab's scratch once it exists, unless env is given."""
    if (ROOT / "scratch").is_dir():
        kwargs.setdefault("env", dict(os.environ, TMPDIR=str(ROOT / "scratch")))
    return subprocess.run([str(x) for x in argv], check=True, **kwargs)


def environment():
    """the environment of every lab command: the shell's, less keys, accounts and tool settings,
    plus the lab's ca bundle, home, caches, venv, proxy bypass and the origins fetchloom may reach."""
    env = {key: value for key, value in os.environ.items() if not INHERITED.match(key)}
    env.update(REQUESTS_CA_BUNDLE=BUNDLE, SSL_CERT_FILE=BUNDLE,
               TMPDIR=str(ROOT / "scratch"), HOME=str(ROOT / "home"),
               XDG_CACHE_HOME=str(ROOT / "cache"), DVC_NO_ANALYTICS="true",
               GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null",
               GIT_AUTHOR_NAME="benchmark", GIT_AUTHOR_EMAIL="bench@localhost",
               GIT_COMMITTER_NAME="benchmark", GIT_COMMITTER_EMAIL="bench@localhost",
               # fetchloom refuses a private address that a project, not a person, names
               FETCHLOOM_ALLOWED_ORIGINS=",".join(ORIGINS))
    env["PATH"] = str(VENV / "bin") + os.pathsep + env["PATH"]
    env["NO_PROXY"] = env["no_proxy"] = f"{HOST},{STORE},{SERVER_ADDRESS},localhost,127.0.0.1"
    return env


def exclusive(path=None):
    """the lab lock, held until the returned file is closed; RuntimeError when another benchmark
    has it."""
    lock = (path or ROOT / "run.lock").open("w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        lock.close()
        raise RuntimeError("another benchmark is using the lab") from None
    return lock

def require_root():
    if os.geteuid() != 0:
        raise SystemExit("the lab needs root for namespaces, mounts and drop_caches")


def queue_limit(profile):
    """the netem limit, in packets, that holds a profile's queue time at its rate on the server's side,
    counting the packets that side's half of the delay holds too; None for a link without a rate."""
    shape = PROFILES[profile]
    if not shape["rate_mbit"]:
        return None
    # twice (queue + half the delay) in ms, times the rate in mbit/s, is twice the bits a full queue holds
    doubled = (2 * shape["queue_ms"] + shape["delay_ms"]) * shape["rate_mbit"] * 1000
    return -(-doubled // (2 * 8 * PACKET))


def network_profiles():
    """every profile as the results record it, with the queue limit its shaping sets."""
    return {name: dict(shape, limit_packets=queue_limit(name)) for name, shape in PROFILES.items()}


def shaping(profile, loss=None):
    """the client and server netem specs of a profile: half the delay on each side, the loss on
    both, the rate cap and its queue limit on the server's; ``loss`` replaces the profile's own."""
    shape = PROFILES[profile]
    loss = shape["loss_percent"] if loss is None else loss
    common = ["netem", "delay", f"{shape['delay_ms'] / 2:g}ms", "loss", f"{loss}%"]
    server = common + (["rate", f"{shape['rate_mbit']}mbit"] if shape["rate_mbit"] else [])
    limit = queue_limit(profile)
    return common[:], server + (["limit", str(limit)] if limit else [])


def taken(shown, spec):
    """whether a qdisc tc shows has the rate cap and queue limit of the spec it was given."""
    words = shown.split()
    if ("rate" in words) != ("rate" in spec):
        return False
    if "limit" in spec:
        limit = spec[spec.index("limit") + 1]
        return any(word == "limit" and value == limit for word, value in zip(words, words[1:]))
    return True


def profile(name):
    """shapes both ends of the lab link for a profile, starting from no shaping."""
    require_root()
    if not (ROOT / "owned.json").exists():
        raise RuntimeError("run lab.py setup before changing a profile")
    client, server = shaping(name)
    # replacing a netem qdisc keeps any field the new one leaves out, so a rate cap would outlive
    # a profile without one; each profile starts from no shaping instead
    for prefix, dev, spec in (([], CLIENT, client), (["ip", "netns", "exec", NS], SERVER, server)):
        subprocess.run([*prefix, "tc", "qdisc", "del", "dev", dev, "root"], stderr=subprocess.DEVNULL)
        call([*prefix, "tc", "qdisc", "add", "dev", dev, "root", *spec])
        shown = subprocess.check_output([*prefix, "tc", "qdisc", "show", "dev", dev, "root"], text=True)
        if not taken(shown, spec):
            raise RuntimeError(f"{dev} did not take the {name} profile: {shown.strip()}")


def blackhole(name, on):
    """drops every new packet at both ends of the link (on), or gives a profile its own loss back
    (off). a change keeps the packets netem already holds, so those still arrive."""
    client, server = shaping(name, loss=100 if on else None)
    for prefix, dev, spec in (([], CLIENT, client), (["ip", "netns", "exec", NS], SERVER, server)):
        call([*prefix, "tc", "qdisc", "change", "dev", dev, "root", *spec])


class LinkDrop:
    """a link that drops for `length` seconds from `after` seconds into a run, with nothing stopped;
    `record` says when it dropped and came back, and when the client was receiving again, in seconds
    from the run's start, and when the command's own work began when the drop counts from that."""
    POLL = .01
    # what the client must receive once the link is back for its download to count as moving again
    MOVING = 64 * 1024
    COUNTER = Path(f"/sys/class/net/{CLIENT}/statistics/rx_bytes")

    def __init__(self, profile, after=10, length=10):
        self.profile, self.after, self.length = profile, after, length
        self.record = dict(after_seconds=after, length_seconds=length, anchor_at=None, dropped_at=None, restored_at=None,
                           resumed_at=None, error=None)
        self.stopped = threading.Event()
        self.thread = None

    def begin(self, start, anchor=None):
        """starts the clock of the drop at `start`, a time.monotonic() reading, or once `anchor`, a
        callable giving such a reading or None, gives one."""
        self.thread = threading.Thread(target=self.drop, args=(start, anchor), daemon=True)
        self.thread.start()

    def drop(self, start, anchor):
        try:
            base = start
            if anchor is not None:
                while (base := anchor()) is None:
                    if self.stopped.wait(self.POLL):
                        return
                self.record["anchor_at"] = base - start
            if self.stopped.wait(max(0, base + self.after - time.monotonic())):
                return
            blackhole(self.profile, True)
            self.record["dropped_at"] = time.monotonic() - start
            # a run that ends first gets its link back at once: the drop never outlives it
            self.stopped.wait(max(0, base + self.after + self.length - time.monotonic()))
            blackhole(self.profile, False)
            self.record["restored_at"] = time.monotonic() - start
            self.watch(start)
        except Exception as error:
            self.record["error"] = f"{type(error).__name__}: {error}"

    def received(self):
        """bytes the client's end of the link has received so far, or None where it cannot be read."""
        try:
            return int(self.COUNTER.read_text())
        except (OSError, ValueError):
            return None

    def watch(self, start):
        """notes when the client has received MOVING bytes more than when the link came back, unless
        the run ends first: until then the tool sat idle."""
        base = self.received()
        if base is None:
            return
        while not self.stopped.wait(self.POLL):
            now = self.received()
            if now is not None and now - base >= self.MOVING:
                self.record["resumed_at"] = time.monotonic() - start
                return

    def end(self):
        """waits until the link is back, cutting a drop short when the run ended first."""
        self.stopped.set()
        if self.thread is not None:
            self.thread.join()


def forget_connections():
    """drops what each side's kernel kept from earlier connections, such as how far packets were
    reordered, which loss recovery would start from, so each run meets the server fresh."""
    call(["ip", "-n", NS, "tcp_metrics", "flush", "all"])
    # the host keeps entries for its other peers too, which are not the lab's to reset
    shown = subprocess.check_output(["ip", "tcp_metrics", "show"], text=True)
    if any(line.split()[:1] == [SERVER_ADDRESS] for line in shown.splitlines()):
        call(["ip", "tcp_metrics", "flush", SERVER_ADDRESS])


# every measured run's cgroup sits under this one, which hands each the cpu, memory and io controllers
TREES = Path("/sys/fs/cgroup/fl-bench")
# how often a run's anonymous memory is read
SAMPLE = .02


def trees(root=None):
    """the cgroup every measured run's own goes under, with the controllers they need; RuntimeError
    when this machine cannot make it, since a run's helpers would then go uncounted."""
    root = root or TREES
    try:
        root.mkdir(exist_ok=True)
        (root / "cgroup.subtree_control").write_text("+cpu +memory +io\n")
    except OSError as error:
        raise RuntimeError(f"cannot make the cgroup v2 each run is measured in at {root}: {error}") from None
    return root


def flat(path):
    """a cgroup file of `key value` lines as a dict of ints."""
    return {key: int(value) for key, value in (line.split() for line in Path(path).read_text().splitlines())}


class Tree:
    """a cgroup v2 that a measured command and every process it starts run in, so helpers and workers
    count: cpu time from cpu.stat, the most anonymous memory held at once, read from memory.stat every
    SAMPLE seconds, and the bytes written to a disk from io.stat."""

    def __init__(self, name, root=None):
        self.path = (root or TREES) / name
        self.path.mkdir()
        self.peak = self.samples = 0
        self.stopped = threading.Event()
        self.thread = None

    def adopt(self, pid):
        """moves a process in; one moved before it runs anything starts all its work here."""
        (self.path / "cgroup.procs").write_text(f"{pid}\n")

    def anon(self):
        return flat(self.path / "memory.stat")["anon"]

    def cpu(self):
        """user and system seconds of every process that ran here so far."""
        stat = flat(self.path / "cpu.stat")
        return stat["user_usec"] / 1e6, stat["system_usec"] / 1e6

    def written(self, device):
        """bytes written so far to the disk with this device number, as os.stat gives it."""
        wanted = f"{os.major(device)}:{os.minor(device)}"
        for line in (self.path / "io.stat").read_text().splitlines():
            name, *fields = line.split()
            if name == wanted:
                return int(dict(field.split("=", 1) for field in fields).get("wbytes", 0))
        return 0

    def begin(self):
        """starts reading the anonymous memory, its peak counted afresh."""
        self.peak = self.samples = 0
        self.stopped.clear()
        self.thread = threading.Thread(target=self.sample, daemon=True)
        self.thread.start()

    def sample(self):
        while True:
            try:
                self.peak = max(self.peak, self.anon())
                self.samples += 1
            except (OSError, KeyError, ValueError):
                pass
            if self.stopped.wait(SAMPLE):
                return

    def end(self):
        """stops reading; returns the peak and how many readings it rests on."""
        self.stopped.set()
        if self.thread is not None:
            self.thread.join()
            self.thread = None
        return self.peak, self.samples

    def close(self):
        """kills whatever is still in it, such as a helper the command left behind, and removes it."""
        self.end()
        try:
            (self.path / "cgroup.kill").write_text("1\n")
            deadline = time.monotonic() + 5
            while "populated 1" in (self.path / "cgroup.events").read_text() and time.monotonic() < deadline:
                time.sleep(.01)
            self.path.rmdir()
        except FileNotFoundError:
            pass


DROP = Path("/proc/sys/vm/drop_caches")


def _syncfs(fd):
    import ctypes
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.syncfs(fd) != 0:
        error = ctypes.get_errno()
        raise OSError(error, os.strerror(error))


def flush(*paths):
    """writes out what the filesystems holding these paths have in memory, each once. a global
    sync would also wait on every other filesystem of the machine, such as a hung network mount."""
    done = set()
    for path in paths:
        device = os.stat(path).st_dev
        if device in done:
            continue
        done.add(device)
        fd = os.open(path, os.O_RDONLY)
        try:
            _syncfs(fd)
        finally:
            os.close(fd)


def filesystems():
    """the lab's root and the test filesystems mounted under it."""
    mounted = [ROOT / "fs" / fs for fs in ("ext4", "xfs")]
    return [ROOT, *(target for target in mounted if os.path.ismount(target))]


def drop_caches(files):
    """flushes the lab filesystems and drops the page cache, then reads these files back in, so
    only they start cached."""
    require_root()
    flush(*filesystems())
    DROP.write_text("3\n")
    for path in files:
        with Path(path).open("rb") as f:
            while f.read(1024 * 1024):
                pass


def nginx_config(cert):
    """the lab server: www under both names, every request logged as ACCESS_FORMAT says, and each
    path under /r/ on the server's own name sent on to the storage host with a 302."""
    return f'''pid {ROOT}/nginx.pid;
error_log {ROOT}/nginx-error.log;
events {{}}
http {{
    log_format bench escape=json '{ACCESS_FORMAT}';
    access_log {ROOT}/access.jsonl bench;
    ssl_certificate {cert}/server.crt;
    ssl_certificate_key {cert}/server.key;
    root {ROOT}/www;
    # curl -z sends its own file's time, which is when it downloaded the file; the default
    # exact match would answer that with the whole body again
    if_modified_since before;
    server {{
        listen {SERVER_ADDRESS}:443 ssl http2;
        server_name {HOST};
        location ~ ^/r/(.*)$ {{
            return 302 https://{STORE}/$1;
        }}
    }}
    server {{
        listen {SERVER_ADDRESS}:443 ssl http2;
        server_name {STORE};
    }}
}}
'''


def setup(image_gib=12, cpus=None):
    """a 7-day ca and certificate for both lab names, nginx serving www in its own network namespace
    and loop-mounted ext4 and xfs images; refuses what it did not make and undoes itself on failure."""
    require_root()
    if (ROOT / "owned.json").exists():
        teardown()
    if ROOT.exists() and any(ROOT.iterdir()):
        raise SystemExit(f"refusing to overwrite unowned {ROOT}")
    if CA.exists() or any(name in HOSTS.read_text() for name in (HOST, STORE)):
        raise SystemExit(f"{HOST}, {STORE} or their CA already exists without an ownership marker")
    namespaces = subprocess.check_output(["ip", "netns", "list"], text=True)
    if NS in namespaces or subprocess.run(["ip", "link", "show", CLIENT],
                                          capture_output=True).returncode == 0:
        raise SystemExit("unowned benchmark network already exists")
    ROOT.mkdir(parents=True, exist_ok=True)
    (ROOT / "owned.json").write_text(json.dumps({"image_gib": image_gib}))
    try:
        for name in ("www", "cert", "fs", "images", "prepared", "results", "scratch", "home", "cache"):
            (ROOT / name).mkdir(exist_ok=True)
        (ROOT / "www" / "health").write_text("fl-bench\n")
        cert = ROOT / "cert"
        quiet = {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL}
        call(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes",
              "-keyout", cert / "ca.key", "-out", cert / "ca.crt", "-days", "7",
              "-subj", "/CN=fetchloom disposable benchmark CA"], **quiet)
        call(["openssl", "req", "-newkey", "rsa:2048", "-nodes", "-keyout",
              cert / "server.key", "-out", cert / "server.csr", "-subj", f"/CN={HOST}"], **quiet)
        (cert / "extensions").write_text(f"subjectAltName=DNS:{HOST},DNS:{STORE}\nextendedKeyUsage=serverAuth\n")
        call(["openssl", "x509", "-req", "-in", cert / "server.csr", "-CA",
              cert / "ca.crt", "-CAkey", cert / "ca.key", "-CAcreateserial",
              "-out", cert / "server.crt", "-days", "7", "-extfile", cert / "extensions"], **quiet)
        for key in cert.glob("*.key"):
            key.chmod(0o600)
        shutil.copyfile(cert / "ca.crt", CA)
        call(["update-ca-certificates"], **quiet)
        with HOSTS.open("a") as f:
            f.write(HOST_LINE)
        call(["ip", "netns", "add", NS])
        call(["ip", "link", "add", CLIENT, "type", "veth", "peer", "name", SERVER])
        call(["ip", "link", "set", SERVER, "netns", NS])
        call(["ip", "addr", "add", "10.77.0.1/24", "dev", CLIENT])
        call(["ip", "link", "set", CLIENT, "up"])
        for args in (["addr", "add", f"{SERVER_ADDRESS}/24", "dev", SERVER],
                     ["link", "set", SERVER, "up"], ["link", "set", "lo", "up"]):
            call(["ip", "-n", NS, *args])
        (ROOT / "nginx.conf").write_text(nginx_config(cert))
        with affinity(cpus):
            call(["ip", "netns", "exec", NS, "nginx", "-c", ROOT / "nginx.conf"])
        for fs in ("ext4", "xfs"):
            image = ROOT / "images" / f"{fs}.img"
            with image.open("wb") as f:
                f.truncate(image_gib * 1024 ** 3)
            call([f"mkfs.{fs}", "-F" if fs == "ext4" else "-f", image], **quiet)
            target = ROOT / "fs" / fs
            target.mkdir(exist_ok=True)
            call(["mount", "-o", "loop", image, target])
        profile("lan")
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        # the second answers through the redirect, from the storage host
        for path in ("health", "r/health"):
            with opener.open(f"https://{HOST}/{path}", timeout=5) as response:
                if response.read() != b"fl-bench\n":
                    raise RuntimeError(f"unexpected lab server response to /{path}")
    except BaseException:
        teardown()
        raise
    print(f"lab ready at {ROOT}; export REQUESTS_CA_BUNDLE={BUNDLE} SSL_CERT_FILE={BUNDLE}")


def teardown():
    """removes everything setup made, results included."""
    require_root()
    if not (ROOT / "owned.json").exists():
        print("lab is already absent")
        return
    lock = exclusive()
    for fs in ("ext4", "xfs"):
        target = ROOT / "fs" / fs
        if os.path.ismount(target):
            call(["umount", target])
    pids = subprocess.run(["ip", "netns", "pids", NS], capture_output=True, text=True)
    for pid in pids.stdout.split():
        try:
            os.kill(int(pid), 15)
        except ProcessLookupError:
            pass
    time.sleep(0.2)
    subprocess.run(["ip", "netns", "del", NS], capture_output=True)
    subprocess.run(["ip", "link", "del", CLIENT], capture_output=True)
    HOSTS.write_text("".join(line for line in HOSTS.read_text().splitlines(keepends=True)
                             if not line.rstrip().endswith("# fl-bench")))
    CA.unlink(missing_ok=True)
    call(["update-ca-certificates"], stdout=subprocess.DEVNULL)
    shutil.rmtree(ROOT)
    lock.close()
    print("lab removed, including its results; copy wanted results before teardown")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("setup", "teardown", "profile"))
    parser.add_argument("--profile", choices=PROFILES, default="lan")
    parser.add_argument("--image-gib", type=int, default=12)
    parser.add_argument("--cpus", type=cpu_choice, default=os.environ.get("FL_BENCH_CPUS", ""))
    args = parser.parse_args()
    if args.action == "setup":
        setup(args.image_gib, args.cpus)
    elif args.action == "teardown":
        teardown()
    else:
        with exclusive():
            profile(args.profile)


if __name__ == "__main__":
    main()
