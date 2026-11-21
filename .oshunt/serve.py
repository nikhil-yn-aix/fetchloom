"""a small http server for the os hunt.

it serves files from a folder, or bytes made on the fly for a big file, over http/1.1 with a
strong etag, ranges, if-range and 304 answers to conditional requests, as a real file server
does. a file can be slowed to a total rate across all its connections, or cut once after some
bytes, and every request is logged, so a journey can tell a resumed download from one that
started over.
"""
from __future__ import annotations

import hashlib
import http.server
import socket
import threading
import time
import urllib.parse
from pathlib import Path

CHUNK = 256 * 1024
LAST_MODIFIED = "Wed, 01 Jan 2025 00:00:00 GMT"


class Made:
    """size bytes made from seed on the fly: a repeating 1 MiB block."""

    def __init__(self, size: int, seed: str):
        self.size = size
        self.seed = seed
        self.block = hashlib.shake_256(seed.encode()).digest(1 << 20)

    def read(self, start: int, length: int) -> bytes:
        out = bytearray()
        position = start
        end = start + length
        while position < end:
            offset = position % len(self.block)
            take = min(len(self.block) - offset, end - position)
            out += self.block[offset:offset + take]
            position += take
        return bytes(out)

    def sha256(self) -> str:
        digest = hashlib.sha256()
        position = 0
        while position < self.size:
            take = min(8 << 20, self.size - position)
            digest.update(self.read(position, take))
            position += take
        return digest.hexdigest()


class Site:
    """what every request shares: the files, their pacing, their cuts and the log."""

    def __init__(self, root: Path):
        self.root = root
        self.made: dict = {}
        self.rate: dict = {}
        self.cut: dict = {}
        self.free_at: dict = {}
        self.sent: dict = {}
        self.log: list = []
        self.lock = threading.Lock()

    def add_made(self, path: str, size: int, seed: str) -> Made:
        made = Made(size, seed)
        self.made[path] = made
        return made

    def pace(self, path: str, rate) -> None:
        """limits path to rate bytes a second in total, or lifts the limit for None."""
        with self.lock:
            if rate:
                self.rate[path] = float(rate)
            else:
                self.rate.pop(path, None)
            self.free_at.pop(path, None)

    def cut_once(self, path: str, after) -> None:
        """the next answer for path closes its connection after `after` body bytes."""
        with self.lock:
            if after is None:
                self.cut.pop(path, None)
            else:
                self.cut[path] = int(after)

    def mark(self) -> int:
        """a position in the log, so a journey can look at the requests after it."""
        with self.lock:
            return len(self.log)

    def since(self, mark: int, path=None) -> list:
        with self.lock:
            entries = [dict(entry) for entry in self.log[mark:]]
        return [entry for entry in entries if path is None or entry["path"] == path]

    def sent_since(self, mark: int, path=None) -> int:
        return sum(entry["sent"] for entry in self.since(mark, path))

    def total_sent(self, path: str) -> int:
        with self.lock:
            return self.sent.get(path, 0)

    def wait_pace(self, path: str, length: int) -> None:
        """holds a chunk until its turn at the file's rate. a sleep that overshoots, as on a
        runner that coalesces timers, is made up by sending the next chunks at once, so the
        average rate holds; more than a second behind starts the schedule afresh."""
        with self.lock:
            rate = self.rate.get(path)
            if not rate:
                return
            now = time.monotonic()
            start = self.free_at.get(path)
            if start is None or start < now - 1.0:
                start = now
            due = start + length / rate
            self.free_at[path] = due
        delay = due - time.monotonic()
        if delay > 0:
            time.sleep(delay)

    def count(self, path: str, entry: dict, length: int) -> None:
        with self.lock:
            self.sent[path] = self.sent.get(path, 0) + length
            entry["sent"] += length

    def take_cut(self, path: str):
        with self.lock:
            return self.cut.pop(path, None)


def parse_range(header, size: int):
    """(start, end) of a single `bytes=a-b`, `bytes=a-` or `bytes=-n` range, end exclusive,
    "unsatisfiable", or None for no usable range."""
    if not header or not header.strip().lower().startswith("bytes="):
        return None
    spec = header.strip()[6:]
    if "," in spec:
        return None
    first, _, last = spec.partition("-")
    try:
        if first == "":
            length = int(last)
            if length <= 0:
                return None
            return max(0, size - length), size
        start = int(first)
        end = size if last == "" else min(int(last) + 1, size)
    except ValueError:
        return None
    if start >= size or start >= end:
        return "unsatisfiable"
    return start, end


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    disable_nagle_algorithm = True
    site = None

    def log_message(self, *_):
        pass

    def do_HEAD(self):
        self.answer(body=False)

    def do_GET(self):
        self.answer(body=True)

    def resolve(self, path: str):
        made = self.site.made.get(path)
        if made is not None:
            return made.size, f'"m-{made.seed}-{made.size}"', made
        relative = path.lstrip("/")
        if ".." in relative.split("/"):
            return None
        file = self.site.root / relative
        if not file.is_file():
            return None
        info = file.stat()
        tag = hashlib.sha256(f"{relative}:{info.st_size}:{info.st_mtime_ns}".encode()).hexdigest()[:16]
        return info.st_size, f'"f-{tag}"', file

    def answer(self, body: bool):
        path = urllib.parse.unquote(self.path.split("?", 1)[0])
        entry = {
            "t": time.time(), "method": self.command, "path": path,
            "range": self.headers.get("Range"), "if_range": self.headers.get("If-Range"),
            "if_none_match": self.headers.get("If-None-Match"),
            "if_modified_since": self.headers.get("If-Modified-Since"),
            "status": None, "start": 0, "end": 0, "sent": 0, "done": False, "cut": None,
        }
        with self.site.lock:
            self.site.log.append(entry)
        found = self.resolve(path)
        if found is None:
            entry["status"] = 404
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()
            entry["done"] = True
            return
        size, etag, source = found
        tags = [tag.strip() for tag in (self.headers.get("If-None-Match") or "").split(",") if tag.strip()]
        since = self.headers.get("If-Modified-Since")
        if (tags and (etag in tags or "*" in tags)) or (not tags and since and since.strip() == LAST_MODIFIED):
            entry["status"] = 304
            self.send_response(304)
            self.send_header("ETag", etag)
            self.send_header("Last-Modified", LAST_MODIFIED)
            self.end_headers()
            entry["done"] = True
            return
        asked = parse_range(self.headers.get("Range"), size)
        if_range = self.headers.get("If-Range")
        if asked is not None and if_range is not None and if_range.strip() not in (etag, LAST_MODIFIED):
            asked = None
        if asked == "unsatisfiable":
            entry["status"] = 416
            self.send_response(416)
            self.send_header("Content-Range", f"bytes */{size}")
            self.send_header("Content-Length", "0")
            self.end_headers()
            entry["done"] = True
            return
        start, end = asked if asked else (0, size)
        entry.update(start=start, end=end, status=206 if asked else 200)
        self.send_response(entry["status"])
        self.send_header("Content-Type", "application/zip" if path.endswith(".zip") else "application/octet-stream")
        self.send_header("Content-Length", str(end - start))
        self.send_header("ETag", etag)
        self.send_header("Last-Modified", LAST_MODIFIED)
        self.send_header("Accept-Ranges", "bytes")
        if asked:
            self.send_header("Content-Range", f"bytes {start}-{end - 1}/{size}")
        self.end_headers()
        if not body:
            entry["done"] = True
            return
        cut = self.site.take_cut(path)
        entry["cut"] = cut
        stream = None
        try:
            if not isinstance(source, Made):
                stream = source.open("rb")
                stream.seek(start)
            position = start
            while position < end:
                take = min(CHUNK, end - position)
                if cut is not None:
                    left = cut - entry["sent"]
                    if left <= 0:
                        self.drop()
                        return
                    take = min(take, left)
                self.site.wait_pace(path, take)
                data = source.read(position, take) if stream is None else stream.read(take)
                if not data:
                    break
                self.wfile.write(data)
                self.site.count(path, entry, len(data))
                position += len(data)
            entry["done"] = position >= end
        except OSError:
            self.close_connection = True
        finally:
            if stream is not None:
                stream.close()

    def drop(self):
        """ends the connection mid-body, as a network that fails does."""
        self.close_connection = True
        try:
            self.wfile.flush()
            self.connection.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 256


def start(root: Path):
    """serves root on a free port of 127.0.0.1 from a background thread; returns the server,
    its site and its base url."""
    site = Site(root)
    handler = type("BoundHandler", (Handler,), {"site": site})
    server = Server(("127.0.0.1", 0), handler)
    thread = threading.Thread(target=server.serve_forever, kwargs={"poll_interval": 0.1}, daemon=True)
    thread.start()
    return server, site, f"http://127.0.0.1:{server.server_address[1]}"
