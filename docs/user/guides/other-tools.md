# other tools

fetchloom takes over one job from the tools people fetch dataset files with: getting the files, checking them and pinning them so the next person gets the same bytes. loading, versioning your own files, running pipelines and uploading stay with the tools you use for them, and leaving fetchloom keeps your files where they are.

## replacing a download step

each row is a command people run to fetch dataset files, and what does that job in a project. `add` pins every file in data.lock by its SHA-256 and places it in the data folder, so a teammate's `fetchloom sync` gets the same bytes. `get` fetches once, without a project.

| you run | with fetchloom | notes |
|---|---|---|
| `curl -LO <link>`, then `sha256sum -c` | `fetchloom add <link>`, or once: `fetchloom get <link> -o <file> --sha256 <hash>` | `get` checks the hash before the file takes its name |
| `wget -i links.txt`, `aria2c -i links.txt` | `fetchloom add -i links.txt --as <name>` | the listed files as one dataset, as [below](#a-list-of-files-and-their-hashes) |
| `wget` of each file in a `SHA256SUMS` or `MD5SUMS`, then `sha256sum -c` or `md5sum -c` | `fetchloom add <link to the SHA256SUMS>` | every file it lists, each checked, as [below](#a-list-of-files-and-their-hashes) |
| `gdown <id>`, `gdown --folder <link>` | `fetchloom add drive:<id>`, or the shared link | [google drive](../sources/google-drive.md) |
| `zenodo_get 3608990` | `fetchloom add zenodo:3608990` | [zenodo](../sources/zenodo.md) |
| `osf -p 3eunr clone` | `fetchloom add osf:3eunr` | [OSF](../sources/osf.md) |
| `kaggle datasets download -d uciml/breast-cancer-wisconsin-data` | `fetchloom add kaggle:uciml/breast-cancer-wisconsin-data` | pins the dataset version and reads existing kaggle credentials; [kaggle](../sources/kaggle.md) |
| `kaggle competitions download -c titanic` | `fetchloom add kaggle:competitions/titanic` | needs `fetchloom auth login kaggle` and, for most competitions, the rules accepted on kaggle |
| `hf download org/name --repo-type dataset --include "*.parquet"` | `fetchloom add hf:org/name --select "*.parquet"` | pins the commit; [hugging face](../sources/hugging-face.md) |
| `aws s3 cp --no-sign-request --recursive s3://openneuro.org/ds000001/ ds000001/` | `fetchloom add s3://openneuro.org/ds000001/` | [cloud buckets](../sources/buckets.md) |
| `gcloud storage cp -r gs://cloud-samples-data/bigquery/us-states .` | `fetchloom add gs://cloud-samples-data/bigquery/us-states/` | [cloud buckets](../sources/buckets.md) |
| `az storage blob download-batch --account-name azureopendatastorage --source mnist --destination mnist` | `fetchloom add azure:azureopendatastorage/mnist/` | [azure blob storage](../sources/azure.md) |
| `gh release download jq-1.8.2 --repo jqlang/jq --pattern jq-linux-amd64` | `fetchloom add github:jqlang/jq@jq-1.8.2 --select jq-linux-amd64` | gitlab releases too; [github and gitlab releases](../sources/releases.md) |
| `fasterq-dump SRR390728` | `fetchloom add ena:SRR390728` | the FASTQ ENA keeps for the run, checked against its MD5; [ENA](../sources/ena.md) |
| `getGEOSuppFiles("GSE1000")` in R | `fetchloom add geo:GSE1000 --select "suppl/*"` | [GEO](../sources/geo.md) |
| `dandi download DANDI:000027` | `fetchloom add dandi:000027` | [DANDI](../sources/dandi.md) |
| `openneuro-py download --dataset=ds000001` | `fetchloom add openneuro:ds000001` | pins the newest snapshot; [OpenNeuro](../sources/openneuro.md) |
| `dvc import-url <link> data/<file>` | `fetchloom add <link>` | as [below](#dvc-import-url) |
| `pooch.retrieve(link, known_hash="sha256:<hash>")` | `fetchloom.get(link, "<file>", sha256="<hash>")` | the [python helper](../reference/python-helper.md) |
| a pooch registry with its `base_url` | `fetchloom add list:<base_url>/registry.txt --as <name>` | the registry read at its link, as [below](#a-list-of-files-and-their-hashes) |
| `kagglehub.dataset_download("uciml/breast-cancer-wisconsin-data")` | once `fetchloom add kaggle:uciml/breast-cancer-wisconsin-data --as cancer`, then `fetchloom.path("cancer")` | the files are in `data/cancer` |

### a list of files and their hashes

a readme often gives a `SHA256SUMS` or `MD5SUMS` file beside the data, with the hash before each name. `fetchloom add` of the list's link pins every file it lists, each at the list's folder plus its path, as one dataset: an MD5 is checked as the file arrives and data.lock pins its SHA-256. a pooch registry, `path hash` or `path hash link` a line, with `md5:` or `sha256:` before a hash, is added by `list:` and its link, and a BSD list, `SHA256 (path) = hash`, by its link or `list:`, and each pins the same files as the GNU form. a list of links on this machine, such as earthaccess or ESGF tools write, is `fetchloom add -i <list> --as <name>`, kept in the project. a SHA-1 or any hash other than MD5 and SHA-256 is refused by its name. [a list of files](../commands/add.md#a-list-of-files) has every form and rule.

### files in one folder

the files of one list are one dataset in one folder. datasets of several links each get a folder of their own, and code that reads every file from one folder gets them there through `path` in data.toml, which datasets may share while no two files clash:

```toml
[datasets.iris]
ref = "https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv"
path = "raw"

[datasets.tips]
ref = "https://raw.githubusercontent.com/mwaskom/seaborn-data/master/tips.csv"
path = "raw"
```

`fetchloom sync` then moves them; stderr from a real run:

```text
     Placed data/raw · 2 files linked to the cache
    Removed 2 files from data/ that no dataset needs
     Synced 2 datasets into data
```

[data.toml](../reference/data-toml.md) has the folder rules.

### dvc import-url

for each file `dvc import-url` brought in, run `fetchloom add` with the link its `.dvc` file names under `deps`, then `dvc remove <file>.dvc` so dvc stops tracking it. dvc keeps everything else it tracks.

## keeping the tools you have

| tool | keep it for | with fetchloom |
|---|---|---|
| hugging face datasets, torchvision, tfds, webdataset, litdata | loading and streaming | give them the folder `fetchloom path <name>` prints and turn their own download off; webdataset reads [packed shards](../reference/archives.md#webdataset-shards) |
| make, snakemake, nextflow, airflow, dvc pipelines | running steps | run `fetchloom sync --locked` before the first step, which stops rather than changes a pin, and find inputs with `fetchloom path` |
| git, git lfs, datalad, dvc remotes | versions of files you change | commit data.toml and data.lock with your code, and data.lock.d once a big dataset keeps its file list there; `add` keeps `data/` out of git. [dvc, git-lfs and datalad](#dvc-git-lfs-and-datalad) says how each works beside fetchloom |
| rclone, s5cmd, aws cli | copying files you own to and from storage | fetchloom reads buckets and never writes to one |
| mlflow, weights and biases | runs and results | log data.lock with a run to record the bytes it read |
| uv, pixi, conda, docker | environments | data.lock sits beside uv.lock or the environment file; a container keeps the cache on a volume, as [notebooks and containers](notebooks-and-containers.md) shows |

fetchloom hosts no data and keeps no accounts. [limits](../reference/limits.md) lists what else it leaves to other tools.

## dvc, git-lfs and datalad

these tools version files: they keep each version of what you make or change, and the history to go back to. fetchloom pins published files and places them, so each works on its own files beside the other.

| tool | what it does well | with fetchloom |
|---|---|---|
| dvc | versions data and models you make, in its cache and a remote such as a bucket, and reruns a pipeline's stages when what they read changes | fetchloom fetches and pins the published inputs and dvc tracks what your stages make from them: run `fetchloom sync --locked` before `dvc repro`. a file `dvc import-url` brought in moves as [above](#dvc-import-url) |
| git-lfs | keeps large files in a git repository as small pointer files, their bytes on an LFS server, usually the git host | keep it for large files you change with your code. a published file stored there can leave: `fetchloom add` its link and `git rm` the file, which older commits still hold. a hugging face dataset is a repository like this, and fetchloom reads it at a pinned commit without git or git-lfs. a raw link to such a file on github or gitlab serves the pointer, which fetchloom refuses with the link that serves the file instead |
| datalad | versions whole datasets with git and git-annex, nests them, and records the command that made a file with `datalad run` | keep it for datasets you curate. `git annex whereis <file>` lists where a file's bytes are, links among them, and `fetchloom add` takes such a link. an OpenNeuro dataset is in its S3 bucket too, as in the table [above](#replacing-a-download-step) |

fetchloom keeps no history of files you change, beyond what git keeps of `data.toml` and `data.lock`, and writes to no remote but a folder [mirror](mirrors.md). it records where a dataset came from, not how a result was made from it, a whole pipeline or an experiment's runs.

## leaving fetchloom

the files in the data folder are ordinary files: clones, hardlinks to the cache's copy, or copies. reading them needs no fetchloom, and deleting the cache folder leaves them in place, since a hardlink keeps its bytes. placed files are read-only, and a hardlinked one is the same file in every project linked to it, so changing its permissions changes it everywhere. for files of your own, set `copy = true` on each dataset and run `fetchloom sync` before you go: it replaces each unchanged read-only file with a writable clone or copy.

to fetch the same files again without fetchloom, export the pins, then download and check them on any machine with aria2c, in a folder holding both files. `-d data` names the data folder, since `data.sha256` lists paths from the project's folder; give the folder `data_dir` names when it is not `data`:

```sh
fetchloom export -o data.sha256
fetchloom export --format aria2 -o sources.aria2
aria2c -q -i sources.aria2 -d data --check-integrity=true --auto-file-renaming=false
sha256sum -c data.sha256
```

an archive arrives packed; unpack it where `data.sha256` lists its files. [export](../commands/export.md#what-it-writes) shows a real run, what the aria2c options do and what each form leaves out. data.toml and data.lock are TOML files that [data.lock](../reference/data-lock.md) documents, so any TOML reader can use the pins. then [uninstall](../start/install.md#uninstall) the program.

aria2c writes a file already in its folder again under a new name, as `tips.1.csv` beside `tips.csv`, unless it runs into an empty folder or with `--check-integrity=true --auto-file-renaming=false`: it then keeps a file whose SHA-256 matches, downloads one changed at the same size again, and stops with exit 13 at a file of another size, which you delete before running it again. it cannot write the read-only files sync placed, so point it at a folder sync did not fill. a second run with both options, over the files the first wrote, then the check, from real runs of aria2c 1.37 for a project whose `iris` and `tips` have `path = "raw"`:

```sh
$ aria2c -q -i sources.aria2 -d data --check-integrity=true --auto-file-renaming=false
$ sha256sum -c data.sha256
data/raw/iris.csv: OK
data/raw/tips.csv: OK
```
