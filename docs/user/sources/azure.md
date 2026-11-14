# azure blob storage

fetchloom lists and downloads azure blob containers over HTTPS, without an azure SDK. a public container needs no account. a private container you list in the `azure_containers` setting is read with a SAS, a shared access signature, that you keep in `AZURE_STORAGE_SAS_TOKEN` or in the settings file, with a key of its storage account in `AZURE_STORAGE_KEY`, or with the az cli's login or this machine's managed identity, never with anything in `data.toml` or `data.lock`.

```sh
fetchloom add azure:azureopendatastorage/mnist/
fetchloom get https://azureopendatastorage.blob.core.windows.net/mnist/processedManifest/
FETCHLOOM_AZURE_CONTAINERS=azure:lab/raw AZURE_STORAGE_SAS_TOKEN='sv=...&sig=...' fetchloom add azure:lab/raw/scans/
```

## references

| written form | stored as |
|---|---|
| `azure:<account>/<container>/<prefix>` | the same, with the account in lowercase |
| `https://<account>.blob.core.windows.net/<container>/` or `https://<account>.blob.core.windows.net/<container>/<prefix>/` | `azure:<account>/<container>/<prefix>/` |

a prefix ending in `/`, or none, names a folder. any other prefix names one blob, or the folder of that name when no blob has it, as `azure:azureopendatastorage/mnist/t10k-labels-idx1-ubyte.gz` names one file. a link to one blob stays an ordinary [URL](urls-and-dois.md). account names are 3 to 24 lowercase letters and digits, and container names 3 to 63 lowercase letters, digits and single hyphens; anything else is refused before a request.

```console
$ fetchloom add azure:azureopendatastorage/mnist/t10k-labels-idx1-ubyte.gz --as mnist-labels
  Resolving azure:azureopendatastorage/mnist/t10k-labels-idx1-ubyte.gz
   Resolved azure:azureopendatastorage/mnist/t10k-labels-idx1-ubyte.gz · 1 file · 4.4 KiB
Downloading mnist-labels/t10k-labels-idx1-ubyte.gz · 4.4 KiB
 Downloaded 1 file · 4.4 KiB in 184 ms
      Added mnist-labels, its files are in data/mnist-labels
  hint: scripts can find it with `fetchloom path mnist-labels`
```

## listing

a listing reads up to the `most_files` limit. one that sends a page marker again, or 100 empty pages in a row, is broken; an empty page partway through is fine. [sources](../../dev/architecture/sources.md#a-sas-for-private-azure-containers) and [network](../../dev/architecture/network.md#trust-boundaries) have how azure is listed and which version of its API is asked.

an empty blob named like a folder that other blobs are in is a mark some tools leave for the folder, not a file, so it is left out: `processedManifest/` lists 4 files, not the 5 blobs under it.

## pins

`data.lock` keeps each blob's link, `https://<account>.blob.core.windows.net/<container>/<name>`, its size, and the MD5 azure states for it when it keeps one, which the downloaded bytes are checked against before they are pinned. azure keeps an MD5 for a blob uploaded in one piece, and for a blob uploaded in blocks only when the uploader set one, so some blobs have none; the lock's SHA-256 is then their only check. a blob uploaded again under its name gets new bytes at the same link, and the lock's SHA-256 refuses them, as for any [bucket](buckets.md#pins).

| source | `resolved` | file `url` |
|---|---|---|
| azure container | `azure:<account>/<container>/<prefix>` | `https://<account>.blob.core.windows.net/<container>/<name>` |

## access and limits

a container azure lists only once a SAS, a key or a token goes with the request records `access = "login"` in [data.lock](../reference/data-lock.md), since a teammate needs one too; a public one records no `access`. only azure's public cloud, `*.blob.core.windows.net`, is read: other clouds, data lake (`dfs`) endpoints, static website hosts and the `$root` and `$web` containers are not. a blob in the archive tier fails to download until its owner rehydrates it. identities of azure app service, azure arc, service fabric and workload identity federation are not read, and the planetary computer's own SAS service is not asked: give such a container a SAS from `planetarycomputer.microsoft.com/api/sas/v1/token/<account>/<container>` yourself.
