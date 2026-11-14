# Pins and integrity

## manifest and lock

`data.toml` says what you want: each dataset has a reference and can select files, exclude paths, keep archives packed, ask for writable copies, choose a folder or stay out of the data folder. `data.lock` records what you got: each selected file's SHA-256 and size, the MD5 a source states, the version the reference resolved to and, for an archive, a hash of its unpacked file tree. [data.toml](../reference/data-toml.md) and [data.lock](../reference/data-lock.md) list every field.

files on your own machine can be pinned too: `fetchloom add ./flowers` copies them into the cache, pins them by their bytes and leaves the folder as it was, as [files on this machine](../commands/add.md#files-on-this-machine) shows.

every download must match its locked size and hashes. a source that states a SHA-256 and size lets a file be locked without downloading it. what a reference points at can change, as a plain link's file can, but the lock pins bytes, so a source that serves other bytes fails the check; [the spec hash](../reference/data-lock.md#the-spec-hash) says when an entry is resolved again.

cached files are not hashed again on every use, except one written after it entered the cache, as an edit through a placed hardlink writes it. [cache verify](../commands/cache.md#verify) hashes everything when you want to be sure.

`sync` first updates a missing or stale lock, unless `--locked` asks for an up-to-date one, and unchanged entries keep their pins. `lock --check` checks the lock against data.toml without changing files, and `lock --upgrade` asks the sources for newer versions.

## recipes

a dataset can also be a [recipe](../guides/recipes.md): a command of yours that makes it from other datasets. only `fetchloom build` runs it, on private copies of what it reads, then pins and places what it wrote; `status`, `lock --check` and `sync --locked` say when the pinned result no longer matches the recipe. raw data that only recipes read can stay out of the data folder, as [keep only the result](../guides/recipes.md#keep-only-the-result) shows.
