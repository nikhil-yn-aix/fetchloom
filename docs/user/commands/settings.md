# settings

`settings` shows each setting a run would use, its value and where the value came from, then what fetchloom read about this machine and where the settings file is.

```sh
fetchloom settings
fetchloom settings --json
```

## usage

```sh
fetchloom settings [options]
fetchloom settings set <key> <value>...
fetchloom settings unset <key>
```

there are no command-specific options. [global options](global-options.md) apply; the mirrors `--mirror` names show in the `mirrors` row. `settings set` saves one setting in the settings file and `settings unset` takes it out; `fetchloom help settings set` says how. `settings set cache` refuses a folder runs would refuse, such as one holding other files, as [cache folder](../reference/cache-folder.md#location) says. a relative folder is taken from the folder you run it in, so one given inside a project lands there: a cache, `mirrors` or `push_to` folder in a git work tree that no ignore rule keeps it out of is saved with a `git.folder_in_tree` warning, since `git add .` there would take what fetchloom writes in it for every project. on windows, `settings set cache` to a folder outside your user profile makes each run that opens the cache warn with `cache.foreign`, since such a folder may let other users of the machine change it, as [cache folder](../reference/cache-folder.md) says.

## what it shows

stdout holds one line per setting: its name, its value, where the value came from and the variable that changes it, which is always `FETCHLOOM_` and the name in capitals. at a terminal, a value wider than 24 columns, such as a deep cache folder, puts the rest of its row on a line of its own, while a pipe or a file keeps every row on one line, so `grep cache` finds its origin too; a terminal too narrow for every column first gives the values less room, down to 12 columns, a longer value putting the rest of its row on a line of its own as a deep folder does, so an 80-column terminal keeps the variables; narrower still, it leaves out the variable, then, when a value and its origin still do not fit side by side, puts each origin on a line of its own under its value. the first rows from a real run:

```text
resources         default                  default        FETCHLOOM_RESOURCES
cache             /scratch/lab/fetchloom-cache
                                           settings file  FETCHLOOM_CACHE
jobs              32                       settings file  FETCHLOOM_JOBS
host_jobs         8                        environment    FETCHLOOM_HOST_JOBS
retries           4                        default        FETCHLOOM_RETRIES
timeout           30 s                     default        FETCHLOOM_TIMEOUT
ask_above         10.0 GiB                 default        FETCHLOOM_ASK_ABOVE
memory            4.0 GiB                  this machine   FETCHLOOM_MEMORY
```

| origin | where the value came from |
|---|---|
| `environment` | a nonempty `FETCHLOOM_*` variable, or an empty `FETCHLOOM_ALLOWED_ORIGINS`, `FETCHLOOM_S3_BUCKETS`, `FETCHLOOM_GCS_BUCKETS`, `FETCHLOOM_AZURE_CONTAINERS`, `FETCHLOOM_DRIVE_FOLDERS`, `FETCHLOOM_MIRRORS`, `FETCHLOOM_MIRRORS_ONLY` or `FETCHLOOM_PUSH_TO`, which clears what the settings file names there |
| `settings file` | the [settings file](../reference/environment.md#settings-file) |
| `this machine` | sized from this machine: its processors or the memory it has available |
| `default` | fetchloom's own default |
| `resources = lean`, `resources = max` | the [resources level](../guides/speed-and-memory.md), for a setting no variable or settings file key sets. the `resources` row shows the level and where it came from |
| `variable only` | `threads`, `split` and `accessible`, which only their variables set, never the settings file, when no variable sets them. a variable that does shows as `environment` |
| `command line` | `--mirror`, for the mirrors it names. a run asks them before those of the setting, so with a setting that names others the `mirrors` row shows both, as `command line, settings file` |

the [environment](../reference/environment.md) page explains each setting. `threads`, `split` and `accessible` come from their variables only, and the settings file refuses them by their variable's name: `threads is set by FETCHLOOM_THREADS, not the settings file`. stderr then says what fetchloom read about the machine, its processors, available memory and open-file limit, and where the settings file is, or that there is none yet.

a variable one typo from one fetchloom reads changes nothing, so every command but `help` and `completions` warns about it, naming the variable it is near and never its value. other unknown names are left alone, since a wrapper or another version of fetchloom may read them. `FETCHLOOM_ACCESSIBLE` and `FETCHLOOM_NO_HINTS` change only what you see, so a value in either that is neither true nor false leaves it off with a warning instead of stopping the command.

```text
warning: FETCHLOM_CACHE is set, but fetchloom reads no such variable, so it changes nothing
  help: did you mean FETCHLOOM_CACHE?
```

`settings` needs no project, downloads nothing and changes nothing. a bad value in a variable or in the settings file stops it as it stops every named command, so it also checks the settings file. `--no-config`, or `FETCHLOOM_NO_CONFIG=1`, leaves the file unread for the run, so the variables and defaults show, and stderr says so:

```text
   Settings ~/.config/fetchloom/settings.toml (bypassed by --no-config)
```

a file `FETCHLOOM_SETTINGS` names is read in place of the usual one, never beside it, so while the usual one is there, the line says it is not read. stderr of a real run under a lab's settings file, its path shortened:

```text
   Settings /srv/lab/settings.toml, which FETCHLOOM_SETTINGS names, so ~/.config/fetchloom/settings.toml is not read
```

where you may not change that file, as a lab's shared one, the hint below it says so in place of offering `fetchloom settings set`, and names a `FETCHLOOM_*` variable or a settings file of your own as the ways to change a value.

JSON returns `result.settings`, an array of `name`, `value`, `origin` and `variable` records; a setting only its variable sets keeps its `origin` there, as `default` or `machine` where no variable sets it. `value` is a number for counts, bytes and seconds, text for folders and the split mode, a list of text for `allowed_origins`, `s3_buckets`, `gcs_buckets`, `azure_containers`, `drive_folders`, `mirrors` and `reuse`, true or false for `mirrors_only`, `web_archives` and `accessible`, and null for no rate cap or no folder. `origin` is `environment`, `file`, `machine`, `default`, `flag` or `resources`, as in the table above, and `flag,environment` or `flag,file` for mirrors from both. `result.machine` and `result.file` hold what stderr says; [JSON output](../reference/json-output.md#command-results) gives their fields.

## exit codes

0 means the settings were shown. a bad value in a variable or in the settings file fails with kind `usage` and exit 2, naming it; an error about the settings file ends its help with `--no-config runs without the settings file`, since `--no-config` lets `settings` show the rest. a settings file that is not TOML is named with the line and column where reading it stopped, without quoting that line, and a key one typo from a setting, such as `jbos`, is told the setting it is near.
