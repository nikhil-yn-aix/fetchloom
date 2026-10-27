# CLI

the bar: someone who is nervous in a terminal runs fetchloom for the first time and feels looked after. every screen answers what happened, whether it is fine and what to do next, without a manual.

## the feel

- friendly in the welcome and the closing line, never cute in the middle of work or in an error.
- words a researcher uses: dataset, file, download, folder. "object" appears only with `-v`, and "cache" and "hash" only where they help, in a prompt or an error.
- quick: the first line appears within 100 ms, and every wait longer than that shows what it is waiting for.
- no live dashboard and no full-screen interface. lines scroll like any other command.

## a first run

```
$ fetchloom
fetchloom 0.1.0

get a first dataset into this folder:

  fetchloom add zenodo:3608990

  fetchloom add <link>    add a dataset, pin it and download it into data/
  fetchloom sync          make data/ match data.lock on this machine
  fetchloom status        see what changed
  fetchloom path <name>   print where a dataset is, for scripts
  fetchloom get <link>    fetch something once, without a project

<link> can be a DOI, a zenodo, figshare, dataverse, osf, hugging face or kaggle id, or any link you would paste in a browser.
```

```
$ fetchloom add zenodo:3608990
   Resolved zenodo:3608990 · DatasetIris · 1 file · 4.1 KiB · CC-BY-4.0
 Downloaded iris.csv · 4.1 KiB in 0.4 s
     Locked 1 file in data.lock
     Placed data/iris · linked, no extra disk
✓ added iris. its files are in data/iris
  hint: scripts can find it with `fetchloom path iris`
```

```
$ fetchloom sync
✓ everything already matches data.lock. nothing to do
```

## while it works

- a step that has finished prints one line: a right-aligned verb, then the facts. the same rhythm as cargo and uv.
- a step in progress shows a spinner and a present-tense verb that names the real action and the host, plus elapsed time after 2 seconds: `⠹ downloading from zenodo.org · 12 s`.
- downloads show one bar per active file, as many as fit the terminal, plus one bar for the total. a finished download collapses into its summary line.
- the rate is a moving average, and the time left stays hidden until the rate settles.
- every run ends with one line: a green `✓` saying what happened, or a red `✗` with the reason. at most one hint follows it, and only when it teaches something not yet done.
- a run that changes nothing says so. silence looks like a hang or a bug.

## before anything moves

- what a reference resolved to is printed before the first byte: title, version, file count, size, license.
- a download over 1 GiB asks first on a terminal. outside one, it stops and names the flag: `--yes`.
- a license that restricts use, such as non-commercial or no derivatives, is shown as a yellow line.
- a gated dataset (a hugging face repository that needs its terms accepted, a kaggle competition) stops with the page to open.

```
$ fetchloom add hf:datasets/big-org/huge-corpus
   Resolved hf:datasets/big-org/huge-corpus@3f2a91c · 412 files · 186.0 GiB · CC-BY-NC-4.0
    License CC-BY-NC-4.0 does not allow commercial use
download 186.0 GiB into the cache? [y/N]
```

## errors

```
✗ the pattern "*.edf" matched no file in eeg
  data.toml:7   select = ["*.edf"]
  12 files were considered, for example sub-01/eeg/sub-01_task-mi_eeg.edf
  help: patterns match the whole path. try "**/*.edf"
```

```
✗ iris.csv from zenodo:3608990 is not the file that was locked
  expected sha256 b1f0b46c3e27, got 7a90c2d14e5b
  help: if the new version is intended, run `fetchloom lock --upgrade iris`
```

```
✗ drive refused the download: too many people downloaded this file recently
  this is a temporary limit on google's side, usually lifted within a day
  help: try again later, or ask the owner for another link
```

- an error says what happened in one sentence, shows where when the cause is in a file, and ends with `help:` and what to do next, as a command to copy when there is one.
- an error is printed once. more detail comes with `-v`, never by default.
- a mistyped command or dataset name gets "did you mean", never a guess that runs.
- the error's kind, such as `integrity.mismatch`, appears with `-v` and in `--json`.

## streams and loudness

- stdout carries the answer and nothing else: a path, or a JSON document. `fetchloom path iris > p.txt` leaves one line in the file.
- stderr carries the story: progress, what was resolved, warnings, errors, hints.
- `--json` replaces the story with one JSON document on stdout, described in [formats](formats.md#the-json-document).

| flag | shows |
|---|---|
| `-q` | the answer only |
| none | what was resolved, progress, one line per finished step, warnings, errors |
| `-v` | plus detail per dataset, the cache and placement used, timings and counters |
| `-vv` | plus debug logs |

each level adds lines and never removes one.

## numbers

one module formats every size, rate, duration and count: `4.1 KiB`, `84 MiB/s`, `0.4 s`, `1,204 files`. a hash prints as its first 12 hex characters, and in full with `-v` and in `--json`. a path or a reference is never cut short.

## color and symbols

- green for done, cyan for in progress, yellow for warnings, red for errors, dim for detail. meaning never depends on color alone: every colored line also carries a word or a symbol.
- color is decided once at start, in this order: `--color`, then `NO_COLOR`, then `CLICOLOR_FORCE`, then `CLICOLOR=0`, then `TERM=dumb` or `CI`, then whether stderr is a terminal.
- where the terminal cannot show unicode, such as the legacy windows console, symbols fall back to ASCII: `[ok]`, `[x]`, `[!]` and a `-\|/` spinner.
- off a terminal, or in CI, there are no spinners, no bars and no prompts. lines print plainly and a question becomes an error naming the flag that answers it.

the crates that draw all of this are listed in [decisions](decisions.md#19-the-terminal-stack-is-small).
