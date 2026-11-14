# The cache and placement

## cache

the cache is one folder on this machine that names each file by its SHA-256, so projects share the same bytes instead of downloading a copy each; [cache folder](../reference/cache-folder.md) says where it is and what it holds. a sync registers its project with the cache, so [`cache gc`](../commands/cache.md#gc) keeps what the project needs; `get` and `lock` register nothing.

one user's processes can share a cache. on a shared machine, give each user their own: a cache folder fetchloom creates on linux and macos is readable by you only. a cache shared by a group suits only trusted users: create its folder first with the group permissions you want, which fetchloom keeps, as [one cache for a whole lab](../guides/clusters.md#one-cache-for-a-whole-lab) shows, and use `copy = true` for students or jobs that edit inputs.

## placement

| method | how files land | writable? |
|---|---|---|
| reflink | a clone that shares the cache file's blocks until either is written | yes, with `copy = true` |
| hardlink | another name for the cache file itself | no |
| copy | independent bytes | yes, with `copy = true` |

sync tries a reflink, then a hardlink, then a copy, as the disk allows; on windows, a read-only file tries a hardlink first, to reuse a cached file defender already scanned. it never makes symlinks. a folder fetchloom recognizes as synced, such as onedrive, dropbox or icloud drive, gets no hardlinks but may get reflinks; [windows](../guides/windows.md#onedrive-and-synced-folders) and [macos](../guides/macos.md#icloud-drive-and-cloud-storage) list the folders.

files are read-only by default. for an analysis that writes into its inputs, set `copy = true` and run `fetchloom sync`:

```toml
[datasets.iris]
ref = "zenodo:3608990"
copy = true
```

sync then replaces unchanged read-only files with writable reflinks or copies, never hardlinks, and keeps files you edited. turning `copy` off leaves writable files as they are.

keep your own results outside the data folder, or make them with a [recipe](../guides/recipes.md). a file you delete from the data folder comes back at the next sync, since data.lock still pins it; [`fetchloom drop <path>`](../commands/drop.md) leaves a bad sample out for good, with a pattern in data.toml that every clone follows. never make a hardlinked file writable to edit it: the edit changes the cache's copy, which every project linked to it shares, and sync treats that as damage, as [local changes](../commands/sync.md#local-changes) explains. [working with raw data](../guides/edit-input-files.md) shows each case from a real run.

sync keeps local edits unless forced, and status differences and kept conflicts still exit 0; [scripting](../guides/scripts.md) shows how to check for them.

### Links need one drive
