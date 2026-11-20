"""the os hunt's windows journeys and helpers: volumes, short names, ctrl-c to another console,
a file held open by another process, junctions, long paths, powershell and cmd, vhd volumes."""
from __future__ import annotations

import contextlib
import ctypes
import json
import os
import re
import shutil
import subprocess
import sys
import time
import winreg
from ctypes import wintypes
from pathlib import Path

import hunt
from hunt import MiB, Skip, exists, last_lines, long, sha256_file, tree, writable

kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
FLAGS = {"case_sensitive_search": 0x1, "case_preserved_names": 0x2, "unicode_on_disk": 0x4,
         "persistent_acls": 0x8, "sparse_files": 0x40, "hard_links": 0x00400000,
         "block_refcounting": 0x08000000, "read_only_volume": 0x00080000}


def volume_info(path) -> dict:
    """the filesystem holding path, as windows names it, with its capability flags."""
    root = ctypes.create_unicode_buffer(1024)
    if not kernel32.GetVolumePathNameW(ctypes.c_wchar_p(os.path.abspath(str(path))), root, 1024):
        return {"error": f"GetVolumePathNameW {ctypes.get_last_error()}"}
    name = ctypes.create_unicode_buffer(261)
    fs = ctypes.create_unicode_buffer(261)
    serial, maxlen, flags = wintypes.DWORD(), wintypes.DWORD(), wintypes.DWORD()
    if not kernel32.GetVolumeInformationW(root.value, name, 261, ctypes.byref(serial), ctypes.byref(maxlen),
                                          ctypes.byref(flags), fs, 261):
        return {"volume": root.value, "error": f"GetVolumeInformationW {ctypes.get_last_error()}"}
    return {"volume": root.value, "filesystem": fs.value, "label": name.value,
            "drive_type": kernel32.GetDriveTypeW(root.value),
            "flags": [key for key, bit in FLAGS.items() if flags.value & bit]}


def short_path(path) -> str:
    buffer = ctypes.create_unicode_buffer(1024)
    if not kernel32.GetShortPathNameW(str(path), buffer, 1024):
        return ""
    return buffer.value


def send_ctrl_c(pid: int) -> int:
    """attaches to pid's console and sends ctrl-c to everything on it, this process aside."""
    kernel32.FreeConsole()
    if not kernel32.AttachConsole(pid):
        print(f"AttachConsole({pid}) failed: {ctypes.get_last_error()}")
        return 2
    kernel32.SetConsoleCtrlHandler(None, True)
    if not kernel32.GenerateConsoleCtrlEvent(0, 0):
        print(f"GenerateConsoleCtrlEvent failed: {ctypes.get_last_error()}")
        return 3
    time.sleep(1)
    kernel32.FreeConsole()
    return 0


def hold(path: str, share: int, ready: str, release: str) -> int:
    """opens path for reading with the given share mode and keeps it open until release exists."""
    create = kernel32.CreateFileW
    create.restype = wintypes.HANDLE
    create.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID, wintypes.DWORD,
                       wintypes.DWORD, wintypes.HANDLE]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    target = path if path.startswith("\\\\?\\") or len(path) < 240 else "\\\\?\\" + path
    handle = create(target, 0x80000000, share, None, 3, 0x80, None)
    if handle in (None, wintypes.HANDLE(-1).value):
        Path(ready).write_text(f"error {ctypes.get_last_error()}")
        return 2
    Path(ready).write_text("held")
    deadline = time.monotonic() + 1800
    while not os.path.exists(release) and time.monotonic() < deadline:
        time.sleep(0.1)
    kernel32.CloseHandle(handle)
    return 0


@contextlib.contextmanager
def held(h, path, share: int):
    """another process holds path open with `share` (0 shares nothing, 1 lets others read)."""
    stamp = time.monotonic_ns()
    ready, release = h.work / f"hold-{stamp}.ready", h.work / f"hold-{stamp}.release"
    holder = subprocess.Popen([sys.executable, str(hunt.HERE / "hunt.py"), "hold", long(path), str(share),
                               str(ready), str(release)])
    deadline = time.monotonic() + 30
    while not ready.exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    state = ready.read_text() if ready.exists() else "no answer"
    if state != "held":
        release.write_text("go")
        raise RuntimeError(f"could not hold {path}: {state}")
    h.say(f"  (another process holds {path} open, share mode {share})")
    try:
        yield
    finally:
        release.write_text("go")
        holder.wait(timeout=60)
        h.say(f"  (released {path})")


def reg(key: str, name: str):
    root, _, sub = key.partition("\\")
    hive = {"HKLM": winreg.HKEY_LOCAL_MACHINE, "HKCU": winreg.HKEY_CURRENT_USER}[root]
    try:
        with winreg.OpenKey(hive, sub) as handle:
            return winreg.QueryValueEx(handle, name)[0]
    except OSError:
        return None


def set_dword(key: str, name: str, value: int) -> None:
    root, _, sub = key.partition("\\")
    hive = {"HKLM": winreg.HKEY_LOCAL_MACHINE, "HKCU": winreg.HKEY_CURRENT_USER}[root]
    with winreg.OpenKey(hive, sub, 0, winreg.KEY_SET_VALUE) as handle:
        winreg.SetValueEx(handle, name, 0, winreg.REG_DWORD, value)


FILESYSTEM_KEY = "HKLM\\SYSTEM\\CurrentControlSet\\Control\\FileSystem"


def powershell(h, command: str, shell: str = "powershell", timeout=300):
    return h.sh([shell, "-NoProfile", "-NonInteractive", "-Command", command], timeout=timeout, show=False)


def facts(h) -> dict:
    out = {}
    version_key = "HKLM\\SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion"
    out["windows"] = {name: reg(version_key, name) for name in
                      ("ProductName", "EditionID", "DisplayVersion", "CurrentBuild", "UBR", "InstallationType")}
    out["long_paths_enabled"] = reg(FILESYSTEM_KEY, "LongPathsEnabled")
    out["ntfs_8dot3_creation"] = reg(FILESYSTEM_KEY, "NtfsDisable8dot3NameCreation")
    out["developer_mode"] = reg("HKLM\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\AppModelUnlock",
                                "AllowDevelopmentWithoutDevLicense")
    out["admin"] = bool(ctypes.windll.shell32.IsUserAnAdmin())
    out["code_pages"] = {name: reg("HKLM\\SYSTEM\\CurrentControlSet\\Control\\Nls\\CodePage", name)
                         for name in ("ACP", "OEMCP")}
    res = powershell(h, "Get-MpComputerStatus | Select-Object AMRunningMode,AntivirusEnabled,"
                        "RealTimeProtectionEnabled,IsTamperProtected,AMProductVersion,OnAccessProtectionEnabled "
                        "| ConvertTo-Json -Compress")
    out["defender"] = res.out.strip() or f"exit {res.code}: {res.err.strip()[:300]}"
    res = powershell(h, "(Get-MpPreference).ExclusionPath -join ';'")
    out["defender_exclusions"] = res.out.strip() or res.err.strip()[:200]
    for shell in ("powershell", "pwsh"):
        res = powershell(h, "$PSVersionTable.PSVersion.ToString() + ' ' + [Console]::OutputEncoding.WebName", shell)
        out[f"{shell}_version_encoding"] = res.out.strip() or f"exit {res.code}"
    res = h.sh(["cmd", "/d", "/c", "chcp"], show=False)
    out["chcp"] = res.out.strip()
    drives = sorted({Path(p).drive for p in (h.work, Path.home(), h.product) if Path(p).drive})
    out["8dot3"] = {}
    for drive in drives:
        res = h.sh(["fsutil", "8dot3name", "query", drive], show=False)
        out["8dot3"][drive] = " ".join(res.text.split())[:300]
    res = h.sh(["wsl", "--status"], show=False, timeout=60)
    out["wsl"] = f"exit {res.code}: " + res.text.replace("\x00", "").strip()[:300]
    probe = h.work / "symlink-probe"
    try:
        os.symlink(h.work, probe, target_is_directory=True)
        out["symlinks"] = "creatable"
        os.rmdir(probe)
    except OSError as err:
        out["symlinks"] = f"no: {err}"
    return out


def mklink(h, link, target, kind="/J") -> bool:
    res = h.sh(["cmd", "/d", "/c", "mklink", kind, str(link), str(target)], show=False)
    return res.code == 0


def vhd(h, name: str, filesystem: str, size_mb: int):
    """a vhdx with one volume of `filesystem`, on a free drive letter; returns (letter, image, how)."""
    letter = next((d for d in "RSTUVWXYZ" if not os.path.exists(f"{d}:\\")), None)
    if letter is None:
        raise Skip("no free drive letter")
    image = h.work / f"{name}.vhdx"
    script = h.work / f"{name}-diskpart.txt"
    script.write_text("\n".join([f'create vdisk file="{image}" maximum={size_mb} type=expandable',
                                 f'select vdisk file="{image}"', "attach vdisk", "create partition primary",
                                 f"assign letter={letter}"]) + "\n", encoding="ascii")
    res = h.sh(["diskpart", "/s", script], timeout=600)
    if res.code:
        raise Skip(f"diskpart could not make a volume: exit {res.code}, {last_lines(res.out)}")
    h.cleanups.append(lambda: vhd_detach(h, image))
    attempts = [f"Format-Volume -DriveLetter {letter} -FileSystem {filesystem} -NewFileSystemLabel osh{name} -Confirm:$false -Force"]
    if filesystem.lower() == "refs":
        attempts.append(f"Format-Volume -DriveLetter {letter} -DevDrive -NewFileSystemLabel osh{name} -Confirm:$false -Force")
    tried = []
    for command in attempts:
        res = powershell(h, command, timeout=900)
        info = volume_info(f"{letter}:\\")
        tried.append(f"{command}: exit {res.code} {res.err.strip()[:200]} -> {info.get('filesystem')}")
        if (info.get("filesystem") or "").lower() == filesystem.lower():
            h.note(f"{name}-volume", f"{letter}: {info}; made with {command}")
            return letter, image, command
    raise Skip(f"no {filesystem} volume on this runner: {' | '.join(tried)}")


def vhd_detach(h, image) -> None:
    script = h.work / f"detach-{time.monotonic_ns()}.txt"
    script.write_text(f'select vdisk file="{image}"\ndetach vdisk\n', encoding="ascii")
    h.sh(["diskpart", "/s", script], timeout=300, show=False)


# -- journeys ----------------------------------------------------------------------------------

PS_SCRIPT = r"""
$ErrorActionPreference = 'Continue'
$r = [ordered]@{}
$r.ps = $PSVersionTable.PSVersion.ToString()
$r.console_output_encoding = [Console]::OutputEncoding.WebName
$out = fetchloom status 2>&1
$r.status_exit = $LASTEXITCODE
$r.status_dollar_q = $?
$r.status_types = (@($out | ForEach-Object { $_.GetType().Name }) | Sort-Object -Unique) -join ','
$r.status_text = ($out | Out-String)
$p = fetchloom path lc/tables/a.csv
$r.path_exit = $LASTEXITCODE
$r.path = "$p"
$r.path_exists = [bool](Test-Path -LiteralPath "$p")
$ErrorActionPreference = 'Stop'
try { $null = fetchloom status 2>&1; $r.stop_survived = $true } catch { $r.stop_survived = $false; $r.stop_error = "$_" }
$ErrorActionPreference = 'Continue'
$s = SUGGESTION
$r.suggest_exit = $LASTEXITCODE
$r.suggest_out = "$s"
$r.suggest_exists = [bool]("$s" -and (Test-Path -LiteralPath "$s"))
[IO.File]::WriteAllText($env:OSH_OUT, ($r | ConvertTo-Json -Depth 3), (New-Object System.Text.UTF8Encoding($false)))
"""

CMD_SCRIPT = r"""@echo off
fetchloom status > "%OSH_OUT%.status.txt" 2>&1
echo %ERRORLEVEL%> "%OSH_OUT%.status.code"
for /f "usebackq delims=" %%p in (`fetchloom path lc/tables/a.csv`) do (
  echo %%p> "%OSH_OUT%.path.txt"
  if exist "%%p" (echo yes> "%OSH_OUT%.path.exists") else (echo no> "%OSH_OUT%.path.exists")
)
SUGGESTION > "%OSH_OUT%.suggest.txt" 2>&1
echo %ERRORLEVEL%> "%OSH_OUT%.suggest.code"
"""


def j_shells(h):
    """windows powershell 5.1, powershell 7 and cmd, in a folder with a space and an accent."""
    proj = h.fresh("shells", "Datos Econ\u00f3micos", "proyecto")
    cache = h.cache_dir("shells")
    path = str(h.binary.parent) + os.pathsep + os.environ.get("PATH", "")
    env = h.env(cache, PATH=path)
    for zipname, name in (("lc.zip", "lc"), ("quote.zip", "q")):
        res = h.fl(["add", h.url(zipname), "--as", name], proj, env)
        if res.code:
            raise RuntimeError(f"could not add {name}: exit {res.code}")
    res = h.fl(["path", "q/nope.csv"], proj, env)
    offered = [c for c in re.findall(r"`(fetchloom [^`]+)`", res.err) if "q/" in c]
    suggestion = offered[0] if offered else 'fetchloom path "q/x,1kb.csv"'
    h.check("suggestion", bool(offered) and '"' in suggestion, "a help line suggests a command with the name in double quotes",
            last_lines(res.err), doc="docs/user/guides/windows.md#commands-in-help-lines", res=res)
    h.note("suggestion-text", suggestion)
    for shell in ("powershell", "pwsh"):
        script = h.work / f"shells-{shell}.ps1"
        script.write_text(PS_SCRIPT.replace("SUGGESTION", suggestion), encoding="utf-8-sig")
        result = h.work / f"shells-{shell}.json"
        res = h.sh([shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script],
                   cwd=proj, env=dict(env, OSH_OUT=str(result)), timeout=300)
        if res.code == 127 and not result.exists():
            h.skip(shell, f"{shell} is not on this runner")
            continue
        try:
            r = json.loads(result.read_text(encoding="utf-8"))
        except (OSError, ValueError) as err:
            raise RuntimeError(f"{shell} wrote no result: {err}; exit {res.code}, {res.err[-500:]}")
        h.say(f"  {shell}: {json.dumps(r, ensure_ascii=False)[:1500]}")
        h.check(f"{shell}-2>&1-exit", r.get("status_exit") == 0,
                f"`fetchloom status 2>&1` in {shell} leaves $LASTEXITCODE at 0",
                f"{r.get('status_exit')}", doc="docs/user/guides/windows.md#red-text-in-windows-powershell")
        red = "NativeCommandError" in (r.get("status_text") or "")
        if shell == "powershell":
            h.check("powershell-red-text", red, "windows powershell 5.1 shows stderr as NativeCommandError text with 2>&1, as the docs say",
                    f"text {(r.get('status_text') or '')[:300]!r}", doc="docs/user/guides/windows.md#red-text-in-windows-powershell",
                    kind="note")
        else:
            h.check("pwsh-plain-text", not red, "powershell 7 shows the lines as they are with 2>&1",
                    f"text {(r.get('status_text') or '')[:300]!r}", doc="docs/user/guides/windows.md#red-text-in-windows-powershell")
        h.note(f"{shell}-dollar-q", f"$? after `fetchloom status 2>&1` is {r.get('status_dollar_q')}; record types {r.get('status_types')}")
        h.check(f"{shell}-stop-preference", r.get("stop_survived") is True,
                f"`fetchloom status 2>&1` survives $ErrorActionPreference = 'Stop' in {shell}",
                f"{r.get('stop_error')}", doc="docs/user/guides/windows.md#red-text-in-windows-powershell", kind="note")
        h.check(f"{shell}-path-usable", r.get("path_exit") == 0 and r.get("path_exists") is True,
                f"`$p = fetchloom path lc/tables/a.csv` in {shell} gives a path Test-Path finds, in a folder named Datos Econ\u00f3micos",
                f"exit {r.get('path_exit')}, $p = {r.get('path')!r}, exists {r.get('path_exists')}, console encoding "
                f"{r.get('console_output_encoding')}", doc="docs/user/guides/windows.md#paths-and-open-files")
        h.check(f"{shell}-suggestion-pastes", r.get("suggest_exit") == 0 and r.get("suggest_exists") is True,
                f"the suggested `{suggestion}` runs as written in {shell}",
                f"exit {r.get('suggest_exit')}, printed {r.get('suggest_out')!r}",
                doc="docs/user/guides/windows.md#commands-in-help-lines")
    batch = h.work / "shells.cmd"
    batch.write_text(CMD_SCRIPT.replace("SUGGESTION", suggestion), encoding="ascii")
    result = h.work / "shells-cmd"
    res = h.sh(["cmd", "/d", "/c", str(batch)], cwd=proj, env=dict(env, OSH_OUT=str(result)), timeout=300)

    def part(suffix):
        target = Path(f"{result}.{suffix}")
        return target.read_bytes().decode("utf-8", "replace").strip() if target.exists() else None
    h.say(f"  cmd: status {part('status.code')}, path {part('path.txt')!r} exists {part('path.exists')}, "
          f"suggest {part('suggest.code')} {part('suggest.txt')!r}")
    h.check("cmd-status", part("status.code") == "0", "`fetchloom status` in cmd exits 0", part("status.code"))
    h.check("cmd-path-usable", part("path.exists") == "yes",
            "a path captured with `for /f` in cmd names a file `if exist` finds, in a folder named Datos Econ\u00f3micos",
            f"captured {part('path.txt')!r}, exists {part('path.exists')}", kind="note")
    h.check("cmd-suggestion-pastes", part("suggest.code") == "0", f"the suggested `{suggestion}` runs as written in cmd",
            f"exit {part('suggest.code')}: {part('suggest.txt')!r}", doc="docs/user/guides/windows.md#commands-in-help-lines")
    h.epilogue(proj, ["lc", "q"], env, cache)


def j_held(h):
    """a placed file another process holds open, during sync, remove and gc."""
    proj = h.fresh("held", "proj")
    cache = h.cache_dir("held")
    env = h.env(cache)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, env)
    if res.code:
        raise RuntimeError(f"could not add lc: exit {res.code}")
    kind = h.placement_kind(proj, "lc", cache)
    h.note("placement", f"lc placed as {kind}")
    target = proj / "data" / "lc" / "tables" / "a.csv"
    pinned = h.read(target)
    with held(h, target, 0):
        res = h.fl(["status"], proj, env)
        h.check("status-held", res.code == 0 or "a.csv" in res.err,
                "status with a placed file another process holds without sharing exits 0 or names the file",
                f"exit {res.code}: {last_lines(res.err)}", res=res)
        res = h.fl(["sync"], proj, env)
        h.check("sync-held", res.code == 0 or "a.csv" in res.err, "a settled sync with the file held exits 0 or names it",
                f"exit {res.code}: {last_lines(res.err)}", res=res)
        h.set_copy(proj, "lc", True)
        res = h.fl(["sync"], proj, env)
        named = "a.csv" in res.err
        h.check("copy-held", named and not res.timed_out,
                "a sync that must replace the held file names it and says to close the program holding it",
                f"exit {res.code}: {last_lines(res.err, 4)}", doc="docs/user/guides/windows.md#paths-and-open-files", res=res)
        h.note("copy-held-exit", f"exit {res.code}; other files writable: "
               f"{sum(writable(proj / 'data' / 'lc' / rel) for rel in tree(proj / 'data' / 'lc'))} of {len(tree(proj / 'data' / 'lc'))}")
    res = h.fl(["sync"], proj, env)
    h.check("copy-after-release", res.code == 0, "once the file is closed, sync finishes", f"exit {res.code}: {last_lines(res.err)}",
            res=res)
    h.placed_ok(proj, "lc", "lc.zip", readonly=False, label="released")
    h.check("held-bytes", h.read(target) == pinned, "the held file still holds the pinned bytes", "changed")
    with held(h, target, 1):
        res = h.fl(["remove", "lc", "--yes"], proj, env)
        h.check("remove-held", "a.csv" in res.err and not res.timed_out,
                "remove with a file held open names it and says to run sync once it is closed",
                f"exit {res.code}: {last_lines(res.err, 4)}", doc="docs/user/commands/remove.md", res=res)
        toml = (h.read(proj / "data.toml") or b"").decode()
        h.note("remove-held-state", f"exit {res.code}; data.toml still has lc: {'[datasets.lc]' in toml}; "
               f"left in data/lc: {sorted(tree(proj / 'data' / 'lc'))[:5]}")
    res = h.fl(["sync"], proj, env)
    h.check("remove-finish", res.code == 0 and not exists(proj / "data" / "lc"),
            "sync after closing the file finishes the removal", f"exit {res.code}, data/lc there: {exists(proj / 'data' / 'lc')}",
            doc="docs/user/commands/remove.md", res=res)
    res = h.fl(["status", "--check"], proj, env)
    h.check("remove-settled", res.code == 0, "status --check is clean afterwards", f"exit {res.code}: {res.out[:200]}", res=res)
    objects = h.cache_objects(cache)
    if not objects:
        h.skip("gc-held", "the cache held no object to hold")
        return
    victim = objects[0]
    with held(h, victim, 0):
        res = h.fl(["cache", "gc", "--now", "--yes"], proj, env)
        h.check("gc-held", res.code == 50 and res.err.strip(),
                "gc with a cached file another process holds exits 50 and names what it could not remove",
                f"exit {res.code}: {last_lines(res.err, 4)}", doc="docs/user/commands/cache.md#gc", res=res)
        h.note("gc-held-left", f"objects left {len(h.cache_objects(cache))}; held {victim.name[:12]} named: "
               f"{victim.name[:12] in res.text}")
    res = h.fl(["cache", "gc", "--now", "--yes"], proj, env)
    h.check("gc-after-release", res.code == 0 and not h.cache_objects(cache), "gc once the file is closed frees the rest",
            f"exit {res.code}, objects left {len(h.cache_objects(cache))}", res=res)


def j_junction(h):
    """projects, caches and data folders reached through junctions."""
    base = h.fresh("junction")
    real = base / "real"
    (real / "proj").mkdir(parents=True)
    link = base / "link"
    if not mklink(h, link, real):
        raise Skip("mklink /J failed")
    cache = h.cache_dir("junction")
    env = h.env(cache)
    through = link / "proj"
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], through, env)
    h.check("add-through", res.code == 0, "add in a project reached through a junction works", f"exit {res.code}", res=res)
    h.placed_ok(real / "proj", "lc", "lc.zip")
    res = h.fl(["status", "--check"], real / "proj", env)
    h.check("status-real", res.code == 0, "status through the real path is clean", f"exit {res.code}: {res.out[:200]}", res=res)
    res = h.fl(["sync"], real / "proj", env)
    h.check("sync-real", res.code == 0 and "nothing to do" in res.err, "sync through the real path finds nothing to do",
            last_lines(res.err), res=res)
    report = h.cache_report(real / "proj", env)
    same = [p for p in report.get("projects") or []
            if os.path.normcase(os.path.realpath(p)) == os.path.normcase(os.path.realpath(real / "proj"))]
    h.check("one-registration", len(same) == 1, "the cache registers the project once, by either path",
            report.get("projects"), doc="docs/user/commands/cache.md#inspect")
    h.epilogue(real / "proj", ["lc"], env, cache)
    cache_real = base / "cache-real"
    cache_real.mkdir()
    cache_link = base / "cache-link"
    mklink(h, cache_link, cache_real)
    p2 = base / "p2"
    p2.mkdir()
    env2 = h.env(cache_link)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], p2, env2)
    h.check("cache-through", res.code == 0 and h.cache_objects(cache_real), "a cache reached through a junction works",
            f"exit {res.code}, objects {len(h.cache_objects(cache_real))}", res=res)
    h.note("cache-through-placement", f"placed as {h.placement_kind(p2, 'lc', cache_real)}")
    h.epilogue(p2, ["lc"], env2, cache_real, label="cache-link-end")
    for folder, doc in ((".fetchloom", "docs/user/reference/project-folder.md"),
                        ("data", "docs/user/reference/data-toml.md#destination-paths")):
        proj = base / f"p-{folder.strip('.')}"
        proj.mkdir()
        outside = base / f"outside-{folder.strip('.')}"
        outside.mkdir()
        mklink(h, proj / folder, outside)
        envx = h.env(h.cache_dir(f"junction-{folder.strip('.')}"))
        res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, envx)
        h.check(f"{folder}-junction-refused", res.code != 0 and res.err.strip(),
                f"a junction as {folder} is refused with a message", f"exit {res.code}: {last_lines(res.err)}", doc=doc, res=res)
        h.check(f"{folder}-junction-untouched", not os.listdir(outside), f"nothing is written through the {folder} junction",
                os.listdir(outside)[:10])
    proj = base / "p-ds"
    proj.mkdir()
    envd = h.env(h.cache_dir("junction-ds"))
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, envd)
    if res.code:
        raise RuntimeError(f"could not add lc for the dataset-folder junction: exit {res.code}")
    outside = base / "outside-ds"
    outside.mkdir()
    shutil.move(str(proj / "data" / "lc"), str(base / "moved-lc"))
    mklink(h, proj / "data" / "lc", outside)
    res = h.fl(["status"], proj, envd)
    h.check("ds-junction-status", "Blocked" in res.out or "lc" in res.out, "status reports the junction in the dataset's place",
            res.out[:300], doc="docs/user/commands/status.md", res=res)
    res = h.fl(["sync"], proj, envd)
    h.check("ds-junction-untouched", not os.listdir(outside), "sync writes nothing through a junction in a dataset's place",
            f"exit {res.code}; outside holds {os.listdir(outside)[:10]}", res=res)
    res = h.fl(["sync", "--force"], proj, envd)
    h.check("ds-junction-force", res.code == 0 and not os.listdir(outside) and os.path.isdir(outside),
            "sync --force removes the junction itself, never what it points to, and places the files",
            f"exit {res.code}; outside holds {os.listdir(outside)[:10] if os.path.isdir(outside) else 'gone'}",
            doc="docs/user/commands/sync.md", res=res)
    h.placed_ok(proj, "lc", "lc.zip", label="after-force")


def j_longpath(h):
    """paths past 260 characters: deep files in a dataset, and a deep project folder."""
    before = reg(FILESYSTEM_KEY, "LongPathsEnabled")
    h.note("long-paths-enabled", f"LongPathsEnabled = {before}")
    deep_run(h, "as-is")
    if before == 1:
        try:
            set_dword(FILESYSTEM_KEY, "LongPathsEnabled", 0)
            h.cleanups.append(lambda: set_dword(FILESYSTEM_KEY, "LongPathsEnabled", 1))
            deep_run(h, "off")
        except OSError as err:
            h.skip("long-paths-off", f"could not turn LongPathsEnabled off: {err}")
    root = h.work / "longroot" / ("r" * 80) / ("s" * 80) / ("t" * 80) / "proj"
    os.makedirs(long(root))
    env = h.env(h.cache_dir("longroot"))
    res = h.fl(["-C", root, "add", h.url("plain.csv"), "--as", "plain"], h.work, env)
    h.note("deep-root-length", f"project folder is {len(str(root))} characters")
    if res.code == 0:
        h.check("deep-root-add", True, "", "")
        for attempt in (1, 2):
            res = h.fl(["-C", root, "status", "--check"], h.work, env)
            h.check(f"deep-root-status-{attempt}", res.code == 0, "status in the deep project is clean", f"exit {res.code}", res=res)
        res = h.fl(["-C", root, "path", "plain/plain.csv"], h.work, env)
        h.note("deep-root-path", f"exit {res.code}, printed {res.out.strip()!r}", res=res)
        res = h.fl(["-C", root, "remove", "plain", "--yes"], h.work, env)
        h.check("deep-root-remove", res.code == 0 and not exists(root / "data" / "plain"), "remove in the deep project works",
                f"exit {res.code}", res=res)
    else:
        h.check("deep-root-message", re.search(r"\d+ characters|too long|shorter", res.err) is not None,
                "a working folder too long for -C fails with its length and a suggestion to move the project",
                f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/guides/windows.md#paths-and-open-files", res=res)


def deep_run(h, label):
    proj = h.fresh("long", label)
    cache = h.cache_dir(f"long-{label}")
    env = h.env(cache)
    target = proj / "data" / "long" / hunt.LONG_MEMBER
    h.note(f"{label}-length", f"the deep file's path is {len(str(target))} characters")
    res = h.fl(["add", h.url("long.zip"), "--as", "long"], proj, env)
    if res.code != 0:
        h.check(f"{label}-add", re.search(r"\d+ characters|too long|shorter", res.err) is not None,
                "a dataset path past 260 characters is placed, or refused with its length",
                f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/guides/windows.md#paths-and-open-files", res=res)
        return
    h.placed_ok(proj, "long", "long.zip", label=label)
    for attempt in (1, 2):
        res = h.fl(["status", "--check"], proj, env)
        h.check(f"{label}-status-{attempt}", res.code == 0, "status --check is clean with a path past 260 characters",
                f"exit {res.code}: {res.out[:200]}", res=res)
    res = h.fl(["path", f"long/{hunt.LONG_MEMBER}"], proj, env)
    printed = res.out.strip()
    h.check(f"{label}-path", res.code == 0 and exists(printed), "path prints the deep file", f"{printed!r}", res=res)
    h.note(f"{label}-path-form", f"printed path starts {printed[:8]!r}, {len(printed)} characters; plain open() "
           f"{'works' if can_open(printed) else 'fails'}")
    h.epilogue(proj, ["long"], env, cache, label=f"{label}-end")


def can_open(path: str) -> bool:
    try:
        with open(path, "rb"):
            return True
    except OSError:
        return False


def j_onedrive(h):
    """a project in a folder OneDrive's variables name, beside a control outside it."""
    fake = h.fresh("onedrive", "OneDrive - Contoso")
    cache = h.cache_dir("onedrive")
    env = h.env(cache, OneDrive=fake, OneDriveCommercial=fake)
    control = h.fresh("onedrive-control", "proj")
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], control, env)
    control_kind = h.placement_kind(control, "lc", cache)
    h.note("control", f"outside onedrive: exit {res.code}, placed as {control_kind}")
    proj = fake / "proj"
    proj.mkdir()
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, env)
    kind = h.placement_kind(proj, "lc", cache)
    h.check("copied", res.code == 0 and kind != "hardlink", "a project inside onedrive gets no hardlinks",
            f"exit {res.code}, placed as {kind} (control {control_kind})", doc="docs/user/guides/windows.md#onedrive-and-synced-folders",
            res=res)
    h.check("said", "onedrive" in res.err.lower(), "the add warns that onedrive breaks links, so files were copied",
            last_lines(res.err, 4), doc="docs/user/guides/windows.md#onedrive-and-synced-folders", res=res)
    h.epilogue(proj, ["lc"], env, cache, sole=False)
    h.epilogue(control, ["lc"], env, cache, label="control-end")


def j_refs(h):
    """a ReFS volume, as a dev drive is: clones, two projects, writable copies."""
    letter, image, how = vhd(h, "refs", "ReFS", 51200)
    res = h.sh(["fsutil", "devdrv", "query", f"{letter}:"], show=False)
    h.note("devdrv", " ".join(res.text.split())[:300])
    proj = Path(f"{letter}:\\p")
    proj.mkdir()
    cache = Path(f"{letter}:\\cache")
    env = h.env(cache)
    for name, url in (("lc", h.url("lc.zip")), ("mid", h.url("mid.bin"))):
        res = h.fl(["add", url, "--as", name], proj, env, timeout=400)
        h.check(f"add-{name}", res.code == 0, f"add {name} on ReFS exits 0", f"exit {res.code}: {last_lines(res.err)}", res=res)
        if res.code == 50 and "name" in res.err:
            h.note("refs-lockup", "the naming lock-up the windows guide describes", res=res,
                   doc="docs/user/guides/windows.md#a-run-that-stops-on-a-dev-drive")
    h.placed_ok(proj, "lc", "lc.zip", readonly=True)
    report = h.cache_report(proj, env)
    h.note("placement", f"lc {h.placement_kind(proj, 'lc', cache)}, mid {h.placement_kind(proj, 'mid', cache)}; "
           f"cache copies={report.get('copies')} cloned={report.get('cloned')} linked={report.get('linked')}")
    twin = Path(f"{letter}:\\twin")
    twin.mkdir()
    res = h.fl(["add", h.url("mid.bin"), "--as", "mid"], twin, env)
    h.check("twin", res.code == 0, "the same dataset in a second project on ReFS adds", f"exit {res.code}", res=res)
    stored = [o for o in h.cache_objects(cache) if o.stat().st_size == h.made["/mid.bin"].size]
    h.check("stored-once", len(stored) == 1, "the cache holds the 64 MiB file once for two projects", f"{len(stored)} copies")
    free = shutil.disk_usage(f"{letter}:\\").free
    h.set_copy(proj, "mid", True)
    res = h.fl(["sync"], proj, env)
    h.check("copy-sync", res.code == 0, "copy = true then sync exits 0", f"exit {res.code}", res=res)
    target = proj / "data" / "mid" / "mid.bin"
    h.check("copy-writable", exists(target) and writable(target), "copy = true gives a writable file", "read-only")
    used = free - shutil.disk_usage(f"{letter}:\\").free
    h.note("copy-space", f"the writable copy took {used / MiB:.1f} MiB of free space for a 64 MiB file")
    with open(target, "r+b") as stream:
        stream.write(b"edited")
    obj = stored[0] if stored else None
    h.check("clone-independent", obj is not None and sha256_file(obj) == h.made_sha("/mid.bin"),
            "editing the writable copy leaves the cache's copy as pinned", "the cache's copy changed")
    h.set_copy(proj, "mid", False)
    res = h.fl(["sync", "--force"], proj, env)
    h.epilogue(proj, ["lc", "mid"], env, cache, sole=False)
    h.epilogue(twin, ["mid"], env, cache, label="twin-end")


def j_fat32(h):
    """a FAT32 volume, as a usb stick is: copies, coarse times, a mirror, the 4 GiB limit."""
    letter, image, how = vhd(h, "fat", "FAT32", 4096)
    proj = Path(f"{letter}:\\p")
    proj.mkdir()
    cache = h.cache_dir("fat32")
    env = h.env(cache)
    res = h.fl(["add", h.url("lc.zip"), "--as", "lc"], proj, env)
    h.check("add", res.code == 0, "add onto FAT32 exits 0", f"exit {res.code}: {last_lines(res.err)}", res=res)
    h.placed_ok(proj, "lc", "lc.zip")
    h.note("placement", f"placed as {h.placement_kind(proj, 'lc', cache)}; read-only "
           f"{[not writable(proj / 'data' / 'lc' / rel) for rel in tree(proj / 'data' / 'lc')].count(True)} of "
           f"{len(tree(proj / 'data' / 'lc'))}")
    hunt.edit_through_link(h, proj, env, cache)
    mirror = Path(f"{letter}:\\mirror")
    for attempt in (1, 2):
        res = h.fl(["push", "--to", mirror, "--yes"], proj, env)
        h.check(f"push-{attempt}", res.code == 0, "push to a FAT32 folder exits 0", f"exit {res.code}: {last_lines(res.err)}", res=res)
    clone = h.clone(proj, "fat32", "clone")
    cenv = h.env(h.cache_dir("fat32-clone"))
    res = h.fl(["--offline", "sync", "--mirror", mirror], clone, cenv)
    h.check("restore", res.code == 0, "an offline sync from the FAT32 mirror restores a clone", f"exit {res.code}", res=res)
    h.placed_ok(clone, "lc", "lc.zip", label="restore")
    free = shutil.disk_usage(cache.parent if cache.parent.exists() else h.work).free
    if free < 12 * (1 << 30):
        h.skip("4gib", f"only {free >> 30} GiB free beside the cache")
    else:
        h.make("/huge.bin", 4 << 30, "huge")
        mark = h.site.mark()
        res = h.fl(["add", h.url("huge.bin"), "--as", "huge"], proj, env, timeout=1500)
        sent = h.site.sent_since(mark, "/huge.bin")
        h.check("4gib-refused", res.code != 0 and re.search(r"4 GiB|FAT", res.err) is not None,
                "a 4 GiB file for a FAT32 project is refused, saying why", f"exit {res.code}: {last_lines(res.err)}",
                doc="docs/user/reference/limits.md; docs/user/guides/macos.md#clones-and-links", res=res)
        h.note("4gib-downloaded", f"{sent / MiB:.0f} MiB downloaded before the answer, in {res.secs:.0f} s")
        h.check("4gib-nothing-left", not exists(proj / "data" / "huge"), "nothing of it is left in the project",
                sorted(os.listdir(proj / "data")))
        res = h.fl(["status", "--check"], proj, env)
        h.note("4gib-status", f"status --check exit {res.code}: {res.out[:200]}", res=res)
        toml = (h.read(proj / "data.toml") or b"").decode()
        if "[datasets.huge]" in toml:
            h.fl(["remove", "huge", "--yes"], proj, env)
    h.epilogue(proj, ["lc"], env, cache, sole=False)
    h.epilogue(clone, ["lc"], cenv, h.cache_dir("fat32-clone"), label="clone-end")


JOURNEYS = [("shells", j_shells), ("held", j_held), ("junction", j_junction), ("longpath", j_longpath),
            ("onedrive", j_onedrive), ("refs", j_refs), ("fat32", j_fat32)]
