# other tools

fetchloom takes over one job from the tools people fetch dataset files with: getting the files, checking them and pinning them so the next person gets the same bytes. loading, versioning your own files, running pipelines and uploading stay with the tools you use for them, and leaving fetchloom keeps your files where they are.

## replacing a download step

each row is a command people run to fetch dataset files, and what does that job in a project. `add` pins every file in data.lock by its SHA-256 and places it in the data folder, so a teammate's `fetchloom sync` gets the same bytes. `get` fetches once, without a project.

| you run | with fetchloom | notes |
|---|---|---|
| `curl -LO <link>`, then `sha256sum -c` | `fetchloom add <link>`, or once: `fetchloom get <link> -o <file> --sha256 <hash>` | `get` checks the hash before the file takes its name |
| `wget -i links.txt`, `aria2c -i links.txt` | `fetchloom add -i links.txt` | a dataset for each link, as [below](#a-list-of-files-and-their-hashes) |
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
| `kagglehub.dataset_download("uciml/breast-cancer-wisconsin-data")` | once `fetchloom add kaggle:uciml/breast-cancer-wisconsin-data --as cancer`, then `fetchloom.path("cancer")` | the files are in `data/cancer` |

### a list of files and their hashes

a readme often gives a base link and a `SHA256SUMS` file, with the hash before each name. `add -i` reads a link, then its SHA-256, so one `awk` line turns one into the other; it drops the `*` that `sha256sum -b` writes before a name. from a real run at a terminal against files on github, in a new folder `~/paper`, with stderr shown:

```sh
$ cat SHA256SUMS
9cc1c345c71bcc9b486b74cbf6063fa66f4bb5e0f603a4b3c3471ec2e5e8e355  iris.csv
e54cc4d2ce1bff65d32ca60b3e4b802e06bde1d7e7caf6f796f6bf7370e863b0  tips.csv
$ awk -v base=https://raw.githubusercontent.com/mwaskom/seaborn-data/master/ 'NF {sub(/^\*/, "", $2); print base $2, $1}' SHA256SUMS > links.txt
$ fetchloom add -i links.txt
 Downloaded 2 files · 13.3 KiB in 108 ms
    Started a project in ~/paper
      Added iris, its files are in data/iris
      Added tips, its files are in data/tips
  hint: scripts can find each with `fetchloom path <name>`
  next: commit data.toml and data.lock; anyone who clones runs `fetchloom sync`
```

a pooch registry gives the name first, then the hash, which may be `md5:`, and may give a file its own link third. a list for `add -i` takes only a SHA-256, and any other hash stops the add, naming the line:

```text
error: "md5:0123456789abcdef0123456789abcdef" on line 2 of links.txt is not a SHA-256
  this looks like an MD5, which has 32 hex digits. give the file's SHA-256, which has 64
  help: a line is a link, or a link and its SHA-256: 64 hex digits, or sha256: and 64 hex digits. blank lines and lines starting with # are skipped
```

so leave such hashes out of the list, take a file's own link where the registry gives one, and check the md5 hashes once the files are in place, so a file that changed since the registry was written is not pinned unseen. `add -i` names each dataset and file after its link, not after the registry, so the check looks for each registry md5 among the files `fetchloom export` lists, wherever they were placed:

```sh
awk -v base=https://raw.githubusercontent.com/mwaskom/seaborn-data/master/ '!/^#/ && NF {print ($3 != "" ? $3 : base $1), ($2 ~ /^md5:/ ? "" : $2)}' registry.txt > links.txt
fetchloom add -i links.txt
fetchloom export | cut -c67- | tr '\n' '\0' | xargs -0 md5sum > placed.md5
awk 'NR == FNR {at[$1] = substr($0, 35); next} !/^#/ && $2 ~ /^md5:/ {md5 = substr($2, 5); if (md5 in at) print $1 ": OK, " at[md5]; else {print $1 ": no file add placed has this md5"; bad = 1}} END {exit bad}' placed.md5 registry.txt
```

stdout of the last line from a real run against a server on this machine, for a registry naming `sub/measurements.csv`, `Measurements.csv`, `field.csv` with its own link to `elsewhere.csv`, and `tips.csv`, which the server changed after the registry was written:

```text
sub/measurements.csv: OK, data/measurements/measurements.csv
Measurements.csv: OK, data/measurements-2/Measurements.csv
field.csv: OK, data/elsewhere/elsewhere.csv
tips.csv: no file add placed has this md5
```

a name with no file is one whose link served other bytes than the registry lists, and the line then exits 1. leave data.lock uncommitted: `fetchloom list` shows the dataset each link made, `fetchloom remove <name>` drops one, and whoever keeps the registry knows which bytes are right. md5sum names on stderr a placed file it cannot read; `fetchloom sync` puts it back. an archive is unpacked as it is added, so no placed file holds its own md5 and it shows as having none too; check one against its link, as `curl -sL <link> | md5sum`.

### files in one folder

each link is its own dataset in its own folder. code that reads every file from one folder gets them there through `path` in data.toml, which datasets may share while no two files clash:

```toml
[datasets.iris]
ref = "https://raw.githubusercontent.com/mwaskom/seaborn-data/master/iris.csv"
path = "raw"

[datasets.tips]
ref = "https://raw.githubusercontent.com/mwaskom/seaborn-data/master/tips.csv"
path = "raw"
```

`fetchloom sync` then moves them, stderr from the same run:

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
| make, snakemake, nextflow, airflow, dvc pipelines | running steps | run `fetchloom sync --locked` before the first step, which stops rather than changes a pin, and find inputs with `fetchloom path`. a short step can be a [recipe](recipes.md): `fetchloom build` runs it, pins what it made and places it, and in a job `fetchloom build --locked` makes what no cache or mirror holds, as [CI](ci.md#recipes) shows |
| git, git lfs, datalad, dvc remotes | versions of files you change | commit data.toml and data.lock with your code, and data.lock.d once a recipe or a big dataset keeps its file list there; `add` keeps `data/` out of git. [dvc, git-lfs and datalad](#dvc-git-lfs-and-datalad) says how each works beside fetchloom |
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

fetchloom keeps no history of files you change, beyond what git keeps of `data.toml` and `data.lock`, and writes to no remote but a folder [mirror](mirrors.md). it records how a dataset was made only for a [recipe](recipes.md): what it read, its command, code and variables, not a whole pipeline or an experiment's runs.

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
