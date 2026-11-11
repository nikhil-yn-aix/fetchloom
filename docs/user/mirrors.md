# mirrors

a mirror is a folder or a web server that holds pinned files under their SHA-256, so a lab disk or a usb drive can give a project's files instead of their sources.

data.lock names every file by its SHA-256, so any copy with that hash is the same file. fetchloom looks in the cache first, then asks each mirror in the order given, then the file's source. every file a mirror gives is checked against its pin before the cache keeps it, so a mirror can withhold a file but never change one.

## naming mirrors

mirrors come from you alone: the `mirrors` setting in the [settings file](reference/environment.md#settings-file), the `FETCHLOOM_MIRRORS` variable or `--mirror` on the command line. data.toml and data.lock cannot name one, since a cloned project could then send your requests, and the hashes you ask for, to a server nobody chose.

```toml
mirrors = ["/mnt/lab/fetchloom"]
```

```sh
fetchloom sync --mirror /media/usb/fetchloom
```

text holding `://` is a web address and anything else a folder, so `C:\lab` is a folder. `--mirror` can be repeated; its mirrors come first, in the order given, then those of `FETCHLOOM_MIRRORS`, or of the settings file when the variable is unset. a relative folder in the settings file is taken from the file's folder, and one on the command line or in the variable from the working folder. [`fetchloom settings`](commands/settings.md) shows the mirrors a run would ask.

## a folder mirror

a folder mirror is a folder this machine can read: a lab nas, a network share, a usb drive. it holds:

```text
fetchloom-mirror           first line "fetchloom mirror 1"
objects/<2 hex>/<62 hex>   a pinned file, named by its SHA-256
trees/<2 hex>/<62 hex>     an archive's list of files, named by the SHA-256 of that list
```

the `fetchloom-mirror` file tells a mirror from an empty folder, such as the one a network disk mounts on when it is not mounted. `objects` is laid out as the cache's own [objects folder](reference/cache-folder.md). a tree is the record of what an archive unpacks to, kept in the cache under `unpacked/`, and its SHA-256 is the `unpacks_to` that data.lock pins for the archive. with an archive's tree on a mirror, a fresh machine takes the archive's files one by one, and never needs the archive itself.

each file is copied into the cache, never linked, so a later change on the mirror cannot reach a project. on linux, where the mirror and the cache share a disk that makes reflinks, such as btrfs or xfs, the copy is a reflink and costs no space. either way the copy is read once to check its SHA-256 before the cache keeps it.

a cold clone on a machine that can read the lab's nas:

```sh
fetchloom sync --locked --mirror /mnt/lab/fetchloom
```

stderr:

```text
     Copied 6 files · 91 B from /mnt/lab/fetchloom
     Placed data/dataset · 5 files linked to the cache
     Placed data/iris · 1 file linked to the cache
     Synced 2 datasets into data
```

no request went to a source. with `--json`, `work.mirror_objects` and `work.mirror_bytes` count what mirrors gave, as [JSON output](reference/json-output.md#work-counters) describes.

## what goes wrong

| a mirror | what fetchloom does |
|---|---|
| lacks a file | asks the next mirror, then the source |
| holds a copy of another size or with other bytes, or a link or a special file in its place | warns, naming the mirror and the file, and asks the next mirror, then the source |
| is not there, or holds no `fetchloom-mirror` file | warns once and skips the mirror for the rest of the run |

a warning for a damaged copy, on stderr:

```text
warning: the mirror /mnt/lab/fetchloom holds a damaged copy of data/iris/iris.csv, so it was not used
  help: ask whoever keeps the mirror to replace it
```

after five such warnings for one mirror, the rest are counted on one line.

## offline

`--offline` reads folder mirrors, since a folder needs no network, so a machine without one restores a project from a usb drive:

```sh
fetchloom --offline sync --mirror /media/usb/fetchloom
```

## filling a folder mirror

a mirror is filled by copying files under their SHA-256, as a cache holds them. on linux or macos, from a cache that synced the project:

```sh
mirror=/mnt/lab/fetchloom
cache=~/.cache/fetchloom
mkdir -p "$mirror/trees"
printf 'fetchloom mirror 1\n' > "$mirror/fetchloom-mirror"
cp -R "$cache/objects" "$mirror/"
for record in "$cache"/unpacked/*; do
  hash=$(sha256sum "$record" | cut -c1-64)
  mkdir -p "$mirror/trees/${hash:0:2}"
  cp "$record" "$mirror/trees/${hash:0:2}/${hash:2}"
done
```

a cache holds the files of every project that used it, so copy only what you may share. [cache folder](reference/cache-folder.md) says where your cache is. fetchloom only reads mirrors; it never writes one, and a damaged copy stays until someone replaces it.

## privacy

a mirror is asked only for files the cache lacks, by their SHA-256 alone: no project name, path or link goes with the request. still, a mirror learns which hashes you ask for, and the hash of a public file names its dataset; whoever runs a folder mirror's file server can see the same in its logs.

## a mirror is not an archive

a mirror keeps bytes, not their licences, versions, retention or access rules. it is a convenience for a lab or a machine without a network, and it may lose files at any time; the pins in data.lock still say what each file must be. to preserve data, deposit it where it gets a DOI.
