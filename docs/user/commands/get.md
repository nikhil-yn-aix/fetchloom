# get

`get` downloads data once, without creating a project: into a folder, or with `-o` into the one file you name. the bytes pass through the shared cache.

```sh
fetchloom get iris
fetchloom get https://example.org/iris.csv -o iris.csv
fetchloom get https://huggingface.co/datasets/scikit-learn/iris --dir iris
fetchloom get https://www.cs.toronto.edu/~kriz/cifar-10-python.tar.gz --no-extract
fetchloom get zenodo:3608990 figshare:34744689@1 --dir downloads
fetchloom get -i links.txt
fetchloom get https://example.org/flowers.zip --dry-run
fetchloom get https://example.org/iris.csv -o iris.csv --sha256 d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d
```

## usage

```sh
fetchloom get [options] <link>...
```

| option | effect |
|---|---|
| `-o <file>`, `--output <file>` | write the one downloaded file at exactly `<file>`, as it came, never unpacked; see [one file](#one-file) |
| `--force` | with `-o`, replace a file at `<file>` that holds other bytes |
| `-d <folder>`, `--dir <folder>` | choose the output folder; with several links, the folder that holds one folder for each |
| `-i <file>`, `--input-file <file>` | read links from a file, one per line, each with an optional SHA-256, or from stdin for `-` |
| `--sha256 <hex>` | check the one downloaded file against this SHA-256 before it takes its name or goes in the folder |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more |
| `--exclude <glob>` | drop matching files; repeat for more |
| `--no-extract` | keep archives packed; webdataset loaders read `.tar` shards packed |
| `-n`, `--dry-run` | tell what would be fetched and where it would land without downloading a file's bytes or writing anything; see [dry run](#dry-run) |

[global options](README.md#global-options) also apply. plain words [search](search.md#choosing-a-dataset) as with `add`; quote several words, as in `fetchloom get "fashion mnist"`, since unquoted words read as several links. quote patterns too; they match the files a source lists, not the files inside an archive, and each select pattern must match at least one. a link to one file its site does not list is a usage error before any download, and its help names the file the link likely meant, as hugging face's `Iris.csv` for a typed `iris.csv`.

## what happens

without `-o`:

1. resolve the reference and filter its files. a `Resolved` line names what the reference resolved to, and counts the files kept, as `1 of 4 files`, when a file link, `--select` or `--exclude` keeps fewer than the source lists. [webdataset shards](../concepts.md#webdataset-shards) get a hint before anything downloads.
2. reuse cached bytes or download the rest. a plain link is checked with `ETag` or `Last-Modified`.
3. unpack archives unless `--no-extract` says not to, then place the files in the output folder.
4. print the absolute output folder on stdout. no project file is written.

a single file goes in a folder named after its file name without the extension, and several files in one named after their title or reference. a direct download's `Content-Disposition` file name, also one received after redirects, replaces the name guessed from the link.

stderr excerpt from a snapshot for a single-file download on a filesystem that supports links. `[time]` replaces the measured duration:

```text
 Downloaded iris.csv · 81 B in [time]
    Fetched iris.csv into iris · linked to the cache
```

when a dataset has several files, the `Fetched` line is followed by the folder's top level, largest first, so the main file is easy to find. a folder shows how many files it holds. at most ten names are listed, then how many more there are:

```text
    Fetched 7 files into wine-quality · linked to the cache
            winequality-white.csv  258.2 KiB
            data/                  2 files · 101.7 KiB
            winequality-red.csv    82.2 KiB
            winequality.names      3.2 KiB
            .gitattributes         2.3 KiB
            README.md              913 B
```

a download that went on from bytes an earlier run kept says how much came from that run, as in `Downloaded 3 files · 79.6 MiB in 2.4 s · 23.6 MiB/s · 23.0 MiB from an earlier run`, and one that could not says so first, as [interruptions](../concepts.md#interruptions) shows. a slow disk can take a while longer to keep the file: a bar shows `waiting for the disk`, and the closing line names that part, as in `Downloaded big.bin · 1.0 GiB in 30.2 s, 12.3 s of it waiting for the disk · 57.2 MiB/s`. the time a download waits for your answer to a question, such as whether to download a large file, counts toward neither its time nor its speed.

a plain link whose server answers that the file is unchanged, with a 304, downloads nothing while the cache or the folder holds what it served last. without such validators, the bytes may download again. a later `get` takes changed bytes from the source; use [add](add.md) to pin them.

## one file

`-o <file>` writes the one file a link resolves to at exactly that path, as `curl -o` does, and prints the file's absolute path on stdout. stderr, then stdout, of a run against a server on the same machine, with the working folder shortened to `/home/me/work`:

```text
 Downloaded iris.csv · 81 B in 20 ms
      Saved data.csv
/home/me/work/data.csv
```

- the file is yours: writable, and no other name shares its bytes. archives stay packed, and so do bytes a server compressed on their way, as `Content-Encoding: gzip` says, since those are what a published hash matches; a warning says how to unpack or decompress them, unless the name already says so.
- a download nothing else on this machine needs moves out of the cache into the file when both sit on one disk, so a 200 GB file needs 200 GB, not 400, and no time to copy. otherwise the file is a clone of the cached bytes where the drive makes clones, else a copy, as always on windows.
- it takes its name only after its size, any MD5 its source states and any `--sha256` are checked, and only once its bytes are on disk, without replacing a file that appeared meanwhile.
- missing folders on the path are made first. a path that starts with a folder named `~`, as a quoted `~` reaches fetchloom, means a folder of that name here, not your home folder, and a warning says so.
- bytes this run downloaded leave the cache once the file holds them, unless a project pins them. bytes the cache held before stay until [`cache gc`](cache.md#gc) frees them, as do those of a run that stopped before the file took them, which a rerun reuses.
- two runs of one link share its download: the second says `Waiting iris.csv · another fetchloom is downloading it`, then decides as a run started afterwards would.

`-o` takes one link that resolves to one file. several links, `-i` and a dataset with several files left after `--select` are usage errors, exit 2, before any file downloads, and they say to use `--dir`. a path that names a folder is one too:

```text
$ fetchloom get https://example.org/data/iris.csv -o results
error: -o needs a file name, and results names a folder
  help: name the file inside it, as in `-o results/iris.csv`
```

a file its source sends inside a zip, as kaggle may send a competition's file, is a usage error too once its bytes show it. `-o -` is refused, since the file is checked before it appears.

a path beneath a file, where that file stands in place of a folder, fails before anything downloads or is written, with exit 50, as `local.io`. the error names the file in the way and another path:

```text
$ fetchloom get https://example.org/data/iris.csv -o data/py.csv/iris.csv
error: cannot write data/py.csv/iris.csv: data/py.csv is a file, not a folder
  help: choose another name with -o, such as `-o data/iris.csv`, or rename data/py.csv so a folder can take its name
```

### a file already there

a file already at the path stays as it is unless `--force` asks to replace it:

| the file there | without `--force` | with `--force` |
|---|---|---|
| holds what the link serves | `Ok`, exit 0, untouched | the same |
| holds other bytes | `local.not_empty`, exit 50, untouched | replaced |
| is a link to nothing, or a special file such as a pipe | `local.not_empty`, exit 50, untouched; the error names where a link to nothing points | replaced |

a file that still holds what the link served last time, while the link serves other bytes now, is refused the same way, and the error says its source changed it since. a link to a file counts as that file's bytes, and `--force` replaces the link itself, never the file it leads to. the replacement takes the name in one rename, so the old file stays whole until then. on windows a read-only file there loses its flag for the rename, and on FAT and exFAT, which may refuse it, the old file is moved aside first and back again if the new one cannot take its name.

with `--sha256`, or a hash the source states, the file there is hashed before anything downloads, and a run that finds those bytes downloads nothing. a stated MD5 counts once this machine has downloaded bytes that match it. without such a hash, the bytes the link served the last time this machine fetched it stand in. offline they decide, so a run without a network keeps a file that still holds them. that holds for a link only its site can list, such as a hugging face link, too: once a run fetched its one file, an offline run takes that file in place of the listing, and with `--sha256` an offline run needs no earlier one. online, a file that still holds them makes the run ask the server whether they changed, even after they left the cache for the file: an unchanged answer leaves the file as it is, and a large file asks about its size only when new bytes come. otherwise the run downloads the file to compare it, and says so before it starts, so a large download never ends in a refusal you did not see coming; a file of another size than the source states is refused before any download. a refused run leaves those bytes in the cache, so `--force` afterwards can reuse them when the server says they are unchanged:

```text
$ fetchloom get http://127.0.0.1:19450/files/iris81.csv -o other.csv
   Checking other.csv · already there, so the link downloads first to compare
 Downloaded iris81.csv · 81 B in 7 ms
error: other.csv already exists and holds other bytes than the link serves now
  help: add --force to replace it, or choose another name with -o
$ fetchloom get http://127.0.0.1:19450/files/iris81.csv -o other.csv --force
    Checked iris81.csv · 127.0.0.1 still serves the bytes downloaded last time
   Replaced other.csv
/home/me/work/other.csv
```

## several links

links given together, on the command line or with `-i`, share one run: fetchloom resolves them at once, asks once about the total size when it is large, and downloads their files together. each link gets its own folder, named as it would be alone, in the working folder or inside the `--dir` folder, and stdout lists each folder in the order given. two links named alike get `iris`, then `iris-2`, and a rerun of the same list reuses them.

with several links, each must be a link, a DOI or an id; a bare word among them is a usage error. `--select`, `--exclude` and `--no-extract` apply to every link.

a link that fails leaves the others to finish; one that fails before the downloads, as one that cannot be resolved, gets a `Failed` line at once. the run ends with one error naming each failed link, and exits with the code of the first failed link in the order given. stderr from a snapshot where the first link is cached and the second answers 404. `[port]` replaces the fixture server's port:

```text
    Checked iris.csv · 127.0.0.1 still serves the bytes downloaded last time
         Ok iris/iris.csv is already up to date
error: 1 of 2 links could not be fetched
  127.0.0.1 has nothing at http://127.0.0.1:[port]/gone.csv (404)
  help: check the link in a browser. if it moved, run get with the new link
```

links that fail alike are told once, the others listed under the first. the first ten failed links are named and the rest counted; `--json` keeps every link's own failure under `failures`. stderr from a snapshot of a list where three links answer 404 and one 403:

```text
error: 4 of 4 links could not be fetched
  127.0.0.1 has nothing at http://127.0.0.1:[port]/gone/a.csv (404)
  the same for:
    http://127.0.0.1:[port]/gone/b.csv
    http://127.0.0.1:[port]/gone/d.csv
  help: check the link in a browser. if it moved, run get with the new link
  127.0.0.1 refused access to http://127.0.0.1:[port]/denied.csv (403)
  help: check that the link opens in a browser. if it needs a login, ask its owner for a public link to pass to `fetchloom add` or `fetchloom get`
  help: fetched links stay in place and downloaded files stay in the cache. fix the errors above, then run `fetchloom get -i links.txt` again
```

an interruption, a refused question about the total size or a cache that cannot open stops the whole run. during placement, ctrl-c stops new files; completed files stay, and unfinished copies never take their final names. once the last file is placed, or once `-o` starts writing its file, a ctrl-c lets get finish, as [interruptions](../concepts.md#interruptions) explains.

## link lists

`-i <file>` reads one link a line, after any links on the command line. blank lines and lines starting with `#` are skipped, and spaces around a link are ignored. a link may be followed by the SHA-256 of its one file, as 64 hex digits or `sha256:` and 64 hex digits, which is checked as `--sha256` checks it:

```text
# measurements to compare
https://example.org/iris.csv sha256:d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d
zenodo:3608990
```

`-i -` reads the list from stdin, so another command can supply it:

```sh
fetchloom get -i - < links.txt
```

a list always runs as several links, even when it holds one, so `--dir` names the folder that holds each link's folder. the whole list is checked before any request: a line of search words, a path on this machine, a line naming the same data as an earlier one, a line that is no link, or a list with no links, stops the run naming the line. a line whose hash names other bytes fails that link alone with `integrity.mismatch`, naming the line, as `wine.csv does not match the SHA-256 on line 2 of links.txt`.

## checking a hash

`--sha256 <hex>` takes the 64 hex digits a publisher states for a file, in either case, alone or after `sha256:` as a list line takes them, with surrounding whitespace ignored. a value of 32 or 40 hex digits is refused as the MD5 or SHA-1 it looks like, since sites often state one beside the SHA-256. it needs exactly one link that keeps exactly one file; use `--select` to keep one file of a dataset. a source that states another SHA-256 for the file fails before the download, and so does a file already at the `-o` path that holds other bytes: its error says the hash or the file may be wrong, and shows both hashes, since a hash typed or copied by hand is as likely to be off as the file. otherwise fetchloom hashes the bytes as they arrive, and bytes with another hash fail with `integrity.mismatch` and exit 30 before anything is placed or written. the details show the expected and received hashes whole, one under the other within 80 columns, to check against a published checksum; `--json` gives both in one detail:

```text
error: cows.csv does not match --sha256
  expected 0000000000000000000000000000000000000000000000000000000000000000
  got      492d5ea496056f1a6a6592241032fab764c321596317930b4fa0e1e8bc3b7470
  help: check the hash given to --sha256, or ask the file's owner which bytes are right
```

## dry run

`--dry-run` resolves each link as get does, so a source may be asked what it holds, then tells what get would fetch and where the files would land, and stops there. it downloads no file's bytes and writes nothing: no folder, no file and no cache folder. a size the source does not state reads `size unknown`, and an archive this machine never unpacked keeps its files unknown, so they are not counted. from a real run:

```text
$ fetchloom get http://127.0.0.1:19430/flowers.zip --dry-run
      Would fetch flowers.zip · size unknown
      Would open flowers.zip
  hint: an archive's files are known once it is unpacked, so they are not counted
      Would put its files in flowers
         Ok dry run, nothing was changed
```

a file the cache already holds is no download, and one fetched before with no hash to vouch for it reads `Would check`, since its server is asked whether it changed. with `-o`, the last line says whether get would write, replace or keep the file there, and refuses a file that holds other bytes as get would. offline, a dry run answers from the cache alone, and a file it would have to download fails with `offline.missing`, exit 20, as get does.

with `--json`, `result` has `dry_run: true` and `changes`, shaped as a dry run of add shapes it: `download` lists each file with its `size`, or null when unknown, `unpack` the archives whose files only unpacking tells, and `place` and `replace` the paths the files would take, relative to the current folder; `folder`, or `file` with `-o`, is where they would land.

## existing folders

an output folder that already holds other files at its top is refused, so get never mixes its files with yours; files that match what it would place stay. before it replaces an earlier version of a file, get checks every file it would place in that folder, so a conflict leaves the earlier files in place. it replaces a file only when the cache holds that file's old bytes, and then says so, as `Replaced iris/iris.csv · the source changed it since this folder got it`. `--force` works only with `-o`, so choose another folder to keep your files.

the folder `--dir` names for a single link may be a link to a folder elsewhere; folders get names itself must be real folders, and links inside an output folder are refused.

## exit codes

in [mirror-only mode](../mirrors.md#mirror-only-mode) `get` is refused before any request, with `network.refused`, exit 20, since it fetches each link from its source.

0 returns an output folder for every link, or with `-o` the file. missing approval exits 1; invalid usage exits 2; an occupied destination, including a file `-o` will not replace without `--force`, exits 50 with `local.not_empty`; a `--sha256` mismatch exits 30 with `integrity.mismatch`. source, download, access and integrity errors use the [shared exit codes](../reference/exit-codes.md). with several links, the first failed link decides the code, and `--json` keeps every link's result in [`result.items`](../reference/json-output.md#command-results).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a rerun fetches changed data | `get` has no project lock | use `add` and commit the pins, or check the bytes with `--sha256` |
| an output folder has unrelated contents | `get` protects existing files | choose another folder with `--dir` |
| offline get cannot resolve a repository | only a link that kept one file, fetched on this machine before, runs offline | run it once without `--offline`, or for several files follow [offline preparation](../concepts.md#offline) |
| a drive folder includes google docs | selected exports cannot be pinned reliably | [exclude the docs](../sources/google-drive.md) or request fixed files |
| the downloaded archive stayed packed | its name or bytes are unsupported | check [archive rules](../concepts.md#archives) |
| `"fashion" is not a link, a DOI or an id` | unquoted search words read as several links | quote the words of one search, as in `fetchloom get "fashion mnist"`, or get one dataset at a time |
| `--sha256 checks one file` | several links, or a dataset with several files | pass one link, and keep one file with `--select` |
| `hf:datasets/scikit-learn/iris@main has no file iris.csv` | the link names a file its site lists under another name, often in other case | write the name the help gives in the link |
| `-o writes one file, and <link> has <n> files` | `-o` writes exactly one file | keep one with `--select`, or put them all in a folder with `--dir` |
| `data.csv already exists and holds other bytes than the link serves now` | `-o` never replaces a file without being asked | choose another name with `-o`, or add `--force` |
| `--force replaces only the file -o names, so it cannot go with --dir` | `--force` replaces the one file `-o` writes, and get never replaces the files of a `--dir` folder | leave out `--force` and give `--dir` an empty folder, or write one file with `-o <file> --force` |
| `data.csv already exists, and its bytes are not the ones --sha256 names` | the hash or the file is not the one you meant | check the hash; if it is right, choose another name with `-o`, or add `--force` |