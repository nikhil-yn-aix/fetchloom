# user guide

new to fetchloom? start with [getting started](getting-started.md).

| page | when to read it |
|---|---|
| [getting started](getting-started.md) | install fetchloom, add a first dataset, restore it and undo it |
| [concepts](concepts.md) | understand pins, placement, archives and offline work |
| [working with raw data](raw-data.md) | keep results apart from inputs, leave out bad samples, edit inputs and free space |
| [other tools](other-tools.md) | move a download step from curl, gdown, kaggle, hf, aws, dvc or pooch to fetchloom, keep the tools you use, work beside dvc, git-lfs or datalad, or leave |
| [recipes](recipes.md) | declare a dataset your own command makes from others, build it, and see when it needs building again |
| [commands](commands/README.md) | choose a command and check global options |
| [add](commands/add.md) | put a dataset in a project and download it |
| [sync](commands/sync.md) | restore pinned inputs and handle local conflicts |
| [lock](commands/lock.md) | check or update exact input versions |
| [build](commands/build.md) | run the recipes in data.toml, pin what they made and place it |
| [list](commands/list.md) | see dataset references, locked sizes and folders |
| [export](commands/export.md) | write checksums or links of the project's files for other tools |
| [push](commands/push.md) | copy a project's pinned files into a folder mirror others sync from |
| [search](commands/search.md) | find a dataset by words and choose a canonical reference |
| [status](commands/status.md) | find missing, edited and extra files |
| [path](commands/path.md) | locate a dataset, file or subfolder |
| [remove](commands/remove.md) | remove an entry without discarding edited files |
| [drop](commands/drop.md) | leave bad samples or unwanted files out of a dataset for good |
| [get](commands/get.md) | fetch data once without project files |
| [cache](commands/cache.md) | inspect, collect or verify cached bytes |
| [settings](commands/settings.md) | see each setting and where it came from |
| [auth](commands/auth.md) | sign in for private, restricted or gated data with one key |
| [completions](commands/completions.md) | let your shell complete commands, options and dataset names |
| [sources](sources/README.md) | compare accepted references, pins, credentials and limits |
| [URLs and DOIs](sources/urls-and-dois.md) | fetch a direct link or resolve a persistent handle |
| [zenodo](sources/zenodo.md) | choose records or individual record files |
| [figshare](sources/figshare.md) | pin article versions, including institutional articles |
| [dataverse](sources/dataverse.md) | use public dataset or file versions |
| [dryad](sources/dryad.md) | pin published dataset versions, downloaded with a dryad API key |
| [mendeley data](sources/mendeley.md) | pin published dataset versions and their folders |
| [ENA](sources/ena.md) | list the read files of runs, experiments, samples and studies |
| [GEO](sources/geo.md) | list the files GEO keeps for a series, a sample or a platform |
| [DANDI](sources/dandi.md) | list the assets of a dandiset at one version, checked against the SHA-256 dandi states |
| [OSF](sources/osf.md) | fetch osfstorage files and understand node changes |
| [hugging face](sources/hugging-face.md) | pin repository commits and configure approved access |
| [kaggle](sources/kaggle.md) | use dataset versions and competitions with existing kaggle credentials |
| [google drive](sources/google-drive.md) | use shared files or folders, read private ones you list, and exclude google docs |
| [dropbox](sources/dropbox.md) | download shared files or folder zips |
| [github and gitlab releases](sources/releases.md) | pin the files of a release at its tag |
| [cloud buckets](sources/buckets.md) | list and pin S3 and google cloud storage prefixes, private S3 buckets with your AWS keys and private cloud storage buckets with your google credentials |
| [azure blob storage](sources/azure.md) | list and pin azure containers and folders, private ones with a SAS you keep out of the project |
| [data.toml](reference/data-toml.md) | edit datasets, paths and selection patterns |
| [data.lock](reference/data-lock.md) | inspect pins, spec hashes and archive tree hashes |
| [JSON output](reference/json-output.md) | parse result fields, warnings and work counters |
| [exit codes](reference/exit-codes.md) | branch on failure kinds and choose a recovery step |
| [environment](reference/environment.md) | set credentials, cache location, limits or terminal behavior |
| [compatibility](reference/compatibility.md) | check which systems, terminals, sources and versions are supported |
| [cache folder](reference/cache-folder.md) | inspect shared cache objects and indexes |
| [project folder](reference/project-folder.md) | inspect local owner records and placement state |
| [scripting](scripting.md) | use JSON recipes or the python helper |
| [mirrors](mirrors.md) | restore pinned files from a lab disk, a usb drive or a company server instead of their sources |
| [CI](ci.md) | keep data between CI jobs so it downloads once per lock change |
| [clusters](hpc.md) | use a cluster's home and scratch disks, many jobs and offline nodes, or share one cache in a lab |
| [resources](resources.md) | trade speed against memory with one word, lean, default or max, and see what a run uses |
| [notebooks and containers](notebooks.md) | keep the cache in hosted notebooks, dev containers and kubernetes jobs, with read-only roots and small disks |
| [AI agents](ai-agents.md) | let a coding agent find and fetch data safely |
| [windows](windows.md) | choose drives, avoid synced-folder surprises and handle paths |
| [macos](macos.md) | find the cache, use apfs clones and icloud drive, and handle unicode names |
| [troubleshooting](troubleshooting.md) | match a symptom to its cause and fix |
| [limits](limits.md) | check what fetchloom does not do |

for code changes, start with the [developer guide](../dev/README.md).
