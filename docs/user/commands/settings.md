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

there are no command-specific options. [global options](README.md#global-options) apply.

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
| `environment` | a nonempty `FETCHLOOM_*` variable |
| `settings file` | the [settings file](../reference/environment.md#settings-file) |
| `this machine` | sized from this machine: its processors or the memory it has available |
| `default` | fetchloom's own default |

the [environment](../reference/environment.md) page explains each setting. `threads`, `rate` and `split` come from their variables only. stderr then says what fetchloom read about the machine, its processors, available memory and open-file limit, and where the settings file is, or that there is no file there yet.

`settings` needs no project, downloads nothing and changes nothing. a bad value in a variable or in the settings file stops it as it stops every named command, so it also checks the settings file.

JSON returns `result.settings`, an array of `name`, `value`, `origin` and `variable` records. `value` is a number for counts, bytes and seconds, text for the cache folder and the split mode, and null for no rate cap or no cache folder. `origin` is `environment`, `file`, `machine` or `default`. `result.machine` holds `processors`, `memory`, the bytes this machine has available, read on linux only and null elsewhere, and `open_files`, the most files a run may hold open or null where the system sets no limit. `result.file` holds `path`, null when there is no folder for a settings file, and `found`.

## exit codes

0 means the settings were shown. a bad value in a variable or in the settings file fails with kind `usage` and exit 2, naming it.
