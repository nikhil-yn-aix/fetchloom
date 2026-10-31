# user guide

fetchloom manages research inputs through a manifest, a lock and one shared cache.

| page | when to read it |
|---|---|
| [user guide](README.md) | choose a task or reference |
| [getting started](getting-started.md) | install fetchloom, add a first dataset or restore a shared project |
| [concepts](concepts.md) | understand pins, placement, archives and offline work |
| [commands](commands/README.md) | choose a command and check global options |
| [add](commands/add.md) | put a dataset in a project and download it |
| [sync](commands/sync.md) | restore pinned inputs and handle local conflicts |
| [lock](commands/lock.md) | check or update exact input versions |
| [list](commands/list.md) | see dataset references, locked sizes and folders |
| [search](commands/search.md) | find a dataset by words and choose a canonical reference |
| [status](commands/status.md) | find missing, edited and extra files |
| [path](commands/path.md) | locate a dataset, file or subfolder |
| [remove](commands/remove.md) | remove an entry without discarding edited files |
| [get](commands/get.md) | fetch data once without project files |
| [cache](commands/cache.md) | inspect, collect or verify cached bytes |
| [sources](sources/README.md) | compare accepted references, pins, credentials and limits |
| [URLs and DOIs](sources/urls-and-dois.md) | fetch a direct link or resolve a persistent handle |
| [zenodo](sources/zenodo.md) | choose records or individual record files |
| [figshare](sources/figshare.md) | pin article versions, including institutional articles |
| [dataverse](sources/dataverse.md) | use public dataset or file versions |
| [OSF](sources/osf.md) | fetch osfstorage files and understand node changes |
| [hugging face](sources/hugging-face.md) | pin repository commits and configure approved access |
| [kaggle](sources/kaggle.md) | use dataset versions and existing kaggle credentials |
| [google drive](sources/google-drive.md) | use shared files or folders and exclude google docs |
| [dropbox](sources/dropbox.md) | download shared files or folder zips |
| [data.toml](reference/data-toml.md) | edit datasets, paths and selection patterns |
| [data.lock](reference/data-lock.md) | inspect pins, spec hashes and archive tree hashes |
| [JSON output](reference/json-output.md) | parse result fields, warnings and work counters |
| [exit codes](reference/exit-codes.md) | branch on failure kinds and choose a recovery step |
| [environment](reference/environment.md) | set credentials, cache location, limits or terminal behavior |
| [cache folder](reference/cache-folder.md) | inspect shared cache objects and indexes |
| [project folder](reference/project-folder.md) | inspect local owner records and placement state |
| [scripting](scripting.md) | use JSON recipes or the python helper |
| [windows](windows.md) | choose drives, avoid synced-folder surprises and handle paths |
| [troubleshooting](troubleshooting.md) | match a symptom to its cause and fix |
| [limits](limits.md) | check what fetchloom does not do |

for code changes, start with the [developer guide](../dev/README.md).
