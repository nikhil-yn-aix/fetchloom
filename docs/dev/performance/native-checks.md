# native checks

native runs verify filesystem behavior that cross-compilation cannot exercise.

## macos copy durability

copies on one output filesystem get plain `fsync` per file and one `F_FULLFSYNC` before publication. apfs clones do not need a payload flush. use a separate output filesystem to force copies; otherwise a same-volume apfs run only checks clones.

from a checkout, build with `cargo build --release --locked`. create a private fixture and image; the commands refuse an existing image rather than formatting a physical disk:

```sh
mkdir -p native/www native/mount
python3 - <<'PY'
from pathlib import Path
from zipfile import ZipFile, ZipInfo, ZIP_STORED
with ZipFile('native/www/members.zip', 'w', compression=ZIP_STORED) as archive:
    for index in range(1000):
        entry = ZipInfo(f'member-{index:04}.bin', (2020, 1, 1, 0, 0, 0))
        entry.external_attr = 0o644 << 16
        archive.writestr(entry, index.to_bytes(4, 'little') * 1024)
PY
hdiutil create -size 1g -fs APFS -volname fetchloom-check native/apfs.dmg
hdiutil attach -nobrowse -mountpoint "$PWD/native/mount" native/apfs.dmg
export FETCHLOOM_CACHE="$PWD/native/cache"
export BIN="$PWD/target/release/fetchloom"
mkdir -p native/mount/project
printf '[datasets.files]\nref = "http://127.0.0.1:8765/members.zip"\n' > native/mount/project/data.toml
```

in a second terminal, serve the fixture in the foreground with `python3 -m http.server 8765 --bind 127.0.0.1 --directory native/www`. use a free port, changing the manifest if needed. run an untimed setup sync:

```sh
(cd native/mount/project && "$BIN" --yes --json sync) > native/setup.json
```

require `work.copies` to be 1000 and `work.reflinks` to be zero. remove only `native/mount/project/data`, leaving the lock and cached members, before each warm run. run `sync --locked --offline --json` from that project. when system policy permits, capture one untimed run with `sudo dtruss -f`: check for a plain fsync per copied file, one full-drive flush before publication, and no new opens for writing after a file's writer closes. record any tracing restriction rather than changing system policy.

verify every placed member after the run:

```sh
python3 - <<'PY'
from pathlib import Path
for index in range(1000):
    path = Path('native/mount/project/data/files') / f'member-{index:04}.bin'
    assert path.read_bytes() == index.to_bytes(4, 'little') * 1024, path
PY
```

also run `cargo test --locked --lib place::` on the mac. the tests cover flush failure, cancellation before naming, and completion of the copy batch. repeat same-volume placement separately and require clones to remain clones without payload flushes. repeat on a private exfat image for filesystem-name detection and serialized placement. a drive that rejects `F_FULLFSYNC` must still complete each file's plain fsync before publication.

stop the foreground server, detach the private image with `hdiutil detach native/mount`, remove only `native/apfs.dmg`, and restore the prior environment settings. repeat image setup with `-fs ExFAT` and a new filename if testing that driver.

## windows placement

run the placement tests on ntfs, refs/dev drive and exfat. record the actual filesystem and defender settings. readonly placements should hardlink where supported; writable and sync-client placements must remain independent files. use a source cache on another volume to force copies. Process Monitor should show each copy written through one original handle and `FlushFileBuffers` completed before publication. different workers may flush concurrently; there is no filesystem-wide batch guarantee.

check junction roots, aliases above a selected root, paths longer than 260 characters, low thread quotas and cleanup after interruption. directory handles must prevent replacement while their paths are used. fat and exfat should use one placement worker; other filesystems allow at most four mutating workers per parent, within the command's total ceiling. verify the filesystem probe on machines with disconnected removable drives too.

for performance comparisons, use the [native runner](../../../bench/native.py) with two release builds, ten alternating rounds, and a private root on the filesystem being tested. retain method counts and byte checks alongside CPU and wall results. linux loop-image results do not establish native driver performance.
