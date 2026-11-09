# get

`get` downloads into a folder through the shared cache without creating a project.

```sh
fetchloom get iris
fetchloom get https://huggingface.co/datasets/scikit-learn/iris -o iris
fetchloom get https://www.cs.toronto.edu/~kriz/cifar-10-python.tar.gz --no-extract
fetchloom get zenodo:3608990 figshare:34744689@1 -o downloads
fetchloom get -i links.txt
fetchloom get https://example.org/iris.csv --sha256 d144ff83495e88cf892b8415db97343ff575bf11e47b6d08727499c4f32ffa9d
```

## usage

```sh
fetchloom get [options] <link>...
```

| option | effect |
|---|---|
| `-o <dir>`, `--output <dir>` | choose the output folder; with several links, the folder that holds one folder for each |
| `-i <file>`, `--input-file <file>` | read links from a file, one per line, or from stdin for `-` |
| `--sha256 <hex>` | check the one downloaded file against this SHA-256 before putting it in the folder |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more |
| `--exclude <glob>` | drop matching files; repeat for more |
| `--no-extract` | keep archives packed |

`-o -` is refused: get writes files to a folder, not their contents to stdout.

[global options](README.md#global-options) also apply. plain words use [catalog search and selection](search.md#choosing-a-dataset); quote a multiword query, as in `fetchloom get "fashion mnist"`. search words work only as the one link of a run, so `fetchloom get fashion mnist` is a usage error that says to quote the words of one search or to get one dataset at a time. quote patterns; they match source paths before download, not archive members. each select pattern must match at least one path before exclusions, and some files must remain.

## what happens

1. resolve the reference and filter the source's listed files.
2. reuse checksummed cached bytes or fetch the missing data. unpinned URLs can be checked with `ETag` or `Last-Modified`.
3. unpack recognized archives unless disabled, then place the files in the output folder.
4. print the absolute output folder on stdout. no manifest, lock or project state is written.

a single file defaults to a folder named after its filename stem. several files use a name derived from their title or reference. a direct download's `Content-Disposition` filename, including one received after redirects, replaces the name guessed from the URL. unsafe characters are replaced and names shortened for filesystem limits.

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

a download that went on from bytes an earlier run kept says how much came from that run, and its speed counts only the bytes moved this time, as in `Downloaded 3 files · 79.6 MiB in 2.4 s · 23.6 MiB/s · 23.0 MiB from an earlier run`.

an unchanged direct URL can return a 304 response, avoiding its file-body download. without validators, bytes may download again. source listings can still need requests even when payloads are cached. a later `get` accepts changed upstream bytes; use [add](add.md) for pinned inputs.

## several links

links given together, on the command line or with `-i`, share one run. fetchloom resolves them at once, asks once about the total size when it is large, and downloads all their files together, at most `FETCHLOOM_JOBS` at a time as in a [project sync](../reference/environment.md). each link then gets its own folder, named as it would be alone, in the working folder or inside the `-o` folder. stdout lists each placed folder on its own line, in the order given.

with several links, each must be a link, a DOI or an id; a bare word among them is a usage error. `--select`, `--exclude` and `--no-extract` apply to every link. two links named alike each get a folder: `iris`, then `iris-2`, so no link's files replace another's.

batch destination checks use those final folders, after downloads have supplied filenames. rerunning the same ordered batch reuses its suffixed folders. a refused destination can leave downloaded bytes in the cache.

a link that fails is reported on a `Failed` line while the others finish. the run then ends with one error naming each failed link, and exits with the code of the first failed link in the order given. links already placed stay in place. stderr from a snapshot where the first link is cached and the second answers 404. `[port]` replaces the fixture server's port:

```text
    Checked iris.csv · unchanged on 127.0.0.1
         Ok iris/iris.csv is already up to date
     Failed http://127.0.0.1:[port]/gone.csv · 127.0.0.1 has nothing at http://127.0.0.1:[port]/gone.csv (404)
error: 1 of 2 links could not be fetched
  http://127.0.0.1:[port]/gone.csv: 127.0.0.1 has nothing at http://127.0.0.1:[port]/gone.csv (404)
  help: check the link in a browser. if it moved, give the new link on the command line or as ref in data.toml
```

an interruption, a refused question about the total size or a cache that cannot open still stops the whole run. during placement, ctrl-c stops new files and checks between copy blocks. completed files stay; unfinished copies never take their final names.

## link lists

`-i <file>` reads one link a line, after any links on the command line. blank lines and lines starting with `#` are skipped, and spaces around a link are ignored. `-i -` reads the list from stdin, so another command can supply it:

```sh
fetchloom get -i - < links.txt
```

a list always runs as several links, even when it holds one, so `-o` names the folder that holds each link's folder. a line of search words, or a list with no links, is a usage error before anything downloads. ctrl-c while reading the list exits 130 without waiting for its producer to close the pipe.

## checking a hash

`--sha256 <hex>` takes the 64 hex digits a publisher states for a file, in either case, with surrounding whitespace ignored. it needs exactly one link that keeps exactly one file; use `--select` to keep one file of a dataset. a source that states another SHA-256 for the file fails before the download. otherwise fetchloom hashes the bytes as they arrive, and bytes with another hash fail with `integrity.mismatch` and exit 30 before anything is placed. the details show the first 12 digits of the expected and received hashes.

## existing folders

unrelated top-level entries are refused. fetchloom ignores its own leftover copy files; nested extras below an expected folder can remain. matching files stay.

before removing any earlier version, get checks every target in that output folder and the cached bytes it needs. a later conflict or a missing cached object leaves earlier files in place. other contents can be replaced only if those bytes already exist in the cache. that does not prove fetchloom created them. there is no `get --force`. choose a different folder to protect your files.

the output folder explicitly chosen with `-o` for a single link may itself link elsewhere. automatically named folders, including each link's folder inside a multi-link `-o` parent, must be real folders. links below an output folder are refused. read-only placement, extraction paths and offline requirements follow the [concepts](../concepts.md).

## exit codes

0 returns an output folder for every link. missing approval exits 1; invalid usage exits 2; an occupied destination can exit 50 with `local.not_empty`; a `--sha256` mismatch exits 30 with `integrity.mismatch`. source, download, access and integrity errors use the [shared exit codes](../reference/exit-codes.md). with several links, the first failed link decides the code, and `--json` keeps every link's result in [`result.items`](../reference/json-output.md#command-results).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a rerun fetches changed data | `get` has no project lock | use `add` and commit the pins, or check the bytes with `--sha256` |
| an output folder has unrelated contents | `get` protects existing files | choose another folder with `-o` |
| offline get cannot resolve a repository | the reference needs network resolution | follow [offline preparation](../concepts.md#offline) |
| a drive folder includes google docs | selected exports cannot be pinned reliably | [exclude the docs](../sources/google-drive.md) or request fixed files |
| the downloaded archive stayed packed | its name or bytes are unsupported | check [archive rules](../concepts.md#archives) |
| `"fashion" is not a link, a DOI or an id` | unquoted search words read as several links | quote the words of one search, as in `fetchloom get "fashion mnist"`, or get one dataset at a time |
| `--sha256 checks one file` | several links, or a dataset with several files | pass one link, and keep one file with `--select` |