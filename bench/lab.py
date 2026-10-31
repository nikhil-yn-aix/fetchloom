"""Root-owned, disposable Linux network and filesystem lab."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import shutil
import subprocess
import time
import urllib.request

ROOT = Path("/srv/fl-bench")
NS = "flb-srv"
CLIENT = "flb-client"
SERVER = "flb-server"
BUNDLE = "/etc/ssl/certs/ca-certificates.crt"
CA = Path("/usr/local/share/ca-certificates/fl-bench.crt")
HOST_LINE = "10.77.0.2 lab.flb # fl-bench\n"
PROFILES = {
    "lan": (0, 0, None, 1024 * 1024 * 1024, 1000),
    "home": (20, 0, 100, 256 * 1024 * 1024, 1000),
    "far": (150, 1, 50, 4 * 1024 * 1024, 50),
    "poor": (300, 3, 10, 1024 * 1024, 20),
}


def call(argv, **kwargs):
    if (ROOT / "scratch").is_dir():
        kwargs.setdefault("env", dict(os.environ, TMPDIR=str(ROOT / "scratch")))
    return subprocess.run([str(x) for x in argv], check=True, **kwargs)


def environment():
    env = os.environ.copy()
    env.update(REQUESTS_CA_BUNDLE=BUNDLE, SSL_CERT_FILE=BUNDLE,
               TMPDIR=str(ROOT / "scratch"), HOME=str(ROOT / "home"),
               XDG_CACHE_HOME=str(ROOT / "cache"), DVC_NO_ANALYTICS="true",
               GIT_AUTHOR_NAME="benchmark", GIT_AUTHOR_EMAIL="bench@localhost",
               GIT_COMMITTER_NAME="benchmark", GIT_COMMITTER_EMAIL="bench@localhost")
    env["PATH"] = "/opt/fl-bench/venv/bin:" + env["PATH"]
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
    call(["tc", "qdisc", "replace", "dev", CLIENT, "root", *client])
    call(["ip", "netns", "exec", NS, "tc", "qdisc", "replace", "dev", SERVER,
          "root", *server])


def drop_caches(files):
    require_root()
    os.sync()
    Path("/proc/sys/vm/drop_caches").write_text("3\n")
    for path in files:
        with Path(path).open("rb") as f:
            while f.read(1024 * 1024):
                pass


def setup(image_gib=12):
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
    }}
}}
'''
        (ROOT / "nginx.conf").write_text(config)
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
    args = parser.parse_args()
    if args.action == "setup":
        setup(args.image_gib)
    elif args.action == "teardown":
        teardown()
    else:
        with exclusive():
            profile(args.profile)


if __name__ == "__main__":
    main()
