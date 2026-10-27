# vision

fetchloom puts the exact data a project needs onto any machine with one command, from wherever that data lives, and keeps one copy of it per machine.

## the problem

research code needs data, and the data lives everywhere: zenodo, figshare, dataverse, osf, hugging face, kaggle, google drive, dropbox, github releases and plain links. so a project's readme turns into a list of links to click, archives to unzip and folders to rename.

- links die. the odds that a paper's data still exists fall about 17% a year ([vines et al. 2014](https://doi.org/10.1016/j.cub.2013.11.014)).
- files change upstream without notice, and nobody can say which version a result used.
- google drive refuses popular files with "too many users" and answers large ones with a virus scan page instead of the file.
- every project, labmate and cluster node downloads its own copy.

downloading is already solved. gdown, zenodo_get, datahugger, huggingface_hub and kagglehub fetch from one place or many. none of them pins what you got across all those hosts, or tells a second machine exactly which bytes to fetch.

## who it is for

first, two groups:

- people who share research code that needs data, and everyone who runs that code. the data section of the readme shrinks to one line: `uvx fetchloom sync`.
- students and ML researchers who pull from hugging face, kaggle, zenodo and drive links into several projects at once.

once they are served well: labs on HPC clusters, where one shared cache saves quota and compute nodes have no internet.

## what you get

- `data.toml`, the one file you write: the datasets your project needs.
- `data.lock`, the one file fetchloom writes: every file pinned by SHA-256, with the places it was found.
- `fetchloom sync` on any machine gives the same bytes, or names each missing file and where it was last seen.
- a second project that asks for data already on the machine gets it at once, with no download and no extra disk.
- archives unpacked safely and files placed under `data/`, where your code expects them.

## principles

1. a hash is a file's identity. a URL is only a hint about where to find it.
2. nothing happens silently. a changed file, a dead link or a fallback is always said out loud.
3. a run that stops halfway breaks nothing, and running it again finishes the job.
4. stdout is the answer, stderr is the story, `--json` is for programs.
5. one copy of each file per machine.
6. kind by default. every screen says what happened, whether it is fine, and what to do next.
7. fast by design first, then proven with a profiler.
8. when another tool does a job well, we call it instead of rebuilding it.

## what we will not build

| not building | instead |
|---|---|
| a workflow engine or a preprocessing cache | snakemake, nextflow or make, calling `fetchloom path` |
| data versioning, branches and merges | dvc, oxen, lakefs |
| publishing to repositories | the repository's own upload page |
| copying or mirroring between clouds | rclone |
| a dataset search engine | you paste the link you found |
| a plugin system | sources live in this repository |
| a GUI, a terminal dashboard, editor extensions | a good terminal and `--json` |
| hosting or accounts | a shared cache is a folder |
| access to controlled data like dbGaP or UK biobank | institutions grant that, not tools |
| telemetry | fetchloom talks only to the hosts your data lives on |

## success

- someone who has never seen fetchloom gets a first dataset with `uvx fetchloom get <link>` in under a minute.
- `fetchloom sync` in a fresh clone gives byte-identical data on windows, macos and linux.
- a second project that needs data already on the machine moves zero bytes, and for a dataset of a thousand files it finishes in under a second.
- a project cloned years later syncs the same bytes from wherever they still exist, or names each missing file and its last known location.
- three researchers use it on real projects before anything new is added.
