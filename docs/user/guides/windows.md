# windows

windows supports the same fetchloom commands, with placement affected by the drive and synced folders.

## defender and new files

many small files give Microsoft Defender many new files to scan. the [historical defender observations](../../dev/performance/measurements/historical.md#recorded-workload) describe one laptop; costs depend on the machine and its protection settings.

a dev drive with Defender's performance mode defers scans until after file open completes. [Microsoft's performance mode comparison](https://learn.microsoft.com/en-us/defender-endpoint/microsoft-defender-endpoint-antivirus-performance-mode#performance-mode-compared-to-real-time-protection) states the trade-off:

> Deferring scans until after file open completes inherently provides faster performance, but with less protection. However, enabling performance mode provides significantly better protection than other performance tuning methods, such as using folder exclusions, which block security scans altogether.

creating a dev drive needs an administrator. ask your IT team whether performance mode suits your machine. if the drive is `D:`, choose a cache there for the powershell session:

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
```

keep projects on the same drive to permit linking. changing the variable selects a cache location; it does not migrate the old cache. [cache folder](../reference/cache-folder.md#location) gives the defaults.

## a run that stops on a dev drive

a dev drive is formatted as ReFS, and windows' ReFS driver can lock up while it gives a file a name, which fetchloom does for each file it puts in the cache or a project. the command then waits for an answer that never comes, and no program can end it, task manager included. fetchloom waits 90 seconds for each name, then stops with an error on stderr and exit code 50, as this run of `fetchloom add .\iris.csv` shows:

```text
error: windows has not finished giving D:\docs-sample\.fetchloom\.gitignore its name after 1 min 30 s, so fetchloom stopped
  help: on a ReFS drive, such as a Dev Drive, windows can lock up while it names a file, and only a restart ends that. restart windows, then run `fetchloom add ".\iris.csv"` again: the files fetchloom finished are kept, and this window may not close before the restart
```

restart windows, then run the command again. the files fetchloom finished before the lock-up stay in the cache, so the next run does not download them again. until the restart the window may stay open, and other programs that use the drive may wait too. fetchloom's [tests on a new ReFS volume](../../dev/engineering/ci.md#filesystem-runs) met this about once in 160 runs, each within a minute of the volume's format.

## short name

the installer also makes `fl.exe` beside `fetchloom.exe`, made again on every install so it never runs an old version. `fl sync` runs `fetchloom sync` in cmd and other shells. powershell runs an alias before any program, and there `fl` is the alias of `Format-List`, so type `fl.exe sync`, or give `fl` to fetchloom in your powershell profile:

```powershell
Set-Alias fl fetchloom -Force
```

`-Force` is needed because the `Format-List` alias is read-only; `Format-List` keeps its full name. when another program is called `fl`, the installer makes no `fl.exe` and says so, and the alias still works in powershell.

## onedrive and synced folders

recognized synced folders skip hardlinks. they may still use reflinks, or copy when the drive cannot clone files. fetchloom detects onedrive roots through `OneDrive`, `OneDriveConsumer` and `OneDriveCommercial`, dropbox through its `info.json`, and the windows icloud drive folder. [macos](macos.md) lists the folders recognized there.

stderr warning from a copied download inside onedrive:

```text
warning: iris is inside onedrive, which breaks links, so the file was copied
  help: a folder outside onedrive lets fetchloom try links
```

use a project folder outside synced storage if you want fetchloom to try hardlinks. a copied file still has the same pinned bytes. [placement](../concepts/cache-and-placement.md#placement) explains the order and writable-copy setting.

do not make a managed hardlink writable to edit it. set `copy = true` on the dataset and sync. an edit made through another project's hardlink shows at once: `status` and `sync` open each hardlinked file to read it as it is, since a windows folder listing can keep the old size and time.

## red text in windows powershell

fetchloom writes its progress and results to stderr. windows powershell 5.1 shows stderr as red `NativeCommandError` text when you redirect it with `2>&1`, even when the command worked. the text is harmless; the exit code still tells you whether the command worked. powershell 7, or running without `2>&1`, shows the lines as they are.

## commands in help lines

a command fetchloom suggests puts each name in double quotes, which powershell and cmd both read as written. that covers a name holding a comma, which windows powershell 5.1 reads bare as a list and passes on changed, as `x,1kb` becomes `x,1024`, and one starting with a single `-` and holding a dot, which powershell splits at the dot. a `\` that ends a name is doubled inside the quotes, since a program reads `\"` as a quote. a name holding what cmd acts on even inside quotes, `& | < > ^ % !`, or what powershell's quotes cannot keep, `$`, a backtick or a double quote, is never put in a command, so pasting one runs nothing hidden in a name: the command shows a placeholder such as `"<file>"` or `"<folder>"`, in double quotes, so no shell reads it as a redirection from a file of that name, and a line of its own gives the name it stands for, to type quoted as your shell needs. one name keeps one placeholder across an error's or a warning's message, details and help, in `--json` too, where that line is one of the details, and across the rows a command such as `status` prints. `--json` gives such a help no `fix`.

## paths and open files

use `/` inside `data.toml` paths and `fetchloom path iris/Iris.csv` on windows too. returned absolute paths use the platform's filesystem representation.

fetchloom uses the windows long-path form for project metadata renames. a working directory can still be too long for `-C`; that error reports the path length and suggests moving the project to a shorter path before rerunning. other programs opening your data may still impose their own path limits. keep the project root and cache path short when working with deeply nested datasets. a short `path` in the manifest can shorten the dataset folder too.

if a replacement or deletion fails, close editors, notebooks or other programs holding the file open, then check permissions. [troubleshooting](../troubleshooting/README.md) covers error handling without discarding local work.

## company proxies

fetchloom uses the proxy that `HTTPS_PROXY` names, or the system's proxy settings when that is unset. a proxy that asks for your windows sign-in needs a local relay that signs in for you, as [company proxies](../troubleshooting/README.md#company-proxies) shows.
