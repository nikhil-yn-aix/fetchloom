# notebooks and containers

hosted notebooks and containers often start each session on a fresh disk, so fetchloom's cache needs a folder that lasts, or every session downloads the data again.

a sync writes in two places: the project folder, which holds `data.toml`, `data.lock`, `data/` and `.fetchloom/`, and the cache folder, which `FETCHLOOM_CACHE` chooses; [cache folder](reference/cache-folder.md#location) gives the defaults. keep `data.toml` and `data.lock` with your code, in git. with them, `fetchloom sync --locked` puts the same files back on any fresh machine, so a lost cache costs a download, never a different input. keep the project and the cache on one disk where you can: a cache on another disk means a copy of every file, as [clusters](hpc.md#keep-the-project-and-the-cache-on-one-drive) shows.

## hosted notebooks

| platform | what lasts between sessions | where to keep the cache |
|---|---|---|
| google colab | google drive, once mounted | the default; each new runtime downloads again |
| kaggle notebooks | up to 20 GB in `/kaggle/working`, saved with a version | `/kaggle/temp`, beside the project |
| sagemaker notebook instances | `/home/ec2-user/SageMaker` only | `/home/ec2-user/SageMaker/fetchloom-cache` |
| sagemaker studio jupyterlab | `/home/sagemaker-user` | the default |
| databricks | volumes and workspace files | the driver's disk; each new cluster downloads again |
| jupyterhub on kubernetes | `/home/jovyan` | the default |
| github codespaces | `/workspaces` | inside the workspace folder, [below](#dev-containers-and-codespaces) |
| dev containers | the workspace folder | inside the workspace folder, [below](#dev-containers-and-codespaces) |

these facts come from each platform's own documentation; fetchloom has not been tested on these platforms. a program installed outside the folder that lasts goes with the session too, so install fetchloom into that folder or at the start of each session. until the first release, installing means [building it from source](getting-started.md#install).

[google colab](https://research.google.com/colaboratory/faq.html) deletes a runtime's virtual machine when it sits idle and when it reaches a maximum lifetime, with everything on its disk, `/content` and the home folder included. clone the project into `/content` and sync it there in each new runtime. a cache on google drive, mounted at `/content/drive`, would keep the bytes between runtimes, but every file would then be copied from drive, which is another disk, and colab's FAQ warns that drive times out or fails on folders holding many items.

fetchloom treats that mount, which linux lists with the type `fuse.drive` that [colab's drive helper](https://github.com/googlecolab/colabtools/blob/main/google/colab/drive.py) looks for, as a synced folder like onedrive: a project or `get` folder inside `/content/drive` gets copies, never links, and a sync there says why on stderr:

```text
warning: data is inside google drive, which breaks links, so files were copied
  help: a folder outside google drive lets fetchloom try links
```

a new kaggle notebook's first cell says what lasts: `/kaggle/input` is read-only, up to 20 GB written to `/kaggle/working` is kept as output when you save a version, and `/kaggle/temp` lasts one session. keep the project and the cache in `/kaggle/temp`, unless the data should be saved with the version, where it counts against the 20 GB. turn the notebook's internet setting on to download.

[sagemaker notebook instances](https://docs.aws.amazon.com/sagemaker/latest/dg/howitworks-create-ws.html) keep only `/home/ec2-user/SageMaker`, a volume of 5 GB unless you chose a larger one, so the default cache in the home folder is lost when the instance stops. keep projects in that folder and set `FETCHLOOM_CACHE` to a folder beside them. in [sagemaker studio's jupyterlab](https://docs.aws.amazon.com/sagemaker/latest/dg/studio-updated-jl-user-guide.html), the space's storage volume outlives the instance it runs on, and the [home folder](https://docs.aws.amazon.com/sagemaker/latest/dg/studio-updated-jl-admin-guide.html), `/home/sagemaker-user`, is on that volume, so the default cache already lasts.

[databricks](https://docs.databricks.com/aws/en/files/download-internet-files) keeps volumes and workspace files, but says volumes do not support random writes, and fetchloom writes the parts of a split download out of order. keep the project and the cache on the driver's own disk, which lasts as long as the cluster, and sync again after each start.

[jupyterhub on kubernetes](https://z2jh.jupyter.org/en/stable/jupyterhub/customizing/user-storage.html) keeps each user's `/home/jovyan`, 10 GiB by default, and resets everything else when the server restarts. the default cache is in that home folder. a volume every user mounts can hold [one cache for a whole lab](hpc.md#one-cache-for-a-whole-lab).

### commands in a notebook cell

each `!` line runs in a shell of its own, so a `cd` or an `export` there does not reach the next line. `%cd` and `%env` change the folder and the variables for every later line. a first cell for a kaggle notebook whose project is in git:

```text
%cd /kaggle/temp
!git clone https://example.org/lab/project.git
%cd project
%env FETCHLOOM_CACHE=/kaggle/temp/fetchloom-cache
!fetchloom sync --locked --yes
```

on colab, clone into `/content` and leave `FETCHLOOM_CACHE` unset. the [python helper](scripting.md#python-helper) passes its environment to fetchloom, so set `os.environ["FETCHLOOM_CACHE"]` before calling it.

## dev containers and codespaces

[github codespaces](https://docs.github.com/en/codespaces/developing-in-a-codespace/rebuilding-the-container-in-a-codespace) keeps `/workspaces` through stops and rebuilds; the rest of the container survives a stop, not a rebuild. a [dev container](https://code.visualstudio.com/docs/devcontainers/containers) on your own machine keeps the workspace folder, which is mounted from your disk or a volume, and loses everything else when it is rebuilt. in both, keep the cache inside the workspace folder. `containerEnv` in `.devcontainer/devcontainer.json` sets it for every process in the container, as the [dev container reference](https://containers.dev/implementors/json_reference/) describes:

```json
{
  "image": "mcr.microsoft.com/devcontainers/base:ubuntu",
  "containerEnv": {
    "FETCHLOOM_CACHE": "${containerWorkspaceFolder}/.fetchloom-cache"
  }
}
```

then add `/.fetchloom-cache/` to the repository's `.gitignore`: fetchloom adds `data/` there itself, not the cache. the cache and `data/` share one mount, so fetchloom can link files instead of copying them.

## containers and kubernetes

a container's own files go when the container does, and a read-only root filesystem refuses writes everywhere but its mounted volumes. a sync writes only in the project folder and the cache, so one writable volume holding both is enough. with the default cache on a read-only root, sync stops at once (stderr):

```text
error: could not open the cache at /root/.cache/fetchloom: read-only file system
  help: set FETCHLOOM_CACHE to a folder fetchloom may write to
```

an image can carry `data.toml` and `data.lock` with the code, but sync writes `data/` and `.fetchloom/` beside them, so copy the two files into the volume first.

### a kubernetes job

this job restores the inputs in an init container, then runs the training container, which only reads them. the image holds the code, `data.toml` and `data.lock` in `/app`:

```yaml
apiVersion: v1
kind: PersistentVolumeClaim
metadata:
  name: fetchloom-work
spec:
  accessModes: [ReadWriteOnce]
  resources:
    requests:
      storage: 50Gi
---
apiVersion: batch/v1
kind: Job
metadata:
  name: train
spec:
  template:
    spec:
      restartPolicy: Never
      initContainers:
        - name: restore-data
          image: registry.example.org/lab/train:1.0
          command:
            - sh
            - -c
            - |
              set -e
              mkdir -p /work/project
              cp /app/data.toml /app/data.lock /work/project/
              cd /work/project
              fetchloom sync --locked
              fetchloom cache gc --now
          env:
            - name: FETCHLOOM_CACHE
              value: /work/fetchloom-cache
            - name: FETCHLOOM_ASK_ABOVE
              value: "40G"
          securityContext:
            readOnlyRootFilesystem: true
          volumeMounts:
            - name: work
              mountPath: /work
      containers:
        - name: train
          image: registry.example.org/lab/train:1.0
          command: [python, /app/train.py, --data, /work/project/data/iris]
          securityContext:
            readOnlyRootFilesystem: true
          volumeMounts:
            - name: work
              mountPath: /work
              readOnly: true
      volumes:
        - name: work
          persistentVolumeClaim:
            claimName: fetchloom-work
```

- only the init container needs the network. `set -e` makes it fail when any command fails, so the job stops instead of training on missing data.
- the project and the cache share the volume, so files are linked, not copied. a later job on the same volume finds them in place, and sync says `Ok all pinned files are in place by size and time, nothing to do`.
- `--locked` fails with `lock.outdated`, exit 1, when the image's `data.toml` and `data.lock` disagree, instead of changing the lock.
- `FETCHLOOM_ASK_ABOVE` approves downloads up to 40 GiB. `--yes` would approve any size, and would also skip the check that the volume has room, as [small disks](#small-disks) shows.
- `cache gc --now` drops files the image's lock no longer needs, so old versions do not fill the volume.
- the training container mounts the volume read-only. the files are in `/work/project/data/<name>`. with the same `FETCHLOOM_CACHE`, `fetchloom -C /work/project --offline path iris` prints that folder there too; without it, fetchloom looks for the default cache on the read-only root and stops.
- a volume that pods on several nodes write at once needs storage that allows it, `ReadWriteMany`, and the [locks on a shared disk](hpc.md#the-cache-on-a-shared-disk) apply.

### small disks

before it downloads, fetchloom checks the free space of the disk holding the cache, counting each archive whose files are not known yet once more for unpacking ([approval](concepts.md#approval)). with the job above on a 2 MiB volume, a 4 MiB sync stops before any download (stderr):

```text
error: this sync needs at least 4.0 MiB on the disk holding the cache at /work/fetchloom-cache, which has 2.0 MiB free
  help: free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one, then run `fetchloom sync --locked` again. add --yes to go on anyway, as on a disk that compresses what it stores
```

`--yes` answers that question too. with it, the same sync downloads until the disk is full, then stops with `local.io`, exit 50, and its unfinished bytes stay under the cache's `partial/` folder until `fetchloom cache gc --now` frees them. a job that may meet a small disk can set `FETCHLOOM_ASK_ABOVE` to the largest download it expects instead of passing `--yes`.

the check sees only what the disk reports. kubernetes enforces an `emptyDir` `sizeLimit` or an `ephemeral-storage` limit by [evicting the pod](https://kubernetes.io/docs/concepts/storage/ephemeral-storage/), and a quota or a platform's own limit, such as kaggle's 20 GB of output, may not show either. a project on another disk than its cache needs room there for a second copy, which the check does not count.
