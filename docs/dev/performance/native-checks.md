# native checks

native runs check filesystem behavior, credential stores and resource costs that linux compilation cannot establish for windows or macos.

[the native runner](native.md) measures wall time with loopback fixtures. the checks below add byte verification and native observations; they do not claim a native performance result. use the final revisions selected for comparison, not intermediate group branches. cross-target linting does not execute the target, and a check that substitutes a C compiler does not validate native compilation or linking.

## prepare the machine

use an otherwise idle machine and an owned local folder outside every git repository, fetchloom project and synced folder. keep Defender, system tracing restrictions, long-path policy, power mode and descriptor limits unchanged. build and run correctness checks before reserving an exclusive measurement window; do not build, test or profile beside a timed run.

record both commits, toolchain, native target, executable SHA-256s, OS version, CPU, filesystem, storage device, power mode and relevant environment settings. `--version -v` adds the embedded commit when available. retain ordinary ntfs and dev-drive results separately, including Defender's real-time and performance modes. a cross-volume copy is not an apfs clone or a same-volume hardlink.

build each revision in its own checkout with the same [build prerequisites](../engineering/building-and-testing.md#build-requirements). copy its release executable to `baseline` and `candidate` under the private run folder, with `.exe` on windows. do not use wrappers for a process-startup claim.

in a fresh macos shell, from the checkout:

```sh
set -eu
native="$HOME/fetchloom-native-run-01"
mkdir "$native"
mkdir "$native/test-files" "$native/results"
export TMPDIR="$native/test-files" CARGO_BUILD_JOBS=8 RUST_TEST_THREADS=8
cargo build --locked --release --target-dir target
cp target/release/fetchloom "$native/candidate"
sw_vers
uname -m
mount
ulimit -n
shasum -a 256 "$native/candidate"
```

in a fresh windows powershell, from the checkout:

```powershell
$ErrorActionPreference = 'Stop'
$native = Join-Path $env:LOCALAPPDATA 'fetchloom-native-run-01'
if (Test-Path $native) { throw 'choose a new run folder' }
New-Item -ItemType Directory $native | Out-Null
New-Item -ItemType Directory "$native\test-files", "$native\results" | Out-Null
$env:TEMP = "$native\test-files"
$env:TMP = $env:TEMP
$env:CARGO_BUILD_JOBS = '8'
$env:RUST_TEST_THREADS = '8'
cargo build --locked --release --target-dir target
if ($LASTEXITCODE) { throw 'native build failed' }
Copy-Item target\release\fetchloom.exe "$native\candidate.exe"
Get-Volume
Get-MpComputerStatus | Select-Object RealTimeProtectionEnabled
Get-FileHash "$native\candidate.exe" -Algorithm SHA256
```

use the native host target, including arm64 on an arm64 machine. run the [full gate](../engineering/README.md#the-fast-loop) once on each host. for a focused rerun, these filters cover the group checks without depending on unpublished helper scripts:

| behavior | focused commands |
|---|---|
| cache claims, original writers, durable objects and root markers | `cargo test --locked --lib cache::` |
| archive readers, refused writers, staging and limits | `cargo test --locked --lib archive::` |
| DNS categories, IPv6 scope refusal, quotas and credential origins | `cargo test --locked --lib net::`; `cargo test --locked --lib auth::` |
| redirects and secret masking through the executable | `cargo test --locked --test cli secrets::`; `cargo test --locked --test cli proxy::` |
| concurrent cold downloads and gc exclusion | `cargo test --locked --test cli concurrent::`; `cargo test --locked --test cli cache::` |
| folder handles, link roots, long paths, copies and interruption | `cargo test --locked --lib place::`; `cargo test --locked --test cli placing::`; `cargo test --locked --test cli recovery::` |
| project pins, aliases, help routing and JSON | `cargo test --locked --test cli project::`; `cargo test --locked --test cli basics::`; `cargo test --locked --test cli hints::`; `cargo test --locked --lib cli::sync::disk::` |

check selected-test counts and native exit codes. windows cases include metadata crossing 260 characters, a refused 300-character working folder, non-BMP names and complete JSON failures. on macos, run composed/decomposed-name cases on apfs and hfs+, and distinguish aliases from separate files on case-sensitive apfs when available. use the [CI filesystem recipes](../engineering/ci.md#filesystem-runs) for owned virtual volumes; never format a physical disk for this check.

## compare whole builds

clear unrelated `FETCHLOOM_*` settings, especially offline, rate, split, threads and trace settings. record any setting deliberately kept. set `NO_PROXY=127.0.0.1,localhost` and remove unintended proxy overrides for loopback runs. use a new root and output directory for each invocation.

macos, with both binaries prepared:

```sh
python3 bench/native.py --baseline "$native/baseline" --candidate "$native/candidate" --root "$native/apfs-01" --scenarios small,zip --rounds 10 --out "$native/results/apfs-01"
```

windows, with both binaries prepared:

```powershell
py -3 bench/native.py --baseline "$native\baseline.exe" --candidate "$native\candidate.exe" --root "$native\ntfs-01" --scenarios small,zip --rounds 10 --out "$native\results\ntfs-01"
```

repeat on an existing, owned ReFS/dev-drive or exfat volume by putting `--root` there. verify the actual volume type first. on macos repeat on hfs+ and exfat when available. `large` adds a 1 GiB fixture by default; request it separately when there is enough space. ten pairs are a starting point, not proof that noise is low enough. retain every failed attempt and repeat selected comparisons with a second fresh root.

`small` puts one file in each of 10,000 dataset folders; it is not the one-folder placement control. `zip` places 10,000 members in one dataset. the runner reports wall time only and deletes its placed files. it does not independently verify them or preserve per-command JSON. do the following audit with each measured binary as well.

## verify the fixture bytes

save this snippet as `native_audit.py` in the checkout for the local check, without committing it. pass the checkout, a binary, the runner's retained `www` directory and a new audit root. it locks against a fresh loopback server, keeps separate preparation and measured-data caches, saves JSON and stderr, and compares placed files with the served fixture bytes after cold, warm and settled syncs. verification is outside timing.

```python
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import urllib.request
import zipfile


def digest(stream):
    value = hashlib.sha256()
    for block in iter(lambda: stream.read(1024 * 1024), b''):
        value.update(block)
    return value.hexdigest()


def verify(project, expected):
    project = Path(project)
    actual = {p.relative_to(project).as_posix(): p
              for p in (project / 'data').rglob('*') if p.is_file()}
    if set(actual) != set(expected):
        raise RuntimeError('placed paths differ from the fixture')
    for name, (size, sha256) in expected.items():
        with actual[name].open('rb') as stream:
            if actual[name].stat().st_size != size or digest(stream) != sha256:
                raise RuntimeError('wrong bytes: ' + name)


def audit(repo, binary, fixtures, root):
    repo, binary, fixtures, root = [Path(p).resolve() for p in (repo, binary, fixtures, root)]
    root.mkdir()
    sys.path.insert(0, str(repo / 'bench'))
    import native
    server = native.serve(fixtures)
    base = f'http://127.0.0.1:{server.server_port}'
    try:
        with urllib.request.urlopen(base + '/', timeout=5) as response:
            response.read()
        for source in sorted(p for p in fixtures.iterdir() if p.is_dir()):
            names = sorted(p.name for p in source.iterdir() if p.is_file() and p.name != '.complete')
            if not names:
                continue
            work = root / source.name
            template = work / 'template'
            template.mkdir(parents=True)
            (template / 'data.toml').write_text(native.manifest(base, source.name, names), encoding='utf-8')
            expected = {}
            for index, name in enumerate(names):
                prefix = f'data/f{index:05}/'
                if name == 'members.zip':
                    with zipfile.ZipFile(source / name) as archive:
                        for member in archive.infolist():
                            with archive.open(member) as stream:
                                expected[prefix + member.filename] = [member.file_size, digest(stream)]
                else:
                    with (source / name).open('rb') as stream:
                        expected[prefix + name] = [(source / name).stat().st_size, digest(stream)]
            (work / 'expected.json').write_text(json.dumps(expected), encoding='utf-8')

            def command(label, project, cache, arguments):
                done = subprocess.run([str(binary), '--json', '--yes', *arguments], cwd=project,
                                      env=native.environment(cache), capture_output=True,
                                      encoding='utf-8', timeout=1800)
                (work / (label + '.stdout')).write_text(done.stdout, encoding='utf-8')
                (work / (label + '.stderr')).write_text(done.stderr, encoding='utf-8')
                answer = json.loads(done.stdout)
                if done.returncode or answer['schema'] != 1 or answer['status'] != 'ok':
                    raise RuntimeError(label + ' failed; inspect the saved output')
                return answer

            command('lock', template, work / 'prepare-cache', ['lock'])
            cache = work / 'cache'
            for phase in ('cold', 'warm', 'settled'):
                project = work / ('warm' if phase == 'settled' else phase)
                if phase != 'settled':
                    project.mkdir()
                    for name in ('data.toml', 'data.lock'):
                        shutil.copyfile(template / name, project / name)
                args = ['sync', '--locked'] + ([] if phase == 'cold' else ['--offline'])
                answer = command(phase, project, cache, args)
                if answer['result']['placement']['kept']:
                    raise RuntimeError('sync kept a conflict')
                if phase != 'cold' and answer['work']['requests']:
                    raise RuntimeError('offline phase made requests')
                verify(project, expected)
            answer = command('path', project, cache, ['path', 'f00000', '--offline'])
            if Path(answer['result']) != project / 'data' / 'f00000':
                raise RuntimeError('path returned another folder')
            answer = command('status', project, cache, ['status', '--offline'])
            if any(value for value in answer['result'].values() if isinstance(value, list)):
                raise RuntimeError('status found differences')
            command('verify', project, cache, ['cache', 'verify', '--offline'])
    finally:
        server.shutdown()
        server.server_close()


if __name__ == '__main__':
    audit(*sys.argv[1:])
```

run it for both binaries, with separate audit roots. for example, on macos:

```sh
python3 native_audit.py "$PWD" "$native/baseline" "$native/apfs-01/www" "$native/audit-baseline"
python3 native_audit.py "$PWD" "$native/candidate" "$native/apfs-01/www" "$native/audit-candidate"
```

on windows use `py -3 native_audit.py $PWD.Path "$native\candidate.exe" "$native\ntfs-01\www" "$native\audit-candidate"`, and repeat for the baseline. the helper expects no shared top folder to strip from an archive, as in the native runner and the additional layout fixture below. do not use it as an arbitrary archive extractor.

## copies and folder layouts

for a one-folder and a 100-folder control, save this as `native_layouts.py` and give it a new fixture directory. both archives hold the same 10,000 distinct 4 KiB payloads; only member paths differ.

```python
import hashlib
from pathlib import Path
import sys
import zipfile

root = Path(sys.argv[1])
root.mkdir()
for folders in (1, 100):
    folder = root / f'folders-{folders}'
    folder.mkdir()
    with zipfile.ZipFile(folder / 'members.zip', 'w', compression=zipfile.ZIP_STORED) as archive:
        for index in range(10000):
            name = f'file-{index:05}.bin'
            if folders > 1:
                name = f'folder-{index % folders:03}/' + name
            info = zipfile.ZipInfo(name, date_time=(2020, 1, 1, 0, 0, 0))
            info.external_attr = 0o644 << 16
            archive.writestr(info, hashlib.shake_256(str(index).encode()).digest(4096))
```

create and audit those fixtures before timing. macos commands, with `py -3` and native paths substituted on windows:

```sh
python3 native_layouts.py "$native/layouts"
python3 native_audit.py "$PWD" "$native/candidate" "$native/layouts" "$native/audit-layouts"
```

for each sample, create a new project on the destination volume and copy only `data.toml` and `data.lock` from `audit-layouts/folders-1/template` or `folders-100/template`. set `FETCHLOOM_CACHE` to that layout's absolute `cache` folder. use `sync --locked --offline --json --yes`; save stdout, stderr, exit code and `work` method counts. prepare and verify outside the clock. never change permissions through an old project's hardlinks to reset a sample.

on macos, with a second owned volume mounted at `/Volumes/fetchloom-checks`, a copy sample is:

```sh
project=/Volumes/fetchloom-checks/copy-01
mkdir "$project"
cp "$native/audit-layouts/folders-1/template/data.toml" "$native/audit-layouts/folders-1/template/data.lock" "$project/"
export FETCHLOOM_CACHE="$native/audit-layouts/folders-1/cache"
/usr/bin/time -lp "$native/candidate" -C "$project" --json --yes --offline sync --locked > "$native/results/copy-01.json" 2> "$native/results/copy-01.time"
python3 -c 'import json,sys; from native_audit import verify; verify(sys.argv[1], json.load(open(sys.argv[2])))' "$project" "$native/audit-layouts/folders-1/expected.json"
```

on windows choose a fresh project on an existing second volume, copy the same two template files with `Copy-Item`, set `$env:FETCHLOOM_CACHE` to the source cache, and use the process measurement below. require `work.copies == 10000`, `work.reflinks == 0` and `work.hardlinks == 0` for a forced-copy sample. otherwise the sample did not measure copy flushing. a same-volume apfs control should use clones when supported; read-only ntfs/ReFS controls try hardlinks first.

repeat one-folder and 100-folder samples at `FETCHLOOM_THREADS` 1, 2, 4 and 8. repeat on exfat rather than transferring a linux driver result to the native driver. collect both CPU and wall time. include handle counts for many folders: placement keeps one root handle and opens each parent as it needs it; low-descriptor-limit behavior has separate regression coverage.

## native memory and CPU

for 100,000 small files, use a fresh root and the runner's existing scenario definition, then audit its fixtures as above:

```sh
python3 -c "import sys; sys.path.insert(0,'bench'); import native; native.SCENARIOS['small']['count']=100000; native.main()" --baseline "$native/baseline" --candidate "$native/candidate" --root "$native/100k-01" --scenarios small --rounds 10 --out "$native/results/100k-01"
```

on windows use `py -3`, `.exe` paths and powershell variables. retain the command because the file count differs from the default. use a fresh project copied from the audit template, with its populated cache, for each memory sample. `/usr/bin/time -l` on macos reports maximum resident set size in bytes and user/system CPU time. do not compare it with requested heap allocations from a linux allocation profiler.

in powershell, after setting `$binary`, `$project` and `$env:FETCHLOOM_CACHE` to that sample's absolute paths:

```powershell
$start = New-Object System.Diagnostics.ProcessStartInfo
$start.FileName = $binary
$start.Arguments = '--json --offline --yes sync --locked'
$start.WorkingDirectory = $project
$start.UseShellExecute = $false
$start.RedirectStandardOutput = $true
$start.RedirectStandardError = $true
$process = New-Object System.Diagnostics.Process
$process.StartInfo = $start
$watch = [Diagnostics.Stopwatch]::StartNew()
[void]$process.Start()
$out = $process.StandardOutput.ReadToEndAsync()
$err = $process.StandardError.ReadToEndAsync()
$peak = 0L
while (-not $process.WaitForExit(5)) {
    try {
        $process.Refresh()
        $peak = [Math]::Max($peak, $process.PeakWorkingSet64)
    } catch {
        if (-not $process.HasExited) { throw }
    }
}
$process.WaitForExit()
$watch.Stop()
$out.Result | Set-Content -Encoding utf8 "$native\results\memory.stdout"
$err.Result | Set-Content -Encoding utf8 "$native\results\memory.stderr"
[pscustomobject]@{
    wall_seconds = $watch.Elapsed.TotalSeconds
    cpu_seconds = $process.TotalProcessorTime.TotalSeconds
    sampled_peak_working_set_bytes = $peak
    exit = $process.ExitCode
} | ConvertTo-Json | Set-Content -Encoding utf8 "$native\results\memory.json"
$process.Dispose()
```

use a new result filename per sample. sampling can miss the final memory peak of a short process; report that limit rather than calling zero a measured peak. record median, range and sample coefficient of variation across paired runs: sample standard deviation divided by the mean. a linux no-body job-memory checkpoint does not establish a windows or macos memory budget.

## durability and system activity

tracing changes costs, so collect traces in separate, untimed runs after the ordinary comparisons.

| platform | check and expected boundary |
|---|---|
| macos cache roots | run `cache --json` twice with a fresh owned cache. `xattr -p com.apple.metadata:com_apple_backup_excludeItem "$FETCHLOOM_CACHE"` reports `com.apple.backupd`, and `.metadata_never_index` and `CACHEDIR.TAG` exist. inspect first and second initialization with `sudo fs_usage -w -f filesys fetchloom`; root markers should not be rewritten on the second open |
| macos writer flushes | use `dtruss -f` where policy permits. distinguish `F_FULLFSYNC` (fcntl command 51) from other fcntl calls. a standalone payload flushes through its original writer; archive members get plain fsync before the batch full flush and publication. metadata has separate flushes. include inline/refused archive writers and an external filesystem that refuses full flush; retain the documented plain-fsync limit |
| windows writer handles | in Process Monitor, filter `Process Name is fetchloom.exe` and the owned cache/project paths. retain PML and CSV for `CreateFile`, `WriteFile`, `FlushBuffersFile` and `CloseFile`. each new `.part` or archive `.new` payload has one successful write-capable open, not a second write open of its digest path. copies flush before publication; there is no filesystem-wide windows batch-flush promise |
| windows Defender | in a separate elevated recorder terminal, use `New-MpPerformanceRecording -RecordTo <owned-path>`, run the small-file/archive case, then stop recording and use `Get-MpPerformanceReport -Path <owned-path> -TopFiles 20`. retain the ETL, protection mode, bytes and file count. do not disable protection or add exclusions |

inspect readonly attributes and hidden-name cleanup after copies, including long paths and junction refusal. if tracing is denied, record the missing evidence; do not weaken system policy. repeat macos root initialization on a filesystem without extended attributes to check the tag and spotlight-marker fallback.

fetchloom does not set windows `NotContentIndexed` on its cache root. to measure that separate proposal, change only an owned root, retain its original flags and restore them in `finally`:

```powershell
$root = (Resolve-Path $env:FETCHLOOM_CACHE).Path
$old = [IO.File]::GetAttributes($root)
try {
    [IO.File]::SetAttributes($root, $old -bor [IO.FileAttributes]::NotContentIndexed)
    & $binary --json cache
} finally {
    [IO.File]::SetAttributes($root, $old)
}
```

place the chosen cold-cache workload inside that `try` block when measuring it, and compare with a fresh root carrying the original flags. keep the same bytes and protection settings, and record indexer activity alongside wall and CPU time. changing the root flag recursively is not part of this proposal; reading an existing root with an open-options flag does not set it.

## credential stores and network behavior

use a disposable OS account, never an account holding real source credentials. in that account, with the candidate on PATH:

```sh
printf 'synthetic-only\n' | fetchloom auth login zenodo --with-token --offline --yes
fetchloom auth --offline --json
fetchloom auth logout zenodo --yes
```

in powershell the first line is `'synthetic-only' | fetchloom auth login zenodo --with-token --offline --yes`. check keychain or credential-manager prompts and the reported storage origin without recording the key. offline status has `valid: null`, not proof that the synthetic key works. repeat after setting `FETCHLOOM_KEYRING=0`, check file permissions or inherited ACLs, then log out and restore the variable. no real service request is needed.

native DNS and proxy tests must execute on the host: overlong names, unknown-host categories, numeric IPv6 scope refusal and normalized account masking are not established by cross-target compilation. the loopback runner has one initial hostname and no shaped link, so it cannot establish host-interleaving, HTTP/2-versus-HTTP/1.1 or lossy-link gains. use the [lab profiles](benchmark-design.md#network-profiles) and retain lan/home controls alongside far/poor cases.

for an optional live comparison, pin the complete reference and every selected path before running. the 200-file hugging face probe uses `hf:datasets/huggingface/documentation-images@dd11cba61e91b894782c9ea63fbd89456b758082`; the exact 200-path selection must accompany the result. pass each as `--select`, keep `--no-extract`, and compare `FETCHLOOM_JOBS=1` with the unset default using fresh caches and outputs. keep per-file hashes, requests, redirects, traces and failures. the repository does not include that selection, so this page supplies no reproducible live timing claim without it.

a background-write control also needs an exclusive window: use only a newly created owned 2 GiB file beside the cache, join its writer before the next sample, and retain ten paired rounds and the exact pressure command. this control measures shared device and journal pressure, not isolated per-file durability; do not substitute an unrelated stress tool or infer a wall gain from syscall counts.

## follow-ups for adopted changes

these changes were measured on linux only. check each natively before relying on it there:

| change | native follow-up |
|---|---|
| lean partials for small downloads | cold small files and archives; an interrupted download still resumes; count writer opens, since defender scans each new file when it is closed |
| macos copy batching | forced copies on a second volume (below): 10,000 copies, the flush before any name, then a same-volume clone control |
| folder-aware placement limits | one and 100 folders and exfat; report cpu as well as wall time |
| parallel status scan, commented lock reader, lazy jobs, ready waiters | 10,000- and 100,000-file projects: time, memory and exact bytes |
| several connections per host, early permit release, host interleaving | multi-host or shaped-link workloads; the native runner alone does not exercise them |

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

## keep evidence and clean up

retain commands, settings, binary hashes, native runner reports, audit outputs, expected hashes, per-run resource records and trace captures. label unexecuted native checks and unavailable probes explicitly. do not fill missing measurements from cross-target checks or historical timings.

stop local servers and recorders, join background writers, detach only virtual images created for the check and verify that their mount points are gone. restore changed attributes and environment settings. remove only the owned workloads and local helper scripts after copying wanted evidence; never clean up through a shared cache or another project's hardlinks.
