"""the os hunt's macos journeys and helpers: disk images, apfs clones, exfat, quarantine, synced
folders, a real terminal, locales and system-protected folders."""
from __future__ import annotations

import fcntl
import os
import plistlib
import re
import shutil
import signal
import struct
import termios
import threading
import time
from pathlib import Path

from hunt import (MiB, Skip, edit_through_link, exists, last_lines, run_plain, sha256_file, tree,
                  writable)

ANSI = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\r")


def fs_info(path) -> dict:
    code, out, _ = run_plain(["df", "-P", str(path)])
    lines = out.strip().splitlines()
    if code or len(lines) < 2:
        return {"error": "df failed"}
    parts = lines[-1].split(None, 5)
    device, mount = parts[0], parts[5] if len(parts) > 5 else None
    info = {"device": device, "mount": mount}
    code, out, _ = run_plain(["diskutil", "info", "-plist", mount or str(path)], text=False)
    if code == 0:
        try:
            data = plistlib.loads(out)
            for key in ("FilesystemType", "FilesystemName", "FilesystemUserVisibleName", "VolumeName", "Writable",
                        "Internal", "Encryption", "FileVault"):
                if key in data:
                    info[key] = data[key]
        except Exception as err:  # noqa: BLE001
            info["diskutil_error"] = repr(err)
    code, out, _ = run_plain(["mount"])
    for line in out.splitlines():
        if mount and f" on {mount} (" in line:
            info["mount_line"] = line
    return info


def facts(h) -> dict:
    out = {}
    for name, argv in (("sw_vers", ["sw_vers"]), ("uname", ["uname", "-a"]),
                       ("model", ["sysctl", "-n", "hw.model", "machdep.cpu.brand_string", "hw.memsize"]),
                       ("maxfiles", ["launchctl", "limit", "maxfiles"]), ("csrutil", ["csrutil", "status"]),
                       ("spctl", ["spctl", "--status"]), ("xcode_select", ["xcode-select", "-p"]),
                       ("ulimit_n", ["sh", "-c", "ulimit -n; ulimit -Hn"])):
        code, text, err = run_plain(argv)
        out[name] = " ".join((text + err).split())[:400]
    return out


def attach(h, name: str, filesystem: str, size: str, volume: str, readonly_image=None):
    """a disk image of `filesystem` mounted without finder; returns (mount, image)."""
    image = readonly_image or h.work / f"{name}.dmg"
    mount = h.work / f"{name}-mnt"
    mount.mkdir(parents=True, exist_ok=True)
    if readonly_image is None:
        res = h.sh(["hdiutil", "create", "-quiet", "-size", size, "-fs", filesystem, "-volname", volume, image], timeout=300)
        if res.code:
            raise Skip(f"hdiutil could not make a {filesystem} image: {last_lines(res.err)}")
    argv = ["hdiutil", "attach", "-nobrowse", "-mountpoint", mount, image]
    if readonly_image is not None:
        argv.insert(2, "-readonly")
    res = h.sh(argv, timeout=300)
    if res.code:
        raise Skip(f"hdiutil could not attach the {filesystem} image: {last_lines(res.err)}")
    h.cleanups.append(lambda: detach(h, mount))
    h.sh(["sudo", "-n", "mdutil", "-i", "off", mount], timeout=60, show=False)
    return mount, image


def detach(h, mount) -> None:
    for attempt in range(3):
        if h.sh(["hdiutil", "detach", mount], timeout=120, show=False).code == 0:
            return
        time.sleep(2)
    h.sh(["hdiutil", "detach", "-force", mount], timeout=120)


def j_clones(h):
    """copy = true on apfs: writable clones that take no extra space."""
    h.make("/clone.bin", 128 * MiB, "clone")
    size = h.made["/clone.bin"].size
    proj = h.fresh("clones", "proj")
    cache = h.cache_dir("clones")
    env = h.env(cache)
    res = h.fl(["add", h.url("clone.bin"), "--as", "cl"], proj, env)
    h.check("add", res.code == 0, "add exits 0", f"exit {res.code}", res=res)
    report = h.cache_report(proj, env)
    h.note("placement", f"placed as {h.placement_kind(proj, 'cl', cache)}; cache cloned={report.get('cloned')} "
           f"copies={report.get('copies')} linked={report.get('linked')}; stderr {last_lines(res.err)}")
    free = shutil.disk_usage(proj).free
    h.set_copy(proj, "cl", True)
    res = h.fl(["sync"], proj, env)
    h.check("copy-sync", res.code == 0, "copy = true then sync exits 0", f"exit {res.code}", res=res)
    target = proj / "data" / "cl" / "clone.bin"
    h.check("writable", exists(target) and writable(target), "copy = true gives a writable file", "read-only",
            doc="docs/user/guides/macos.md#clones-and-links")
    used = free - shutil.disk_usage(proj).free
    report = h.cache_report(proj, env)
    cloned = report.get("cloned") or 0
    h.check("no-extra-space", used < 32 * MiB or cloned >= size * 0.9,
            "the writable copies are clones that take no extra space on apfs",
            f"free space fell {used / MiB:.1f} MiB for a {size / MiB:.0f} MiB file; cache says cloned={cloned} "
            f"copies={report.get('copies')}", doc="issue #34; docs/user/guides/macos.md#clones-and-links")
    obj = [o for o in h.cache_objects(cache) if o.stat().st_size == size]
    with open(target, "r+b") as stream:
        stream.write(b"edited")
    h.check("clone-independent", obj and sha256_file(obj[0]) == h.made_sha("/clone.bin"),
            "editing the clone leaves the cache's copy as pinned", "the cache's copy changed")
    res = h.fl(["status"], proj, env)
    h.check("edit-seen", "clone.bin" in res.out, "status lists the edited clone", res.out[:200], res=res)
    h.set_copy(proj, "cl", False)
    h.fl(["sync", "--force"], proj, env)
    h.epilogue(proj, ["cl"], env, cache)


def j_exfat(h):
    """a project on an exfat disk image, as an external drive is: copies, times, names, a mirror."""
    mount, image = attach(h, "exfat", "ExFAT", "1g", "OSHX")
    h.note("volume", f"{fs_info(mount)}")
    probe = mount / "mtime-probe"
    probe.write_bytes(b"x")
    os.utime(probe, ns=(1_700_000_000_123_456_789, 1_700_000_001_123_456_789))
    h.note("mtime-resolution", f"set mtime ...001.123456789 s, read back {os.stat(probe).st_mtime_ns} ns")
    probe.unlink()
    proj = mount / "p"
    proj.mkdir()
    cache = h.cache_dir("exfat")
    env = h.env(cache)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, env)
    h.check("add", res.code == 0, "add onto exfat exits 0", f"exit {res.code}: {last_lines(res.err)}", res=res)
    h.placed_ok(proj, "lc", "lc.zip")
    kind = h.placement_kind(proj, "lc", cache)
    h.check("copied", kind != "hardlink", "every file on exfat is copied", kind, doc="docs/user/guides/macos.md#clones-and-links")
    h.note("writable", f"{sum(writable(proj / 'data' / 'lc' / r) for r in tree(proj / 'data' / 'lc'))} of "
           f"{len(tree(proj / 'data' / 'lc'))} placed files writable (the guide says exfat keeps no permissions)")
    edit_through_link(h, proj, env, cache)
    (proj / "data" / "lc" / ".DS_Store").write_bytes(b"finder")
    (proj / "data" / "lc" / "._x.json").write_bytes(b"appledouble")
    res = h.fl(["status", "--check"], proj, env)
    h.check("system-files", res.code == 0 and ".DS_Store" not in res.out and "._x.json" not in res.out,
            "finder's .DS_Store and ._ files are no difference", f"exit {res.code}: {res.out[:300]}",
            doc="docs/user/commands/status.md#categories", res=res)
    res = h.fl(["add", h.url("nfconly.zip"), "--as", "nfc"], proj, env)
    h.check("nfc-add", res.code == 0, "an archive with a composed name adds onto exfat", f"exit {res.code}", res=res)
    listed = [name for name in os.listdir(proj / "data" / "nfc")] if exists(proj / "data" / "nfc") else []
    h.note("nfc-listed", f"exfat lists {[ascii(n) for n in listed]}")
    for attempt in (1, 2):
        res = h.fl(["status", "--check"], proj, env)
        h.check(f"nfc-status-{attempt}", res.code == 0 and "Missing" not in res.out and "Extra" not in res.out,
                "status does not report a name exfat lists decomposed as missing and extra",
                f"exit {res.code}: {res.out[:300]}", doc="docs/user/guides/macos.md#names", res=res)
    res = h.fl(["sync"], proj, env)
    h.check("nfc-sync", res.code == 0 and "nothing to do" in res.err, "sync does not place it again",
            last_lines(res.err), doc="docs/user/guides/macos.md#names", res=res)
    res = h.fl(["add", h.url("resv.zip"), "--as", "resv"], proj, env)
    h.check("reserved-taken", res.code == 0 and "cannot unpack the archive that holds them" in res.err,
            "macos's exfat takes every name windows cannot store, and the warning keeps the archive packed for windows",
            f"exit {res.code}: {last_lines(res.err, 4)}", doc="docs/user/reference/data-toml.md#destination-paths", res=res)
    toml = (h.read(proj / "data.toml") or b"").decode()
    h.note("reserved-state", f"exit {res.code}; data.toml has resv: {'[datasets.resv]' in toml}")
    if res.code == 0:
        h.placed_ok(proj, "resv", "resv.zip", label="exfat")
        h.note("reserved-listed", f"exfat lists {[ascii(n) for n in sorted(os.listdir(proj / 'data' / 'resv'))]}")
        res = h.fl(["status", "--check"], proj, env)
        h.note("reserved-status", f"status --check exit {res.code}: {res.out[:300]}", res=res)
    if "[datasets.resv]" in toml:
        h.fl(["remove", "resv", "--yes"], proj, env)
    mirror = mount / "mirror"
    for attempt in (1, 2):
        res = h.fl(["push", "--to", mirror, "--yes"], proj, env)
        h.check(f"push-{attempt}", res.code == 0, "push to an exfat folder exits 0", f"exit {res.code}: {last_lines(res.err)}",
                res=res)
    clone = h.clone(proj, "exfat", "clone")
    cenv = h.env(h.cache_dir("exfat-clone"))
    res = h.fl(["--offline", "sync", "--mirror", mirror], clone, cenv)
    h.check("restore", res.code == 0, "an offline sync from the exfat mirror restores a clone on apfs", f"exit {res.code}",
            res=res)
    h.epilogue(proj, ["lc", "nfc"], env, cache, sole=False)
    h.check("folder-gone", not exists(proj / "data" / "lc"),
            "a dataset folder left holding only .DS_Store and ._ files goes with them", sorted(os.listdir(proj / "data"))
            if exists(proj / "data") else "", doc="docs/user/commands/remove.md")
    h.epilogue(clone, ["lc", "nfc"], cenv, h.cache_dir("exfat-clone"), label="clone-end")
    p2 = mount / "p2"
    p2.mkdir()
    cache2 = mount / "cache"
    env2 = h.env(cache2)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], p2, env2)
    h.check("cache-on-exfat", res.code == 0, "a cache on exfat works", f"exit {res.code}: {last_lines(res.err)}", res=res)
    h.placed_ok(p2, "lc", "lc.zip", label="exfat-cache")
    res = h.fl(["cache", "verify"], p2, env2)
    h.check("verify-on-exfat", res.code == 0, "cache verify on exfat exits 0", f"exit {res.code}: {last_lines(res.err)}", res=res)
    p3 = mount / "p3"
    p3.mkdir()
    env3 = h.env(h.cache_dir("exfat-ro"))
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], p3, env3)
    h.epilogue(p2, ["lc"], env2, cache2, label="exfat-cache-end")
    detach(h, mount)
    mount, _ = attach(h, "exfat", "ExFAT", "1g", "OSHX", readonly_image=image)
    p3 = mount / "p3"
    for command, expect_ok in ((["status", "--check"], True), (["sync"], True), (["path", "lc"], True),
                               (["add", h.url("plain.csv"), "--as", "plain"], False), (["remove", "lc", "--yes"], False)):
        res = h.fl(command, p3, env3)
        if expect_ok:
            h.check(f"readonly-{command[0]}", res.code == 0, f"`{command[0]}` works on a read-only mount of a settled project",
                    f"exit {res.code}: {last_lines(res.err)}", res=res)
        else:
            h.check(f"readonly-{command[0]}", res.code == 50 and res.err.strip() and not res.timed_out,
                    f"`{command[0]}` on a read-only mount fails with local.io and a message", f"exit {res.code}: {last_lines(res.err)}",
                    doc="docs/user/reference/exit-codes.md", res=res)


def j_quarantine(h):
    """the quarantine mark a browser puts on a download, on the binary itself."""
    folder = h.fresh("quarantine")
    marked = folder / "fetchloom"
    shutil.copy2(h.binary, marked)
    mark = f"0083;{int(time.time()):x};Safari;F643CD5F-6071-46AB-83AB-390BA944DEC5"
    res = h.sh(["xattr", "-w", "com.apple.quarantine", mark, marked])
    h.check("marked", res.code == 0, "the quarantine mark goes on, as safari writes it", f"exit {res.code}", res=res)
    res = h.sh(["xattr", "-p", "com.apple.quarantine", marked])
    h.note("mark", res.out.strip())
    for argv in (["spctl", "--status"], ["spctl", "-a", "-vv", "-t", "execute", marked], ["codesign", "-dv", marked]):
        res = h.sh(argv, timeout=120)
        h.note(argv[0] + ("-assess" if "-a" in argv else ""), f"exit {res.code}: {' '.join(res.text.split())[:300]}")
    res = h.fl(["--version"], folder, h.env(), binary=marked, timeout=120)
    h.note("quarantined-run", f"the marked binary ran from a shell: exit {res.code}, {res.text.strip()[:200]!r}", res=res,
           doc="docs/user/start/install.md#macos")
    res = h.sh(["xattr", "-d", "com.apple.quarantine", marked])
    res = h.fl(["--version"], folder, h.env(), binary=marked)
    h.check("cleared-run", res.code == 0, "after `xattr -d com.apple.quarantine fetchloom` it runs",
            f"exit {res.code}", doc="docs/user/start/install.md#macos", res=res)
    proj = h.fresh("quarantine", "proj")
    cache = h.cache_dir("quarantine")
    env = h.env(cache)
    res = h.fl(["add", h.url("plain.csv"), "--as", "plain"], proj, env)
    marks = []
    for path in [proj / "data" / "plain" / "plain.csv", *h.cache_objects(cache)]:
        code, text, _ = run_plain(["xattr", "-l", str(path)])
        marks.append(f"{path.name}: {' '.join(text.split())[:120] or 'none'}")
    h.check("downloads-unmarked", not any("quarantine" in m for m in marks),
            "fetchloom puts no quarantine mark on what it downloads", marks, kind="note")
    h.epilogue(proj, ["plain"], env, cache)


def j_synced(h):
    """icloud drive and onedrive folders on a disk that would otherwise hardlink; HOME names an
    hfs+ image, since apfs clones would hide whether links were skipped."""
    mount, _ = attach(h, "hfs", "HFS+", "512m", "OSHH")
    home = mount / "home"
    cache = mount / "cache"
    env = h.env(cache, HOME=home)
    control = mount / "plain" / "proj"
    control.mkdir(parents=True)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], control, env)
    control_kind = h.placement_kind(control, "lc", cache)
    h.note("control", f"outside synced folders: exit {res.code}, placed as {control_kind}")
    if control_kind != "hardlink":
        h.note("control-not-linked", "the control did not hardlink, so the synced folders below cannot show a difference")
    for label, folder, client in (
            ("icloud", home / "Library" / "Mobile Documents" / "com~apple~CloudDocs" / "proj", "icloud"),
            ("onedrive", home / "Library" / "CloudStorage" / "OneDrive-Contoso" / "proj", "onedrive")):
        folder.mkdir(parents=True)
        res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], folder, env)
        kind = h.placement_kind(folder, "lc", cache)
        h.check(f"{label}-no-hardlinks", res.code == 0 and kind != "hardlink", f"a project in {label} gets no hardlinks",
                f"exit {res.code}, placed as {kind}, control {control_kind}",
                doc="docs/user/guides/macos.md#icloud-drive-and-cloud-storage", res=res)
        h.check(f"{label}-said", client in res.err.lower(), f"the add says {label} breaks links", last_lines(res.err, 4),
                doc="docs/user/guides/windows.md#onedrive-and-synced-folders", res=res, kind="note")
        h.epilogue(folder, ["lc"], env, cache, sole=False, label=f"{label}-end")
    h.epilogue(control, ["lc"], env, cache, label="control-end")


def winsize(fd, rows, cols) -> None:
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


def in_terminal(h, args, proj, env):
    """starts fetchloom on a new pseudo-terminal; returns (running, master, output buffer)."""
    import pty
    master, slave = pty.openpty()
    winsize(slave, 24, 80)
    running = h.start(args, proj, env, pty_fd=slave)
    os.close(slave)
    output = bytearray()

    def read():
        while True:
            try:
                chunk = os.read(master, 65536)
            except OSError:
                break
            if not chunk:
                break
            output.extend(chunk)
    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    return running, master, output, reader


def finish(running, master, reader, timeout):
    try:
        code = running.p.wait(timeout=timeout)
    except Exception:  # noqa: BLE001
        running.p.kill()
        code = None
    reader.join(timeout=10)
    os.close(master)
    return code


def j_pty(h):
    """a real terminal: resized during the progress bar, then a ctrl-c typed into it."""
    proj = h.fresh("pty", "resize")
    cache = h.cache_dir("pty-resize")
    env = h.env(cache, TERM="xterm-256color")
    h.site.pace("/mid.bin", 8 * MiB)
    try:
        running, master, output, reader = in_terminal(h, ["add", h.url("mid.bin"), "--as", "mid"], proj, env)
        time.sleep(1.5)
        winsize(master, 10, 30)
        os.kill(running.p.pid, signal.SIGWINCH)
        time.sleep(1.5)
        winsize(master, 50, 200)
        os.kill(running.p.pid, signal.SIGWINCH)
        code = finish(running, master, reader, 180)
    finally:
        h.site.pace("/mid.bin", None)
    text = ANSI.sub("", output.decode("utf-8", "replace"))
    h.say("  terminal output, last lines: " + " | ".join(text.strip().splitlines()[-6:]))
    h.check("resize", code == 0 and "Added mid" in text and "panicked" not in text,
            "a terminal resized twice during the download ends well", f"exit {code}; {text.strip()[-300:]!r}")
    proj2 = h.fresh("pty", "ctrl-c")
    cache2 = h.cache_dir("pty-ctrl-c")
    env2 = h.env(cache2, TERM="xterm-256color")
    size = h.made["/mid.bin"].size
    h.site.pace("/mid.bin", 8 * MiB)
    mark = h.site.mark()
    try:
        running, master, output, reader = in_terminal(h, ["add", h.url("mid.bin"), "--as", "mid"], proj2, env2)
        began = time.monotonic()
        while time.monotonic() - began < 60 and running.p.poll() is None:
            if h.site.sent_since(mark, "/mid.bin") >= 0.4 * size and time.monotonic() - began >= 3:
                break
            time.sleep(0.05)
        before = h.site.sent_since(mark, "/mid.bin")
        os.write(master, b"\x03")
        code = finish(running, master, reader, 90)
    finally:
        h.site.pace("/mid.bin", None)
    sent = h.site.sent_since(mark, "/mid.bin")
    text = ANSI.sub("", output.decode("utf-8", "replace"))
    h.say(f"  ctrl-c typed after {before / MiB:.1f} MiB; terminal output, last lines: "
          + " | ".join(text.strip().splitlines()[-6:]))
    h.check("ctrl-c", code == 130 and "Stopping" in text and "Stopped" in text,
            "ctrl-c typed at the terminal stops the add with `Stopping` and `Stopped`, exit 130",
            f"exit {code}; {text.strip()[-300:]!r}", doc="docs/user/concepts/questions-and-interruptions.md#interruptions")
    h.resumed("pty-ctrl-c", ["add", h.url("mid.bin"), "--as", "mid"], proj2, env2, "/mid.bin", sent,
              proj2 / "data" / "mid" / "mid.bin")
    h.epilogue(proj2, ["mid"], env2, cache2, label="ctrl-c-end")
    h.epilogue(proj, ["mid"], env, cache)


def j_locale(h):
    """turkish and C locales, far time zones, with unicode folder names."""
    for label, folder, extra in (
            ("turkish", "\u0130stanbul \u0131l\u0131k", {"LANG": "tr_TR.UTF-8", "LC_ALL": "tr_TR.UTF-8", "TZ": "Asia/Kathmandu"}),
            ("c-locale", "caf\u00e9 c", {"LANG": "C", "LC_ALL": "C", "TZ": "Pacific/Kiritimati"})):
        proj = h.fresh("locale", folder)
        cache = h.cache_dir(f"locale-{label}")
        env = h.env(cache, **extra)
        res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, env)
        h.check(f"{label}-add", res.code == 0, f"add works under {extra}", f"exit {res.code}", res=res)
        h.placed_ok(proj, "lc", "lc.zip", label=label)
        res = h.fl(["sync"], proj, env)
        h.check(f"{label}-sync", res.code == 0 and "nothing to do" in res.err, "sync finds nothing to do", last_lines(res.err),
                res=res)
        h.epilogue(proj, ["lc"], env, cache, label=f"{label}-end")


def j_sip(h):
    """folders system integrity protection keeps read-only."""
    out = h.fresh("sip", "out")
    res = h.fl(["get", h.url("plain.csv"), "-d", out], h.work, h.env(Path("/System/osh-cache")))
    h.check("cache-in-system", res.code == 50 and "/System/osh-cache" in res.err,
            "a cache folder under /System fails with local.io naming it", f"exit {res.code}: {last_lines(res.err)}",
            doc="docs/user/reference/exit-codes.md", res=res)
    env = h.env(h.cache_dir("sip"))
    res = h.fl(["-C", "/usr/bin", "status"], h.work, env)
    h.check("status-in-usr", res.code != 0 and res.err.strip() and "panicked" not in res.err,
            "status in /usr/bin says there is no project", f"exit {res.code}: {last_lines(res.err)}", res=res)
    res = h.fl(["-C", "/System/Library", "add", h.url("plain.csv"), "--as", "p"], h.work, env)
    h.check("add-in-system", res.code != 0 and res.err.strip() and "panicked" not in res.err,
            "an add in /System/Library fails with a message", f"exit {res.code}: {last_lines(res.err)}", res=res)


JOURNEYS = [("clones", j_clones), ("exfat", j_exfat), ("quarantine", j_quarantine), ("synced", j_synced),
            ("pty", j_pty), ("locale", j_locale), ("sip", j_sip)]
