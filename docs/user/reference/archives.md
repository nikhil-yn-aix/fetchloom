# Archives

fetchloom unpacks `.zip`, `.7z`, `.tar`, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2`, `.tgz`, `.txz`, `.tzst`, `.tbz` and `.tbz2`, ignoring case; the bytes choose the decoder. other files stay as they are, including `.fastq.gz`, `.xlsx`, `.npz` and `.pt`.

a `.rar`, and an archive inside another, stay packed. the add that pins one, and each sync that places it, as a teammate's first sync does, warn once a run with a command for your system that unpacks it beside itself in the data folder, where sync keeps the files and `status` lists them as extra; [compatibility](archives.md) shows each command.

an unsafe, damaged or encrypted archive fails to unpack, as does a 7z compressed with a method fetchloom does not read, such as ppmd. a zip or a tar that ends early, as an upload stopped part way leaves one, fails as `the archive is cut short: a zip lists its files at its end, and that list is missing` or `the archive is damaged: it ends in the middle of a file`. unsafe means a member that would land outside its folder, a symlink, a device or a fifo, or a tar hardlink to anything but a file before it. an archive past the [limits](limits.md) on files, size or compression ratio is refused too. a refused archive can still be kept packed.

`--no-extract` with `add` or `get`, or `extract = false` in data.toml, keeps archives packed. select and exclude patterns match the files a source lists, so an archive and not the files inside it; a [members table](data-toml.md#files-inside-archives) chooses those by their paths inside the archive.

where the files land, relative to the dataset or `get` folder:

| selected files | members in `train.zip` | placed paths |
|---|---|---|
| only `train.zip` | `train/a.csv`, `train/b.csv` | `a.csv`, `b.csv` |
| only `raw/train.zip` | `train/a.csv` | `raw/a.csv` |
| `train.zip` and another file | `train/a.csv`, `train/b.csv` | `train/a.csv`, `train/b.csv` |
| `train.zip` and another file | only `a.csv` | `a.csv` |
| `train.zip` and another file | `a.csv`, `b.csv` | `train/a.csv`, `train/b.csv` |

a sole archive loses one top folder its members share. beside other selected files, a shared top folder or a lone top-level file keeps its name; otherwise members go under the archive's name without its extension. an archive in a folder unpacks into that folder. files a system writes by itself, such as finder's `.DS_Store` and `__MACOSX` or windows' `Thumbs.db`, are left out, as a folder import leaves them out, and one `Skipped` line counts them, as in `Skipped recordings/.DS_Store and 2 more the system makes in finder/finder.zip`. two members, or a member and another dataset's file, that land on one path fail before anything is placed. empty folders are not placed, and an archive with no files makes no dataset folder.

members enter the cache before they are placed, and the archive's own bytes then leave it, unless a project needs them packed. [the tree hash](data-lock.md#the-tree-hash) records member paths before placement drops a top folder.

fetchloom unpacks zip, 7z and tar archives itself on every system. it leaves a `.rar` packed, and the warning's help names a program for the system it runs on and a command that unpacks the archive into a folder beside it, inside the data folder, which fetchloom's `.gitignore` keeps out of git. for `data/sample/sample.rar`, run from the project's folder:

| system | program | the help |
|---|---|---|
| linux | unrar, rarlab's free reader, from the distribution's packages: `unrar` in ubuntu's multiverse, debian's non-free, rpm fusion and arch | `unrar x data/sample/sample.rar data/sample/sample-unpacked/` |
| macos | unar, from homebrew | `brew install unar`, then `unar -D -o data/sample/sample-unpacked data/sample/sample.rar` |
| windows | 7-Zip, from winget | `winget install 7zip.7zip`, then, in powershell, `& "C:\Program Files\7-Zip\7z.exe" x data\sample\sample.rar -odata\sample\sample-unpacked` |

rar volumes, `clips.part1.rar` and on, take the same help for their first volume, from which unrar and 7-Zip read the others. a zip spanned over `.z01` files beside its `.zip` stays packed as well: on linux and macos its help joins the parts with Info-ZIP's `zip -s 0 data/scans/scans.zip --out data/scans/scans-joined.zip` and unpacks that with `unzip data/scans/scans-joined.zip -d data/scans/scans-unpacked`, and on windows it runs 7-Zip, which reads spanned zips since 16.02, as `x data\scans\scans.zip -odata\scans\scans-unpacked`.

an archive that came inside another stays packed as well, since fetchloom unpacks no archive inside another, and its help uses what the system has. for `data/x/inner.zip`, `data/x/train.tar.gz` or `data/x/scans.7z`:

| system | the help |
|---|---|
| linux | `unzip data/x/inner.zip -d data/x/inner-unpacked`; gnu tar for a tar, as `mkdir -p data/x/train-unpacked && tar -xf data/x/train.tar.gz -C data/x/train-unpacked`; bsdtar, from the distribution's packages, for a 7z, which gnu tar does not read |
| macos | the unzip macos has for a zip, and its tar, a libarchive build, for the others |
| windows | powershell's `Expand-Archive data\x\inner.zip -DestinationPath data\x\inner-unpacked` for a zip, and windows' `tar.exe` for the others |

unzip 6.00, gnu tar 1.35 and bsdtar 3.7.2 unpacked zip, `.tar.gz`, `.tar.xz`, `.tar.zst`, `.tar.bz2` and 7z samples this way on linux. a rar inside another archive gets the rar's help above.

the unpacked files are yours to fetchloom, as notes you add would be: sync keeps them, `status` lists them as extra, and `git add .` leaves them out with the rest of the data folder. a `fetchloom get` folder is the exception: its help unpacks next to that folder, `sample-unpacked` beside `sample`, since a later `get` refuses a folder holding files it did not place.

macos's own `tar` and windows' own `tar.exe` are libarchive builds, as is `bsdtar`, and libarchive 3.7.2 refused solid rar4 and rar2 archives, rar5 files with delta or x86 filters and rar5 volumes that unrar read, with `bsdtar: RAR solid archive support unavailable` or `Unsupported block header size`, so the help names programs that read them. unar 1.10.1, which ubuntu 24.04 packages, printed `Failed! (Attempted to read more data than was available)` for each file of a solid rar5 that unrar 7.00, unar 1.10.8 and 7-Zip 26.06 unpacked to the same bytes; use unrar there, unar 1.10.8 or later, or 7-Zip from [7-zip.org](https://www.7-zip.org/download.html), as `7zz x data/sample/sample.rar -odata/sample/sample-unpacked`. unrar makes `data/sample/sample-unpacked/` when the trailing `/` names a folder; unar's `-D` puts the files straight into it, as 7-Zip's `-o` does. [decisions](../../dev/decisions/storage-and-requests.md) links the sources.

## split archives

a big dataset may come as one archive cut into parts, such as `scans.zip.001` to `scans.zip.003` on zenodo, or `train.tar.gz.aa` and `.ab` in a hugging face repository. `split` and 7-Zip cut an archive's bytes, so the parts read one after another are the archive. when a dataset lists every part, fetchloom unpacks them as that one archive and places its files, not the parts. a dataset that is one split archive gets the files straight in its folder, and one beside other files puts them under the name before the counter, `scans/` for `scans.zip.001`. data.lock pins each part as a file, and a [members table](data-toml.md#files-inside-archives) or `fetchloom drop` names the archive by its first part, as in `[datasets.scans.members."scans.zip.001"]`.

parts belong together only inside one dataset and one folder, after the name of an archive fetchloom unpacks and with a counter as `split` or 7-Zip writes it, such as `aa`, `00`, `000` or `001`, so `weights.bin.001` and `data.tar.gz.1` stay files; [split archives in data.lock](#split-archives) gives the exact rules. stderr from `fetchloom add ../zipped`, a folder holding `headers.zip.00` to `.02` that `split -b 256K -d` made of a zip of 300 files:

```text
   Imported zipped · 3 files · 748.6 KiB, copied into the cache; ../zipped is unchanged
   Unpacked zipped/headers.zip.00 · 300 files · 2.2 MiB in 522 ms
```

what stays packed gets one warning with what to do. stderr from `fetchloom add` of a folder missing `headers.zip.01`, of a folder of rar volumes and a zip spanned over `.z01` files, and of `add -i` with one link for each part:

```text
warning: gap/headers.zip.01 is missing, so the parts of headers.zip stay as they are
  help: fetchloom joins a split archive only from every one of its parts: check that select and exclude keep them all, or ask the dataset's authors for the missing one
warning: volumes/clips.part1.rar stays packed with 7 other volumes of its rar, since fetchloom does not unpack rar
  help: unpack it with unrar from your package manager: `unrar x data/volumes/clips.part1.rar data/volumes/clips.part1-unpacked/`. extract = false stops this warning
warning: volumes/maps.zip stays packed with 1 other file it spans, since fetchloom does not unpack a zip spanned over several files
  help: join its parts with zip from your package manager, then unpack them: `zip -s 0 data/volumes/maps.zip --out data/volumes/maps-joined.zip`, then `unzip data/volumes/maps-joined.zip -d data/volumes/maps-unpacked`. extract = false stops this warning
warning: headers-zip/headers.zip.00 stays packed, since it does not unpack on its own
  help: it may be the first part of an archive split into several files, which fetchloom joins only inside one dataset, such as a folder or a record that lists every part
```

with a part missing, the other parts are placed as plain files. rar volumes and spanned zips are not cuts of the bytes, so only their own tools join them. a first part alone, as `add -i` makes each link a dataset of its own, is unpacked when its bytes are a whole archive, and otherwise stays packed with the last warning above. `extract = false` and `--no-extract` keep the parts packed, and turning `extract` back on unpacks them from the cache with no request.

an archive cut into parts is pinned as its parts, each an ordinary file, with `unpacks_to` on the first part alone: the tree of the archive the parts make read in order. no field says which files are parts; their names do, so a reader or `export` finds them again:

- parts share one dataset, one folder and the path before their counters, which names an archive fetchloom unpacks: `.zip`, `.7z`, `.tar` or a compressed tar. a file whose own name is such an archive is never a part.
- between that path and the counter comes `.`, `.part`, `.part-`, `.part_` or `.part.`. a counter is digits or lowercase letters, at least two of them, though one digit may follow a `part`.
- counters sort by length, then as text, and follow one numbering from the first part: zero-padded numbers from 0 or 1, growing past their width as 7-Zip's `.1000` follows `.999`; letters from all `a`s, of one width; or GNU split's own, two characters until the first would be the alphabet's last, which then stays as two more follow, so `9000` follows `89` and `zaaa` follows `yz`.

with `extract = false`, or a part missing, no part carries `unpacks_to` and each is a plain file. rar volumes and zips spanned over `.z01` files are no cut of the bytes and stay packed. the cache keeps what a split archive unpacked to under a key its parts give together, so parts that changed, or one more part, never find the record of other parts, as [archives](../../dev/architecture/archives.md#split-archives) describes.

## webdataset shards

webdataset splits a large dataset into numbered tar files, shards such as `train-000000.tar` to `train-001023.tar`, which its loaders read packed. unpacked, each shard becomes its many small files, each taking an inode, which a cluster's quota counts. so before anything downloads, `add`, `get` and a `sync` that adds or changes a dataset print a hint when the files to unpack include at least 8 tars in one folder named `<prefix><digits>.tar`, `.tar.gz` or `.tgz`, with one prefix and one digit width. in a project, the hint names the one [members table](data-toml.md#one-table-for-many-shards) that keeps only some files of every shard. stderr from `fetchloom add s3://wds/shards/` with a local stand-in for the bucket, on a debug build:

```text
   Resolved s3://wds/shards/ · 1,024 files · 10.0 MiB
  hint: shards holds 1,024 numbered .tar shards, which webdataset loaders read packed. keep them packed with --no-extract, or extract = false in data.toml. to unpack them and keep only some files of each, write one members table, [datasets.shards.members."train-*.tar"]
 Downloaded 1,024 files · 10.0 MiB in 15.5 s · 659.5 KiB/s
```

`sync` names only `extract = false in data.toml`, and `get` only `--no-extract`, since `get` keeps no project for a members table. the hint shows for a new dataset, or when a change to its entry lists the source again, but not for a dataset a members table already covers, nor with `-q`, `--json` or `FETCHLOOM_NO_HINTS=1`. turning `extract` off later needs no new listing, but the shards download again, since sync drops an archive's own bytes once its files are in.

`add` unpacks a `.tar` shard into its dataset's folder, where a [members table](data-toml.md#files-inside-archives) in data.toml keeps only some of its files, one table for every shard under a [glob key](data-toml.md#one-table-for-many-shards) such as `"shard-*.tar"`. webdataset reads shards packed, as `--no-extract` keeps them, but then nothing is unpacked to choose from, so sync refuses a members table:

```text
error: members in datasets.speech chooses files inside archives, but extract = false keeps them packed
  data.toml:5   [datasets.speech.members."shard-000000.tar"]
  help: remove the members tables, or remove extract = false
```

to give webdataset packed shards of only some members, unpack them with their members table and `place = false`, and pack the kept files again in a [recipe](../guides/recipes.md). from a real run with a shard served from this machine:

```toml
[datasets.speech]
ref = "http://127.0.0.1:19357/shard-000000.tar"
place = false

[datasets.speech.members."shard-000000.tar"]
select = ["*.flac", "*.txt"]

[datasets.wds]
from = ["speech"]
run = ["python3", "repack.py", "{speech}", "{out}"]
code = ["repack.py"]
```

`repack.py` packs every file it is given into one tar, with fixed times and owners so the same files make the same bytes:

```python
import sys, tarfile
from pathlib import Path

source, out = Path(sys.argv[1]), Path(sys.argv[2])


def plain(info):
    info.mtime, info.uid, info.gid, info.uname, info.gname = 0, 0, 0, "", ""
    return info


with tarfile.open(out / "speech-000000.tar", "w", format=tarfile.USTAR_FORMAT) as tar:
    for path in sorted(source.rglob("*")):
        if path.is_file():
            tar.add(path, path.relative_to(source).as_posix(), filter=plain)
```

`fetchloom build` placed `data/wds/speech-000000.tar` holding `000000.flac`, `000000.txt`, `000001.flac` and `000001.txt`, retired the unpacked files from `data/speech`, and `fetchloom build --check` made the same bytes again. give webdataset the folder `fetchloom path wds` prints.
