# mirrors

a mirror is a folder or a web server that holds pinned files under their SHA-256, so a lab disk, a usb drive or a company server can give a project's files instead of their sources.

data.lock names every file by its SHA-256, so any copy with that hash is the same file. fetchloom looks in the cache first, then in the project's data folder, at the paths data.lock places the file at, as [moving a project](commands/cache.md#moving-a-project-to-another-machine) describes, then asks each mirror in the order given, then the file's source. every file a mirror gives is checked against its pin before the cache keeps it, so a mirror can withhold a file but never change one.

## naming mirrors

mirrors come from you alone: the `mirrors` setting in the [settings file](reference/environment.md#settings-file), the `FETCHLOOM_MIRRORS` variable or `--mirror` on the command line. data.toml and data.lock cannot name one, since a cloned project could then send your requests, and the hashes you ask for, to a server nobody chose.

```toml
mirrors = ["/mnt/lab/fetchloom", "https://data.lab.example/fetchloom"]
```

```sh
fetchloom sync --mirror /media/usb/fetchloom
```

text holding `://` is a web address and anything else a folder, so `C:\lab` is a folder. `--mirror` can be repeated; its mirrors come first, in the order given, then those of `FETCHLOOM_MIRRORS`, or of the settings file when the variable is unset. a relative folder in the settings file is taken from the file's folder, and one on the command line or in the variable from the working folder. [`fetchloom settings`](commands/settings.md) shows the mirrors a run would ask, those of `--mirror` first, and where each came from.

## a folder mirror

a folder mirror is a folder this machine can read: a lab nas, a network share, a usb drive. it holds:

```text
fetchloom-mirror           first line "fetchloom mirror 1"
objects/<2 hex>/<62 hex>   a pinned file, named by its SHA-256
trees/<2 hex>/<62 hex>     an archive's list of files, named by the SHA-256 of that list
```

the `fetchloom-mirror` file tells a mirror from an empty folder, such as the one a network disk mounts on when it is not mounted. `objects` is laid out as the cache's own [objects folder](reference/cache-folder.md). a tree is the record of what an archive unpacks to, kept in the cache under `unpacked/`, and its SHA-256 is the `unpacks_to` that data.lock pins for the archive. with an archive's tree on a mirror, a fresh machine takes the archive's files one by one, and never needs the archive itself, nor any part of one [split into parts](concepts.md#split-archives).

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

## a web mirror

a web mirror is the same layout on a web server, at an `http://` or `https://` address, so any server of static files works, such as nginx over a folder mirror. fetchloom asks for its `fetchloom-mirror` file once a run, then for each file it lacks at `<address>/objects/<2 hex>/<62 hex>`. the files come through the same downloads as files from sources: resumed after a stop, retried while the server says to try again, split over several connections when slow, and checked against their pins. a tree is read whole, up to 64 MiB, about 600,000 files; an archive with a longer list comes as itself. files a mirror gives over the network count in `work.requests` and `work.bytes_down` too, and the question before a download larger than `ask_above` covers them.

so a reviewer whose copy of a paper's data is gone from its source restores it from the lab's server with `fetchloom sync --locked --mirror https://data.lab.example/fetchloom`, whose first line then reads `Downloaded 6 files · 91 B in 27 ms from https://data.lab.example/fetchloom`, the rest as for a folder mirror.

a mirror you name may be on this machine or your network, such as `http://10.0.0.5:8080/fetchloom`: like a link typed on the command line, its origin, the scheme, host and port of its address, may be reached whatever its address is. another port or scheme of that host is not trusted, so it reaches public addresses only, unless [`allowed_origins`](reference/environment.md) lists it. a login for it goes in `.netrc`, which `fetchloom auth login data.lab.example` writes, and is sent over https only. over plain http the hashes still check every byte, but anyone on the network can see what is fetched.

## what goes wrong

| a mirror | what fetchloom does |
|---|---|
| lacks a file, 404 or 410 on the web | asks the next mirror, then the source |
| holds a copy of another size or with other bytes, or a link or a special file in its place | warns, naming the mirror and the file, and asks the next mirror, then the source |
| is not there, holds no `fetchloom-mirror` file, cannot be reached, refuses with 401 or 403, or fails with 5xx after the retries | warns once and skips the mirror for the rest of the run |

in [mirror-only mode](#mirror-only-mode) the source is never asked, so a file no mirror gives fails the run, named.

a file [imported](commands/add.md#files-on-this-machine) from a folder has no source, so one no mirror gives fails the run in every mode, named as a missing download is: with `network.refused` in mirror-only mode and `offline.missing` offline, both exit 20, and otherwise with `reference.unresolved`, exit 10, and what each mirror said, from a real run:

```text
error: flowers was imported from files, and neither this machine's cache nor a mirror holds them
  flowers/sepals.csv: not there on ../mirror
  help: copy them to data/flowers by hand, then run `fetchloom sync`, or run `fetchloom add <folder> --as flowers` where they are
```

a warning for a damaged copy, on stderr:

```text
warning: the mirror /mnt/lab/fetchloom holds a damaged copy of data/iris/iris.csv, so it was not used
  help: ask whoever keeps the mirror to replace it
```

after five such warnings for one mirror, the rest are counted on one line.

## offline

`--offline` reads folder mirrors, since a folder needs no network, and never asks a web mirror, so a machine without one restores a project from a usb drive:

```sh
fetchloom --offline sync --mirror /media/usb/fetchloom
```

## mirror-only mode

an air-gapped company keeps a mirror inside its network and holds every machine to it. outside, someone syncs the project and copies the mirror to a usb drive; inside, the admin's settings file names the mirror and turns on mirror-only mode:

```toml
mirrors = ["https://data.corp.internal/fetchloom"]
mirrors_only = true
```

then no request goes anywhere but to the mirrors: not to a source, not to a search site, not to check a key. each request is checked before its name is looked up, and every redirect too, so one that leads off a mirror's address is refused. a request to a mirror's host for anything but what the mirror holds is refused as well. `sync --locked` asks the mirror alone, and a file no mirror holds fails at once, before any download, naming each file and what each mirror said, with kind `network.refused`, exit 20:

```text
error: 1 file data.lock names is on no mirror, and mirrors_only lets fetchloom ask its mirrors alone
  iris/iris.csv: not there on https://data.corp.internal/fetchloom
  help: ask whoever keeps the mirrors to add what is missing, or set mirrors_only = false in the settings file to ask sources too
```

| command | in mirror-only mode |
|---|---|
| `sync`, `path`, `lock` | work from the cache and the mirrors; a dataset data.lock does not pin yet is refused, since only its source can list it |
| `add` of a link, `get`, `search` | refused before any request, naming `mirrors_only`. a link data.lock does not pin yet has no SHA-256 to ask a mirror by, so only its source can give it, even when a mirror holds the same bytes |
| `auth` | lists keys without checking them with their sites |

`mirrors_only` needs a mirror: set alone it stops every command with kind `usage`. offline mode is stricter: no network at all, so it reads folder mirrors and no web mirror.

mirror-only mode is fetchloom's promise not to ask anything else; a person can turn it off, so a network that must not reach the internet still needs rules in its firewall or proxy. set `FETCHLOOM_SETTINGS` for every account to a settings file people cannot edit, so the same mirrors apply to all.

## filling a folder mirror

[`fetchloom push`](commands/push.md) fills a folder mirror from this machine's cache with the files a project's `data.lock` pins and the lists of what its archives unpack to, each checked before it takes its name. a lab names its mirror once in each member's settings file, both to read from and to push to:

```toml
mirrors = ["/mnt/lab/fetchloom"]
push_to = "/mnt/lab/fetchloom"
```

whoever synced the project runs `fetchloom push`, and their teammates' syncs then take the files from the nas. push copies the project's files alone, never the rest of a cache other projects share. [push](commands/push.md) shows a run, what it asks first, how a web mirror or a bucket gets a folder mirror through a copying tool such as rclone, and how it replaces a damaged copy.

## freeing raw data a mirror holds

with the raw data recipes read on a folder mirror, as `fetchloom push --inputs <recipe>` puts it there, [`fetchloom cache gc --inputs`](commands/cache.md#raw-input-a-mirror-holds) frees this machine's copy of it and keeps the results, relying only on a folder mirror whose copy it reads whole and hashes in that run. a build that reads that data again takes it from the mirror, or from its source.

## privacy

a mirror is asked only for files the cache lacks, by their SHA-256 alone: no project name, path or link goes with the request. still, a web mirror learns which hashes you ask for, and the hash of a public file names its dataset; whoever runs a folder mirror's file server can see the same in its logs.

## a mirror is not an archive

a mirror keeps bytes, not their licenses, versions, retention or access rules: anyone who can read it can take what was pushed there. it is a convenience for a lab or a machine without a network, and it may lose files at any time; the pins in data.lock still say what each file must be. so `cache gc --inputs` names the mirror it relies on before it frees anything: once that mirror and the source have lost a file, no build can make its result again. to preserve data, deposit it where it gets a DOI.
