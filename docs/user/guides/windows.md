# windows

windows supports the same fetchloom commands, with placement affected by the drive and synced folders.

## defender and new files

many small files give Microsoft Defender many new files to scan. the [historical defender observations](../../dev/performance/windows-defender.md#recorded-workload) describe one laptop; costs depend on the machine and its protection settings.

a dev drive with Defender's performance mode defers scans until after file open completes. [Microsoft's performance mode comparison](https://learn.microsoft.com/en-us/defender-endpoint/microsoft-defender-endpoint-antivirus-performance-mode#performance-mode-compared-to-real-time-protection) states the trade-off:

> Deferring scans until after file open completes inherently provides faster performance, but with less protection. However, enabling performance mode provides significantly better protection than other performance tuning methods, such as using folder exclusions, which block security scans altogether.

creating a dev drive needs an administrator. ask your IT team whether performance mode suits your machine. if the drive is `D:`, choose a cache there for the powershell session:

```powershell
$env:FETCHLOOM_CACHE = 'D:\fetchloom-cache'
```

keep projects on the same drive to permit linking. changing the variable selects a cache location; it does not migrate the old cache. [cache folder](../reference/cache-folder.md#location) gives the defaults.

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

## paths and open files

use `/` inside `data.toml` paths and `fetchloom path iris/Iris.csv` on windows too. returned absolute paths use the platform's filesystem representation.

fetchloom uses the windows long-path form for project metadata renames. a working directory can still be too long for `-C`; that error reports the path length and suggests moving the project to a shorter path before rerunning. other programs opening your data may still impose their own path limits. keep the project root and cache path short when working with deeply nested datasets. a short `path` in the manifest can shorten the dataset folder too.

if a replacement or deletion fails, close editors, notebooks or other programs holding the file open, then check permissions. [troubleshooting](../troubleshooting/README.md) covers error handling without discarding local work.

## recipe steps

`fetchloom build` runs each step of a [recipe](recipes.md) in a job object, a windows group of processes that ends together. when the step ends, anything it started and left running ends with it, and a stop, by ctrl-c or the `build_floor` setting, ends them all at once. the job also ends when fetchloom does, even when task manager or a cancelled CI job ends fetchloom outright, so nothing a step starts outlives it: start a server a step needs before the build, not from the step.

the console's ctrl-c reaches the step as well as fetchloom, so a step may print its own message, such as python's `KeyboardInterrupt`, before the build says it stopped.

a step's program is found as cmd.exe finds it: in the folders of `PATH`, in order, trying in each the extensions of `PATHEXT` that windows can start, `.com`, `.exe`, `.bat` and `.cmd`. so `uv` and `python` start, and a `.cmd` shim such as `npm.cmd` in an earlier folder runs before an `.exe` in a later one. a name that already ends with one of these, such as `npm.cmd`, is looked for as written, and a name with a folder in it, such as `tools/prep`, from the project folder. unlike cmd.exe, fetchloom never looks in the project folder first; write `./tool.bat` to run a program there. other types `PATHEXT` may list, such as `.js`, are passed over: name their program first, as in `run = ["node", "tool.js"]`.

a `.bat` or `.cmd` program runs through cmd.exe, which reads its arguments again. fetchloom leaves the quoting to rust, which quotes each argument so the program gets it as written, `%` and `&` included, and refuses one it cannot pass safely, such as an argument with a line break; that step does not start.

## company proxies

fetchloom uses the proxy that `HTTPS_PROXY` names, or the system's proxy settings when that is unset. a proxy that asks for your windows sign-in needs a local relay that signs in for you, as [company proxies](../troubleshooting/README.md#company-proxies) shows.
