# Credentials

fetchloom reads each source's key where the source's own tools keep it, so a key made for them works here too. [signing in](../guides/sign-in.md) says how to sign in; the table gives the order fetchloom looks in. the first match wins; empty values and blank files are skipped.

| account | where fetchloom looks, in order |
|---|---|
| hugging face | `HF_TOKEN`, then one token file: the file `HF_TOKEN_PATH` names, else `token` in `HF_HOME`, else `huggingface/token` in `XDG_CACHE_HOME`, else `~/.cache/huggingface/token` |
| kaggle | `KAGGLE_API_TOKEN`, a key or a file holding one; `access_token` or `access_token.txt` in `~/.kaggle`, as kagglesdk reads it; then a username and key: `KAGGLE_USERNAME` and `KAGGLE_KEY`, each over the same field of `kaggle.json` in `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME`. a `KAGGLE_API_TOKEN` that is set but unusable skips the `~/.kaggle` token files, as kagglesdk does |
| zenodo, figshare, osf, dryad | `ZENODO_TOKEN`, `FIGSHARE_TOKEN`, `OSF_TOKEN`, `DRYAD_TOKEN`; then a key saved by `fetchloom auth login` |
| github | `GH_TOKEN`, then `GITHUB_TOKEN`, as the gh command reads them; then a key saved by `fetchloom auth login github` |
| gitlab | `GITLAB_TOKEN`; then a key saved by `fetchloom auth login gitlab` |
| semantic scholar | `S2_API_KEY`, as semantic scholar's examples read it; then a key saved by `fetchloom auth login s2` |
| dataverse | `DATAVERSE_API_TOKEN` for the installation `DATAVERSE_URL` names, as pyDataverse reads them; then a key saved for that host |
| a host's login | the `machine` entry for that host in `.netrc`, or the file `NETRC` names; on windows `_netrc` when `.netrc` does not exist. `default` entries are ignored |

| variable | effect |
|---|---|
| `NETRC` | the `.netrc` file to read and to save logins in |

[where a key goes](../concepts/privacy.md#where-a-key-goes) lists the hosts each key is sent to.

## AWS keys

AWS keys sign requests only for the buckets `s3_buckets` lists, in the order [private S3 buckets](../guides/private-cloud-storage.md#private-s3-buckets) gives. they are read where the AWS tools keep them:

| variable | effect |
|---|---|
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | a key and its secret, which come before any profile; one without the other fails with `auth.required` |
| `AWS_SESSION_TOKEN` | the session of temporary keys, sent with them |
| `AWS_PROFILE` | the profile to read, `default` when unset; one neither file has fails with `usage` |
| `AWS_SHARED_CREDENTIALS_FILE`, `AWS_CONFIG_FILE` | the credentials and config files, `~/.aws/credentials` and `~/.aws/config` when unset |
| `AWS_REGION`, `AWS_DEFAULT_REGION` | the region to sign for when neither the host nor S3 names one, before the profile's `region` |
| `AWS_ENDPOINT_URL_S3`, `AWS_ENDPOINT_URL` | the S3-compatible service the keys above sign for, instead of AWS; a profile's `endpoint_url` does the same |
| `AWS_IGNORE_CONFIGURED_ENDPOINT_URLS` | `true` ignores the endpoint settings |
| `PATH` | the folders that hold the AWS CLI and a `credential_process` named without its folder; only folders named by their full path are searched |

a profile with `credential_process` gets its keys from that program, and one that signs in to IAM identity center or takes on a role from the AWS CLI, version 2.9 or later, as [keys from a program](../guides/private-cloud-storage.md#keys-from-a-program) describes.

## google application default credentials

google's application default credentials get a token only for the buckets `gcs_buckets` lists and the drive folders and files `drive_folders` lists, as [private cloud storage buckets](../guides/private-cloud-storage.md#private-cloud-storage-buckets) and [private drive files and folders](../guides/private-cloud-storage.md#private-google-drive-files) explain. they are read where google's own libraries read them, the first that is set:

| variable | effect |
|---|---|
| `GOOGLE_APPLICATION_CREDENTIALS` | a service account's JSON key file, which comes before a login; one that cannot be read fails with `usage` rather than leave fetchloom to use another identity |
| `CLOUDSDK_CONFIG` | the gcloud folder whose `application_default_credentials.json`, the login `gcloud auth application-default login` saves, is read, `~/.config/gcloud` when unset, or `%APPDATA%\gcloud` on windows |

fetchloom asks only `https://oauth2.googleapis.com/token` for tokens. it refuses with `auth.required` a file that names another token endpoint, and credentials for workload identity federation or service account impersonation, rather than use another identity, and it never asks a metadata server for the identity of the machine it runs on, so on a google cloud virtual machine use a key file or a login. a service account asks for the scope it needs, `devstorage.read_only` for cloud storage and `drive.readonly` for drive; a login's token carries the scopes it was granted, and `gcloud auth application-default login` grants no drive scope unless it signs in through an oauth client of your own, as [with your own login](../guides/private-cloud-storage.md#with-your-own-login) shows.

## azure credentials

a SAS, an account key or a sign-in's token goes only to the containers `azure_containers` lists, as [private containers](../guides/private-cloud-storage.md#private-containers) explains. a container's own entry in the settings file may carry its SAS link; otherwise each is read where the az cli reads it:

| variable | effect |
|---|---|
| `AZURE_STORAGE_SAS_TOKEN` | a SAS, the query the azure portal or `az storage container generate-sas` gives, with or without its leading `?`; one without `sv` and `sig` fails with `auth.required` once a listed container needs it, without repeating it |
| `AZURE_STORAGE_ACCOUNT` | the storage account that SAS or key is for, so it goes only to listed containers in that account. unset, the listed containers without a SAS of their own must all be in one account: in two or more, a request that needs the SAS fails with `auth.required` and names this variable, before the SAS goes anywhere |
| `AZURE_STORAGE_KEY` | one of that account's access keys, in base64 as the azure portal or `az storage account keys list` gives it, which signs the requests of its listed containers and is never sent. it needs `AZURE_STORAGE_ACCOUNT`; set without it, or holding no key fetchloom can read, it fails with `auth.required` once a listed container needs it, without repeating it. a SAS for the same account comes first |
| `AZURE_TOKEN_CREDENTIALS` | how to sign in for a listed container no SAS or key covers, as azure's own libraries read it, in any case: unset, `AzureCliCredential` or `dev` take the az cli's login, through `az account get-access-token`; `ManagedIdentityCredential` or `prod` take this machine's managed identity, from its instance metadata service, which nothing else lets fetchloom ask; `prod` beside `AZURE_CLIENT_SECRET`, `AZURE_CLIENT_CERTIFICATE_PATH` or `AZURE_FEDERATED_TOKEN_FILE`, or any other value, fails with `auth.required` once a container needs a token |
| `AZURE_CLIENT_ID` | the client id of the user-assigned managed identity to sign in as; unset takes the machine's own |
| `AZURE_CONFIG_DIR` | the az cli's folder, `~/.azure` when unset, where [`fetchloom auth`](../commands/auth.md#status) reads whom `az login` signed in, without running the az cli |
| `PATH` | the folders that hold the az cli, `az`, or `az.cmd` on windows; only folders named by their full path are searched, and unset `AZURE_TOKEN_CREDENTIALS` with no az cli there means no sign-in |
