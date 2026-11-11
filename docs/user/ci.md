# CI

a CI job can restore a project's data without downloading it on every run: keep the fetchloom cache between jobs, keyed on `data.lock`, so the bytes come down once per change to the lock.

## github actions

after checkout, with fetchloom [installed](getting-started.md#install):

```yaml
env:
  FETCHLOOM_CACHE: ${{ github.workspace }}/.fetchloom-cache

steps:
  - uses: actions/cache@v6
    with:
      path: .fetchloom-cache
      key: data-${{ runner.os }}-${{ runner.arch }}-${{ hashFiles('data.lock') }}
      restore-keys: |
        data-${{ runner.os }}-${{ runner.arch }}-
  - name: restore research inputs
    shell: bash
    run: fetchloom sync --locked --yes
  - name: keep only what this lock needs
    shell: bash
    run: fetchloom cache gc --now
```

- the exact key hits when `data.lock` is unchanged. every file is then in the cache, and sync places them without using the network.
- after a change to `data.lock`, `restore-keys` brings back the last cache, so only the files that changed download.
- `cache gc --now` drops the files the new lock no longer needs before the cache is saved, so it does not grow with every change.
- `--locked` fails with `lock.outdated`, exit 1, when `data.toml` and `data.lock` disagree, instead of changing the lock in CI.
- without `--yes`, a download larger than 1 GiB, or the size `FETCHLOOM_ASK_ABOVE` sets, or one the cache's disk has no room for, fails the job with `unconfirmed`, exit 1, instead of starting.

## gitlab ci

```yaml
variables:
  FETCHLOOM_CACHE: "$CI_PROJECT_DIR/.fetchloom-cache"

restore-data:
  cache:
    key:
      files: [data.lock]
      prefix: data
    paths: [.fetchloom-cache]
  script:
    - fetchloom sync --locked --yes
    - fetchloom cache gc --now
```

jobs on different systems, such as linux and windows runners, should use different prefixes, as the github key does with `runner.os`.

a kubernetes job or another container keeps its cache on a volume instead; [notebooks and containers](notebooks.md#a-kubernetes-job) has an example with a read-only root filesystem.

## keys

a private, gated or competition dataset needs its source's key in the job's environment, from the CI system's secrets: `HF_TOKEN` for hugging face, `KAGGLE_API_TOKEN` for kaggle, and the others in [environment](reference/environment.md#credentials). never write a key into the cache folder or the repository; fetchloom keeps keys out of its cache, logs and output.

## a mirror for runners

runners inside a lab or company network can take pinned files from a [mirror](mirrors.md) there instead of the internet: set `FETCHLOOM_MIRRORS` in the job's environment to the mirror's folder or address. a runner that must not reach the internet at all also sets `FETCHLOOM_MIRRORS_ONLY=1`, so a file the mirror lacks fails the job, named, rather than being fetched from its source.

## exact inputs

`--locked` trusts a cached file whose size and time still match, so a cached file that changed but kept both is placed as it is. a job that needs exactly the pinned bytes adds `--verify`:

```sh
fetchloom sync --locked --verify --yes
```

it hashes every placed file, downloads again any whose bytes differ from `data.lock`, and keeps the changed copy beside it as `<path>.changed`, with a warning. it reads every byte of the data, so it takes longer than plain `--locked`.

## recipes

in a project with [recipes](recipes.md), `fetchloom sync --locked` places each pinned result from the cache or a mirror, and fails with `build.needed`, exit 60, when a recipe changed since it was built or neither holds its result, as on a fresh runner. a job that builds runs the repository's code, so give it only branches you trust:

```sh
fetchloom build --json
```

a job that builds commits nothing: commit `data.lock` and `data.lock.d` from the machine you built on, so a runner places the same results. a step that fails exits 60 with `build.failed`, and the last lines of its output are in `errors[0].details`.

## checking the result

the commands above check that sync worked. a job that must also fail on files someone edited in the data folder can read `--json` output, as [scripting](scripting.md#checking-and-syncing) shows. [exit codes](reference/exit-codes.md) lists every failure kind and what to do next.
