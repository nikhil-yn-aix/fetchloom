# windows

windows supports the same fetchloom commands, with placement affected by the drive and synced folders.

## defender and new files

many small files give Microsoft Defender many new files to scan. the [historical defender observations](../dev/performance/windows-defender.md#recorded-workload) describe one laptop; costs depend on the machine and its protection settings.

a dev drive with Defender's performance mode defers scans until after file open completes. [Microsoft's performance mode comparison](https://learn.microsoft.com/en-us/defender-endpoint/microsoft-defender-endpoint-antivirus-performance-mode#performance-mode-compared-to-real-time-protection) states the trade-off:

> Deferring scans until after file open completes inherently provides faster performance, but with less protection. However, enabling performance mode provides significantly better protection than other performance tuning methods, such as using folder exclusions, which block security scans altogether.

creating a dev drive needs an administrator. ask your IT team whether performance mode suits your machine. if the drive is `D:`, choose a cache there for the powershell session:

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
```

keep projects on the same drive to permit linking. changing the variable selects a cache location; it does not migrate the old cache. [cache folder](reference/cache-folder.md#location) gives the defaults.

## short name

```powershell
Set-Alias fl fetchloom
fl sync
```

put the `Set-Alias` line in your powershell profile to keep it in later sessions. the installed executable remains `fetchloom`; the alias belongs to your shell.

on a linux shell, use:

```sh
alias fl=fetchloom
```

## onedrive and synced folders

recognized synced folders skip hardlinks. they may still use reflinks, or copy when the drive cannot clone files. fetchloom detects onedrive roots through `OneDrive`, `OneDriveConsumer` and `OneDriveCommercial`, dropbox through its `info.json`, and the windows icloud drive folder.

stderr warning from a copied download inside onedrive:

```text
warning: iris is inside onedrive, which breaks links, so the file was copied. a folder outside onedrive lets fetchloom try links
```

use a project folder outside synced storage if you want fetchloom to try hardlinks. a copied file still has the same pinned bytes. [placement](concepts.md#placement) explains the order and writable-copy setting.

do not make a managed hardlink writable to edit it. set `copy = true` on the dataset and sync. windows may not expose another project's hardlink edit until that project syncs, cache verification runs, or `sync --force` reads each hardlink by name.

## paths and open files

use `/` inside `data.toml` paths and `fetchloom path iris/iris.csv` on windows too. returned absolute paths use the platform's filesystem representation.

fetchloom uses the windows long-path form for project metadata renames. a working directory can still be too long for `-C`; that error reports the path length and suggests moving the project to a shorter path before rerunning. other programs opening your data may still impose their own path limits. keep the project root and cache path short when working with deeply nested datasets. a short `path` in the manifest can shorten the dataset folder too.

if a replacement or deletion fails, close editors, notebooks or other programs holding the file open, then check permissions. [troubleshooting](troubleshooting.md) covers error handling without discarding local work.
