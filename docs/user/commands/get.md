# get

`get` downloads into a folder through the shared cache without creating a project.

```sh
fetchloom get zenodo:3608990
fetchloom get zenodo:3608990 -o flowers
fetchloom get https://www.cs.toronto.edu/~kriz/cifar-10-python.tar.gz --no-extract
```

## usage

```sh
fetchloom get [options] <link>
```

| option | effect |
|---|---|
| `-o <dir>`, `--output <dir>` | choose the output folder |
| `--select <glob>` | keep files matching any supplied pattern; repeat for more |
| `--exclude <glob>` | drop matching files; repeat for more |
| `--no-extract` | keep archives packed |

[global options](README.md#global-options) also apply. quote patterns; they match source paths before download, not archive members. each select pattern must match at least one path before exclusions, and some files must remain.

## what happens

1. resolve the reference and filter the source's listed files.
2. reuse checksummed cached bytes or fetch the missing data. unpinned URLs can be checked with `ETag` or `Last-Modified`.
3. unpack recognized archives unless disabled, then place the files in the output folder.
4. print the absolute output folder on stdout. no manifest, lock or project state is written.

a single file defaults to a folder named after its filename stem. several files use a name derived from their title or reference. a direct download's `Content-Disposition` filename replaces the name guessed from the URL. unsafe characters are replaced and names shortened for filesystem limits.

stderr excerpt for a single-file download on a filesystem that supports links:

```text
     Placed iris/iris.csv · linked from the cache
    Fetched iris.csv into iris
```

an unchanged direct URL can return a 304 response, avoiding its file-body download. without validators, bytes may download again. source listings can still need requests even when payloads are cached. a later `get` accepts changed upstream bytes; use [add](add.md) for pinned inputs.

## existing folders

unrelated top-level entries are refused. fetchloom ignores its own leftover copy files; nested extras below an expected folder can remain. matching files stay.

other contents can be replaced only if those bytes already exist in the cache. that does not prove fetchloom created them. there is no `get --force`. choose a different folder to protect your files.

the output folder itself may link elsewhere. links below it are refused. read-only placement, extraction paths and offline requirements follow the [concepts](../concepts.md).

## exit codes

0 returns an output folder. missing approval exits 1; invalid usage exits 2; an occupied destination can exit 50 with `local.not_empty`. source, download, access and integrity errors use the [shared exit codes](../reference/exit-codes.md).

## common mistakes

| symptom | cause | fix |
|---|---|---|
| a rerun fetches changed data | `get` has no project lock | use `add` and commit the pins |
| an output folder has unrelated contents | `get` protects existing files | choose another folder with `-o` |
| offline get cannot resolve a repository | repository listings are not cached for offline resolution | prepare a locked project for offline use |
| a drive folder includes google docs | selected exports cannot be pinned reliably | [exclude the docs](../sources/google-drive.md) or request fixed files |
| the downloaded archive stayed packed | its name or bytes are unsupported | check [archive rules](../concepts.md#archives) |
