# Questions and interruptions

## approval

```sh
fetchloom sync --yes
```

a download of more than 1 GiB asks first. `FETCHLOOM_ASK_ABOVE`, or `ask_above` in the [settings file](../reference/environment.md#settings-file), moves that line, as `FETCHLOOM_ASK_ABOVE=50G` on a machine meant for big data.

| download | bytes counted |
|---|---|
| `get` | known bytes still to download; cached files with checksums and unpacked archives do not count |
| project commands | known bytes still missing from the cache |
| a file of unknown size | the length its server states, or the bytes received once they pass the line |
| a file of known size | counted up front; a server stating another length is refused before the download, and a body without a stated length stops once it passes the known size |

a batch asks at most one question about files of unknown size. offline runs never ask.

fetchloom also checks the free space on the cache's disk. a run that needs more than is free, counting each archive whose files are not known yet once more for unpacking, asks before anything starts, naming the cache folder and both numbers; a sync asks once for all its downloads. `--yes` goes on anyway, since a disk that compresses what it stores can hold more than it says. a file whose server never states its length is checked as it arrives and stops before it fills the disk.

`--yes` or `-y` answers yes to every question, but never picks a dataset from a [search](../commands/search.md#choosing-a-dataset). a question shows its default in capitals, as `[y/N]`: downloads, a project in your home folder and the offer to sign in after a refusal default to no, and saving a key you typed defaults to yes. answer y, yes, n or no and press enter; enter alone takes the default, and any other answer asks again. questions need a terminal on stdin and stderr, so pipes, CI, [coding agents](../guides/ai-agents.md) and `--json` cannot answer them: an unapproved download fails with exit 1 and `unconfirmed`. a run asks before it places any file, so a no changes nothing, except in a `sync` that had already written `data.lock`, which stays.

## interruptions

| ctrl-c comes | fetchloom prints | exit |
|---|---|---|
| at a question, a sign-in prompt or the dataset picker | one `Cancelled` line, such as `Cancelled the download of huge.bin (2.0 GiB), so nothing changed`, since a cancel you chose is not an error | 130 |
| during other work | `Stopping (press ctrl-c again to quit at once)`, keeps what it downloaded, and ends with one `Stopped` line, such as `Stopped while talking to 127.0.0.1` | 130 |
| once every byte is downloaded and only the disk is still writing | `Finishing (the download is complete; ctrl-c again to quit at once)`, and the run finishes | 0 |
| once a command is writing its changes or its last lines | `Finishing what it started (ctrl-c again to quit at once)`, then its usual closing line, such as `Added iris` | 0 |
| a second time | `Quit at once`, without waiting for the work to wind down | 130 |

the two `Finishing` moments finish, since stopping there would throw work away. a dim line under `Stopped` says how much the unfinished downloads keep and whether the next run continues from there, which needs a server that sends parts of a file; a later run that cannot continue says so once, as `Restarting big.bin · the 2.0 MiB an earlier run kept cannot be continued`, and starts that file over. a first `add` quit at once before it wrote anything leaves its folder as it was. on a slow disk, the process can outlast the `Quit` line until the operating system finishes flushing a file.

on linux and macos, SIGTERM, which `kill`, a container's stop and a scheduler such as slurm send, does what a first ctrl-c does in every command, in its own words: `Stopping (a second SIGTERM quits at once)`, then a `Stopped` line that names it, such as `Stopped by a SIGTERM while talking to zenodo.org, so nothing changed`, and exit 130. with `--json`, the run still prints its one document, with an `interrupted` error. a second SIGTERM, or a ctrl-c after it, quits at once.
