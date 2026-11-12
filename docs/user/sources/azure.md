# azure blob storage

fetchloom lists and downloads azure blob containers over HTTPS, without an azure SDK. a public container needs no account. a private container you list in the `azure_containers` setting is read with a SAS, a shared access signature, that you keep in `AZURE_STORAGE_SAS_TOKEN` or in the settings file, or with a key of its storage account in `AZURE_STORAGE_KEY`, never in `data.toml` or `data.lock`.

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
```

## listing

fetchloom asks azure's `List Blobs` for the prefix, a page of up to 5,000 blobs at a time, and follows each page's marker, up to 1,000 pages and the `most_files` limit. every request to a blob host asks for version 2020-10-02 of azure's API. without a version, azure answers an anonymous request in the account's default, which can be as old as 2009-09-19, and a container holding newer kinds of blob then refuses to list.

an empty blob named like a folder that other blobs are in is a mark some tools leave for the folder, not a file, so it is left out: `processedManifest/` lists 4 files, not the 5 blobs under it.

## pins

`data.lock` keeps each blob's link, `https://<account>.blob.core.windows.net/<container>/<name>`, its size, and the MD5 azure states for it when it keeps one, which the downloaded bytes are checked against before they are pinned. azure keeps an MD5 for a blob uploaded in one piece, and for a blob uploaded in blocks only when the uploader set one, so some blobs have none; the lock's SHA-256 is then their only check. a blob uploaded again under its name gets new bytes at the same link, and the lock's SHA-256 refuses them, as for any [bucket](buckets.md#pins).

## private containers

the `azure_containers` setting lists the containers fetchloom may send credentials to, in the [settings file](../reference/environment.md#settings-file) or `FETCHLOOM_AZURE_CONTAINERS`, as `azure:<account>/<container>` or the container's link:

```toml
azure_containers = [
  "azure:lab/raw",
  "https://lab.blob.core.windows.net/scans?sv=2022-11-02&sr=c&sp=rl&se=2026-12-31T00%3A00%3A00Z&sig=...",
]
```

a request for a listed container goes without credentials first, as for any container. once azure refuses it, the request goes again with them, and the container's later requests carry them from the start. a container not listed never gets any, whatever `data.toml` names. credentials go only to the container's own blob host over HTTPS, never on to where a redirect sends a request.

each listed container takes the first of these it has:

| credential | where it comes from | which containers it goes to |
|---|---|---|
| its own SAS | its entry, as in the second line above, the SAS link the azure portal gives for a container | that one |
| a SAS | `AZURE_STORAGE_SAS_TOKEN`, as the az cli reads it | those in the account `AZURE_STORAGE_ACCOUNT` names |
| an account key | `AZURE_STORAGE_KEY`, with the account it is for in `AZURE_STORAGE_ACCOUNT` | those in that account |

a SAS is one account's, so when `AZURE_STORAGE_ACCOUNT` names none, the listed containers without a SAS of their own must all be in one account. listed containers in two accounts, with `AZURE_STORAGE_ACCOUNT` unset, fail the first request that needs the SAS with `auth.required`, exit 40, asking for `AZURE_STORAGE_ACCOUNT`, before the SAS goes to either. a SAS needs read and list permission, `sp=rl`, for a container's listing.

a key signs each request as azure's shared key defines it: the request's time in `x-ms-date`, and an `Authorization: SharedKey <account>:<signature>` header worked out from the key. the key itself never leaves this machine. a key reads every container of its account, and an account whose owner turned off access with keys refuses it.

```console
$ fetchloom add https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/01/C/CV/ --dry-run
  Resolving azure:sentinel2l2a01/sentinel2-l2/01/C/CV/
error: azure:sentinel2l2a01/sentinel2-l2/01/C/CV/ is private, or does not exist: azure refused to list it without credentials
  sentinel2l2a01.blob.core.windows.net said NoAuthenticationInformation: Server failed to authenticate the request. Please refer to the information in the www-authenticate header.
  help: if you trust azure:sentinel2l2a01/sentinel2-l2 with your azure credentials, list it in azure_containers in the settings file or FETCHLOOM_AZURE_CONTAINERS, and set AZURE_STORAGE_SAS_TOKEN, or AZURE_STORAGE_KEY with AZURE_STORAGE_ACCOUNT
```

azure refuses a container that does not exist as it refuses a private one, so the error says both. a SAS azure refuses, as an expired one or one for another container, fails with `auth.required`, exit 40, naming where the SAS came from and the expiry it states; a key azure refuses fails the same way, naming `AZURE_STORAGE_KEY` and its account. `add` refuses a link that carries a SAS, and `sig`, `se` and `sp` are masked wherever a link is shown; `fetchloom get` downloads a SAS link once without keeping it.

## access and limits

azure containers record no `access`. only azure's public cloud, `*.blob.core.windows.net`, is read: other clouds, data lake (`dfs`) endpoints, static website hosts and the `$root` and `$web` containers are not. a blob in the archive tier fails to download until its owner rehydrates it. credentials from the az cli or a managed identity are not read, and the planetary computer's own SAS service is not asked: give such a container a SAS from `planetarycomputer.microsoft.com/api/sas/v1/token/<account>/<container>` yourself.
