# settings

`settings` shows each setting a run would use, its value and where the value came from, then what fetchloom read about this machine and where the settings file is.

```sh
fetchloom settings
fetchloom settings --json
```

## usage

```sh
fetchloom settings [options]
```

there are no command-specific options. [global options](README.md#global-options) apply; the mirrors `--mirror` names show in the `mirrors` row.

## what it shows

stdout holds one line per setting: its name, its value, where the value came from and the variable that changes it.

```text
cache      /scratch/lab/fetchloom-cache  settings file  FETCHLOOM_CACHE
jobs       32                            settings file  FETCHLOOM_JOBS
host_jobs  8                             environment    FETCHLOOM_HOST_JOBS
retries    4                             default        FETCHLOOM_RETRIES
memory     4.0 GiB                       this machine   FETCHLOOM_MEMORY
```

| origin | where the value came from |
|---|---|
| `environment` | a nonempty `FETCHLOOM_*` variable, or an empty `FETCHLOOM_MIRRORS`, `FETCHLOOM_MIRRORS_ONLY` or `FETCHLOOM_PUSH_TO`, which clears the mirrors, mirror-only mode or push folder the settings file names |
| `settings file` | the [settings file](../reference/environment.md#settings-file) |
| `this machine` | sized from this machine: its processors, the memory it has available, or for `build_floor` the disk holding the cache |
| `default` | fetchloom's own default |
| `command line` | `--mirror`, for the mirrors it names. a run asks them before those of the setting, so with a setting that names others the `mirrors` row shows both, as `command line, settings file` |

the [environment](../reference/environment.md) page explains each setting. `threads`, `rate` and `split` come from their variables only. stderr then says what fetchloom read about the machine, its processors, available memory and open-file limit, and where the settings file is, or that there is no file there yet.

`settings` needs no project, downloads nothing and changes nothing. a bad value in a variable or in the settings file stops it as it stops every named command, so it also checks the settings file.

JSON returns `result.settings`, an array of `name`, `value`, `origin` and `variable` records. `value` is a number for counts, bytes and seconds, text for the cache folder, the `push_to` folder and the split mode, a list of text for `allowed_origins`, `s3_buckets` and `mirrors`, true or false for `mirrors_only`, and null for no rate cap, no cache folder or no `push_to` folder. `origin` is `environment`, `file`, `machine`, `default` or `flag`, for `--mirror`; mirrors from `--mirror` and from a setting that names others show both, the flag first, as `flag,environment` or `flag,file`. `result.machine` holds `processors`, `memory`, the bytes this machine has available, read on linux only and null elsewhere, and `open_files`, the most files a run may hold open or null where the system sets no limit. `result.file` holds `path`, null when there is no folder for a settings file, and `found`.

## exit codes

0 means the settings were shown. a bad value in a variable or in the settings file fails with kind `usage` and exit 2, naming it. a settings file that is not TOML is named with the line and column where reading it stopped, without quoting that line, and a key one typo from a setting, such as `jbos`, is told the setting it is near.
