"""Root-owned, disposable Linux network and filesystem lab."""
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
import time
import urllib.request

ROOT = Path(os.environ.get("FL_BENCH_ROOT", "/srv/fl-bench")).resolve()
VENV = Path(os.environ.get("FL_BENCH_VENV", "/opt/fl-bench/venv")).resolve()
NS = "flb-srv"
CLIENT = "flb-client"
SERVER = "flb-server"
BUNDLE = "/etc/ssl/certs/ca-certificates.crt"
CA = Path("/usr/local/share/ca-certificates/fl-bench.crt")
HOST_LINE = "10.77.0.2 lab.flb # fl-bench\n"
# runs are public and reproducible, so no key, account file or fetchloom setting is inherited
INHERITED = re.compile(r"(FETCHLOOM|HF|HUGGING_FACE|KAGGLE|KAGGLEHUB|ZENODO|FIGSHARE|OSF|DATAVERSE|AWS|GOOGLE|AZURE)_"
                       r"|(NETRC|XDG_CONFIG_HOME|XDG_DATA_HOME|DBUS_SESSION_BUS_ADDRESS)$")
PROFILES = {
    "lan": (0, 0, None, 1024 * 1024 * 1024, 1000),
    "home": (20, 0, 100, 256 * 1024 * 1024, 1000),
    "far": (150, 1, 50, 4 * 1024 * 1024, 50),
    "poor": (300, 3, 10, 1024 * 1024, 20),
}
PREPARE_TIMEOUT = 1200


def cpu_list(value, available=None):
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
    if (ROOT / "scratch").is_dir():
        kwargs.setdefault("env", dict(os.environ, TMPDIR=str(ROOT / "scratch")))
    return subprocess.run([str(x) for x in argv], check=True, **kwargs)


def environment():
    env = {key: value for key, value in os.environ.items() if not INHERITED.match(key)}
    env.update(REQUESTS_CA_BUNDLE=BUNDLE, SSL_CERT_FILE=BUNDLE,
               TMPDIR=str(ROOT / "scratch"), HOME=str(ROOT / "home"),
               XDG_CACHE_HOME=str(ROOT / "cache"), DVC_NO_ANALYTICS="true",
               GIT_AUTHOR_NAME="benchmark", GIT_AUTHOR_EMAIL="bench@localhost",
               GIT_COMMITTER_NAME="benchmark", GIT_COMMITTER_EMAIL="bench@localhost")
    env["PATH"] = str(VENV / "bin") + os.pathsep + env["PATH"]
    env["NO_PROXY"] = env["no_proxy"] = "lab.flb,10.77.0.2,localhost,127.0.0.1"
    return env


def exclusive(path=None):
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


def shaping(profile):
    delay, loss, rate, _, _ = PROFILES[profile]
    common = ["netem", "delay", f"{delay / 2:g}ms", "loss", f"{loss}%"]
    return common[:], common + (["rate", f"{rate}mbit"] if rate else [])


def profile(name):
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
        if ("rate" in shown.split()) != ("rate" in spec):
            raise RuntimeError(f"{dev} did not take the {name} profile: {shown.strip()}")


def drop_caches(files):
    require_root()
    os.sync()
    Path("/proc/sys/vm/drop_caches").write_text("3\n")
    for path in files:
        with Path(path).open("rb") as f:
            while f.read(1024 * 1024):
                pass


def setup(image_gib=12, cpus=None):
    require_root()
    if (ROOT / "owned.json").exists():
        teardown()
    if ROOT.exists() and any(ROOT.iterdir()):
        raise SystemExit(f"refusing to overwrite unowned {ROOT}")
    if CA.exists() or "lab.flb" in Path("/etc/hosts").read_text():
        raise SystemExit("lab.flb or its CA already exists without an ownership marker")
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
              cert / "server.key", "-out", cert / "server.csr", "-subj", "/CN=lab.flb"], **quiet)
        (cert / "extensions").write_text("subjectAltName=DNS:lab.flb\nextendedKeyUsage=serverAuth\n")
        call(["openssl", "x509", "-req", "-in", cert / "server.csr", "-CA",
              cert / "ca.crt", "-CAkey", cert / "ca.key", "-CAcreateserial",
              "-out", cert / "server.crt", "-days", "7", "-extfile", cert / "extensions"], **quiet)
        for key in cert.glob("*.key"):
            key.chmod(0o600)
        shutil.copyfile(cert / "ca.crt", CA)
        call(["update-ca-certificates"], **quiet)
        with Path("/etc/hosts").open("a") as f:
            f.write(HOST_LINE)
        call(["ip", "netns", "add", NS])
        call(["ip", "link", "add", CLIENT, "type", "veth", "peer", "name", SERVER])
        call(["ip", "link", "set", SERVER, "netns", NS])
        call(["ip", "addr", "add", "10.77.0.1/24", "dev", CLIENT])
        call(["ip", "link", "set", CLIENT, "up"])
        for args in (["addr", "add", "10.77.0.2/24", "dev", SERVER],
                     ["link", "set", SERVER, "up"], ["link", "set", "lo", "up"]):
            call(["ip", "-n", NS, *args])
        config = f'''pid {ROOT}/nginx.pid;
error_log {ROOT}/nginx-error.log;
events {{}}
http {{
    log_format bench escape=json '{{"request":"$request","status":$status,"bytes":$body_bytes_sent}}';
    access_log {ROOT}/access.jsonl bench;
    server {{
        listen 10.77.0.2:443 ssl http2;
        server_name lab.flb;
        ssl_certificate {cert}/server.crt;
        ssl_certificate_key {cert}/server.key;
        root {ROOT}/www;
        # curl -z sends its own file's time, which is when it downloaded the file; the default
        # exact match would answer that with the whole body again
        if_modified_since before;
    }}
}}
'''
        (ROOT / "nginx.conf").write_text(config)
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
        with opener.open("https://lab.flb/health", timeout=5) as response:
            if response.read() != b"fl-bench\n":
                raise RuntimeError("unexpected lab server response")
    except BaseException:
        teardown()
        raise
    print(f"lab ready at {ROOT}; export REQUESTS_CA_BUNDLE={BUNDLE} SSL_CERT_FILE={BUNDLE}")


def teardown():
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
    hosts = Path("/etc/hosts")
    hosts.write_text(hosts.read_text().replace(HOST_LINE, ""))
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
