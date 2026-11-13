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
| a token of the az cli's login | `az account get-access-token`, which runs once a container needs it | every listed container |
| a token of this machine's managed identity, in place of the az cli's | the virtual machine's instance metadata service, asked only when `AZURE_TOKEN_CREDENTIALS` names it | every listed container |

a SAS is one account's, so when `AZURE_STORAGE_ACCOUNT` names none, the listed containers without a SAS of their own must all be in one account. listed containers in two accounts, with `AZURE_STORAGE_ACCOUNT` unset, fail the first request that needs the SAS with `auth.required`, exit 40, asking for `AZURE_STORAGE_ACCOUNT`, before the SAS goes to either. a SAS needs read and list permission, `sp=rl`, for a container's listing.

a key signs each request as azure's shared key defines it, and never leaves this machine. a key reads every container of its account, and an account whose owner turned off access with keys refuses it.

### the az cli's login

with no SAS or key for a listed container, fetchloom asks the az cli, version 2.54 or later, for a token that reads azure storage: `az account get-access-token --resource https://storage.azure.com/ --output json`. sign in first with `az login`. the az cli is found in a folder PATH names by its full path, never one named relatively such as `.`, and runs as a `credential_process` does for S3, as [keys from a program](buckets.md#keys-from-a-program) says, so nothing it prints reaches a message, a log or a file. the token reads the listed containers on which your login has a role that reads blobs, such as Storage Blob Data Reader.

an az cli without a login fails the request with `auth.required`, exit 40, and help to sign in with `az login`; a role azure refuses fails the same way and names the role to ask for. [`fetchloom auth`](../commands/auth.md#status) names the SAS, key or sign-in the environment gives, without running the az cli or asking for a token.

`AZURE_TOKEN_CREDENTIALS`, which azure's own libraries read, chooses the sign-in: unset, `AzureCliCredential` or `dev` take the az cli's login, `ManagedIdentityCredential` or `prod` take this machine's managed identity, and any other credential it names fails with `auth.required` once a container needs a token, as [environment](../reference/environment.md#azure-credentials) lists.

### a managed identity

on an azure virtual machine with a managed identity, `AZURE_TOKEN_CREDENTIALS=ManagedIdentityCredential` reads the listed containers as that identity, with no az cli:

```sh
export AZURE_TOKEN_CREDENTIALS=ManagedIdentityCredential
FETCHLOOM_AZURE_CONTAINERS=azure:lab/raw fetchloom add azure:lab/raw/scans/
```

the token comes from the machine's instance metadata service, which fetchloom asks only when your environment names the identity, never because `data.toml` or `data.lock` does, as [network](../../dev/architecture/network.md#trust-boundaries) describes. `AZURE_CLIENT_ID` picks a user-assigned identity by its client id, as azure's libraries read it; unset takes the machine's own. `prod` does the same unless the environment names a service principal's secret or certificate, or a federated token, which azure's libraries would sign in with first: then it fails with `auth.required`, and `ManagedIdentityCredential` asks for the identity alone.

a machine without that identity fails with `auth.required`, exit 40, in azure's words, and a machine where no metadata service answers, as off azure, fails at once and says so.

```console
$ fetchloom add https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/01/C/CV/ --dry-run
  Resolving azure:sentinel2l2a01/sentinel2-l2/01/C/CV/
error: azure:sentinel2l2a01/sentinel2-l2/01/C/CV/ is private, or does not exist: azure refused to list it without credentials
  sentinel2l2a01.blob.core.windows.net said NoAuthenticationInformation: Server failed to authenticate the request. Please refer to the information in the www-authenticate header.
  help: if you trust azure:sentinel2l2a01/sentinel2-l2 with your azure credentials, list it in azure_containers in the settings file or FETCHLOOM_AZURE_CONTAINERS, and sign in with `az login` or set AZURE_STORAGE_SAS_TOKEN
```

azure refuses a container that does not exist as it refuses a private one, so the error says both. a SAS azure refuses, as an expired one or one for another container, fails with `auth.required`, exit 40, naming where the SAS came from and the expiry it states; a key azure refuses fails the same way, naming `AZURE_STORAGE_KEY` and its account. `add` refuses a link that carries a SAS, and `sig`, `se` and `sp` are masked wherever a link is shown; `fetchloom get` downloads a SAS link once without keeping it.

## access and limits

a container azure lists only once a SAS, a key or a token goes with the request records `access = "login"` in [data.lock](../reference/data-lock.md), since a teammate needs one too; a public one records no `access`. only azure's public cloud, `*.blob.core.windows.net`, is read: other clouds, data lake (`dfs`) endpoints, static website hosts and the `$root` and `$web` containers are not. a blob in the archive tier fails to download until its owner rehydrates it. identities of azure app service, azure arc, service fabric and workload identity federation are not read, and the planetary computer's own SAS service is not asked: give such a container a SAS from `planetarycomputer.microsoft.com/api/sas/v1/token/<account>/<container>` yourself.
