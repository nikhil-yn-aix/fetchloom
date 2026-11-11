# macos

macos runs the same fetchloom commands as linux and windows, on intel and apple silicon macs with macos 11 or later. its disks and synced folders change how files are placed.

## install

no release has been published yet, so build from source: `xcode-select --install` provides the command line tools, then follow [getting started](getting-started.md#install).

after the first release, the installer in the [readme](https://github.com/nikhil-yn-aix/fetchloom#install) picks the x86_64 or arm64 build. a terminal running under rosetta on apple silicon still gets the arm64 build. it installs into `~/.local/bin` and, when PATH lacks that folder, adds it to `~/.zshrc` as well as `~/.profile`, since zsh is the default shell and never reads `~/.profile`.

the release executables are not signed by a developer account. `curl`, the installer and `pip` do not mark what they download, so macos runs it without asking. an archive saved from a browser is marked as quarantined; after unpacking it, clear the mark before the first run:

```sh
xattr -d com.apple.quarantine fetchloom
```

## cache

the default cache is `~/Library/Caches/fetchloom`, where macos expects applications to keep files they can download again. a nonempty `FETCHLOOM_CACHE` still wins:

```sh
export FETCHLOOM_CACHE="$HOME/fetchloom-cache"
```

put that line in `~/.zshrc` to keep it in later sessions. changing the variable selects a cache location; it does not move the old cache. [cache folder](reference/cache-folder.md#location) lists the defaults on every platform.

## clones and links

apfs supports copy-on-write clones: a clone shares its blocks with the cached file until either file is written. `copy = true` requests writable clones. hfs+ cannot clone, so fetchloom tries hardlinks there. exFAT and FAT32 drives make neither, so every file is copied, and FAT32 holds no file of 4 GiB or more, which fetchloom refuses before copying, as [limits](limits.md#scale) lists. a project on another volume than the cache gets copies too; a cache on the same drive, chosen with `FETCHLOOM_CACHE`, lets fetchloom link again.

stderr excerpt when the project and cache share an apfs disk:

```text
     Placed data/iris · 1 file linked to the cache
```

## icloud drive and cloud storage

sync clients break hardlinks or upload each one as a whole file, so fetchloom skips hardlinks in folders it recognizes. clones are still made where the disk allows them; otherwise files are copied.

| folder | client |
|---|---|
| `~/Library/Mobile Documents` | icloud drive, including `iCloud Drive` in finder |
| `~/Library/CloudStorage/OneDrive-*` | onedrive, one folder per account |
| `~/Library/CloudStorage/Dropbox*` | dropbox, one folder per account |
| folders named in dropbox's `~/.dropbox/info.json` | dropbox, such as an older `~/Dropbox` |

the client prefixes above and names under `~/Library` compare without case, as apfs compares them, and a folder reached through a link counts as the folder it leads to. other clients in `~/Library/CloudStorage`, such as google drive, are not recognized. a folder synced some other way is recognized only when its path leads into one of the folders above. a project outside synced storage lets fetchloom try every link; [placement](concepts.md#placement) explains the order.

## names

apfs and hfs+ ignore case by default, and both see `café` written with one character and with an `e` and a combining accent as one name. apfs keeps a name in the form that created it, while hfs+ lists every name in the decomposed form. a file fetchloom placed under one form and listed under the other is the same file: `status` does not report it missing and extra, `sync` does not place it again, and `get` does not count it as a foreign file in its output folder.

two files in a dataset whose paths differ only in case would be one file on such a disk. fetchloom refuses to place them on every platform, so a project works the same on each, and its message names both paths and suggests how to leave one out.

## short name

the installer also makes `fl` in `~/.local/bin`, a link to fetchloom, so `fl sync` runs `fetchloom sync` and follows every upgrade. when another program is already called `fl`, the installer leaves it alone and says so. an alias then gives you the short name in your own shell:

```sh
alias fl=fetchloom
```

put the alias in `~/.zshrc` to keep it in later sessions. [completions](commands/completions.md) shows how zsh and bash complete fetchloom's commands and options, under either name.
