"""exercise an installed executable against fixed loopback bytes, not a source-tree binary."""

import argparse
import hashlib
import http.server
import io
import json
import os
from pathlib import Path
import re
import shutil
import socketserver
import stat
import subprocess
import sys
import tempfile
import threading
import urllib.parse
import zipfile

ROOT = Path(__file__).resolve().parents[1]
DATA = b"sepal_length,species\n5.1,setosa\n"
DIGEST = "8b1ffd452ff4bb8e22402d04ac3f2eab66202dd50aee35e61d420bdd99b285eb"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def digest(path):
    value = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def wheel_version(version):
    """the version a wheel records for Cargo's `version`: maturin writes pep 440's spelling, so
    0.1.0-rc.1 becomes 0.1.0rc1, -alpha.N becomes aN and -beta.N becomes bN."""
    match = re.fullmatch(r"([0-9]+\.[0-9]+\.[0-9]+)(?:-(alpha|beta|rc)\.([0-9]+))?", version)
    require(match is not None, f"{version} is not a release or an alpha, beta or rc pre-release, such as 0.1.0-rc.1")
    release, kind, number = match.groups()
    if kind is None:
        return release
    return release + {"alpha": "a", "beta": "b", "rc": "rc"}[kind] + str(int(number))


def environment(home, cache=None, origin=None):
    """this environment without credentials, proxies, python paths or the user's caches and
    settings, ``home`` as every home folder and loopback reached directly; ``cache`` is
    fetchloom's cache and ``origin`` the one origin it may download from."""
    env = {name: value for name, value in os.environ.items()
           if not name.upper().startswith(("FETCHLOOM_", "AWS_", "HF_", "KAGGLE_", "DATAVERSE_", "ONEDRIVE"))
           and not name.upper().endswith(("_TOKEN", "_PROXY"))
           and name.upper() not in {"PYTHONPATH", "PYTHONHOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "NETRC"}}
    env.update(HOME=str(home), USERPROFILE=str(home), APPDATA=str(home), LOCALAPPDATA=str(home),
               FETCHLOOM_KEYRING="0", CI="1", CLICOLOR="0", NO_PROXY="127.0.0.1", LC_ALL="C.UTF-8")
    if cache is not None:
        env["FETCHLOOM_CACHE"] = str(cache)
    if origin is not None:
        env["FETCHLOOM_ALLOWED_ORIGINS"] = origin
    return env


def check_version(binary, version, env=None, commit=None, cwd=None):
    """refuse ``binary`` unless ``--version`` says ``version``, and ``-v`` names ``commit`` when
    given; ``binary`` may be a command, as ``python -I -m fetchloom``."""
    command = [str(part) for part in binary] if isinstance(binary, (list, tuple)) else [str(binary)]
    said = subprocess.check_output([*command, "--version", *(["-v"] if commit else [])], env=env, cwd=cwd,
                                   text=True, timeout=20)
    require(said.splitlines()[0] == f"fetchloom {version}", said)
    if commit:
        require(commit in said, f"the artifact does not identify commit {commit}: {said}")
    return said


def remove_tree(path):
    """delete ``path`` and all in it, read-only files too, which windows refuses to delete."""
    def writable(function, name, error):
        os.chmod(name, stat.S_IWRITE | stat.S_IREAD)
        function(name)
    if path.exists():
        shutil.rmtree(path, onerror=writable)


class Server:
    """a loopback http server answering HEAD and GET for the paths in ``bodies``, keeping each
    request."""

    def __init__(self, bodies):
        self.bodies = dict(bodies)
        self.requests = []
        self.lock = threading.Lock()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, format, *args):
                pass

            def do_HEAD(self):
                self.answer(False)

            def do_GET(self):
                self.answer(True)

            def answer(self, body):
                name = urllib.parse.unquote(urllib.parse.urlsplit(self.path).path)
                with owner.lock:
                    content = owner.bodies.get(name)
                    owner.requests.append((self.command, name))
                if content is None:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Length", str(len(content)))
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("ETag", '"' + hashlib.sha256(content).hexdigest() + '"')
                self.send_header("Connection", "close")
                self.end_headers()
                if body:
                    self.wfile.write(content)
                self.close_connection = True

        # not http.server's server: it looks up the name of 127.0.0.1 as it binds, which takes 35 s
        # in every new process on the mac runners
        self.server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        # shutdown waits out one poll, which is half a second by default
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.05}, daemon=True)
        self.origin = f"http://127.0.0.1:{self.server.server_address[1]}"

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=10)
        require(not self.thread.is_alive(), "loopback server did not stop")

    def url(self, path):
        return self.origin + urllib.parse.quote(path, safe="/")

    def count(self):
        with self.lock:
            return len(self.requests)


class Journey:
    """fetchloom's main path, run with the installed ``binary`` in a home and cache of its own."""

    def __init__(self, binary, root, server):
        self.binary = Path(binary).resolve()
        self.root = root
        self.server = server
        self.home = root / "home"
        self.home.mkdir()
        self.cache = root / "cache"
        self.env = environment(self.home, self.cache, server.origin)

    def project(self, name):
        path = self.root / name
        path.mkdir()
        return path

    def run(self, project, *arguments, success=True, cache=None):
        """fetchloom's JSON answer in ``project``, refused unless it ended as ``success`` says."""
        env = dict(self.env)
        if cache is not None:
            env["FETCHLOOM_CACHE"] = str(cache)
        result = subprocess.run([str(self.binary), "--json", "-C", str(project), *arguments],
                                cwd=project, env=env, stdin=subprocess.DEVNULL,
                                capture_output=True, text=True, timeout=90)
        print("+ fetchloom", *arguments, "=>", result.returncode, flush=True)
        require((result.returncode == 0) == success, result.stdout + result.stderr)
        document = json.loads(result.stdout)
        require((document["status"] == "ok") == success, result.stdout)
        return document

    def completions(self, project):
        for shell in ["bash", "zsh"]:
            completion = project / f"fetchloom.{shell}"
            generated = subprocess.check_output([str(self.binary), "completions", shell],
                                                env=self.env, cwd=project, timeout=30)
            completion.write_bytes(generated)
            script = '. "$1"; complete -p fetchloom' if shell == "bash" else (
                'autoload -U compinit; compinit -u -d "$HOME/.zcompdump"; . "$1"; whence -w _fetchloom')
            flags = ["--noprofile", "--norc"] if shell == "bash" else ["-f"]
            result = subprocess.run([shell, *flags, "-c", script, shell, str(completion)],
                                    env=self.env, cwd=project, capture_output=True, text=True, timeout=30)
            require(result.returncode == 0 and "fetchloom" in result.stdout, result.stdout + result.stderr)

    def clone(self, source, name):
        clone = self.project(name)
        for path in ["data.toml", "data.lock"]:
            shutil.copyfile(source / path, clone / path)
        if (source / "data.lock.d").exists():
            shutil.copytree(source / "data.lock.d", clone / "data.lock.d")
        return clone

    def check(self, version, commit=None, helper=False):
        """add, restore offline from the shared cache, keep an edit, refuse corrupt bytes and
        unpack an archive, then return the proof check_artifacts.py records; ``helper`` restores
        through the installed python helper."""
        require(hashlib.sha256(DATA).hexdigest() == DIGEST, "the independent fixture hash is wrong")
        check_version(self.binary, version, self.env, commit)
        project = self.project("original")
        self.run(project, "add", self.server.url("/iris.csv"), "--as", "iris")
        placed = project / "data/iris/iris.csv"
        require(placed.read_bytes() == DATA and digest(placed) == DIGEST, "downloaded bytes differ")
        listing = self.run(project, "list")["result"]["datasets"][0]
        require(listing["locked_files"][0]["sha256"] == DIGEST, "data.lock did not pin the expected bytes")
        clone = self.clone(project, "clone")
        before = self.server.count()
        restored = self.run(clone, "sync", "--locked", "--offline")
        require(self.server.count() == before, "offline restoration made a network request")
        require((clone / "data/iris/iris.csv").read_bytes() == DATA, "shared cache restored wrong bytes")
        require(restored["work"]["reflinks"] + restored["work"]["hardlinks"] > 0, "default drive did not reuse bytes by a link")
        copied = self.clone(project, "copy")
        (copied / "data.toml").write_text(f'[datasets.iris]\nref = "{self.server.url("/iris.csv")}"\ncopy = true\n', encoding="utf-8")
        self.run(copied, "sync", "--locked", "--offline")
        remove_tree(copied / "data")
        if helper:
            result = subprocess.run([sys.executable, "-I", str(Path(__file__).resolve()), "--helper-worker",
                                     str(copied), "--binary", str(self.binary)], cwd=copied,
                                    env=self.env, capture_output=True, text=True, timeout=90)
            require(result.returncode == 0, result.stdout + result.stderr)
            print(result.stdout.strip())
        else:
            self.run(copied, "sync", "--locked", "--offline")
        copy = copied / "data/iris/iris.csv"
        require(copy.read_bytes() == DATA, "restoration failed")
        copy.write_bytes(b"my preserved edit\n")
        kept = self.run(copied, "sync", "--locked", "--offline")
        require(kept["result"]["placement"]["kept"], "an edit was not reported")
        require(copy.read_bytes() == b"my preserved edit\n", "an edit was overwritten")
        require((project / "data/iris/iris.csv").read_bytes() == DATA, "a writable copy changed shared bytes")
        broken = self.clone(project, "bad-download")
        self.server.bodies["/iris.csv"] = b"x" * len(DATA)
        before = self.server.count()
        refused = self.run(broken, "sync", "--locked", success=False, cache=self.root / "fresh-cache")
        require(self.server.count() > before, "the bad download was never attempted")
        require(refused["errors"] and "sha" in json.dumps(refused["errors"]).lower(), "failure did not identify a checksum mismatch")
        require(not (broken / "data/iris/iris.csv").exists(), "corrupt bytes were placed")
        require(not (self.root / "fresh-cache/objects" / DIGEST[:2] / DIGEST[2:]).exists(), "corrupt bytes were accepted into the cache")
        archive = self.project("archive")
        self.run(archive, "get", self.server.url("/bundle.zip"), "--dir", "unpacked")
        files = {path.relative_to(archive / "unpacked").as_posix(): path.read_bytes()
                 for path in (archive / "unpacked").rglob("*") if path.is_file()}
        require(sorted(files.values()) == [b"", b"archive fixture\n"], f"archive bytes differ: {files}")
        self.run(project, "auth", "--offline")
        if sys.platform == "darwin":
            self.completions(project)
            default = subprocess.run([str(self.binary), "--json", "cache"], cwd=project,
                                     env=environment(self.home), capture_output=True, text=True, timeout=30)
            require(default.returncode == 0, default.stderr)
            require(Path(json.loads(default.stdout)["result"]["path"]) == self.home / "Library/Caches/fetchloom", "wrong mac cache location")
            cloud = self.home / "Library/CloudStorage/OneDrive-ci/project"
            cloud.mkdir(parents=True)
            for name in ["data.toml", "data.lock"]:
                shutil.copyfile(project / name, cloud / name)
            cloud_sync = self.run(cloud, "sync", "--locked", "--offline")
            require(cloud_sync["work"]["hardlinks"] == 0, "a recognized cloud folder received a hardlink")
        return {"ok": True, "binary_sha256": digest(self.binary), "version": version,
                "commit": commit, "helper": helper, "requests": self.server.count()}


def fixtures():
    """the files the loopback server holds: the csv and a zip of two files."""
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("bundle/train.txt", b"archive fixture\n")
        archive.writestr("bundle/empty", b"")
    return {"/iris.csv": DATA, "/bundle.zip": buffer.getvalue()}


def helper_worker(project, binary):
    """inside the installed wheel: the helper must run ``binary`` and sync, find and refuse as its
    documentation says."""
    import fetchloom
    require(Path(fetchloom.__file__).resolve().is_relative_to(Path(sys.prefix).resolve()), "source tree shadowed the installed helper")
    require(Path(fetchloom._program()).resolve() == binary.resolve(), "the helper selected another executable")
    require((Path(fetchloom.__file__).parent / "py.typed").is_file(), "py.typed is missing")
    require(fetchloom.sync(project, offline=True, locked=True, strict=True).resolve() == project.resolve(), "helper sync returned another project")
    path = fetchloom.path("iris/iris.csv", project, offline=True, locked=True)
    require(path.read_bytes() == DATA, "helper path returned wrong bytes")
    empty = project.parent / "outside-project"
    empty.mkdir()
    try:
        fetchloom.path("iris", empty)
    except fetchloom.Error as error:
        require(error.kind == "project.invalid", f"wrong helper error kind: {error.kind}")
    else:
        raise AssertionError("the helper accepted a nonexistent project")
    print(json.dumps({"ok": True, "python": sys.version.split()[0], "module": fetchloom.__file__, "program": str(binary)}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--commit")
    parser.add_argument("--helper", action="store_true")
    parser.add_argument("--helper-worker", type=Path)
    parser.add_argument("--work-root", type=Path)
    parser.add_argument("--report", type=Path)
    args = parser.parse_args()
    if args.helper_worker:
        helper_worker(args.helper_worker, args.binary)
        return
    work_root = args.work_root or os.environ.get("RUNNER_TEMP")
    with tempfile.TemporaryDirectory(prefix="fetchloom-journey-", dir=work_root) as directory, Server(fixtures()) as server:
        result = Journey(args.binary, Path(directory), server).check(args.version, args.commit, args.helper)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result))


if __name__ == "__main__":
    main()
