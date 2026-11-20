#!/usr/bin/env python3
"""the os hunt: fetchloom's user journeys on a windows or macos runner, as a new user meets them.

usage:
  hunt.py product --into DIR            check out the commit under test in DIR: OSH_PRODUCT when
                                        set, else where this branch left origin/main
  hunt.py run --product DIR --image LABEL --out DIR [--only a,b]
  hunt.py gitinstall --product DIR --out DIR
  hunt.py ctrlc PID                     windows: a ctrl-c to the console PID runs in
  hunt.py hold PATH SHARE READY RELEASE windows: hold PATH open with SHARE until RELEASE exists

every check is ok, a lead (the docs or a reasonable user expect otherwise), a note (recorded, not
judged) or a skip with its reason. an exception inside a journey is a harness error: it fails the
step, while leads do not.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import threading
import time
import traceback
import unicodedata
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
if __name__ == "__main__":
    # win.py and mac.py import hunt; they must get this module, not a second copy of it
    sys.modules["hunt"] = sys.modules[__name__]
import serve  # noqa: E402

IS_WIN = os.name == "nt"
IS_MAC = sys.platform == "darwin"
EXE = ".exe" if IS_WIN else ""
MiB = 1 << 20
REPO = "https://github.com/nikhil-yn-aix/fetchloom"
# names windows or fat disks cannot store; the docs say windows refuses them by name and other
# systems place them with one warning (docs/user/reference/data-toml.md, destination paths)
RESERVED = ["CON", "nul.txt", "aux.csv", "COM1.dat", "lpt9", "trail.", "space ", "pipe|.csv",
            "star*.csv", "what?.csv", "colon:x.csv", "CONIN$", "COM\u00b9.txt"]
LONG_MEMBER = "/".join(f"level{i:02d}_" + "x" * 40 for i in range(6)) + "/deep.csv"
# makes the terminal on stdin the controlling terminal, then runs the command in its place
TAKE_TERMINAL = ("import fcntl, os, sys, termios; os.setsid(); fcntl.ioctl(0, termios.TIOCSCTTY, 0); "
                 "os.execv(sys.argv[1], sys.argv[1:])")


class Skip(Exception):
    """a journey or part of one that this runner cannot do, with the reason."""


def blob(size: int, seed: str) -> bytes:
    return hashlib.shake_256(seed.encode()).digest(size)


def csv(rows: int, seed: str) -> bytes:
    return "".join(f"{i},{seed},{(i * 7919) % 1000 / 10:.1f}\n" for i in range(rows)).encode()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with open(long(path), "rb") as stream:
        for block in iter(lambda: stream.read(MiB), b""):
            digest.update(block)
    return digest.hexdigest()


def long(path, force=False) -> str:
    """path in a form python can open past 260 characters on windows."""
    text = os.path.abspath(str(path))
    if IS_WIN and (force or len(text) > 240) and not text.startswith("\\\\"):
        return "\\\\?\\" + text
    return text


def exists(path) -> bool:
    return os.path.lexists(long(path))


def tree(folder: Path, marker=False) -> dict:
    """every file below folder: its path with / mapped to (size, sha256). without `marker`, the
    .gitignore fetchloom writes at the top of each dataset folder is left out."""
    found = {}
    base = long(folder, force=True)
    if not os.path.isdir(base):
        return found
    for top, _dirs, files in os.walk(base):
        for name in files:
            full = os.path.join(top, name)
            rel = os.path.relpath(full, base).replace(os.sep, "/")
            if rel == ".gitignore" and not marker:
                continue
            with open(full, "rb") as stream:
                data = stream.read()
            found[rel] = (len(data), hashlib.sha256(data).hexdigest())
    return found


def listing(folder: Path) -> list:
    """the files below folder, without opening them, the dataset marker left out."""
    base = long(folder, force=True)
    found = []
    for top, _dirs, files in os.walk(base):
        for name in files:
            rel = os.path.relpath(os.path.join(top, name), base).replace(os.sep, "/")
            if rel != ".gitignore":
                found.append(rel)
    return sorted(found)


def nfc(text: str) -> str:
    return unicodedata.normalize("NFC", text)


def placed_names(members) -> dict:
    """the files a sole archive places: a top folder every member shares is dropped."""
    names = [name for name, _ in members]
    firsts = {name.split("/", 1)[0] for name in names}
    strip = len(firsts) == 1 and all("/" in name for name in names)
    return {(name.split("/", 1)[1] if strip else name): (len(data), hashlib.sha256(data).hexdigest())
            for name, data in members}


def writable(path) -> bool:
    return bool(os.stat(long(path)).st_mode & stat.S_IWRITE)


def make_writable(path) -> None:
    os.chmod(long(path), os.stat(long(path)).st_mode | stat.S_IWRITE)


def force_remove(path) -> None:
    """removes a file, read-only or not, as `del /f` or explorer's prompt does."""
    if os.path.lexists(long(path)):
        make_writable(path)
        os.remove(long(path))


def rmtree(path) -> None:
    def retry(function, target, _info):
        try:
            os.chmod(target, stat.S_IWRITE | stat.S_IREAD)
            function(target)
        except OSError:
            pass
    if os.path.lexists(long(path)):
        shutil.rmtree(long(path, force=True), onerror=retry)


def last_lines(text: str, count: int = 3) -> str:
    lines = [line for line in text.strip().splitlines() if line.strip()]
    return " | ".join(lines[-count:])


class Res:
    """one finished command."""

    def __init__(self, argv, cwd, code, out, err, secs, timed_out=False):
        self.argv, self.cwd, self.code = argv, cwd, code
        self.out, self.err, self.secs, self.timed_out = out, err, secs, timed_out

    @property
    def text(self) -> str:
        return self.out + self.err

    def line(self) -> str:
        shown = ["fetchloom" if i == 0 and Path(a).stem == "fetchloom" else a for i, a in enumerate(self.argv)]
        return " ".join(f'"{a}"' if (" " in a or not a) else a for a in shown)


class Running:
    """a command started in the background, its output read as it comes."""

    def __init__(self, popen, argv, cwd, read=True):
        self.p, self.argv, self.cwd, self.start = popen, argv, cwd, time.monotonic()
        self.out, self.err = bytearray(), bytearray()
        self.readers = []
        for stream, sink in ((popen.stdout, self.out), (popen.stderr, self.err)):
            if stream is None or not read:
                continue
            reader = threading.Thread(target=self.drain, args=(stream, sink), daemon=True)
            reader.start()
            self.readers.append(reader)

    @staticmethod
    def drain(stream, sink):
        while True:
            chunk = stream.read1(65536) if hasattr(stream, "read1") else stream.read(65536)
            if not chunk:
                break
            sink.extend(chunk)

    def poll(self):
        return self.p.poll()

    def wait(self, timeout: float) -> Res:
        timed_out = False
        try:
            self.p.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            kill_tree(self.p)
            try:
                self.p.wait(timeout=30)
            except subprocess.TimeoutExpired:
                pass
        for reader in self.readers:
            reader.join(timeout=10)
        return Res(self.argv, self.cwd, self.p.returncode, self.out.decode("utf-8", "replace"),
                   self.err.decode("utf-8", "replace"), time.monotonic() - self.start, timed_out)


def kill_tree(popen) -> None:
    if IS_WIN:
        subprocess.run(["taskkill", "/T", "/F", "/PID", str(popen.pid)], capture_output=True)
    else:
        try:
            os.killpg(popen.pid, signal.SIGKILL)
        except OSError:
            popen.kill()


def run_plain(argv, cwd=None, env=None, timeout=600, text=True):
    """a helper command, not fetchloom: (code, stdout, stderr)."""
    try:
        done = subprocess.run(argv, cwd=cwd, env=env, capture_output=True, timeout=timeout)
    except FileNotFoundError as err:
        return 127, "", str(err)
    except subprocess.TimeoutExpired:
        return 124, "", f"timed out after {timeout} s"
    if not text:
        return done.returncode, done.stdout, done.stderr
    return (done.returncode, done.stdout.decode("utf-8", "replace"),
            done.stderr.decode("utf-8", "replace"))


def git(*args, cwd=None) -> str:
    code, out, err = run_plain(["git", *args], cwd=cwd)
    if code:
        raise RuntimeError(f"git {' '.join(args)}: {err.strip()}")
    return out.strip()


class Hunt:
    def __init__(self, args):
        self.image = args.image
        self.out = Path(args.out).resolve()
        self.out.mkdir(parents=True, exist_ok=True)
        self.product = Path(args.product).resolve()
        self.binary = self.product / "target" / "release" / f"fetchloom{EXE}"
        temp = Path(os.environ.get("RUNNER_TEMP") or self.out.parent)
        # on windows the work goes in the user profile, where a new user's projects and the
        # default cache live; a cache outside it warns cache.foreign on every run
        self.work = (Path.home() if IS_WIN else temp) / "osh-work"
        rmtree(self.work)
        self.work.mkdir(parents=True)
        self.www = temp / "osh-www"
        rmtree(self.www)
        self.www.mkdir(parents=True)
        self.records = []
        self.journeys = []
        self.facts = {}
        self.name = "setup"
        self.harness_errors = 0
        self.logfile = open(self.out / "hunt.log", "w", encoding="utf-8")
        self.server, self.site, self.base = serve.start(self.www)
        self.zips = {}
        self.made = {}
        self.made_sums = {}
        self.cleanups = []

    # -- output --------------------------------------------------------------------------

    def say(self, text: str, log_only: bool = False) -> None:
        self.logfile.write(text + "\n")
        self.logfile.flush()
        if not log_only:
            print(text, flush=True)

    def show(self, res: Res, limit: int = 24) -> None:
        state = "timed out" if res.timed_out else f"exit {res.code}"
        self.say(f"$ {res.line()}    [in {res.cwd}] -> {state} in {res.secs:.1f} s")
        for tag, text in (("o", res.out), ("e", res.err)):
            lines = text.splitlines()
            for line in lines[:4000]:
                self.logfile.write(f"    {tag}| {line}\n")
            if len(lines) > limit:
                lines = lines[:limit // 2] + [f"... {len(lines) - limit} more lines in hunt.log ..."] + lines[-limit // 2:]
            for line in lines:
                print(f"    {tag}| {line}", flush=True)
        self.logfile.flush()

    def record(self, cid, status, expect, observed, doc=None, res=None):
        entry = {"journey": self.name, "id": cid, "status": status, "expect": expect,
                 "observed": str(observed)[:2000], "doc": doc}
        if res is not None:
            entry.update(command=res.line(), cwd=str(res.cwd), exit=res.code, timed_out=res.timed_out,
                         stderr_tail="\n".join(res.err.strip().splitlines()[-12:]),
                         stdout_tail="\n".join(res.out.strip().splitlines()[-6:]))
        self.records.append(entry)
        return entry

    def check(self, cid, ok, expect, observed="", doc=None, res=None, kind="lead") -> bool:
        status = "ok" if ok else kind
        self.record(cid, status, expect, observed, doc, res)
        if ok:
            self.say(f"  ok    {self.name}/{cid}")
        else:
            self.say(f"  {kind.upper():5} {self.name}/{cid}: expected {expect}; observed {observed}")
            if kind == "lead":
                message = f"{expect} / observed: {observed}".replace("\n", " ")[:600]
                print(f"::warning title=lead {self.image} {self.name}/{cid}::{message}", flush=True)
        return bool(ok)

    def note(self, cid, observed, res=None, doc=None) -> None:
        self.record(cid, "note", "", observed, doc, res)
        self.say(f"  note  {self.name}/{cid}: {observed}")

    def skip(self, cid, reason) -> None:
        self.record(cid, "skip", "", reason)
        self.say(f"  skip  {self.name}/{cid}: {reason}")

    # -- commands ------------------------------------------------------------------------

    def env(self, cache=None, **extra) -> dict:
        env = dict(os.environ)
        for name in ("CI", "GITHUB_ACTIONS", "FETCHLOOM_CACHE", "FETCHLOOM_NO_CONFIG"):
            env.pop(name, None)
        # the test server is on 127.0.0.1, which fetchloom follows again only once it is
        # allowed, as its own warning after the first add says
        env["FETCHLOOM_ALLOWED_ORIGINS"] = self.base
        if cache is not None:
            env["FETCHLOOM_CACHE"] = str(cache)
        for name, value in extra.items():
            if value is None:
                env.pop(name, None)
            else:
                env[name] = str(value)
        return env

    def start(self, args, cwd, env, console=False, nofile=None, binary=None, pty_fd=None,
              stderr=None, read=True) -> Running:
        """starts fetchloom. no preexec_fn: the server's threads make it unsafe, so a file limit
        goes through `sh -c 'ulimit -n'` and a terminal through a small python that takes it as
        its controlling terminal; both exec fetchloom, which keeps their pid."""
        shown = [str(binary or self.binary), *[str(a) for a in args]]
        argv = list(shown)
        if nofile and not IS_WIN:
            argv = ["/bin/sh", "-c", f'ulimit -n {int(nofile)} && exec "$0" "$@"', *argv]
        options = {"cwd": str(cwd), "env": env}
        if IS_WIN:
            options["creationflags"] = subprocess.CREATE_NEW_CONSOLE if console else 0
        elif pty_fd is None:
            options["start_new_session"] = True
        if pty_fd is not None:
            argv = [sys.executable, "-c", TAKE_TERMINAL, *argv]
            popen = subprocess.Popen(argv, stdin=pty_fd, stdout=pty_fd, stderr=pty_fd, **options)
        else:
            popen = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                     stderr=stderr if stderr is not None else subprocess.PIPE, **options)
        return Running(popen, shown, cwd, read=read)

    def closed_stdout(self, args, cwd, env, nofile=None):
        """runs a command whose reader takes one line of stdout and closes the pipe; returns
        (res, first line)."""
        sink_path = self.work / f"stderr-{time.monotonic_ns()}.txt"
        with open(sink_path, "wb") as sink:
            running = self.start(args, cwd, env, nofile=nofile, stderr=sink, read=False)
            line = []
            reader = threading.Thread(target=lambda: line.append(running.p.stdout.readline()), daemon=True)
            reader.start()
            reader.join(timeout=120)
            first = line[0] if line else b""
            timed_out = False
            if not line:
                timed_out = True
                kill_tree(running.p)
                reader.join(timeout=30)
            running.p.stdout.close()
            try:
                running.p.wait(timeout=120)
            except subprocess.TimeoutExpired:
                timed_out = True
                kill_tree(running.p)
                running.p.wait(timeout=30)
        err = sink_path.read_bytes().decode("utf-8", "replace")
        sink_path.unlink()
        res = Res(running.argv, cwd, running.p.returncode, first.decode("utf-8", "replace"), err,
                  time.monotonic() - running.start, timed_out)
        return res, first

    def fl(self, args, cwd, env, timeout=600, show=True, nofile=None, binary=None) -> Res:
        res = self.start(args, cwd, env, nofile=nofile, binary=binary).wait(timeout)
        if show:
            self.show(res)
        if res.timed_out:
            self.check("no-hang", False, f"`{res.line()}` finishes within {timeout} s",
                       "it was still running and was killed", res=res)
        return res

    def sh(self, argv, cwd=None, env=None, timeout=600, show=True) -> Res:
        start = time.monotonic()
        code, out, err = run_plain([str(a) for a in argv], cwd=cwd, env=env, timeout=timeout)
        res = Res([str(a) for a in argv], cwd, code, out, err, time.monotonic() - start, code == 124)
        if show:
            self.show(res, limit=12)
        return res

    def interrupt(self, running: Running, how: str) -> str:
        """sends `how` to a running command; returns what was sent."""
        pid = running.p.pid
        if how == "kill":
            running.p.kill()
            return "TerminateProcess" if IS_WIN else "SIGKILL"
        if how == "ctrl-c":
            if IS_WIN:
                code, out, err = run_plain([sys.executable, str(HERE / "hunt.py"), "ctrlc", str(pid)], timeout=60)
                if code:
                    raise RuntimeError(f"the ctrl-c helper failed: {code} {out} {err}")
                return "CTRL_C_EVENT to its console"
            os.kill(pid, signal.SIGINT)
            return "SIGINT"
        if how == "term":
            os.kill(pid, signal.SIGTERM)
            return "SIGTERM"
        if how == "hup":
            os.kill(pid, signal.SIGHUP)
            return "SIGHUP"
        raise ValueError(how)

    # -- fixtures and state --------------------------------------------------------------

    def fresh(self, *parts) -> Path:
        path = self.work.joinpath(*parts)
        rmtree(path)
        os.makedirs(long(path))
        return path

    def cache_dir(self, name) -> Path:
        return self.work / "caches" / name

    def url(self, name) -> str:
        return f"{self.base}/{name}"

    def zipit(self, name, members) -> None:
        with zipfile.ZipFile(self.www / name, "w", zipfile.ZIP_DEFLATED) as archive:
            for arc, data in members:
                info = zipfile.ZipInfo(arc, date_time=(2024, 1, 1, 0, 0, 0))
                info.external_attr = 0o644 << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                archive.writestr(info, data)
        self.zips[name] = members

    def make(self, path, size, seed) -> None:
        self.made[path] = self.site.add_made(path, size, seed)

    def made_sha(self, path) -> str:
        if path not in self.made_sums:
            self.made_sums[path] = self.made[path].sha256()
        return self.made_sums[path]

    def fixtures(self) -> None:
        self.zipit("lc.zip", [("README.txt", b"lifecycle readme\n"), ("tables/a.csv", csv(2000, "a")),
                              ("tables/b.csv", csv(3000, "b")), ("nested/deep/c.bin", blob(65536, "c")),
                              ("x.json", b'{"k": 1}\n')])
        (self.www / "plain.csv").write_bytes(csv(64, "plain"))
        self.zipit("case.zip", [("Data.csv", b"upper\n"), ("data.csv", b"lower\n"), ("ok.txt", b"ok\n")])
        self.zipit("nfd.zip", [("caf\u00e9.csv", b"nfc\n"), ("cafe\u0301.csv", b"nfd\n"), ("ok.txt", b"ok\n")])
        self.zipit("nfdonly.zip", [("cafe\u0301.csv", b"nfd only\n"), ("ok.txt", b"ok\n")])
        self.zipit("nfconly.zip", [("caf\u00e9.csv", b"nfc only\n"), ("ok.txt", b"ok\n")])
        self.zipit("resv.zip", [(name, f"{name}\n".encode()) for name in RESERVED] + [("ok.csv", b"ok\n")])
        self.zipit("many.zip", [(f"d{i // 100:02}/f{i:04}.txt", blob(512 + (i % 7) * 256, f"m{i}"))
                                for i in range(3000)] + [("top.txt", b"top\n")])
        self.zipit("quote.zip", [("x,1kb.csv", b"comma\n"), ("y.csv", b"plain\n")])
        self.zipit("long.zip", [(LONG_MEMBER, b"deep\n"), ("short.csv", b"short\n")])
        self.make("/big.bin", 192 * MiB, "big")
        self.make("/conc.bin", 96 * MiB, "conc")
        self.make("/mid.bin", 64 * MiB, "mid")

    def placed_ok(self, proj, dataset, zipname=None, files=None, readonly=None, label="placed"):
        """checks data/<dataset> holds exactly what the zip or files say, byte for byte."""
        want = placed_names(self.zips[zipname]) if zipname else files
        got = tree(Path(proj) / "data" / dataset)
        want_n = {nfc(k): v for k, v in want.items()}
        got_n = {nfc(k): v for k, v in got.items()}
        missing = sorted(set(want_n) - set(got_n))
        extra = sorted(set(got_n) - set(want_n))
        wrong = sorted(k for k in set(want_n) & set(got_n) if want_n[k] != got_n[k])
        ok = not missing and not extra and not wrong
        self.check(f"{label}-{dataset}-bytes", ok, f"data/{dataset} holds the {len(want)} pinned files byte for byte",
                   f"missing {missing[:5]}, extra {extra[:5]}, wrong bytes {wrong[:5]}")
        if readonly is not None and got:
            base = Path(proj) / "data" / dataset
            flags = [writable(base / rel) for rel in got]
            if readonly:
                self.check(f"{label}-{dataset}-readonly", not any(flags), "placed files are read-only by default",
                           f"{sum(flags)} of {len(flags)} writable", doc="docs/user/start/first-project.md")
            else:
                self.check(f"{label}-{dataset}-writable", all(flags), "copy = true gives writable files",
                           f"{len(flags) - sum(flags)} of {len(flags)} read-only",
                           doc="docs/user/concepts/cache-and-placement.md#placement")
        return ok

    def read(self, path):
        try:
            with open(long(path), "rb") as stream:
                return stream.read()
        except OSError:
            return None

    def cache_report(self, proj, env) -> dict:
        res = self.fl(["--json", "cache"], proj, env, show=False)
        try:
            return json.loads(res.out).get("result") or {}
        except ValueError:
            self.show(res)
            return {}

    def cache_objects(self, cache: Path) -> list:
        base = Path(cache) / "objects"
        found = []
        if base.is_dir():
            for folder in base.iterdir():
                if folder.is_dir() and len(folder.name) == 2:
                    found.extend(entry for entry in folder.iterdir() if entry.is_file())
        return found

    def partials(self, cache: Path) -> list:
        base = Path(cache) / "partial"
        return sorted(p for p in base.rglob("*") if p.is_file()) if base.is_dir() else []

    def set_copy(self, proj, dataset, value=True) -> None:
        path = Path(proj) / "data.toml"
        text = path.read_text(encoding="utf-8")
        text = re.sub(r"(?m)^copy = (true|false)\n", "", text)
        new = re.sub(rf"(?m)^\[datasets\.{dataset}\]\n", f"[datasets.{dataset}]\ncopy = {str(value).lower()}\n", text)
        if new == text:
            raise RuntimeError(f"no [datasets.{dataset}] in {path}")
        path.write_text(new, encoding="utf-8")

    def clone(self, source, *parts) -> Path:
        """a new project folder holding source's data.toml and data.lock, as a git clone has."""
        target = self.fresh(*parts)
        for name in ("data.toml", "data.lock"):
            shutil.copyfile(Path(source) / name, target / name)
        return target

    def same_file(self, a, b) -> bool:
        try:
            sa, sb = os.stat(long(a)), os.stat(long(b))
        except OSError:
            return False
        return (sa.st_dev, sa.st_ino) == (sb.st_dev, sb.st_ino)

    def placement_kind(self, proj, dataset, cache) -> str:
        """hardlink when a placed file is a cache object's own file, else copy or clone."""
        files = sorted(tree(Path(proj) / "data" / dataset))
        if not files:
            return "none"
        objects = {}
        for obj in self.cache_objects(cache):
            st = os.stat(obj)
            objects[(st.st_dev, st.st_ino)] = obj
        linked = 0
        for rel in files:
            st = os.stat(long(Path(proj) / "data" / dataset / rel))
            linked += (st.st_dev, st.st_ino) in objects
        if linked == len(files):
            return "hardlink"
        return "copy-or-clone" if linked == 0 else f"mixed ({linked} of {len(files)} linked)"

    # -- shared steps ----------------------------------------------------------------------

    def epilogue(self, proj, names, env, cache=None, label="end", sole=True) -> None:
        """status, path, remove and gc after a journey, each run twice."""
        proj = Path(proj)
        first = None
        for attempt in (1, 2):
            mark = self.site.mark()
            res = self.fl(["status", "--check"], proj, env)
            self.check(f"{label}-status-{attempt}", res.code == 0, "status --check exits 0 on a settled project",
                       f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/commands/status.md#exit-codes", res=res)
            self.check(f"{label}-status-{attempt}-offline", not self.site.since(mark),
                       "status asks no server", f"{len(self.site.since(mark))} requests",
                       doc="docs/user/commands/status.md")
            if first is None:
                first = res.out
            else:
                self.check(f"{label}-status-same", res.out == first, "a second status prints the same rows",
                           f"first {first[:200]!r}, second {res.out[:200]!r}")
        for name in names:
            for attempt in (1, 2):
                mark = self.site.mark()
                res = self.fl(["path", name], proj, env)
                printed = res.out.strip()
                self.check(f"{label}-path-{name}-{attempt}", res.code == 0 and printed and exists(printed),
                           f"`path {name}` prints a folder that exists", f"exit {res.code}, printed {printed!r}",
                           doc="docs/user/commands/path.md", res=res)
                self.check(f"{label}-path-{name}-{attempt}-offline", not self.site.since(mark),
                           "path on a ready dataset asks no server", f"{len(self.site.since(mark))} requests")
        before_toml = self.read(proj / "data.toml")
        mark = self.site.mark()
        res = self.fl(["remove", *names, "--yes"], proj, env)
        self.check(f"{label}-remove", res.code == 0, "remove exits 0", f"exit {res.code}: {last_lines(res.err)}",
                   doc="docs/user/commands/remove.md#exit-codes", res=res)
        self.check(f"{label}-remove-offline", not self.site.since(mark), "remove downloads nothing",
                   f"{len(self.site.since(mark))} requests", doc="docs/user/commands/remove.md")
        gone = [name for name in names if not exists(proj / "data" / name)]
        self.check(f"{label}-remove-files", len(gone) == len(names), "remove deletes each dataset's folder",
                   f"still there: {sorted(set(names) - set(gone))}", res=res)
        toml = (self.read(proj / "data.toml") or b"").decode("utf-8", "replace")
        left = [name for name in names if f"[datasets.{name}]" in toml]
        self.check(f"{label}-remove-toml", not left, "remove takes the entries out of data.toml",
                   f"still in data.toml: {left}; changed: {before_toml != self.read(proj / 'data.toml')}")
        res = self.fl(["remove", *names, "--yes"], proj, env)
        self.check(f"{label}-remove-again", res.code == 2, "a second remove is a usage error, exit 2, naming the dataset",
                   f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/commands/remove.md#exit-codes", res=res)
        for attempt in (1, 2):
            res = self.fl(["cache", "gc", "--now", "--yes"], proj, env)
            self.check(f"{label}-gc-{attempt}", res.code == 0, "cache gc --now exits 0",
                       f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/commands/cache.md", res=res)
        if cache is not None and sole:
            report = self.cache_report(proj, env)
            objects = self.cache_objects(cache)
            self.check(f"{label}-gc-empty", not objects and not report.get("unfinished_downloads"),
                       "after remove and gc --now, a cache only this journey used holds no objects or unfinished downloads",
                       f"{len(objects)} objects on disk, report objects={report.get('objects')} "
                       f"unfinished={report.get('unfinished_downloads')}")

    def interrupted(self, args, proj, env, path, how, rate=16 * MiB, fraction=0.5, min_secs=3.0, console=None):
        """starts a slowed download, stops it with `how` once it is past `fraction` and
        `min_secs`; returns (res, bytes sent before, what was sent) or raises Skip."""
        size = self.made[path].size
        self.site.pace(path, rate)
        mark = self.site.mark()
        use_console = IS_WIN and how == "ctrl-c" if console is None else console
        running = self.start(args, proj, env, console=use_console)
        begun = time.monotonic()
        reached = False
        try:
            while time.monotonic() - begun < 120:
                if running.poll() is not None:
                    break
                if self.site.sent_since(mark, path) >= fraction * size and time.monotonic() - begun >= min_secs:
                    reached = True
                    break
                time.sleep(0.05)
            sent_at = self.site.sent_since(mark, path)
            if not reached:
                res = running.wait(5)
                self.show(res)
                self.check(f"slowed-{how}", res.code in (None, 0), "the slowed download is still running when it is stopped",
                           f"it ended first: exit {res.code}: {last_lines(res.err)}", res=res)
                raise Skip(f"the download never got past {fraction:.0%} while running ({sent_at} bytes sent, "
                           f"exit {res.code}); nothing to interrupt")
            sent = self.interrupt(running, how)
            res = running.wait(90)
        finally:
            self.site.pace(path, None)
        self.show(res)
        self.say(f"  ({sent} after {sent_at / MiB:.1f} MiB of {size / MiB:.0f} MiB)")
        if res.timed_out:
            self.check(f"stop-{how}-ends", False, f"fetchloom ends within 90 s of a {sent}",
                       "it was still running and was killed", res=res)
        return res, self.site.sent_since(mark, path), sent

    def resumed(self, label, args, proj, env, path, sent_before, target, slack=32 * MiB) -> Res:
        """runs the same command again at full speed and checks it continued rather than
        starting over."""
        size = self.made[path].size
        mark = self.site.mark()
        res = self.fl(args, proj, env, timeout=600)
        asks = self.site.since(mark, path)
        sent = sum(entry["sent"] for entry in asks)
        starts = sorted({entry["start"] for entry in asks if entry["method"] == "GET"})
        self.check(f"{label}-rerun", res.code == 0, "the same command again exits 0", f"exit {res.code}: {last_lines(res.err)}",
                   doc="docs/user/concepts/questions-and-interruptions.md#interruptions", res=res)
        got = sha256_file(target) if exists(target) else None
        self.check(f"{label}-bytes", got == self.made_sha(path), "the finished file holds the pinned bytes",
                   f"sha256 {got}")
        self.check(f"{label}-ranged", any(start > 0 for start in starts),
                   "the rerun continues with a range request from where the stopped run got",
                   f"GET starts {starts}", doc="docs/user/concepts/questions-and-interruptions.md#interruptions")
        bound = size - sent_before + slack
        self.check(f"{label}-no-refetch", sent <= bound,
                   f"the rerun downloads at most what the stopped run lacked plus {slack // MiB} MiB",
                   f"stopped run got {sent_before / MiB:.1f} MiB, rerun downloaded {sent / MiB:.1f} MiB of "
                   f"{size / MiB:.0f} MiB (bound {bound / MiB:.1f} MiB), requests {[(e['range'], e['sent']) for e in asks][:8]}",
                   doc="docs/user/concepts/questions-and-interruptions.md#interruptions")
        self.check(f"{label}-no-restart-line", "Restarting" not in res.err,
                   "no `Restarting ... cannot be continued` line for a server that sends ranges",
                   last_lines(res.err), doc="docs/user/concepts/questions-and-interruptions.md#interruptions")
        return res

    def seed_pinned(self, label, path, dataset) -> Path:
        """a project whose data.lock pins `path` as `dataset`, made at full speed with its own cache."""
        proj = self.fresh(label, "seed")
        env = self.env(self.cache_dir(f"{label}-seed"))
        res = self.fl(["add", self.url(path.lstrip("/")), "--as", dataset], proj, env)
        if res.code != 0:
            raise RuntimeError(f"could not pin {path} for {label}: exit {res.code}")
        return proj

    # -- facts ---------------------------------------------------------------------------

    def fs(self, path) -> dict:
        path = Path(path)
        info = {"path": str(path)}
        try:
            if IS_WIN:
                import win
                info.update(win.volume_info(path))
            elif IS_MAC:
                import mac
                info.update(mac.fs_info(path))
        except Exception as err:  # noqa: BLE001
            info["error"] = repr(err)
        info.update(self.probe(path))
        return info

    def probe(self, path) -> dict:
        """what this folder's filesystem does with case, unicode spellings and links."""
        out = {}
        probe = Path(path) / f"osh-probe-{os.getpid()}"
        try:
            probe.mkdir(parents=True, exist_ok=True)
            (probe / "CaseProbe").write_bytes(b"x")
            out["case_insensitive"] = (probe / "caseprobe").exists()
            (probe / "caf\u00e9").write_bytes(b"x")
            out["unicode_insensitive"] = (probe / "cafe\u0301").exists()
            try:
                os.link(probe / "CaseProbe", probe / "linked")
                out["hardlinks"] = True
            except OSError as err:
                out["hardlinks"] = f"no: {err}"
        except OSError as err:
            out["probe_error"] = repr(err)
        finally:
            rmtree(probe)
        return out

    def gather_facts(self) -> None:
        facts = self.facts
        facts["image"] = self.image
        facts["image_os"] = os.environ.get("ImageOS")
        facts["image_version"] = os.environ.get("ImageVersion")
        facts["runner_arch"] = os.environ.get("RUNNER_ARCH")
        facts["platform"] = platform.platform()
        facts["machine"] = platform.machine()
        facts["python"] = sys.version
        facts["harness_commit"] = git("rev-parse", "HEAD", cwd=HERE)
        facts["product_commit"] = git("rev-parse", "HEAD", cwd=self.product)
        facts["binary"] = str(self.binary)
        facts["binary_sha256"] = sha256_file(self.binary)
        facts["binary_size"] = self.binary.stat().st_size
        code, out, _ = run_plain(["rustc", "-vV"], cwd=self.product)
        facts["rustc"] = out.strip()
        res = self.fl(["--version", "-v"], self.work, self.env())
        facts["version_v"] = res.text.strip()
        folders = {"work": self.work, "www": self.www, "home": Path.home(),
                   "workspace": os.environ.get("GITHUB_WORKSPACE"), "temp": os.environ.get("TEMP") or os.environ.get("TMPDIR"),
                   "runner_temp": os.environ.get("RUNNER_TEMP"), "product": self.product}
        facts["folders"] = {name: self.fs(path) for name, path in folders.items() if path}
        if IS_WIN:
            import win
            facts.update(win.facts(self))
        elif IS_MAC:
            import mac
            facts.update(mac.facts(self))
        (self.out / "facts.json").write_text(json.dumps(facts, indent=2, ensure_ascii=False), encoding="utf-8")
        for key in ("image", "image_version", "platform", "machine", "product_commit", "binary_sha256"):
            self.say(f"  fact {key}: {facts.get(key)}")
        for name, info in facts["folders"].items():
            self.say(f"  fact folder {name}: {json.dumps(info, ensure_ascii=False)}")

    # -- the run -------------------------------------------------------------------------

    @contextlib.contextmanager
    def journey(self, name):
        self.name = name
        started = time.monotonic()
        before = len(self.records)
        print(f"::group::journey {name}", flush=True)
        self.logfile.write(f"\n===== journey {name}\n")
        outcome = "done"
        try:
            yield
        except Skip as reason:
            self.skip("journey", str(reason))
            outcome = "skipped"
        except Exception:  # noqa: BLE001
            self.harness_errors += 1
            trace = traceback.format_exc()
            self.record("harness", "harness", "the journey runs to its end", trace)
            self.say(trace)
            print(f"::error title=harness {self.image} {name}::{trace.strip().splitlines()[-1][:500]}", flush=True)
            outcome = "harness error"
        finally:
            for cleanup in reversed(self.cleanups):
                try:
                    cleanup()
                except Exception as err:  # noqa: BLE001
                    self.say(f"  cleanup failed: {err!r}")
            self.cleanups = []
            print("::endgroup::", flush=True)
            mine = self.records[before:]
            counts = {status: sum(1 for r in mine if r["status"] == status)
                      for status in ("ok", "lead", "note", "skip", "harness")}
            self.journeys.append({"journey": name, "outcome": outcome, "seconds": round(time.monotonic() - started, 1),
                                  **counts})
            self.say(f"journey {name}: {outcome} in {time.monotonic() - started:.0f} s, {counts}")
            self.save()

    def save(self) -> None:
        data = {"image": self.image, "facts": self.facts, "journeys": self.journeys, "records": self.records,
                "harness_errors": self.harness_errors}
        (self.out / "results.json").write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")

    def summary(self) -> str:
        lines = [f"## os hunt on {self.image}", "",
                 f"product `{self.facts.get('product_commit')}`, binary sha256 `{self.facts.get('binary_sha256')}`, "
                 f"{self.facts.get('platform')}, image {self.facts.get('image_version')}", "",
                 "| journey | outcome | ok | leads | notes | skips | harness | s |", "|---|---|---|---|---|---|---|---|"]
        for j in self.journeys:
            lines.append(f"| {j['journey']} | {j['outcome']} | {j['ok']} | {j['lead']} | {j['note']} | {j['skip']} | "
                         f"{j['harness']} | {j['seconds']} |")
        for status in ("lead", "harness", "skip", "note"):
            chosen = [r for r in self.records if r["status"] == status]
            if not chosen:
                continue
            lines += ["", f"### {status}s", ""]
            for r in chosen:
                text = f"- `{r['journey']}/{r['id']}`: "
                if r["expect"]:
                    text += f"expected {r['expect']}; "
                text += f"observed {r['observed'][:400]}"
                if r.get("command"):
                    text += f" (`{r['command'][:200]}`, exit {r.get('exit')})"
                lines.append(text.replace("\n", " "))
        return "\n".join(lines) + "\n"


# -- journeys both systems run ---------------------------------------------------------------

def j_install(h: Hunt):
    """install from source as getting started says, then find fetchloom from a new shell."""
    env = h.env()
    # from the source folder, so rustup takes its pinned toolchain and cargo reuses the build;
    # gitinstall runs the command exactly as a new user types it
    res = h.sh(["cargo", "install", "--locked", "--path", h.product], cwd=h.product, env=env, timeout=3600)
    h.check("cargo-install", res.code == 0, "`cargo install --locked` of the source exits 0",
            f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/start/install.md", res=res)
    home = Path(os.environ.get("CARGO_HOME") or Path.home() / ".cargo")
    installed = home / "bin" / f"fetchloom{EXE}"
    h.check("installed-binary", installed.is_file(), "cargo put fetchloom in cargo's bin folder", f"{installed} is missing")
    if installed.is_file():
        h.check("installed-same", sha256_file(installed) == h.facts["binary_sha256"], "the installed program is the build",
                f"sha256 {sha256_file(installed)}", kind="note")
    sha = h.facts["product_commit"]
    res = h.fl(["--json", "--version"], h.work, env)
    try:
        about = json.loads(res.out)["result"]
    except (ValueError, KeyError, TypeError):
        about = {}
    h.check("version-commit", about.get("commit") == sha, "--version names the commit the build came from",
            f"{about}", doc="docs/user/commands/global-options.md")
    res = h.fl(["--version", "-v"], h.work, env)
    machine = platform.machine().lower()
    arch = "aarch64" if machine in ("arm64", "aarch64") else "x86_64"
    h.check("version-v", res.code == 0 and sha in res.text and (arch in res.text or machine in res.text.lower()),
            f"`--version -v` names the commit and the architecture ({arch})", res.text.strip()[:300], res=res)
    if IS_WIN:
        shells = [("cmd", ["cmd", "/d", "/c", "fetchloom --version -v"]),
                  ("powershell", ["powershell", "-NoProfile", "-NonInteractive", "-Command", "fetchloom --version -v"]),
                  ("pwsh", ["pwsh", "-NoProfile", "-NonInteractive", "-Command", "fetchloom --version -v"])]
    else:
        shells = [("zsh", ["zsh", "-lc", "command -v fetchloom; fetchloom --version -v"]),
                  ("bash", ["bash", "-lc", "command -v fetchloom; fetchloom --version -v"])]
    for name, argv in shells:
        res = h.sh(argv, cwd=h.work, env=env, timeout=120)
        if res.code == 127 and not res.text.strip():
            h.skip(f"new-{name}", f"{name} is not on this runner")
            continue
        h.check(f"new-{name}", res.code == 0 and sha in res.text, f"a new {name} finds fetchloom on PATH",
                f"exit {res.code}: {res.text.strip()[:300]}", doc="docs/user/start/install.md", res=res)
    if not IS_WIN:
        res = h.sh(["bash", "-lc", "command -v fl || true"], cwd=h.work, env=env)
        if res.out.strip():
            h.note("fl-taken", f"`fl` is already {res.out.strip()}, so the doc's link step is skipped")
        else:
            link = home / "bin" / "fl"
            if not os.path.lexists(link):
                os.symlink("fetchloom", link)
            res = h.sh(["bash", "-lc", "fl --version; fl nope-command 2>&1 | head -3; true"], cwd=h.work, env=env)
            h.check("fl-link", "fetchloom" in res.out and "fl nope" not in res.out,
                    "`fl`, the link getting started makes, runs fetchloom, and its messages say fetchloom",
                    res.out.strip()[:300], doc="docs/user/start/install.md", res=res)
            os.remove(link)


def j_init(h: Hunt):
    """the init a uv or git user types first, and commands outside any project."""
    folder = h.fresh("init", "empty")
    env = h.env(h.cache_dir("init"))
    res = h.fl(["init"], folder, env)
    h.check("init-usage", res.code == 2 and "no init needed" in res.err and "fetchloom add" in res.err,
            "`fetchloom init` is a usage error, exit 2, whose help says add starts a project",
            f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/commands/add.md", res=res)
    h.check("init-untouched", not os.listdir(folder), "init leaves the folder empty", os.listdir(folder))
    res = h.fl([], folder, env)
    h.check("bare-welcome", res.code == 0 and "fetchloom add" in res.text,
            "bare fetchloom outside a project prints the welcome with the quick start", f"exit {res.code}",
            doc="README.md quick start", res=res)
    for command in (["status"], ["sync"], ["path", "iris"], ["remove", "iris"]):
        res = h.fl(command, folder, env)
        h.check(f"{command[0]}-outside", res.code != 0 and res.err.strip() and not res.timed_out,
                f"`{command[0]}` outside a project fails with a message", f"exit {res.code}: {last_lines(res.err)}", res=res)
        if res.code != 0:
            h.note(f"{command[0]}-outside-says", f"exit {res.code}: {last_lines(res.err, 4)}", res=res)
    h.check("outside-untouched", not os.listdir(folder), "commands outside a project leave the folder empty",
            os.listdir(folder))


def j_lifecycle(h: Hunt):
    """add, sync, status, path, list, push and a restore from the mirror, each twice."""
    proj = h.fresh("lifecycle", "proj")
    cache = h.cache_dir("lifecycle")
    env = h.env(cache)
    url = h.url("lc.zip")
    res = h.fl(["add", url, "--as", "lc"], proj, env)
    h.check("add", res.code == 0 and "Added lc" in res.err, "add of a zip over http exits 0 and says `Added lc`",
            f"exit {res.code}: {last_lines(res.err)}", doc="README.md", res=res)
    h.placed_ok(proj, "lc", "lc.zip", readonly=True)
    h.note("placement", f"lc placed as {h.placement_kind(proj, 'lc', cache)}")
    for name in ("data.toml", "data.lock"):
        h.check(f"wrote-{name}", exists(proj / name), f"the first add writes {name}", os.listdir(proj),
                doc="docs/user/start/first-project.md")
    root_ignore = (h.read(proj / ".gitignore") or b"").decode("utf-8", "replace")
    h.check("root-gitignore", "/data/" in root_ignore,
            "the first add outside git puts /data/ in the project's .gitignore, \"in a git repository or not\"",
            f"project holds {sorted(os.listdir(proj))}; .gitignore {root_ignore!r}",
            doc="docs/user/start/first-project.md; README.md; docs/user/reference/project-folder.md says only inside git")
    marker = (h.read(proj / "data" / "lc" / ".gitignore") or b"").decode("utf-8", "replace")
    h.note("dataset-marker", f"data/lc/.gitignore holds {marker[:200]!r}")
    toml, lock = h.read(proj / "data.toml"), h.read(proj / "data.lock")
    mark = h.site.mark()
    res = h.fl(["add", url, "--as", "lc"], proj, env)
    h.check("add-again-state", h.read(proj / "data.toml") == toml and h.read(proj / "data.lock") == lock,
            "the same add again leaves data.toml and data.lock as they were", "they changed", res=res)
    h.check("add-again-exit", res.code == 0, "the same add again exits 0, as a duplicate add",
            f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/reference/json-output.md", res=res, kind="note")
    sent = h.site.sent_since(mark, "/lc.zip")
    h.check("add-again-no-download", sent < (h.www / "lc.zip").stat().st_size,
            "the same add again does not download the zip again", f"{sent} bytes of lc.zip sent", kind="note")
    h.placed_ok(proj, "lc", "lc.zip", label="add-again")
    res = h.fl(["add", h.url("plain.csv"), "--as", "plain"], proj, env)
    h.check("add-plain", res.code == 0, "add of a plain csv exits 0", f"exit {res.code}", res=res)
    plain = {"plain.csv": (len(h.read(h.www / "plain.csv")), sha256_file(h.www / "plain.csv"))}
    h.placed_ok(proj, "plain", files=plain, readonly=True)
    for attempt in (1, 2):
        mark = h.site.mark()
        res = h.fl(["sync"], proj, env)
        h.check(f"sync-{attempt}", res.code == 0, "sync exits 0", f"exit {res.code}", res=res)
        h.check(f"sync-{attempt}-settled", "nothing to do" in res.err,
                "a sync of a settled project says `Ok all pinned files are in place by size and time, nothing to do`",
                last_lines(res.err), doc="docs/user/commands/sync.md", res=res)
        h.check(f"sync-{attempt}-offline", not h.site.since(mark), "a settled sync asks no server",
                f"{len(h.site.since(mark))} requests: {[e['path'] for e in h.site.since(mark)][:5]}")
    res = h.fl(["path", "lc/tables/a.csv"], proj, env)
    want = os.path.normcase(os.path.realpath(proj / "data" / "lc" / "tables" / "a.csv"))
    got = os.path.normcase(os.path.realpath(res.out.strip())) if res.out.strip() else ""
    h.check("path-file", res.code == 0 and got == want, "`path lc/tables/a.csv` prints that file's absolute path",
            f"exit {res.code}, printed {res.out.strip()!r}", doc="docs/user/commands/path.md", res=res)
    res = h.fl(["path", "lc/tables/nope.csv"], proj, env)
    h.check("path-missing", res.code == 2 and "fetchloom path" in res.err and "lc/" in res.err,
            "a path the dataset lacks is a usage error naming paths it holds", f"exit {res.code}: {last_lines(res.err)}",
            doc="docs/user/commands/path.md", res=res)
    res = h.fl(["list"], proj, env)
    h.check("list", res.code == 0 and "lc" in res.out and "plain" in res.out, "list shows both datasets",
            res.out.strip()[:300], res=res)
    edit_through_link(h, proj, env, cache)
    mirror = h.work / "lifecycle" / "mirror"
    for attempt in (1, 2):
        res = h.fl(["push", "--to", mirror, "--yes"], proj, env)
        h.check(f"push-{attempt}", res.code == 0, "push to a folder exits 0", f"exit {res.code}: {last_lines(res.err)}",
                doc="docs/user/commands/push.md", res=res)
        if attempt == 2:
            h.check("push-again-skips", "Skipped" in res.err and "Pushed" not in res.err,
                    "a second push skips the files already there", last_lines(res.err),
                    doc="docs/user/commands/push.md", res=res)
    h.check("mirror-marker", exists(mirror / "fetchloom-mirror"), "the mirror has its fetchloom-mirror file",
            os.listdir(mirror) if exists(mirror) else "no mirror")
    clone = h.clone(proj, "lifecycle", "clone")
    clone_env = h.env(h.cache_dir("lifecycle-clone"))
    mark = h.site.mark()
    res = h.fl(["--offline", "sync", "--mirror", mirror], clone, clone_env)
    h.check("restore-from-mirror", res.code == 0, "an offline sync from the folder mirror restores a clone",
            f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/commands/push.md", res=res)
    h.placed_ok(clone, "lc", "lc.zip", label="restore")
    h.placed_ok(clone, "plain", files=plain, label="restore")
    h.check("restore-offline", not h.site.since(mark), "the offline restore asks no server",
            f"{len(h.site.since(mark))} requests")
    twin = h.fresh("lifecycle", "twin")
    mark = h.site.mark()
    res = h.fl(["add", url, "--as", "lc"], twin, env)
    h.check("twin-add", res.code == 0, "the same dataset in a second project adds", f"exit {res.code}", res=res)
    sent = h.site.sent_since(mark, "/lc.zip")
    h.check("twin-no-download", sent < (h.www / "lc.zip").stat().st_size,
            "the second project takes the bytes from the cache, not a second download", f"{sent} bytes of lc.zip sent",
            doc="README.md: one shared cache keeps each file once")
    report = h.cache_report(proj, env)
    unique = {digest for _, digest in placed_names(h.zips["lc.zip"]).values()} | {plain["plain.csv"][1]}
    stored = len(h.cache_objects(cache))
    h.check("stored-once", stored == len(unique) and report.get("objects") == len(unique),
            f"the cache holds each of the {len(unique)} distinct files once for two projects",
            f"{stored} objects on disk, `fetchloom cache` says {report.get('objects')} objects, {report.get('bytes')} bytes",
            doc="issue #33/#34: the same dataset in two projects, bytes stored once")
    projects = [os.path.normcase(os.path.realpath(p)) for p in report.get("projects") or []]
    h.check("cache-projects", os.path.normcase(os.path.realpath(proj)) in projects and
            os.path.normcase(os.path.realpath(twin)) in projects, "`fetchloom cache` lists both projects",
            report.get("projects"))
    h.note("twin-placement", f"twin placed as {h.placement_kind(twin, 'lc', cache)}; cache says linked={report.get('linked')} "
           f"copies={report.get('copies')} cloned={report.get('cloned')}")
    res = h.fl(["cache"], proj, env)
    h.check("cache-human", res.code == 0 and "Cache" in res.err, "`fetchloom cache` reports the cache", last_lines(res.err),
            res=res)
    h.epilogue(proj, ["lc", "plain"], env, cache, sole=False)
    h.epilogue(clone, ["lc", "plain"], clone_env, h.cache_dir("lifecycle-clone"), label="clone-end")
    h.epilogue(twin, ["lc"], env, cache, label="twin-end")


def edit_through_link(h: Hunt, proj, env, cache) -> None:
    """an edit that keeps the size, the case every disk's listing is slowest to show: status
    must see it, and sync must put the pinned bytes back and keep the edit."""
    target = proj / "data" / "lc" / "x.json"
    original = h.read(target)
    linked = any(h.same_file(target, obj) for obj in h.cache_objects(cache))
    make_writable(target)
    with open(long(target), "r+b") as stream:
        stream.write(b'{"k": 2}\n')
    res = h.fl(["status"], proj, env)
    h.check("edit-seen", "x.json" in res.out and res.code == 0,
            "status lists a same-size edit made right after placing" + (" through a hardlink" if linked else ""),
            f"rows {res.out.strip()[:300]!r}", doc="docs/user/commands/status.md; docs/user/guides/windows.md", res=res)
    res = h.fl(["sync"], proj, env)
    h.check("edit-sync", res.code == 0, "sync after the edit exits 0", f"exit {res.code}: {last_lines(res.err)}", res=res)
    now = h.read(target)
    kept = proj / "data" / "lc" / "x.json.changed"
    if linked:
        h.check("edit-put-back", now == original and h.read(kept) == b'{"k": 2}\n',
                "sync puts the pinned bytes back and keeps the edit as x.json.changed",
                f"x.json now {now!r}, x.json.changed {h.read(kept)!r}", doc="docs/user/commands/sync.md#local-changes", res=res)
        force_remove(kept)
    else:
        h.check("edit-kept", now == b'{"k": 2}\n', "sync keeps an edit of a file of the project's own",
                f"x.json now {now!r}", doc="docs/user/commands/sync.md#local-changes", res=res)
        res = h.fl(["sync", "--force"], proj, env)
        h.check("edit-force", res.code == 0 and h.read(target) == original, "sync --force puts the pinned bytes back",
                f"exit {res.code}, x.json now {h.read(target)!r}", res=res)
    for stray in Path(proj / "data" / "lc").glob(".fetchloom-*.old"):
        h.note("edit-old", f"sync kept {stray.name}")
    res = h.fl(["status", "--check"], proj, env)
    h.check("edit-settled", res.code == 0, "status --check is clean once the edit is dealt with",
            f"exit {res.code}: {res.out.strip()[:300]}", res=res)


def j_resume(h: Hunt):
    """a big download stopped by ctrl-c, a kill, or a dropped connection, then the same
    command again: it should go on from where it stopped."""
    url = h.url("big.bin")
    proj = h.fresh("resume", "add")
    cache = h.cache_dir("resume-add")
    env = h.env(cache)
    args = ["add", url, "--as", "big"]
    res, sent, how = h.interrupted(args, proj, env, "/big.bin", "ctrl-c")
    h.check("add-ctrl-c-exit", res.code == 130, f"a first add stopped by ctrl-c ({how}) exits 130", f"exit {res.code}",
            doc="docs/user/commands/add.md#stopping-an-add", res=res)
    h.check("add-ctrl-c-said", "Stopping" in res.err and "nothing was added" in res.err,
            "it says `Stopping` and ends with `Stopped, and nothing was added`", last_lines(res.err, 4),
            doc="docs/user/commands/add.md#stopping-an-add", res=res)
    leftovers = sorted(os.listdir(proj))
    h.check("add-ctrl-c-folder", not leftovers, "a stopped first add leaves its folder as it was",
            f"left {leftovers}", doc="docs/user/commands/add.md#editing-during-an-add")
    kept = h.partials(cache)
    h.note("add-ctrl-c-partial", f"cache partial/ holds {[(p.name, p.stat().st_size) for p in kept]}")
    h.resumed("add-ctrl-c", args, proj, env, "/big.bin", sent, proj / "data" / "big" / "big.bin")
    toml = (h.read(proj / "data.toml") or b"").decode()
    h.check("add-once", toml.count("[datasets.big]") == 1 and "big-2" not in toml,
            "the add again adds `big` once, not big-2", toml[:300])
    h.check("partial-cleared", not h.partials(cache), "a finished download leaves nothing under partial/",
            [p.name for p in h.partials(cache)], kind="note")
    variants = [("sync-ctrl-c", "ctrl-c"), ("sync-kill", "kill")]
    if not IS_WIN:
        variants += [("sync-term", "term"), ("sync-hup", "hup")]
    for label, signal_name in variants:
        clone = h.clone(proj, "resume", label)
        ccache = h.cache_dir(f"resume-{label}")
        cenv = h.env(ccache)
        try:
            res, sent, how = h.interrupted(["sync"], clone, cenv, "/big.bin", signal_name)
        except Skip as reason:
            h.skip(label, str(reason))
            continue
        if signal_name == "ctrl-c":
            h.check(f"{label}-exit", res.code == 130 and "Stopped" in res.err,
                    f"a sync stopped by ctrl-c ({how}) exits 130 with a `Stopped` line",
                    f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/concepts/questions-and-interruptions.md",
                    res=res)
        elif signal_name == "term":
            h.check(f"{label}-exit", res.code == 130 and "SIGTERM" in res.err,
                    "a SIGTERM stops a sync as ctrl-c does: exit 130 and a `Stopped by a SIGTERM` line",
                    f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/concepts/questions-and-interruptions.md",
                    res=res)
        else:
            h.note(f"{label}-exit", f"after {how}: exit {res.code}; stderr {last_lines(res.err)!r}", res=res)
        h.note(f"{label}-partial", f"partial/ holds {[(p.name, p.stat().st_size) for p in h.partials(ccache)]}")
        h.check(f"{label}-lock-kept", h.read(clone / "data.lock") == h.read(proj / "data.lock"),
                "the stopped sync leaves data.lock as it was", "data.lock changed")
        h.resumed(label, ["sync"], clone, cenv, "/big.bin", sent, clone / "data" / "big" / "big.bin")
        res = h.fl(["status", "--check"], clone, cenv)
        h.check(f"{label}-settled", res.code == 0, "status --check is clean after the resumed sync",
                f"exit {res.code}: {res.out.strip()[:200]}", res=res)
    clone = h.clone(proj, "resume", "cut")
    ccache = h.cache_dir("resume-cut")
    cenv = h.env(ccache)
    size = h.made["/big.bin"].size
    h.site.cut_once("/big.bin", 24 * MiB)
    mark = h.site.mark()
    res = h.fl(["sync"], clone, cenv, timeout=600)
    first_sent = h.site.sent_since(mark, "/big.bin")
    h.site.cut_once("/big.bin", None)
    asks = h.site.since(mark, "/big.bin")
    cut = [e for e in asks if e["cut"] is not None and not e["done"]]
    if not cut:
        h.note("cut-missed", f"no answer was cut: {[(e['range'], e['sent'], e['cut']) for e in asks][:8]}")
    elif res.code != 0:
        h.note("cut-first", f"a connection cut after 24 MiB ended the sync: exit {res.code}, {last_lines(res.err)}", res=res)
        res = h.resumed("cut", ["sync"], clone, cenv, "/big.bin", first_sent, clone / "data" / "big" / "big.bin")
    else:
        h.check("cut-retried", any(e["start"] > 0 for e in asks), "after a connection cut the sync carries on with a range",
                [(e["range"], e["sent"], e["cut"]) for e in asks][:8], res=res)
        h.check("cut-no-refetch", first_sent <= size + 32 * MiB, "the cut costs no second whole download",
                f"{first_sent / MiB:.1f} MiB sent for a {size / MiB:.0f} MiB file")
        got = sha256_file(clone / "data" / "big" / "big.bin")
        h.check("cut-bytes", got == h.made_sha("/big.bin"), "the file holds the pinned bytes", got)
    h.epilogue(proj, ["big"], env, cache)


def j_clash(h: Hunt):
    """two names one case-insensitive or unicode-insensitive disk takes for one file."""
    for zipname, names, shown in (("case.zip", ["Data.csv", "data.csv"], ["Data.csv", "data.csv"]),
                                  ("nfd.zip", ["caf\u00e9.csv", "cafe\u0301.csv"], ["caf\u00e9.csv", "cafe\\u{301}"])):
        label = zipname.split(".")[0]
        proj = h.fresh("clash", label)
        env = h.env(h.cache_dir(f"clash-{label}"))
        res = h.fl(["add", h.url(zipname), "--as", label], proj, env)
        h.check(f"{label}-refused", res.code != 0, f"an archive holding {names} is refused on every system",
                f"exit {res.code}", doc="docs/user/guides/macos.md#names; docs/user/reference/data-toml.md", res=res)
        if label == "case":
            named = all(shown_name in res.err for shown_name in shown)
        else:
            named = "caf\u00e9.csv" in res.err and ("unicode" in res.err or "\\u{301}" in res.err)
        h.check(f"{label}-names-both", named, f"the refusal names both paths ({shown}) or the name and why",
                last_lines(res.err, 5), doc="docs/user/guides/macos.md#names", res=res)
        h.check(f"{label}-help", "--no-extract" in res.err, "its help names --no-extract", last_lines(res.err, 3),
                doc="docs/user/reference/data-toml.md#destination-paths", res=res)
        h.check(f"{label}-help-exclude", "--exclude" in res.err or "members" in res.err,
                "its help also says how to leave one file out (--exclude, or a members table)",
                last_lines(res.err, 2), doc="docs/user/reference/data-toml.md#destination-paths: "
                "its help names --exclude and --no-extract; macos.md#names: suggests how to leave one out", res=res)
        left = sorted(os.listdir(proj))
        h.check(f"{label}-undone", not left, "the refused first add leaves its folder as it was", left,
                doc="docs/user/commands/add.md")
        res = h.fl(["add", h.url(zipname), "--as", label, "--no-extract"], proj, env)
        h.check(f"{label}-no-extract", res.code == 0 and exists(proj / "data" / label / zipname),
                "the add with --no-extract keeps the archive packed and succeeds", f"exit {res.code}", res=res)
        if res.code == 0:
            h.epilogue(proj, [label], env, h.cache_dir(f"clash-{label}"), label=f"{label}-end")
    proj = h.fresh("clash", "nfdonly")
    cache = h.cache_dir("clash-nfdonly")
    env = h.env(cache)
    res = h.fl(["add", h.url("nfdonly.zip"), "--as", "nfdonly"], proj, env)
    h.check("nfdonly-add", res.code == 0, "an archive with one decomposed name adds", f"exit {res.code}", res=res)
    h.placed_ok(proj, "nfdonly", "nfdonly.zip")
    for typed, spelling in (("cafe\u0301.csv", "nfd"), ("caf\u00e9.csv", "nfc")):
        res = h.fl(["path", f"nfdonly/{typed}"], proj, env)
        printed = res.out.strip()
        verdict = f"exit {res.code}, printed {printed!r}, exists {exists(printed) if printed else False}"
        if spelling == "nfc":
            h.check("path-nfc", res.code == 0 and exists(printed), "path finds the file typed composed, as a keyboard types it",
                    verdict, res=res)
        elif IS_MAC:
            h.check("path-nfd", res.code == 0 and exists(printed),
                    "on macos, where both spellings name one file, path finds the file typed as the archive spells it",
                    f"{verdict}; stderr {last_lines(res.err, 2)!r}", doc="docs/user/guides/macos.md#names", res=res)
        else:
            h.note("path-nfd", f"typed decomposed, as the archive spells it: {verdict}; stderr {last_lines(res.err, 2)!r}",
                   res=res)
    h.epilogue(proj, ["nfdonly"], env, cache, label="nfdonly-end")
    imp = h.fresh("clash", "import")
    folder = imp / "pair"
    folder.mkdir()
    (folder / "caf\u00e9.csv").write_bytes(b"nfc\n")
    try:
        (folder / "cafe\u0301.csv").write_bytes(b"nfd\n")
    except OSError as err:
        h.skip("import-pair", f"this disk holds no second spelling: {err}")
        return
    if len(os.listdir(folder)) < 2:
        h.skip("import-pair", "this disk keeps one name for both spellings, so no folder can hold the pair")
        return
    proj = imp / "proj"
    proj.mkdir()
    res = h.fl(["add", "../pair"], proj, h.env(h.cache_dir("clash-import")))
    h.check("import-pair", res.code == 10 and res.err.strip(),
            "importing a folder holding both spellings of one name stops with source.invalid, exit 10",
            f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/commands/add.md#files-on-this-machine", res=res)


def j_reserved(h: Hunt):
    """names windows and fat disks cannot store, inside a dataset."""
    proj = h.fresh("reserved", "proj")
    cache = h.cache_dir("reserved")
    env = h.env(cache)
    res = h.fl(["add", h.url("resv.zip"), "--as", "resv"], proj, env)
    named = [name for name in RESERVED if name.strip() in res.err]
    if IS_WIN:
        h.check("refused", res.code != 0, "windows refuses such names by name before anything is written",
                f"exit {res.code}: {last_lines(res.err, 4)}", doc="docs/user/reference/data-toml.md#destination-paths",
                res=res)
        h.check("named", len(named) >= 1, "the refusal names the file by name", f"named {named}", res=res)
        strays = [rel for rel in tree(proj / "data" / "resv")] if exists(proj / "data" / "resv") else []
        h.check("nothing-written", not strays, "no file of the dataset was written", strays[:10])
        h.check("all-named", len(named) == len(RESERVED) or re.search(r"\d+ (more|files|names)", res.err) is not None,
                f"the refusal tells there are {len(RESERVED)} such names, so one exclude round fixes them all",
                f"named {len(named)} of {len(RESERVED)}: {named}; stderr {last_lines(res.err, 3)}", res=res)
        h.check("undone", not os.listdir(proj), "the refused first add leaves its folder as it was", os.listdir(proj))
        res = h.fl(["add", h.url("resv.zip"), "--as", "resv", "--no-extract"], proj, env)
        h.check("no-extract", res.code == 0, "the archive kept packed adds", f"exit {res.code}", res=res)
        if res.code == 0:
            h.epilogue(proj, ["resv"], env, cache)
        return
    h.check("placed", res.code == 0, "macos places such names as they are", f"exit {res.code}: {last_lines(res.err)}",
            doc="docs/user/reference/data-toml.md#destination-paths", res=res)
    h.check("warned", "warning" in res.err and len(named) >= 3, "with one warning naming them",
            last_lines(res.err, 4), doc="docs/user/reference/data-toml.md#destination-paths", res=res)
    if res.code == 0:
        h.placed_ok(proj, "resv", "resv.zip")
        h.epilogue(proj, ["resv"], env, cache)


def j_unicode_paths(h: Hunt):
    """a project in a folder with a space and an accent, reached by other spellings of its path."""
    proj = h.fresh("unicode", "Datos Econ\u00f3micos", "proyecto")
    cache = h.cache_dir("unicode")
    env = h.env(cache)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, env)
    h.check("add", res.code == 0, "add works in a folder named `Datos Econ\u00f3micos`", f"exit {res.code}", res=res)
    h.placed_ok(proj, "lc", "lc.zip")
    res = h.fl(["path", "lc/tables/a.csv"], proj, env)
    printed = res.out.strip()
    h.check("path", res.code == 0 and exists(printed) and "Econ\u00f3micos" in nfc(printed),
            "path prints the accented path, readable as utf-8", f"{printed!r}", res=res)
    spellings = []
    upper = proj.parent.parent / "DATOS ECON\u00d3MICOS" / "PROYECTO"
    spellings.append(("case", upper))
    if IS_MAC:
        spellings.append(("nfd", Path(unicodedata.normalize("NFD", str(proj)))))
    if IS_WIN:
        import win
        short = win.short_path(proj)
        if short and short.lower() != str(proj).lower():
            spellings.append(("8dot3", Path(short)))
        else:
            h.note("8dot3", f"no short name for {proj}: {short!r}")
    for label, spelled in spellings:
        if not os.path.isdir(spelled):
            h.note(f"{label}-spelling", f"{spelled} does not reach the project on this disk")
            continue
        res = h.fl(["-C", spelled, "status", "--check"], h.work, env)
        h.check(f"{label}-status", res.code == 0, f"status through the {label} spelling of the path is clean",
                f"exit {res.code}: {res.out.strip()[:200]} {last_lines(res.err)}", res=res)
        res = h.fl(["-C", spelled, "sync"], h.work, env)
        h.check(f"{label}-sync", res.code == 0 and "nothing to do" in res.err,
                f"sync through the {label} spelling finds nothing to do", last_lines(res.err), res=res)
    report = h.cache_report(proj, env)

    def key(path):
        return nfc(os.path.normcase(os.path.realpath(path))).lower()
    seen = [p for p in report.get("projects") or [] if key(p) == key(proj)]
    h.check("one-project", len(seen) == 1, "the cache registers the folder once, whichever spelling reached it",
            report.get("projects"), doc="docs/user/commands/cache.md#inspect")
    h.epilogue(proj, ["lc"], env, cache)


def j_many(h: Hunt):
    """a dataset of a few thousand files; on macos with `ulimit -n 256`, its default limit."""
    nofile = 256 if IS_MAC else None
    proj = h.fresh("many", "proj")
    cache = h.cache_dir("many")
    env = h.env(cache)
    res = h.fl(["add", h.url("many.zip"), "--as", "many"], proj, env, nofile=nofile, timeout=1200)
    limit = f" under ulimit -n {nofile}" if nofile else ""
    h.check("add", res.code == 0, f"a 3001-file archive adds{limit}", f"exit {res.code}: {last_lines(res.err)}", res=res)
    h.placed_ok(proj, "many", "many.zip")
    victims = [rel for rel in sorted(tree(proj / "data" / "many")) if rel != "top.txt"]
    for rel in victims:
        target = proj / "data" / "many" / rel
        make_writable(target)
        os.remove(long(target))
    res, first = h.closed_stdout(["status"], proj, env, nofile=nofile)
    h.show(res)
    bad = [word for word in ("panicked", "Broken pipe", "os error", "error:") if word in res.err]
    h.check("closed-stdout", not bad and not res.timed_out,
            "status writing 3000 rows to a reader that took one line and went away ends quietly",
            f"exit {res.code}, stderr has {bad}: {last_lines(res.err)}; first row {first[:80]!r}",
            doc="docs/user/reference/json-output.md: a reader that closes its pipe early is no failure", res=res)
    h.note("closed-stdout-exit", f"exit {res.code}")
    res = h.fl(["status"], proj, env, nofile=nofile, show=False)
    rows = [line for line in res.out.splitlines() if "Missing" in line]
    h.check("missing-rows", len(rows) == len(victims),
            f"status lists each of the {len(victims)} deleted files on a row of its own in a pipe",
            f"{len(rows)} Missing rows; stderr {last_lines(res.err)}", doc="docs/user/commands/status.md")
    res = h.fl(["sync"], proj, env, nofile=nofile, timeout=1200)
    h.check("sync-puts-back", res.code == 0, f"sync puts the deleted files back{limit}", f"exit {res.code}", res=res)
    h.placed_ok(proj, "many", "many.zip", label="put-back")
    bulk = h.work / "many" / "bulk"
    for i in range(3000):
        target = bulk / f"g{i // 100:02}" / f"b{i:04}.txt"
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(blob(256 + i % 500, f"b{i}"))
    res = h.fl(["add", "../bulk"], proj, env, nofile=nofile, timeout=1200)
    h.check("import", res.code == 0, f"importing a folder of 3000 files works{limit}", f"exit {res.code}: {last_lines(res.err)}",
            res=res)
    for attempt in (1, 2):
        res = h.fl(["status", "--check"], proj, env, nofile=nofile)
        h.check(f"status-{attempt}", res.code == 0, f"status --check is clean{limit}", f"exit {res.code}", res=res)
    res = h.fl(["remove", "many", "bulk", "--yes"], proj, env, nofile=nofile, timeout=1200)
    h.check("remove", res.code == 0 and not exists(proj / "data" / "many") and not exists(proj / "data" / "bulk"),
            f"remove deletes 6000 files{limit}", f"exit {res.code}: {last_lines(res.err)}", res=res)
    res = h.fl(["cache", "gc", "--now", "--yes"], proj, env, nofile=nofile, timeout=1200)
    h.check("gc", res.code == 0 and not h.cache_objects(cache), f"gc --now frees them all{limit}",
            f"exit {res.code}, {len(h.cache_objects(cache))} objects left: {last_lines(res.err)}", res=res)


def j_concurrency(h: Hunt):
    """two fetchloom runs on one project, and two projects on one cache, at once."""
    seed = h.seed_pinned("conc", "/conc.bin", "conc")
    one = h.clone(seed, "conc", "one")
    cache = h.cache_dir("conc-one")
    env = h.env(cache)
    h.site.pace("/conc.bin", 12 * MiB)
    first = h.start(["sync"], one, env)
    time.sleep(1.5)
    second = h.start(["sync"], one, env)
    a, b = first.wait(300), second.wait(300)
    h.site.pace("/conc.bin", None)
    h.show(a)
    h.show(b)
    told = re.search(r"another fetchloom|already running|waiting|in use", b.err + a.err, re.I)
    h.check("same-project", (a.code == 0 and b.code == 0) or (0 in (a.code, b.code) and told),
            "two syncs of one project at once both end well, or the second says another run holds the project",
            f"exits {a.code} and {b.code}: {last_lines(b.err)}", res=b)
    h.note("same-project-second-says", last_lines(b.err, 5), res=b)
    got = sha256_file(one / "data" / "conc" / "conc.bin") if exists(one / "data" / "conc" / "conc.bin") else None
    h.check("same-project-bytes", got == h.made_sha("/conc.bin"), "the file holds the pinned bytes", got)
    res = h.fl(["status", "--check"], one, env)
    h.check("same-project-settled", res.code == 0, "status --check is clean afterwards", f"exit {res.code}", res=res)
    p1, p2 = h.clone(seed, "conc", "p1"), h.clone(seed, "conc", "p2")
    shared = h.cache_dir("conc-shared")
    senv = h.env(shared)
    h.site.pace("/conc.bin", 12 * MiB)
    mark = h.site.mark()
    r1, r2 = h.start(["sync"], p1, senv), h.start(["sync"], p2, senv)
    a, b = r1.wait(300), r2.wait(300)
    h.site.pace("/conc.bin", None)
    h.show(a)
    h.show(b)
    h.check("shared-cache", a.code == 0 and b.code == 0, "two projects syncing one file into one cache at once both end well",
            f"exits {a.code} and {b.code}", res=b)
    sent = h.site.sent_since(mark, "/conc.bin")
    size = h.made["/conc.bin"].size
    h.check("shared-cache-once", sent <= size * 1.25, "the file is downloaded about once for both",
            f"{sent / MiB:.1f} MiB sent for a {size / MiB:.0f} MiB file", kind="note")
    h.check("shared-cache-objects", len(h.cache_objects(shared)) == 1, "the shared cache holds the file once",
            f"{len(h.cache_objects(shared))} objects")
    h.epilogue(p1, ["conc"], senv, shared, sole=False, label="p1-end")
    h.epilogue(p2, ["conc"], senv, shared, label="p2-end")


def j_quickstart(h: Hunt):
    """the readme's quick start and getting started, word for word, with the real network."""
    base = Path.home() / "osh-quick"
    rmtree(base)
    measurements = base / "measurements"
    (measurements / "sub").mkdir(parents=True)
    files = {"a.csv": csv(20, "qa"), "b.csv": csv(30, "qb"), "sub/c.csv": csv(5, "qc")}
    for rel, data in files.items():
        (measurements / rel).write_bytes(data)
    proj = base / "iris-study"
    proj.mkdir()
    env = h.env()
    cache = None
    res = h.fl(["add", "https://huggingface.co/datasets/scikit-learn/iris"], proj, env, timeout=300)
    network = res.code == 0
    if res.code in (10, 20):
        h.note("network", f"hugging face did not answer as hoped (exit {res.code}): {last_lines(res.err)}", res=res)
    else:
        h.check("add-hf", res.code == 0 and "Added iris" in res.err, "the getting-started add ends with `Added iris`",
                f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/start/first-project.md", res=res)
    if network:
        csv_path = proj / "data" / "iris" / "Iris.csv"
        h.check("iris-file", exists(csv_path) and not writable(csv_path), "data/iris/Iris.csv is there, read-only",
                f"exists {exists(csv_path)}", doc="docs/user/start/first-project.md")
        res = h.fl(["path", "iris/Iris.csv"], proj, env)
        h.check("path", res.code == 0 and exists(res.out.strip()), "`fetchloom path iris/Iris.csv` prints it",
                res.out.strip(), res=res)
        cache = Path(h.cache_report(proj, env).get("path") or "")
        h.note("default-cache", f"default cache {cache}, iris placed as {h.placement_kind(proj, 'iris', cache)}")
    res = h.fl(["add", "../measurements"], proj, env)
    h.check("import", res.code == 0 and "Imported measurements" in res.err, "`add ../measurements` imports the folder",
            f"exit {res.code}: {last_lines(res.err)}", doc="README.md quick start", res=res)
    h.placed_ok(proj, "measurements", files={k: (len(v), hashlib.sha256(v).hexdigest()) for k, v in files.items()})
    res = h.fl(["add", "../measurements"], proj, env)
    h.check("import-again", res.code == 0 and "already in data.toml" in res.err,
            "the same import again says it is already in data.toml", last_lines(res.err),
            doc="docs/user/commands/add.md#files-on-this-machine", res=res)
    res = h.fl([], proj, env)
    h.check("bare-status", res.code == 0, "bare fetchloom in a project shows its status", f"exit {res.code}", res=res)
    clone = base / "iris-clone"
    clone.mkdir()
    for name in ("data.toml", "data.lock"):
        shutil.copyfile(proj / name, clone / name)
    res = h.fl(["sync"], clone, env, timeout=300)
    h.check("clone-sync", res.code == 0, "a clone runs `fetchloom sync` and gets the same files",
            f"exit {res.code}: {last_lines(res.err)}", doc="README.md quick start", res=res)
    names = ["iris", "measurements"] if network else ["measurements"]
    h.epilogue(clone, names, env, label="clone-end")
    h.epilogue(proj, names, env)
    search = base / "search-try"
    search.mkdir()
    res = h.fl(["add", "iris"], search, env, timeout=300)
    h.note("search-no-terminal", f"`fetchloom add iris` with no terminal: exit {res.code}, {last_lines(res.err, 5)}",
           res=res, doc="docs/user/commands/search.md#choosing-a-dataset")
    h.check("search-untouched", not os.listdir(search) or res.code == 0,
            "a search add that cannot pick leaves the folder as it was", os.listdir(search))
    rmtree(base)


COMMON = [("install", j_install), ("init", j_init), ("lifecycle", j_lifecycle), ("resume", j_resume),
          ("clash", j_clash), ("reserved", j_reserved), ("unicode", j_unicode_paths), ("many", j_many),
          ("concurrency", j_concurrency), ("quickstart", j_quickstart)]


def cmd_run(args) -> int:
    if IS_WIN:
        import ctypes
        ctypes.windll.kernel32.SetConsoleCtrlHandler(None, False)
    h = Hunt(args)
    with h.journey("setup"):
        h.check("binary", h.binary.is_file(), "the release build is there", str(h.binary))
        h.fixtures()
        h.gather_facts()
        selftest(h)
    if h.harness_errors:
        h.server.shutdown()
        return 1
    journeys = list(COMMON)
    if IS_WIN:
        import win
        journeys += win.JOURNEYS
    elif IS_MAC:
        import mac
        journeys += mac.JOURNEYS
    only = set(filter(None, (args.only or "").split(",")))
    for name, function in journeys:
        if only and name not in only:
            continue
        with h.journey(name):
            function(h)
    h.save()
    text = h.summary()
    (h.out / "summary.md").write_text(text, encoding="utf-8")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(text)
    print(text)
    h.server.shutdown()
    return 1 if h.harness_errors else 0


def selftest(h: Hunt) -> None:
    """the server answers ranges, if-range, cuts and pacing as the journeys assume."""
    import urllib.error
    import urllib.request
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def get(path, headers=None):
        request = urllib.request.Request(h.base + path, headers=headers or {})
        with opener.open(request, timeout=30) as answer:
            return answer.status, dict(answer.headers), answer.read()
    status, headers, body = get("/mid.bin", {"Range": "bytes=100-199"})
    assert status == 206 and len(body) == 100 and headers["Content-Range"] == f"bytes 100-199/{64 * MiB}", headers
    assert body == h.made["/mid.bin"].read(100, 100)
    tag = headers["ETag"]
    status, _, body = get("/mid.bin", {"Range": f"bytes={64 * MiB - 10}-", "If-Range": tag})
    assert status == 206 and len(body) == 10
    status, _, body = get("/plain.csv", {"Range": "bytes=0-9", "If-Range": '"other"'})
    assert status == 200 and len(body) == len(h.read(h.www / "plain.csv"))
    _, plain_headers, _ = get("/plain.csv")
    try:
        get("/plain.csv", {"If-None-Match": plain_headers["ETag"]})
        unchanged = None
    except urllib.error.HTTPError as answer:
        unchanged = answer.code
    assert unchanged == 304, f"a matching If-None-Match got {unchanged}"
    h.site.cut_once("/plain.csv", 100)
    try:
        get("/plain.csv")
        cut = False
    except Exception:  # noqa: BLE001
        cut = True
    assert cut, "a cut answer must fail the client"
    h.site.pace("/mid.bin", 4 * MiB)
    began = time.monotonic()
    get("/mid.bin", {"Range": f"bytes=0-{4 * MiB - 1}"})
    took = time.monotonic() - began
    h.site.pace("/mid.bin", None)
    assert 0.6 < took < 5, f"4 MiB at 4 MiB/s took {took:.2f} s"
    h.check("server-selftest", True, "the test server answers ranges, if-range, cuts and pacing", "")


def cmd_product(args) -> int:
    into = Path(args.into).resolve()
    wanted = os.environ.get("OSH_PRODUCT") or git("merge-base", "HEAD", "origin/main")
    sha = git("rev-parse", f"{wanted}^{{commit}}")
    head = git("rev-parse", "HEAD")
    changed = [p for p in git("diff", "--name-only", sha, "HEAD").splitlines() if p]
    outside = [p for p in changed if not (p.startswith(".oshunt/") or p.startswith(".github/workflows/"))]
    print(f"commit under test {sha}; this branch {head}; it changes {len(changed)} files, "
          f"{len(outside)} outside .oshunt/ and .github/workflows/")
    if outside:
        print(f"::warning::the branch changes product files the build will not see: {outside[:10]}")
    rmtree(into)
    git("worktree", "add", "--detach", str(into), sha)
    if os.environ.get("GITHUB_ENV"):
        with open(os.environ["GITHUB_ENV"], "a", encoding="utf-8") as stream:
            stream.write(f"OSH_PRODUCT_SHA={sha}\n")
    return 0


def cmd_gitinstall(args) -> int:
    """`cargo install --locked --git`, the command a new user types, for the commit under test."""
    out = Path(args.out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    sha = git("rev-parse", "HEAD", cwd=Path(args.product).resolve())
    root = Path(os.environ.get("RUNNER_TEMP") or out.parent) / "osh-gitinstall"
    rmtree(root)
    log = []
    code, stable, err = run_plain(["rustup", "update", "stable", "--no-self-update"], timeout=900)
    log.append(f"rustup update stable: exit {code}\n{stable[-2000:]}{err[-2000:]}")
    started = time.monotonic()
    argv = ["cargo", "+stable", "install", "--locked", "--git", REPO, "--rev", sha, "--root", str(root)]
    code, out_text, err_text = run_plain(argv, cwd=str(out), timeout=3600)
    took = time.monotonic() - started
    log.append(f"$ {' '.join(argv)}\nexit {code} in {took:.0f} s\n{err_text[-6000:]}")
    binary = root / "bin" / f"fetchloom{EXE}"
    version = run_plain([str(binary), "--version", "-v"]) if binary.exists() else (127, "", "no binary")
    log.append(f"$ fetchloom --version -v\nexit {version[0]}\n{version[1]}{version[2]}")
    ok = code == 0 and version[0] == 0 and sha in (version[1] + version[2])
    result = {"command": " ".join(argv), "exit": code, "seconds": round(took), "version": (version[1] + version[2]).strip(),
              "ok": ok}
    (out / "gitinstall.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    (out / "gitinstall.log").write_text("\n\n".join(log), encoding="utf-8")
    print("\n\n".join(log))
    text = (f"\n### install from git\n\n`{result['command']}`: exit {code} in {round(took)} s; "
            f"`fetchloom --version -v`: {result['version'][:300]!r}; commit matches: {ok}\n")
    if os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as stream:
            stream.write(text)
    if not ok:
        print(f"::warning title=lead gitinstall::{text.strip()[:600]}")
    return 0


def main() -> int:
    for stream in (sys.stdout, sys.stderr):
        if hasattr(stream, "reconfigure"):
            stream.reconfigure(encoding="utf-8", errors="replace")
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    product = sub.add_parser("product")
    product.add_argument("--into", required=True)
    run = sub.add_parser("run")
    run.add_argument("--product", required=True)
    run.add_argument("--image", required=True)
    run.add_argument("--out", required=True)
    run.add_argument("--only")
    gitinstall = sub.add_parser("gitinstall")
    gitinstall.add_argument("--product", required=True)
    gitinstall.add_argument("--out", required=True)
    ctrlc = sub.add_parser("ctrlc")
    ctrlc.add_argument("pid", type=int)
    hold = sub.add_parser("hold")
    hold.add_argument("path")
    hold.add_argument("share", type=int)
    hold.add_argument("ready")
    hold.add_argument("release")
    args = parser.parse_args()
    if args.command == "product":
        return cmd_product(args)
    if args.command == "run":
        return cmd_run(args)
    if args.command == "gitinstall":
        return cmd_gitinstall(args)
    import win
    if args.command == "ctrlc":
        return win.send_ctrl_c(args.pid)
    return win.hold(args.path, args.share, args.ready, args.release)


if __name__ == "__main__":
    sys.exit(main())
