# troubleshooting

find the error or warning you see in the tables below, with its cause and fix.

```sh
fetchloom status
fetchloom -v sync
```

verbose output adds [what the run spends](../guides/speed-and-memory.md#see-what-a-run-uses), error kinds and work counters. use `--json` for [structured failures](../reference/json-output.md), including every failed file in a download batch.

## collect a debug report

run the failing command again with `-vv` and save stderr, then add the version line:

```sh
fetchloom -vv sync 2> fetchloom-debug.txt
fetchloom --version -v >> fetchloom-debug.txt
```

the version line names the commit fetchloom was built from, or says `commit unknown, built outside git`. on windows, run these lines in cmd or powershell 7, since windows powershell 5.1 rewrites redirected stderr. attach `fetchloom-debug.txt`, the exit code, and the `data.toml` and `data.lock` entries for the dataset. the debug lines say where each file came from, why a file was hashed again, how it was placed, which host a key was sent to, and each retry and redirect.

keys are never logged. a link shows its login as `***`, and each value in its query or fragment too, unless the value's name is known to be harmless, such as `dl`, `version`, `page` or dropbox's `rlkey`, so a token under a name fetchloom does not know stays out of the log. the log also names local paths and every link fetchloom asked for, search words included, so read the file before sharing it. `FETCHLOOM_LOG=fetchloom::net=debug` limits the log to one part of fetchloom when the full log is too long.

## company proxies

fetchloom sends its requests through the proxy that `HTTPS_PROXY` or `HTTP_PROXY` names, or `ALL_PROXY` for both; [environment](../reference/environment.md) lists them. on windows and macos, the system's proxy settings fill in when those variables are unset. a proxy that cannot be reached stops the run within seconds, naming its address and the variable that set it, rather than being tried again. a proxy that inspects https needs its certificate in the system's certificate store, as the certificate error says.

a proxy that asks for your windows sign-in, by NTLM or Kerberos, refuses every request until it gets that sign-in, which fetchloom cannot send. fetchloom then stops at the first answer, with `the proxy at <proxy> asked for a login before it would reach <host>`, exit 20, `network.unreachable`, since no other try would get further. `curl -v -x http://proxy.example.org:8080 https://huggingface.co`, with your proxy's address, shows `407 Proxy Authentication Required` and a `Proxy-Authenticate` header naming `NTLM` or `Negotiate`. a local relay can sign in for you: [px](https://github.com/genotrance/px) uses your windows sign-in, and [cntlm](https://github.com/versat/cntlm) the user name and password in its settings. both take requests from this machine on port 3128 by default, so point fetchloom there:

```powershell
$env:HTTPS_PROXY = 'http://127.0.0.1:3128'
$env:HTTP_PROXY = 'http://127.0.0.1:3128'
```

```sh
export HTTPS_PROXY=http://127.0.0.1:3128
export HTTP_PROXY=http://127.0.0.1:3128
```

the variables win over the system's settings, and fetchloom reaches a proxy on a private or loopback address, such as a relay on your own machine. to keep the variables, put the lines in your shell's profile, as the next section shows.

## slow or metered links

`FETCHLOOM_RATE` caps the bytes fetchloom receives, all downloads together, such as `500K` for 500 KiB a second; [environment](../reference/environment.md) gives the units. the settings file has no key for it, so to keep a cap in every session, set it in your shell's profile: `~/.bashrc` for bash, `~/.zshrc` for zsh, or the file `$PROFILE` names in powershell.

```sh
export FETCHLOOM_RATE=500K
```

```powershell
$env:FETCHLOOM_RATE = '500K'
```

in a new terminal, `fetchloom settings` shows the `rate` row coming from the environment.

## project recovery

- restore a broken lock from git when you need its exact pins; deleting it and running `fetchloom lock` can resolve newer inputs. a lock with a higher `version` comes from a newer fetchloom and needs one.
- a failed `add` puts `data.toml` and `data.lock` back. if that fails, copy them back from the recovery file the error names under `.fetchloom/`, which holds their bytes as they were; never paste it into a log, since it may hold credentials.
- a `remove` that changed `data.toml` and `data.lock` but could not delete files finishes with `fetchloom sync` once you fix the reported error.
- a command that was killed can leave a record that it holds the project. delete only the exact path a waiting command names, and only once the process it names has ended, as [owner record](../reference/project-folder.md#owner-record) explains.

### a project too big for its container

in a container with a memory limit, fetchloom compares what reading a project's files takes, about 650 bytes a file data.lock pins, with what the limit leaves, and stops before reading them when that is too little, with `local.io`, exit 50. in a container limited to 64 MiB, the status of a project of 100,000 files says:

```text
error: reading this project's 100,000 files takes about 70.0 MiB of memory, more than the 60.6 MiB this container leaves fetchloom
  help: raise the container's memory limit by 9.3 MiB or more, or name one dataset, as in `fetchloom path <name>`, which reads only its files
```

`fetchloom path <name>` and a sync of named datasets already in place read only those datasets' pins and records, so they still answer there. memory a system has available outside a container is not checked: past it the system pages memory out rather than ending the run.

### the system cannot start another worker

before it starts, fetchloom checks that the system lets it start a thread, and stops with `local.io`, exit 50, when it does not. lower `FETCHLOOM_THREADS`, or raise the process's or container's thread limit, then retry. this is a resource limit, not a damaged cache.
