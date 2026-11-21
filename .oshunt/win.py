"""the os hunt's windows journeys and helpers: volumes, short names, ctrl-c to another console,
a file held open by another process, junctions, long paths, powershell and cmd, vhd volumes."""
from __future__ import annotations

import contextlib
import ctypes
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import time
import uuid
import winreg
from ctypes import wintypes
from pathlib import Path

import hunt
from hunt import MiB, Skip, exists, last_lines, listing, long, sha256_file, tree, writable

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


def primitives(folder: Path) -> str:
    """each step of a cache write in folder, as fetchloom's windows publish does it (src/volume.rs
    rename_new: a hard link to the final name, then the temporary name deleted), with a writable
    and with a read-only temporary file."""
    steps = []

    def step(name, action):
        try:
            action()
            steps.append(f"{name}: ok")
        except OSError as err:
            steps.append(f"{name}: winerror={getattr(err, 'winerror', None)}")
    sub = folder / "zz"
    step("mkdir", lambda: sub.mkdir(exist_ok=True))
    for label, read_only in (("writable", False), ("read-only", True)):
        temp, final = sub / f"{label}.tmp", sub / f"{label}.final"
        step(f"{label} create", lambda: temp.write_bytes(b"probe"))
        if read_only:
            step(f"{label} seal", lambda: os.chmod(temp, 0o444))
        step(f"{label} link", lambda: os.link(temp, final))
        step(f"{label} delete temp", lambda: os.remove(temp))
    step("delete read-only", lambda: os.remove(sub / "read-only.final"))
    for leftover in sub.glob("*"):
        try:
            os.chmod(leftover, 0o666)
            os.remove(leftover)
        except OSError:
            pass
    return "; ".join(steps)


def vhd_detach(h, image) -> None:
    script = h.work / f"detach-{time.monotonic_ns()}.txt"
    script.write_text(f'select vdisk file="{image}"\ndetach vdisk\n', encoding="ascii")
    h.sh(["diskpart", "/s", script], timeout=300, show=False)


# -- journeys ----------------------------------------------------------------------------------

PS_SCRIPT = r"""
$ErrorActionPreference = 'Continue'
$r = [ordered]@{}
if ($env:OSH_OEM -eq '1') {
  try { [Console]::OutputEncoding = [Text.Encoding]::GetEncoding([Globalization.CultureInfo]::CurrentCulture.TextInfo.OEMCodePage) }
  catch { $r.oem_error = "$_" }
}
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
$was = [Console]::OutputEncoding
[Console]::OutputEncoding = [Text.Encoding]::UTF8
$u = fetchloom path lc/tables/a.csv
$r.utf8_path = "$u"
$r.utf8_exists = [bool](Test-Path -LiteralPath "$u")
[Console]::OutputEncoding = $was
$j = (fetchloom --json path lc/tables/a.csv | ConvertFrom-Json).result.path
$r.json_path = "$j"
$r.json_exists = [bool]("$j" -and (Test-Path -LiteralPath "$j"))
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
CODEPAGE
chcp > "%OSH_OUT%.chcp.txt"
fetchloom status > "%OSH_OUT%.status.txt" 2>&1
> "%OSH_OUT%.status.code" echo %ERRORLEVEL%
for /f "usebackq delims=" %%p in (`fetchloom path lc/tables/a.csv`) do (
  > "%OSH_OUT%.path.txt" echo %%p
  if exist "%%p" (> "%OSH_OUT%.path.exists" echo yes) else (> "%OSH_OUT%.path.exists" echo no)
)
SUGGESTION > "%OSH_OUT%.suggest.txt" 2>&1
> "%OSH_OUT%.suggest.code" echo %ERRORLEVEL%
chcp 65001 > nul
for /f "usebackq delims=" %%p in (`fetchloom path lc/tables/a.csv`) do (
  if exist "%%p" (> "%OSH_OUT%.utf8.exists" echo yes) else (> "%OSH_OUT%.utf8.exists" echo no)
)
"""


def j_shells(h):
    """windows powershell 5.1, powershell 7 and cmd, in a folder with a space and an accent, with
    the runner's console code page and with the oem one a fresh windows console starts in."""
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
    suggestion = suggestion.replace("`", "")
    h.check("suggestion", bool(offered), "a help line suggests a command for a path q holds", last_lines(res.err), res=res)
    h.check("suggestion-quoted", re.search(r'"q/[^"]*,[^"]*"', suggestion) is not None,
            "a suggested name holding a comma is in double quotes, which powershell 5.1 would otherwise split",
            suggestion, doc="docs/user/guides/windows.md#commands-in-help-lines")
    oem = reg("HKLM\\SYSTEM\\CurrentControlSet\\Control\\Nls\\CodePage", "OEMCP") or "437"
    for console in ("runner", "oem"):
        for shell in ("powershell", "pwsh"):
            label = f"{shell}-{console}"
            script = h.work / f"shells-{label}.ps1"
            script.write_text(PS_SCRIPT.replace("SUGGESTION", suggestion), encoding="utf-8-sig")
            result = h.work / f"shells-{label}.json"
            res = h.sh([shell, "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script],
                       cwd=proj, env=dict(env, OSH_OUT=str(result), OSH_OEM="1" if console == "oem" else "0"),
                       timeout=300)
            if res.code == 127 and not result.exists():
                h.skip(label, f"{shell} is not on this runner")
                continue
            try:
                r = json.loads(result.read_text(encoding="utf-8"))
            except (OSError, ValueError) as err:
                raise RuntimeError(f"{label} wrote no result: {err}; exit {res.code}, {res.err[-500:]}")
            h.say(f"  {label}: {json.dumps(r, ensure_ascii=False)[:1500]}")
            h.check(f"{label}-2>&1-exit", r.get("status_exit") == 0,
                    f"`fetchloom status 2>&1` in {shell} leaves $LASTEXITCODE at 0",
                    f"{r.get('status_exit')}", doc="docs/user/guides/windows.md#red-text-in-windows-powershell")
            if console == "runner":
                red = "NativeCommandError" in (r.get("status_text") or "")
                if shell == "powershell":
                    h.check("powershell-red-text", red,
                            "windows powershell 5.1 shows stderr as NativeCommandError text with 2>&1, as the docs say",
                            f"text {(r.get('status_text') or '')[:300]!r}",
                            doc="docs/user/guides/windows.md#red-text-in-windows-powershell", kind="note")
                else:
                    h.check("pwsh-plain-text", not red, "powershell 7 shows the lines as they are with 2>&1",
                            f"text {(r.get('status_text') or '')[:300]!r}",
                            doc="docs/user/guides/windows.md#red-text-in-windows-powershell")
                h.note(f"{shell}-dollar-q", f"$? after `fetchloom status 2>&1` is {r.get('status_dollar_q')}; "
                       f"record types {r.get('status_types')}")
                h.check(f"{shell}-stop-preference", r.get("stop_survived") is True,
                        f"`fetchloom status 2>&1` survives $ErrorActionPreference = 'Stop' in {shell}",
                        f"{r.get('stop_error')}", doc="docs/user/guides/windows.md#red-text-in-windows-powershell",
                        kind="note")
            # windows.md says a plain capture under an oem code page arrives garbled, and gives two ways
            h.check(f"{label}-path-usable", r.get("path_exit") == 0 and r.get("path_exists") is True,
                    f"`$p = fetchloom path lc/tables/a.csv` in {shell} ({console} console, "
                    f"{r.get('console_output_encoding')}) gives a path Test-Path finds, in a folder named Datos Econ\u00f3micos",
                    f"exit {r.get('path_exit')}, $p = {r.get('path')!r}, exists {r.get('path_exists')}, "
                    f"{r.get('oem_error') or ''}", doc="docs/user/guides/windows.md#paths-with-accents-in-scripts",
                    kind="note" if console == "oem" else "lead")
            h.check(f"{label}-utf8-path-usable", r.get("utf8_exists") is True,
                    f"with [Console]::OutputEncoding set to utf-8 in {shell} ({console} console), the captured path "
                    f"is one Test-Path finds", f"$p = {r.get('utf8_path')!r}",
                    doc="docs/user/guides/windows.md#paths-with-accents-in-scripts")
            h.check(f"{label}-json-path-usable", r.get("json_exists") is True,
                    f"result.path from `fetchloom --json path` in {shell} ({console} console) is one Test-Path finds",
                    f"$p = {r.get('json_path')!r}", doc="docs/user/guides/windows.md#paths-with-accents-in-scripts")
            if console == "runner":
                h.check(f"{label}-suggestion-pastes", r.get("suggest_exit") == 0 and r.get("suggest_exists") is True,
                        f"the suggested `{suggestion}` runs as written in {shell}",
                        f"exit {r.get('suggest_exit')}, printed {r.get('suggest_out')!r}",
                        doc="docs/user/guides/windows.md#commands-in-help-lines")
        batch = h.work / f"shells-{console}.cmd"
        codepage = f"chcp {oem} > nul" if console == "oem" else "rem the runner's code page"
        batch.write_text(CMD_SCRIPT.replace("SUGGESTION", suggestion).replace("CODEPAGE", codepage), encoding="ascii")
        result = h.work / f"shells-cmd-{console}"
        res = h.sh(["cmd", "/d", "/c", str(batch)], cwd=proj, env=dict(env, OSH_OUT=str(result)), timeout=300)

        def part(suffix):
            target = Path(f"{result}.{suffix}")
            return target.read_bytes().decode("utf-8", "replace").strip() if target.exists() else None
        h.say(f"  cmd ({console}): {part('chcp.txt')}; status {part('status.code')}, path {part('path.txt')!r} "
              f"exists {part('path.exists')}, suggest {part('suggest.code')} {part('suggest.txt')!r}")
        h.check(f"cmd-{console}-status", part("status.code") == "0", "`fetchloom status` in cmd exits 0", part("status.code"))
        h.check(f"cmd-{console}-path-usable", part("path.exists") == "yes",
                f"a path captured with `for /f` in cmd ({part('chcp.txt')}) names a file `if exist` finds, in a folder named "
                f"Datos Econ\u00f3micos", f"captured {part('path.txt')!r}, exists {part('path.exists')}", kind="note")
        h.check(f"cmd-{console}-utf8-path-usable", part("utf8.exists") == "yes",
                "after `chcp 65001`, a path captured with `for /f` in cmd names a file `if exist` finds",
                f"exists {part('utf8.exists')}", doc="docs/user/guides/windows.md#paths-with-accents-in-scripts")
        if console == "runner":
            h.check(f"cmd-{console}-suggestion-pastes", part("suggest.code") == "0",
                    f"the suggested `{suggestion}` runs as written in cmd",
                    f"exit {part('suggest.code')}: {part('suggest.txt')!r}",
                    doc="docs/user/guides/windows.md#commands-in-help-lines")
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
        files = listing(proj / "data" / "lc")
        h.check("copy-held-others", len(files) == 5,
                "a sync that cannot replace the one held file leaves the dataset's other files in place",
                f"exit {res.code}; data/lc holds {[(rel, 'writable' if writable(proj / 'data' / 'lc' / rel) else 'read-only') for rel in files]}",
                doc="docs/user/guides/windows.md#paths-and-open-files", res=res)
        res = h.fl(["status"], proj, env)
        h.note("copy-held-status", f"status after the failed sync: exit {res.code}, rows {res.out.strip()[:400]!r}", res=res)
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
    failed = []
    for name, url in (("lc", h.url("lc.zip")), ("mid", h.url("mid.bin"))):
        res = h.fl(["add", url, "--as", name], proj, env, timeout=400)
        h.check(f"add-{name}", res.code == 0, f"add {name} on ReFS, with the cache there as the windows guide suggests, exits 0",
                f"exit {res.code}: {last_lines(res.err)}", doc="docs/user/guides/windows.md#defender-and-new-files", res=res)
        if res.code == 50 and "its name" in res.err:
            h.note("refs-lockup", "the naming lock-up the windows guide describes", res=res,
                   doc="docs/user/guides/windows.md#a-run-that-stops-on-a-dev-drive")
        if res.code:
            failed.append(name)
    if failed:
        for argv in (["icacls", f"{letter}:\\"], ["icacls", str(cache)], ["icacls", str(cache / "objects")],
                     ["whoami", "/groups"]):
            res = h.sh(argv, timeout=60)
            h.note("acl-" + "-".join(Path(a).name or a for a in argv[1:])[:40], " ".join(res.text.split())[:900])
        res = h.fl(["-vv", "add", h.url("plain.csv"), "--as", "plain"], proj, env, timeout=400)
        h.note("debug-add", f"exit {res.code}; debug tail: {' | '.join(res.err.strip().splitlines()[-25:])[:3000]}", res=res)
        res = h.sh(["cmd", "/d", "/c", "dir", "/s", "/a", "/b", str(cache)], timeout=60)
        h.note("cache-tree", " | ".join(res.out.strip().splitlines()[:60]))
        h.note("primitives", f"on ReFS {primitives(cache / 'objects')} || on NTFS {primitives(h.work)}")
        raise Skip(f"add failed on this ReFS volume ({failed}), so the rest of the journey has nothing to work on")
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
    if exists(target) and writable(target):
        with open(target, "r+b") as stream:
            stream.write(b"edited")
        obj = stored[0] if stored else None
        h.check("clone-independent", obj is not None and sha256_file(obj) == h.made_sha("/mid.bin"),
                "editing the writable copy leaves the cache's copy as pinned", "the cache's copy changed")
    else:
        h.note("clone-independent", "not checked: the placed file is not writable")
    h.set_copy(proj, "mid", False)
    res = h.fl(["sync", "--force"], proj, env)
    h.epilogue(proj, ["lc", "mid"], env, cache, sole=False)
    h.epilogue(twin, ["mid"], env, cache, label="twin-end")


def j_fat32(h):
    """a FAT32 volume, as a usb stick is: copies, coarse times, a mirror, the 4 GiB limit."""
    letter, image, how = vhd(h, "fat", "FAT32", 16384)
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
        left = sorted(os.listdir(proj / "data" / "huge")) if exists(proj / "data" / "huge") else None
        h.check("4gib-nothing-left", left is None, "the refused add leaves no folder for it in data/",
                f"data/ holds {sorted(os.listdir(proj / 'data'))}; data/huge holds {left}",
                doc="docs/user/commands/add.md: restored data.toml and data.lock", res=res)
        res = h.fl(["status", "--check"], proj, env)
        h.note("4gib-status", f"status --check exit {res.code}: {res.out[:200]}", res=res)
        toml = (h.read(proj / "data.toml") or b"").decode()
        if "[datasets.huge]" in toml:
            h.fl(["remove", "huge", "--yes"], proj, env)
    h.epilogue(proj, ["lc"], env, cache, sole=False)
    h.epilogue(clone, ["lc"], cenv, h.cache_dir("fat32-clone"), label="clone-end")
    small_stick(h)


def small_stick(h):
    """a stick too small for the file: when does fetchloom say so, and how."""
    letter, image, how = vhd(h, "small", "FAT32", 1024)
    proj = Path(f"{letter}:\\p")
    proj.mkdir()
    env = h.env(h.cache_dir("fat32-small"))
    h.make("/stick.bin", 1536 * MiB, "stick")
    free = shutil.disk_usage(f"{letter}:\\").free
    mark = h.site.mark()
    res = h.fl(["add", h.url("stick.bin"), "--as", "stick"], proj, env, timeout=900)
    sent = h.site.sent_since(mark, "/stick.bin")
    h.check("small-refused", res.code != 0 and res.err.strip(), "a 1.5 GiB file for a 1 GiB stick is refused with a message",
            f"exit {res.code}: {last_lines(res.err)}", res=res)
    h.check("small-before-download", sent < 64 * MiB,
            "the size the server states up front is checked against the stick's free space before downloading",
            f"{sent / MiB:.0f} MiB downloaded first; stick had {free / MiB:.0f} MiB free; {last_lines(res.err, 2)}",
            doc="docs/user/concepts/questions-and-interruptions.md#approval (free space is checked before anything starts)",
            res=res)
    h.note("small-message", f"{last_lines(res.err, 3)}; free {free} bytes, file {1536 * MiB} bytes")


SWAP = "/swap.bin"
SWAP_SIZE = 64 * MiB
VICTIM = b"a file outside the project that no sync may change\n" * 1024


def is_reparse(path) -> bool:
    try:
        return bool(os.lstat(long(path)).st_file_attributes & stat.FILE_ATTRIBUTE_REPARSE_POINT)
    except OSError:
        return False


def what_is(path) -> str:
    if not exists(path):
        return "nothing"
    if is_reparse(path):
        return "a link"
    if os.path.isdir(long(path)):
        return "a folder"
    return f"a file ({sha256_file(path)[:12]})"


def plant(h, kind, proj, victim, outside_dir) -> str:
    """swaps, while a sync runs, what `kind` names on the way to data/s/swap.bin for a link out of
    the project; returns what it did, or raises Skip when the window was missed or windows refused."""
    final = proj / "data" / "s" / "swap.bin"
    if exists(final):
        raise Skip(f"data/s/swap.bin was already placed before the swap ({what_is(final)})")
    if kind in ("file-symlink", "name-junction"):
        os.makedirs(long(final.parent), exist_ok=True)
        if kind == "file-symlink":
            try:
                os.symlink(victim, final)
            except OSError as err:
                raise Skip(f"could not make a file symlink: {err}") from err
            return f"data\\s\\swap.bin is a file symlink to {victim}"
        if not mklink(h, final, outside_dir):
            raise Skip("mklink /J failed")
        return f"data\\s\\swap.bin is a junction to {outside_dir}"
    swapped = proj / "data" / "s" if kind == "dataset-junction" else proj / "data"
    if exists(swapped):
        try:
            os.rename(long(swapped), long(swapped.with_name(swapped.name + ".aside")))
        except OSError as err:
            raise Skip(f"windows refused moving {swapped.name} aside while the sync ran: {err}") from err
    else:
        os.makedirs(long(swapped.parent), exist_ok=True)
    if not mklink(h, swapped, outside_dir):
        raise Skip("mklink /J failed")
    return f"{swapped.relative_to(proj)} is a junction to {outside_dir}"


def strings(value) -> list:
    """every string a JSON document holds, its keys left out."""
    if isinstance(value, str):
        return [value]
    if isinstance(value, dict):
        return [s for item in value.values() for s in strings(item)]
    if isinstance(value, list):
        return [s for item in value for s in strings(item)]
    return []


def mentions(texts, rel) -> bool:
    """whether any of `texts` names `rel` itself, in either slash, not only a path below it."""
    for form in (rel, rel.replace("/", "\\")):
        for text in texts:
            at = text.find(form)
            while at >= 0:
                if text[at + len(form):at + len(form) + 1] not in ("/", "\\"):
                    return True
                at = text.find(form, at + 1)
    return False


def j_swap(h):
    """s15: while a sync or add downloads, a pinned file's final name, its dataset folder or data/
    is swapped for a link out of the project. nothing may be written through it; without --force
    the link stays and the run names it, and --force removes the link itself (sync.md, `--force`)."""
    h.make(SWAP, SWAP_SIZE, "swap")
    base = h.fresh("swap", "base")
    base_env = h.env(h.cache_dir("swap-base"))
    res = h.fl(["add", h.url("swap.bin"), "--as", "s"], base, base_env)
    if res.code:
        raise RuntimeError(f"could not add the swap dataset: exit {res.code}: {last_lines(res.err)}")
    pinned = sha256_file(base / "data" / "s" / "swap.bin")
    rels = {"file-symlink": "data/s/swap.bin", "name-junction": "data/s/swap.bin",
            "dataset-junction": "data/s", "data-junction": "data"}
    for kind in rels:
        for command in ("add", "sync", "sync-force"):
            label = f"{kind}-{command}"
            outside = h.fresh("swap", f"outside-{label}")
            victim = outside / "victim.bin"
            victim.write_bytes(VICTIM)
            outside_dir = outside / "dir"
            outside_dir.mkdir()
            before = tree(outside, marker=True)
            if command == "add":
                proj = h.fresh("swap", label)
                args = ["--json", "add", h.url("swap.bin"), "--as", "s"]
            else:
                proj = h.clone(base, "swap", label)
                args = ["--json", "sync", "--force"] if command == "sync-force" else ["--json", "sync"]
            env = h.env(h.cache_dir(f"swap-{label}"))
            h.site.pace(SWAP, 16 * MiB)
            mark = h.site.mark()
            running = h.start(args, proj, env)
            planted = None
            try:
                begun = time.monotonic()
                while time.monotonic() - begun < 120 and running.poll() is None:
                    if h.site.sent_since(mark, SWAP) >= SWAP_SIZE // 4 and time.monotonic() - begun >= 1.0:
                        try:
                            planted = plant(h, kind, proj, victim, outside_dir)
                        except Skip as reason:
                            planted = reason
                        break
                    time.sleep(0.05)
                h.site.pace(SWAP, None)
                res = running.wait(300)
            finally:
                h.site.pace(SWAP, None)
            h.show(res)
            if not isinstance(planted, str):
                h.skip(label, str(planted) if planted else f"the download ended before a quarter was sent (exit {res.code})")
                continue
            h.say(f"  (swapped mid-download: {planted})")
            final = proj / "data" / "s" / "swap.bin"
            swapped = proj / Path(rels[kind])
            after = tree(outside, marker=True)
            h.check(f"{label}-outside-untouched", after == before and not os.listdir(long(outside_dir)),
                    "nothing is written outside the project through the swapped link",
                    f"outside held {sorted(before)}, now {sorted(after)}; the junction's target holds "
                    f"{os.listdir(long(outside_dir))[:10]}; {planted}", doc="docs/user/commands/sync.md (`--force`)",
                    res=res)
            try:
                document = json.loads(res.out)
            except ValueError:
                document = {}
            said = strings(document)
            named = mentions(said, rels[kind])
            errors = document.get("errors") or []
            helps = [error.get("help") or "" for error in errors]
            outcome = (f"exit {res.code}; {rels[kind]} is {what_is(swapped)}, data/s/swap.bin is {what_is(final)}; "
                       f"kept {((document.get('result') or {}).get('placement') or {}).get('kept')}; "
                       f"errors {[error.get('message') for error in errors]}; help {helps}")
            placed = exists(final) and not is_reparse(final) and sha256_file(final) == pinned
            if kind == "data-junction":
                h.note(f"{label}-outcome", outcome, res=res)
            else:
                refused = res.code != 0 and is_reparse(swapped) and named
                if command == "sync-force":
                    # a link made after sync looked is refused; the next --force run removes it
                    h.check(f"{label}-refused-or-replaced", refused or res.code == 0 and not is_reparse(swapped) and placed,
                            "sync --force either refuses the new link, naming it, or removes the link itself and "
                            "places the pinned file", outcome, doc="docs/user/commands/sync.md", res=res)
                else:
                    h.check(f"{label}-link-kept", is_reparse(swapped), "without --force the link stays as it was",
                            outcome, doc="docs/user/commands/sync.md", res=res)
                    h.check(f"{label}-named", named, f"the run names {rels[kind]}, the link in the pinned file's way",
                            outcome, doc="docs/user/commands/sync.md: kept reason `blocked`, `blocked_by`", res=res)
                if errors:
                    h.check(f"{label}-help-sound", not any("short name" in text for text in helps),
                            "the refusal's help says a link is in the way, not that windows gave a file outside the "
                            "project the pinned file's short name", f"help {helps}", res=res)
                h.note(f"{label}-outcome", outcome, res=res)
            if exists(proj / "data.toml") and is_reparse(swapped):
                again = h.fl(["sync", "--force"], proj, env)
                if kind == "data-junction":
                    h.note(f"{label}-force-again", f"exit {again.code}: {last_lines(again.err)}", res=again)
                else:
                    h.check(f"{label}-force-again", again.code == 0 and not is_reparse(swapped) and exists(final)
                            and not is_reparse(final) and sha256_file(final) == pinned,
                            "the next sync --force removes the link itself and places the pinned file",
                            f"exit {again.code}; {rels[kind]} is {what_is(swapped)}: {last_lines(again.err)}",
                            doc="docs/user/commands/sync.md", res=again)
                h.check(f"{label}-force-again-outside", tree(outside, marker=True) == before
                        and not os.listdir(long(outside_dir)), "the next sync --force changes nothing outside",
                        f"outside now {sorted(tree(outside, marker=True))}, the target holds "
                        f"{os.listdir(long(outside_dir))[:10]}", res=again)
            for path in (final, proj / "data" / "s", proj / "data"):
                if is_reparse(path):
                    with contextlib.suppress(OSError):
                        if os.path.isdir(long(path)):
                            os.rmdir(long(path))
                        else:
                            os.unlink(long(path))
            if not exists(proj / "data.toml"):
                continue
            res = h.fl(["sync", "--force"], proj, env)
            h.check(f"{label}-recovers", res.code == 0 and exists(final) and sha256_file(final) == pinned,
                    "with the link gone, sync --force places the pinned file", f"exit {res.code}: {last_lines(res.err)}",
                    res=res)
            h.check(f"{label}-outside-still-untouched", tree(outside, marker=True) == before,
                    "the outside folder is unchanged after the recovering sync", sorted(tree(outside, marker=True)))
    h.epilogue(base, ["s"], base_env, h.cache_dir("swap-base"))

# -- cloud files: a sync root of the harness's own, with online-only placeholders, as onedrive's
# files on-demand makes them (cfapi, cldapi.dll; layouts from cfapi.h, 64-bit) ------------------

class GUID(ctypes.Structure):
    _fields_ = [("Data1", wintypes.DWORD), ("Data2", wintypes.WORD), ("Data3", wintypes.WORD),
                ("Data4", ctypes.c_ubyte * 8)]


class CF_SYNC_REGISTRATION(ctypes.Structure):
    _fields_ = [("StructSize", wintypes.ULONG), ("ProviderName", wintypes.LPCWSTR),
                ("ProviderVersion", wintypes.LPCWSTR), ("SyncRootIdentity", ctypes.c_void_p),
                ("SyncRootIdentityLength", wintypes.DWORD), ("FileIdentity", ctypes.c_void_p),
                ("FileIdentityLength", wintypes.DWORD), ("ProviderId", GUID)]


class CF_SYNC_POLICIES(ctypes.Structure):
    _fields_ = [("StructSize", wintypes.ULONG), ("HydrationPrimary", wintypes.USHORT),
                ("HydrationModifier", wintypes.USHORT), ("PopulationPrimary", wintypes.USHORT),
                ("PopulationModifier", wintypes.USHORT), ("InSync", wintypes.ULONG), ("HardLink", wintypes.ULONG),
                ("PlaceholderManagement", wintypes.ULONG)]


class FILE_BASIC_INFO(ctypes.Structure):
    _fields_ = [("CreationTime", ctypes.c_longlong), ("LastAccessTime", ctypes.c_longlong),
                ("LastWriteTime", ctypes.c_longlong), ("ChangeTime", ctypes.c_longlong),
                ("FileAttributes", wintypes.DWORD)]


class CF_FS_METADATA(ctypes.Structure):
    _fields_ = [("BasicInfo", FILE_BASIC_INFO), ("FileSize", ctypes.c_longlong)]


class CF_PLACEHOLDER_CREATE_INFO(ctypes.Structure):
    _fields_ = [("RelativeFileName", wintypes.LPCWSTR), ("FsMetadata", CF_FS_METADATA),
                ("FileIdentity", ctypes.c_void_p), ("FileIdentityLength", wintypes.DWORD), ("Flags", wintypes.DWORD),
                ("Result", ctypes.c_long), ("CreateUsn", ctypes.c_longlong)]


class CF_CALLBACK_INFO(ctypes.Structure):
    _fields_ = [("StructSize", wintypes.DWORD), ("ConnectionKey", ctypes.c_longlong),
                ("CallbackContext", ctypes.c_void_p), ("VolumeGuidName", ctypes.c_void_p),
                ("VolumeDosName", ctypes.c_void_p), ("VolumeSerialNumber", wintypes.DWORD),
                ("SyncRootFileId", ctypes.c_longlong), ("SyncRootIdentity", ctypes.c_void_p),
                ("SyncRootIdentityLength", wintypes.DWORD), ("FileId", ctypes.c_longlong),
                ("FileSize", ctypes.c_longlong), ("FileIdentity", ctypes.c_void_p),
                ("FileIdentityLength", wintypes.DWORD), ("NormalizedPath", ctypes.c_void_p),
                ("TransferKey", ctypes.c_longlong), ("PriorityHint", ctypes.c_ubyte),
                ("CorrelationVector", ctypes.c_void_p), ("ProcessInfo", ctypes.c_void_p),
                ("RequestKey", ctypes.c_longlong)]


class CF_FETCH_DATA(ctypes.Structure):
    _fields_ = [("Flags", wintypes.DWORD), ("RequiredFileOffset", ctypes.c_longlong),
                ("RequiredLength", ctypes.c_longlong), ("OptionalFileOffset", ctypes.c_longlong),
                ("OptionalLength", ctypes.c_longlong), ("LastDehydrationTime", ctypes.c_longlong),
                ("LastDehydrationReason", wintypes.DWORD)]


class CF_CALLBACK_PARAMETERS(ctypes.Structure):
    _fields_ = [("ParamSize", wintypes.ULONG), ("FetchData", CF_FETCH_DATA)]


class CF_TRANSFER_DATA(ctypes.Structure):
    _fields_ = [("Flags", wintypes.DWORD), ("CompletionStatus", ctypes.c_long), ("Buffer", ctypes.c_void_p),
                ("Offset", ctypes.c_longlong), ("Length", ctypes.c_longlong)]


class CF_OPERATION_PARAMETERS(ctypes.Structure):
    _fields_ = [("ParamSize", wintypes.ULONG), ("TransferData", CF_TRANSFER_DATA)]


class CF_OPERATION_INFO(ctypes.Structure):
    _fields_ = [("StructSize", wintypes.ULONG), ("Type", wintypes.DWORD), ("ConnectionKey", ctypes.c_longlong),
                ("TransferKey", ctypes.c_longlong), ("CorrelationVector", ctypes.c_void_p),
                ("SyncStatus", ctypes.c_void_p), ("RequestKey", ctypes.c_longlong)]


CF_CALLBACK = ctypes.WINFUNCTYPE(None, ctypes.POINTER(CF_CALLBACK_INFO), ctypes.POINTER(CF_CALLBACK_PARAMETERS))


class CF_CALLBACK_REGISTRATION(ctypes.Structure):
    _fields_ = [("Type", wintypes.DWORD), ("Callback", CF_CALLBACK)]


class BY_HANDLE_FILE_INFORMATION(ctypes.Structure):
    # FILETIME is two DWORDs, so nothing here aligns past 4
    _pack_ = 4
    _fields_ = [("FileAttributes", wintypes.DWORD), ("CreationTime", ctypes.c_longlong),
                ("LastAccessTime", ctypes.c_longlong), ("LastWriteTime", ctypes.c_longlong),
                ("VolumeSerialNumber", wintypes.DWORD), ("FileSizeHigh", wintypes.DWORD),
                ("FileSizeLow", wintypes.DWORD), ("NumberOfLinks", wintypes.DWORD),
                ("FileIndexHigh", wintypes.DWORD), ("FileIndexLow", wintypes.DWORD)]


def hr(value) -> str:
    return f"0x{value & 0xffffffff:08x}"


class Cloud:
    """a sync root at `root` with this process as its provider: placeholders are made online-only,
    and a read of one asks fetch() for its bytes, which come from `contents` by file identity."""

    def __init__(self, root: Path):
        self.root = root
        self.api = ctypes.WinDLL("cldapi.dll")
        self.api.CfRegisterSyncRoot.restype = ctypes.c_long
        self.api.CfRegisterSyncRoot.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(CF_SYNC_REGISTRATION),
                                                 ctypes.POINTER(CF_SYNC_POLICIES), wintypes.DWORD]
        self.api.CfUnregisterSyncRoot.restype = ctypes.c_long
        self.api.CfUnregisterSyncRoot.argtypes = [wintypes.LPCWSTR]
        self.api.CfConnectSyncRoot.restype = ctypes.c_long
        self.api.CfConnectSyncRoot.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(CF_CALLBACK_REGISTRATION),
                                                ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(ctypes.c_longlong)]
        self.api.CfDisconnectSyncRoot.restype = ctypes.c_long
        self.api.CfDisconnectSyncRoot.argtypes = [ctypes.c_longlong]
        self.api.CfCreatePlaceholders.restype = ctypes.c_long
        self.api.CfCreatePlaceholders.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(CF_PLACEHOLDER_CREATE_INFO),
                                                   wintypes.DWORD, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)]
        self.api.CfExecute.restype = ctypes.c_long
        self.api.CfExecute.argtypes = [ctypes.POINTER(CF_OPERATION_INFO), ctypes.POINTER(CF_OPERATION_PARAMETERS)]
        self.contents: dict = {}
        self.calls: list = []
        self.key = None
        self.keep: list = []
        self.callback = CF_CALLBACK(self.fetch)

    def register(self) -> int:
        self.root.mkdir(parents=True, exist_ok=True)
        identity = ctypes.create_string_buffer(b"osh-root")
        self.keep.append(identity)
        registration = CF_SYNC_REGISTRATION(StructSize=ctypes.sizeof(CF_SYNC_REGISTRATION),
                                            ProviderName="fetchloom os hunt", ProviderVersion="1.0",
                                            SyncRootIdentity=ctypes.cast(identity, ctypes.c_void_p),
                                            SyncRootIdentityLength=8,
                                            ProviderId=GUID.from_buffer_copy(uuid.uuid4().bytes_le))
        # hydration full, population always full: the root is filled here, never asked for
        policies = CF_SYNC_POLICIES(StructSize=ctypes.sizeof(CF_SYNC_POLICIES), HydrationPrimary=2,
                                    PopulationPrimary=3)
        return self.api.CfRegisterSyncRoot(str(self.root), ctypes.byref(registration), ctypes.byref(policies), 0)

    def connect(self) -> int:
        table = (CF_CALLBACK_REGISTRATION * 2)()
        table[0].Type = 0
        table[0].Callback = self.callback
        table[1].Type = 0xFFFFFFFF
        self.keep.append(table)
        key = ctypes.c_longlong()
        code = self.api.CfConnectSyncRoot(str(self.root), table, None, 0, ctypes.byref(key))
        if code >= 0:
            self.key = key.value
        return code

    def disconnect(self) -> None:
        if self.key is not None:
            self.api.CfDisconnectSyncRoot(self.key)
            self.key = None

    def unregister(self) -> None:
        self.disconnect()
        self.api.CfUnregisterSyncRoot(str(self.root))

    def place(self, folder: Path, files: dict) -> list:
        """online-only placeholders in `folder` for `files`, name to bytes; returns their results."""
        infos = (CF_PLACEHOLDER_CREATE_INFO * len(files))()
        now = int((time.time() + 11644473600) * 10_000_000)
        for index, (name, data) in enumerate(files.items()):
            identity = ctypes.create_string_buffer(f"{folder.name}/{name}".encode())
            self.keep.append(identity)
            self.contents[identity.value.decode()] = data
            info = infos[index]
            info.RelativeFileName = name
            basic = info.FsMetadata.BasicInfo
            basic.CreationTime = basic.LastAccessTime = basic.LastWriteTime = basic.ChangeTime = now
            basic.FileAttributes = 0x80
            info.FsMetadata.FileSize = len(data)
            info.FileIdentity = ctypes.cast(identity, ctypes.c_void_p)
            info.FileIdentityLength = len(identity.value)
            info.Flags = 0x2
        done = wintypes.DWORD()
        code = self.api.CfCreatePlaceholders(str(folder), infos, len(files), 0, ctypes.byref(done))
        return [hr(code)] + [hr(infos[index].Result) for index in range(len(files))]

    def fetch(self, info_pointer, parameters_pointer) -> None:
        try:
            info = info_pointer.contents
            wanted = parameters_pointer.contents.FetchData
            identity = ctypes.string_at(info.FileIdentity, info.FileIdentityLength).decode()
            data = self.contents[identity]
            start = wanted.RequiredFileOffset
            chunk = data[start:start + wanted.RequiredLength]
            buffer = ctypes.create_string_buffer(chunk, max(len(chunk), 1))
            operation = CF_OPERATION_INFO(StructSize=ctypes.sizeof(CF_OPERATION_INFO), Type=0,
                                          ConnectionKey=info.ConnectionKey, TransferKey=info.TransferKey,
                                          RequestKey=info.RequestKey)
            parameters = CF_OPERATION_PARAMETERS(ParamSize=ctypes.sizeof(CF_OPERATION_PARAMETERS))
            parameters.TransferData.Buffer = ctypes.cast(buffer, ctypes.c_void_p)
            parameters.TransferData.Offset = start
            parameters.TransferData.Length = len(chunk)
            code = self.api.CfExecute(ctypes.byref(operation), ctypes.byref(parameters))
            self.calls.append((identity, start, wanted.RequiredLength, hr(code)))
        except Exception as err:  # a callback must not raise into the platform
            self.calls.append(("error", repr(err)))


def read_as_open_plain(path: Path, flags: int = 0x00200000) -> dict:
    """reads `path` as fetchloom's open_plain does on windows: to read, sharing all, the link not
    followed (FILE_FLAG_OPEN_REPARSE_POINT, unless `flags` says otherwise); then the handle's
    attributes and reparse tag, as std's metadata takes them, and the bytes."""
    create = kernel32.CreateFileW
    create.restype = wintypes.HANDLE
    create.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, wintypes.LPVOID, wintypes.DWORD,
                       wintypes.DWORD, wintypes.HANDLE]
    kernel32.GetFileInformationByHandle.argtypes = [wintypes.HANDLE, ctypes.POINTER(BY_HANDLE_FILE_INFORMATION)]
    kernel32.GetFileInformationByHandleEx.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    kernel32.ReadFile.argtypes = [wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD),
                                  wintypes.LPVOID]
    kernel32.CloseHandle.argtypes = [wintypes.HANDLE]
    out = {}
    handle = create(str(path), 0x80000000, 7, None, 3, flags, None)
    if handle in (None, wintypes.HANDLE(-1).value):
        out["open"] = f"error {ctypes.get_last_error()}"
        return out
    try:
        info = BY_HANDLE_FILE_INFORMATION()
        if kernel32.GetFileInformationByHandle(handle, ctypes.byref(info)):
            out["attributes"] = f"0x{info.FileAttributes:08x}"
        tag = (wintypes.DWORD * 2)()
        if kernel32.GetFileInformationByHandleEx(handle, 9, tag, ctypes.sizeof(tag)):
            out["tag"] = f"0x{tag[1]:08x}"
        buffer = ctypes.create_string_buffer(65536)
        got = wintypes.DWORD()
        if kernel32.ReadFile(handle, buffer, 65536, ctypes.byref(got), None):
            out["bytes"] = buffer.raw[:got.value]
        else:
            out["read"] = f"error {ctypes.get_last_error()}"
    finally:
        kernel32.CloseHandle(handle)
    return out


def j_placeholder(h):
    """sec-links' note: data.toml and data.lock open without following a reparse point, and an
    online-only cloud placeholder is one. in a sync root of the harness's own, a project whose two
    files are online-only placeholders is read as the same files on a plain folder are, while the
    provider runs; and with no provider running, the refusal is clear and nothing hangs."""
    base = h.fresh("placeholder")
    source = base / "source"
    source.mkdir()
    env = h.env(h.cache_dir("placeholder"))
    res = h.fl(["add", h.url("plain.csv"), "--as", "p"], source, env)
    if res.code:
        raise RuntimeError(f"could not add the placeholder dataset: exit {res.code}: {last_lines(res.err)}")
    files = {name: (source / name).read_bytes() for name in ("data.toml", "data.lock")}
    # the same two files on a plain folder, as the placeholders will hold them
    plain = base / "plain"
    plain.mkdir()
    for name, data in files.items():
        (plain / name).write_bytes(data)

    def said(res, root) -> str:
        return res.out.replace(str(root), "<project>")

    control = h.fl(["status"], plain, env)
    h.note("control", f"status on plain files: exit {control.code}: {last_lines(control.out + control.err, 4)}",
           res=control)
    cloud = Cloud(base / "root")
    code = cloud.register()
    if code < 0:
        res = h.sh(["fltmc", "load", "cldflt"], timeout=60)
        h.note("cldflt", f"CfRegisterSyncRoot gave {hr(code)}; `fltmc load cldflt` exit {res.code}: {res.text[:200]}")
        code = cloud.register()
    if code < 0:
        raise Skip(f"no sync root can be registered here: CfRegisterSyncRoot gave {hr(code)}")
    try:
        code = cloud.connect()
        if code < 0:
            raise Skip(f"CfConnectSyncRoot gave {hr(code)}")
        probe = cloud.root / "probe"
        probe.mkdir()
        body = b"hydrated through the provider\n"
        made = cloud.place(probe, {"flagged.txt": body, "plain.txt": body})
        attributes = ctypes.windll.kernel32.GetFileAttributesW(str(probe / "flagged.txt"))
        h.note("probe-made", f"CfCreatePlaceholders {made}; attributes 0x{attributes & 0xffffffff:08x} "
               "(0x400000 recall on data access, 0x400 reparse point, 0x1000 offline)")
        plain_read = read_as_open_plain(probe / "plain.txt", flags=0)
        h.note("plain-open-reads", f"an open that follows: {plain_read}; provider calls {cloud.calls}")
        seen = read_as_open_plain(probe / "flagged.txt")
        h.check("open-plain-reads", seen.get("bytes") == body,
                "a read through open_plain's flags hydrates an online-only placeholder and gives its bytes",
                f"{ {k: v for k, v in seen.items() if k != 'bytes'} }; bytes {seen.get('bytes')!r}; "
                f"provider calls {cloud.calls}")
        h.note("open-plain-sees", f"attributes {seen.get('attributes')}, reparse tag {seen.get('tag')}: "
               "std's is_file() is false only for a tag with the name-surrogate bit (0x20000000)")
        again = read_as_open_plain(probe / "plain.txt")
        h.note("open-plain-hydrated", f"open_plain's flags on the placeholder the plain open hydrated: "
               f"{ {k: v for k, v in again.items() if k != 'bytes'} }, bytes {again.get('bytes')!r}")
        project = cloud.root / "project"
        project.mkdir()
        made = cloud.place(project, files)
        before = len(cloud.calls)
        res = h.fl(["status"], project, env, timeout=180)
        h.check("status-reads", res.code == control.code and said(res, project) == said(control, plain),
                "status on online-only placeholders of data.toml and data.lock answers as on plain files",
                f"exit {res.code} (plain {control.code}): {last_lines(res.out + res.err, 4)}; placeholders {made}; "
                f"provider calls {cloud.calls[before:]}", doc="docs/user/guides/windows.md#onedrive-and-synced-folders",
                res=res)
        h.note("status-hydrated", f"the provider served {len(cloud.calls) - before} requests: {cloud.calls[before:]}")
        cloud.disconnect()
        offline = cloud.root / "offline"
        offline.mkdir()
        made = cloud.place(offline, files)
        res = h.fl(["status"], offline, env, timeout=180)
        h.check("offline-refused", not res.timed_out and res.code != 0 and "data.toml" in res.err,
                "with no provider running, status refuses, naming data.toml, and ends",
                f"exit {res.code}, timed out {res.timed_out}: {last_lines(res.err, 3)}; placeholders {made}",
                res=res)
    finally:
        cloud.unregister()


JOURNEYS = [("shells", j_shells), ("held", j_held), ("junction", j_junction), ("longpath", j_longpath),
            ("onedrive", j_onedrive), ("refs", j_refs), ("fat32", j_fat32), ("swap", j_swap), ("placeholder", j_placeholder)]
