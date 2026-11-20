"""o2 probe, scratch only: what a ReFS volume says to the delete calls rust's std and fetchloom make
on a read-only file, beside ntfs folders on the same runner. ctypes only, no fetchloom build.

rust's std unlink (library/std/src/sys/fs/windows.rs) tries DeleteFileW, and on ACCESS_DENIED opens
the file with DELETE and sets FILE_DISPOSITION_INFO_EX with DELETE | POSIX_SEMANTICS |
IGNORE_READONLY_ATTRIBUTE, returning the first error if that fails. fetchloom's cache publishes an
object by hard-linking a sealed (read-only) .part to its object name, then removing the .part.
"""

import ctypes
import json
import os
import platform
import stat
import subprocess
import sys
from ctypes import wintypes as w
from pathlib import Path

k = ctypes.WinDLL("kernel32", use_last_error=True)
k.CreateFileW.restype = w.HANDLE
k.CreateFileW.argtypes = [w.LPCWSTR, w.DWORD, w.DWORD, ctypes.c_void_p, w.DWORD, w.DWORD, w.HANDLE]
k.SetFileInformationByHandle.restype = w.BOOL
k.SetFileInformationByHandle.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
k.DeleteFileW.restype = w.BOOL
k.DeleteFileW.argtypes = [w.LPCWSTR]
k.CloseHandle.restype = w.BOOL
k.CloseHandle.argtypes = [w.HANDLE]

INVALID = w.HANDLE(-1).value
DELETE = 0x00010000
READ_ATTRIBUTES = 0x80
WRITE_ATTRIBUTES = 0x100
SHARE_ALL = 0x7
OPEN_EXISTING = 3
OPEN_REPARSE_POINT = 0x00200000
FILE_BASIC_INFO_CLASS = 0
FILE_DISPOSITION_INFO_CLASS = 4
FILE_DISPOSITION_INFO_EX_CLASS = 21
NORMAL = 0x80

FLAG_DELETE = 0x1
FLAG_POSIX = 0x2
FLAG_IGNORE_READONLY = 0x10

NAMES = {5: "ACCESS_DENIED", 87: "INVALID_PARAMETER", 50: "NOT_SUPPORTED", 1: "INVALID_FUNCTION",
         32: "SHARING_VIOLATION", 2: "FILE_NOT_FOUND"}


class BasicInfo(ctypes.Structure):
    _fields_ = [("creation", ctypes.c_longlong), ("access", ctypes.c_longlong),
                ("write", ctypes.c_longlong), ("change", ctypes.c_longlong), ("attributes", w.DWORD)]


def err(code: int) -> str:
    return f"error {code} {NAMES.get(code, '')}".strip()


def open_handle(path: Path, access: int):
    handle = k.CreateFileW(str(path), access, SHARE_ALL, None, OPEN_EXISTING, OPEN_REPARSE_POINT, None)
    if handle in (None, INVALID):
        return None, ctypes.get_last_error()
    return handle, 0


def disposition_ex(path: Path, flags: int) -> str:
    handle, code = open_handle(path, DELETE)
    if handle is None:
        return f"open {err(code)}"
    value = ctypes.c_ulong(flags)
    ok = k.SetFileInformationByHandle(handle, FILE_DISPOSITION_INFO_EX_CLASS, ctypes.byref(value), 4)
    code = 0 if ok else ctypes.get_last_error()
    k.CloseHandle(handle)
    return "ok" if ok else err(code)


def std_unlink(path: Path) -> str:
    """rust std's unlink, step by step."""
    if k.DeleteFileW(str(path)):
        return "DeleteFileW ok"
    first = ctypes.get_last_error()
    if first != 5:
        return f"DeleteFileW {err(first)}"
    second = disposition_ex(path, FLAG_DELETE | FLAG_POSIX | FLAG_IGNORE_READONLY)
    return f"DeleteFileW {err(first)}, then posix delete: {second}"


def clear_then_delete(path: Path) -> str:
    """the fix's way: clear the read-only flag through a handle, then delete the name."""
    handle, code = open_handle(path, READ_ATTRIBUTES | WRITE_ATTRIBUTES)
    if handle is None:
        return f"open {err(code)}"
    info = BasicInfo(0, 0, 0, 0, NORMAL)
    ok = k.SetFileInformationByHandle(handle, FILE_BASIC_INFO_CLASS, ctypes.byref(info), ctypes.sizeof(info))
    cleared = "ok" if ok else err(ctypes.get_last_error())
    deleted = "ok" if k.DeleteFileW(str(path)) else err(ctypes.get_last_error())
    k.CloseHandle(handle)
    return f"clear {cleared}, DeleteFileW with the handle open {deleted}"


def classic_disposition(path: Path) -> str:
    handle, code = open_handle(path, DELETE)
    if handle is None:
        return f"open {err(code)}"
    value = ctypes.c_ubyte(1)
    ok = k.SetFileInformationByHandle(handle, FILE_DISPOSITION_INFO_CLASS, ctypes.byref(value), 1)
    code = 0 if ok else ctypes.get_last_error()
    k.CloseHandle(handle)
    return "ok" if ok else err(code)


def make(path: Path, readonly: bool, linked_to: Path | None = None) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"probe")
    if linked_to is not None:
        linked_to.parent.mkdir(parents=True, exist_ok=True)
        os.link(path, linked_to)
    if readonly:
        os.chmod(path, stat.S_IREAD)


def describe(path: Path | None) -> str:
    if path is None:
        return ""
    if not path.exists():
        return "; other name gone"
    readonly = not os.stat(path).st_mode & stat.S_IWRITE
    return f"; other name there, read-only {readonly}"


def scrub(folder: Path) -> None:
    for item in sorted(folder.rglob("*"), reverse=True):
        try:
            if item.is_file():
                os.chmod(item, stat.S_IWRITE)
                item.unlink()
            else:
                item.rmdir()
        except OSError:
            pass


def probe(folder: Path) -> list[dict]:
    rows = []
    folder.mkdir(parents=True, exist_ok=True)
    cases = [
        ("DeleteFileW, plain read-only", False, lambda p: "ok" if k.DeleteFileW(str(p)) else err(ctypes.get_last_error())),
        ("std unlink, plain read-only", False, std_unlink),
        ("std unlink, read-only with a second name (the .part)", True, std_unlink),
        ("ex delete|posix|ignore-ro, read-only", False, lambda p: disposition_ex(p, FLAG_DELETE | FLAG_POSIX | FLAG_IGNORE_READONLY)),
        ("ex delete|ignore-ro, read-only", False, lambda p: disposition_ex(p, FLAG_DELETE | FLAG_IGNORE_READONLY)),
        ("ex delete|posix, read-only", False, lambda p: disposition_ex(p, FLAG_DELETE | FLAG_POSIX)),
        ("ex delete, read-only", False, lambda p: disposition_ex(p, FLAG_DELETE)),
        ("classic disposition, read-only", False, classic_disposition),
        ("clear then delete, plain read-only", False, clear_then_delete),
        ("clear then delete, read-only with a second name", True, clear_then_delete),
    ]
    writable = [
        ("ex delete|posix, writable", lambda p: disposition_ex(p, FLAG_DELETE | FLAG_POSIX)),
        ("ex delete, writable", lambda p: disposition_ex(p, FLAG_DELETE)),
    ]
    for number, (name, linked, action) in enumerate(cases):
        path = folder / "partial" / f"case{number}.part"
        other = folder / "objects" / "fd" / f"case{number}" if linked else None
        make(path, True, other)
        result = action(path)
        rows.append({"case": name, "result": result, "left": path.exists(), "other": describe(other)})
    for number, (name, action) in enumerate(writable):
        path = folder / "partial" / f"writable{number}.part"
        make(path, False)
        result = action(path)
        rows.append({"case": name, "result": result, "left": path.exists(), "other": ""})
    scrub(folder)
    return rows


def powershell(command: str) -> str:
    done = subprocess.run(["powershell", "-NoProfile", "-Command", command], capture_output=True, text=True)
    return (done.stdout + done.stderr).strip()


def refs_volume(work: Path) -> str | None:
    letter = next((d for d in "RSTUVWXYZ" if not os.path.exists(f"{d}:\\")), None)
    if letter is None:
        return None
    image = work / "refs.vhdx"
    script = work / "diskpart.txt"
    script.write_text("\n".join([f'create vdisk file="{image}" maximum=51200 type=expandable',
                                 f'select vdisk file="{image}"', "attach vdisk", "create partition primary",
                                 f"assign letter={letter}"]) + "\n", encoding="ascii")
    print(subprocess.run(["diskpart", "/s", str(script)], capture_output=True, text=True).stdout[-400:])
    print(powershell(f"Format-Volume -DriveLetter {letter} -FileSystem ReFS -NewFileSystemLabel o2probe -Confirm:$false -Force | Format-List DriveLetter,FileSystem,Size"))
    return letter


def main() -> int:
    work = Path(os.environ.get("RUNNER_TEMP", ".")) / "o2probe"
    work.mkdir(parents=True, exist_ok=True)
    print(f"windows {platform.version()} {platform.machine()}; python {sys.version.split()[0]}")
    letter = refs_volume(work)
    folders = []
    if letter:
        print(powershell(f"(Get-Volume -DriveLetter {letter}).FileSystem"))
        print(subprocess.run(["fsutil", "fsinfo", "refsinfo", f"{letter}:"], capture_output=True, text=True).stdout.strip())
        folders.append(("ReFS", Path(f"{letter}:\\o2probe")))
    folders.append(("NTFS work", work / "ntfs"))
    folders.append(("NTFS C:", Path("C:\\o2probe")))
    results = {}
    for label, folder in folders:
        rows = probe(folder)
        results[label] = rows
        print(f"== {label} ({folder})")
        for row in rows:
            print(f"   {row['case']}: {row['result']}; file left {row['left']}{row['other']}")
    print("O2-JSON " + json.dumps(results))
    return 0


if __name__ == "__main__":
    sys.exit(main())
