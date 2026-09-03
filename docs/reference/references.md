# Naming data

A reference is what you type after `get`. One verb takes every form, and you
never have to say which kind you have.

## The forms

| Form | Example |
|---|---|
| Bare name | `silesia` |
| Namespaced with a release | `acme/imagenet@2012` |
| Local manifest | `./data.yaml` |
| Remote manifest | `https://lab.edu/eeg.yaml` |
| Direct file | `https://host/x.tar.zst` |
| Local file or directory | `file:///data/raw`, `./src` |
| Object store prefix | `https://s3.amazonaws.com/bucket/prefix/` |
| Provider | `hf:datasets/org/name@rev`, `zenodo:10.5281/zenodo.1234567` |
| Metadata document | `croissant:https://host/metadata.json` |
| Content address | `blake3:<hex>` |

Resolution is deterministic: an explicit scheme wins, then a local path that
exists, then configured source priority. A bare name that matches nothing fails.
It is never guessed at.

## What this build resolves

Five of those forms work today. The rest fail with `reference.unresolved`.

**A direct HTTPS file.**

```
$ fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz --output hello --json
{"status":"materialized","dataset":"hello-2.12.tar.gz","tree":"blake3:516b098d76cd000fe6001281327f1752d89d3cf9d5bbe39bc74ece250f22bbec","entries":462, ... }
```

**A local file, as a `file:` URL or as a path.** A recognized archive is
extracted; anything else is materialized as one file inside the destination.

```
$ fetchloom get file://$PWD/sample.tar.gz --output data --json
{"status":"materialized","dataset":"sample.tar.gz","entries":4, ... }
```

**A local directory.** Every file under it is copied into the destination. A
directory states no file modes, so every file is `0644` and the run says so:

```
{"event":"degrade","requested":"the mode each file carries","used":"0644 for every file","reason":"a filesystem tree states no mode, and reading one back from a volume that carries an executable bit would digest the same tree differently than a volume that does not"}
```

**A local manifest.** A path whose extension is one a manifest is written in
(`.yaml`, `.yml`, `.toml`, `.json`) is read as a manifest and nothing else
decides it. A directory is never searched for one. See [files.md](files.md).

```
$ fetchloom get ./data.yaml --output out --json
{"status":"materialized","dataset":"sample","tree":"blake3:cc6d1e52b3589084fb184dffd1ec06e79d9176e193ba43f2103c800ede49dc87","entries":2, ... }
```

**An object store prefix.** A remote location ending in `/` is a container. It
is listed rather than crawled, and every entry the listing holds is materialized
into the destination. Four index formats are recognized: an object store list
response, a WebDAV multi-status, a generated HTML index, and nothing else. An
index in no recognized format fails with `reference.unresolved` and is never
guessed at.

```
$ fetchloom get https://s3.amazonaws.com/bucket/prefix/ --output data --json
{"status":"materialized","dataset":"prefix","entries":2, ... }
```

`--select` and `--exclude` filter the listing, and a selection that matches no
listed entry fails before anything is published. Running it again against a
destination it already holds reports `unchanged` and writes nothing.

A bare name and a namespaced release resolve through the `sources` list your
configuration names. The reference is appended to each base in order and the
first that answers wins.

```toml
sources = ["https://lab.edu/data/", "hf:datasets/acme/"]
```

Nothing is guessed. A name that matches no base says how many were tried, and a
name given when no base is configured says to configure one:

```
$ fetchloom get some-dataset --output x --json
{"kind":"reference.unresolved", ... ,"next_action":"add a sources list to fetchloom.toml naming where some-dataset is published, because a name resolves through the configured source priority and none is configured"}
$ fetchloom get some-dataset --output x --json
{"kind":"reference.unresolved", ... ,"next_action":"name the location instead, because some-dataset matched none of the 1 configured sources and a name is never guessed at"}
```

A provider reference names a repository or a record and is listed like any other
container. `hf:datasets/org/name@rev` pins a revision; without one it reads the
default branch. `zenodo:10.5281/zenodo.1234567` names a record, and the record
states its own files.

A metadata document is read into a manifest and then fetched like one:
`croissant:https://host/metadata.json` reads the Croissant description at that
location and fetches every file it names.

There is no `s3://` form. An `s3://bucket/prefix/` reference has nowhere to put
the endpoint that serves it, because the authority slot of that form is spent on
the bucket and four different vendors serve the protocol. It could only be
completed from configuration on the machine reading it, which would make one
reference name different bytes on different machines, and a reference that does
that cannot carry a lock across a machine. An object store prefix is named by the
endpoint that serves it, which travels with the reference.

## One object or many

A reference naming one object produces a destination directory holding that one
entry, under the object's own name. A reference naming a container produces
every entry the container holds. The two differ only in what is walked, never in
what a destination is.

A listing counts the links it ignored because they pointed outside the prefix,
and reports the count between `listing.start` and `listing.end`:

```
{"event":"listing.start","source":"http://host/set/"}
{"event":"listing.skipped","count":1}
{"event":"listing.end","entries":2,"duration_ms":3}
```

A link resolving to the container itself points at the prefix rather than outside
it and is not counted. A relative escape and an absolute link elsewhere are.

## Archives

A reference whose final extensions name a container is extracted, unless
`--no-extract` is given. The archive's own header must agree with what the name
said, or the run fails with `archive.unsupported` naming both. Neither the name
nor the bytes decides alone.

| Name | What it is |
|---|---|
| `tar` | A POSIX ustar stream |
| `tar+gzip`, `tar+zstd`, `tar+xz`, `tar+bzip2` | A tar wrapped in one compression |
| `zip` | A zip container, store and deflate methods only |
| `gzip`, `zstd`, `xz`, `bzip2` | One compressed object, materialized as one file |

Recognized extensions are `.tar.gz`, `.tar.zst`, `.tar.xz`, `.tar.bz2`, `.tgz`,
`.tbz2`, `.zip`, `.gz`, `.zst`, `.xz`, `.bz2` and `.tar`. A manifest may state
the format instead, and it is still checked against the bytes.

A name and the bytes must agree, and the bytes are read far enough to find every
magic this build knows, including a tar's, which sits at offset 257:

```
$ fetchloom get ./posix.tar --output d1 --json
{"status":"materialized","dataset":"posix.tar","tree":"blake3:f2f3752199f59bd488aa9346f9e8314ee78691339cf6b512d410ce4788a72064","entries":4, ... }
```

## What a lock is written for

An unlocked run records what it resolved. A reference that resolves to an object
gets a lock entry; one that resolves to no object gets none, and the run says so
with a `degrade`.

A local file is one object, whether or not it is an archive, so it is pinned:

```
$ fetchloom get ./blob.txt --output d3 --lock my.lock
$ cat my.lock
datasets:
  blob.txt:
    artifacts:
      blob.txt:
        digest: "blake3:afa664f576ebf4ba3b5733fae8491ce7636be9cc124cb2216edfcad41ece8e7b"
        interop: "sha256:e816a3bb9b048c85c1a7d814972db5a833e5d37b2f15175e5c63d99f7c82cb03"
        layout: "keep"
        size: 12
    manifest: "blake3:93ee5a5fefd4b14d13c554fab5c3c3bdf0661cfc87d41d699b8f1d3dd9cdf32c"
    tree: "blake3:11a0acf3176fdb4857221e1ce9ab54f8687ef000477be1312d33a040e43a586d"
```

A directory resolves to a tree rather than to an object, so it gets no lock
entry, and the run says so with a `degrade`.

## Selection

`--select` and `--exclude` take globs against the canonical `/`-separated member
path. Matching is on raw bytes, case-sensitive, with no normalization.

A pattern is four rules and nothing else:

- `*` matches any run of bytes inside one path component, including none.
- `**` as a whole component matches any number of components, including none.
- `?` matches exactly one byte inside one component.
- Every other byte is literal, including `[`, `{` and `\`.

A pattern matches member paths, not subtrees. `--select data` selects a member
named `data`; `--select 'data/**'` selects what is under it. Every ancestor
directory of a selected member comes along whether a pattern matched it or not.

`--exclude` is applied after every `--select`.
