# signing in

most data is public and needs no account. a key is needed for the rest: a private, restricted or gated dataset, a kaggle competition, any download from dryad, and a private bucket, container or drive folder. a key is kept on your machine, never in data.toml or data.lock, so everyone who clones a project signs in on their own.

## sources with an account

```sh
fetchloom auth login hf
fetchloom auth
```

`auth login` shows the steps to make a key, takes it without showing it, checks it with the source and saves it. `fetchloom auth` then lists each account with where its key comes from and, online, whose it is. [auth](commands/auth.md) has the details.

| account | a key is needed for | or set |
|---|---|---|
| `hf` | gated and private hugging face repositories | `HF_TOKEN` |
| `kaggle` | competitions | `KAGGLE_API_TOKEN` |
| `zenodo` | restricted, embargoed and closed records | `ZENODO_TOKEN` |
| `figshare` | nothing public; embargoed articles stay refused | `FIGSHARE_TOKEN` |
| `osf` | private projects | `OSF_TOKEN` |
| `dryad` | every download | `DRYAD_TOKEN` |
| `github` | more than 60 requests an hour | `GH_TOKEN` or `GITHUB_TOKEN` |
| `gitlab` | private projects | `GITLAB_TOKEN` |
| `dataverse` | restricted files, on the installation `--host` names | `DATAVERSE_API_TOKEN`, for the installation `DATAVERSE_URL` names |
| `earthdata`, `physionet` or a host name | a server that asks for a username and password | a `machine` line in `.netrc` |

you may be signed in already: fetchloom reads the keys the hf and kaggle commands saved, and a `.netrc` that curl reads, and `auth login` saves to the same places, so those tools share the key. a variable wins over a saved key. [environment](reference/environment.md#credentials) gives the order fetchloom looks in.

a gated dataset or a competition also needs its terms accepted on its own page, with the same account; a key cannot accept them for you.

## private buckets and folders

a cloud store takes the keys its own tools keep, and fetchloom offers them only to the buckets, containers and folders you list in the [settings file](reference/environment.md#settings-file), so a project you clone cannot make fetchloom hand your keys to a bucket of its choosing:

```toml
s3_buckets = ["s3://lab-raw"]
gcs_buckets = ["gs://lab-private"]
drive_folders = ["https://drive.google.com/drive/folders/1AbCdEfGhIjKlMnOpQrStUvWxYz012345"]
azure_containers = ["azure:lab/raw"]
```

| store | list it in | sign in with |
|---|---|---|
| S3 and services that speak its API | `s3_buckets` | AWS keys: `aws configure`, `aws sso login`, a profile's `credential_process`, or `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` |
| google cloud storage | `gcs_buckets` | `gcloud auth application-default login`, or a service account's key in `GOOGLE_APPLICATION_CREDENTIALS` |
| google drive, shared with some people | `drive_folders` | the same google credentials, with drive's scope: a service account the folder is shared with, or your own login through an oauth client you make |
| azure blob storage | `azure_containers` | a SAS, in `AZURE_STORAGE_SAS_TOKEN` or in the container's entry |

each request goes without a key first, so a public bucket never sees one, and the key follows only once the store refused. `fetchloom auth` shows the AWS keys and google credentials it would use, without running a program or asking a server. each store's page has the rest: [private S3 buckets](sources/buckets.md#private-s3-buckets), [private cloud storage buckets](sources/buckets.md#private-cloud-storage-buckets), [private drive files and folders](sources/google-drive.md#private-files-and-folders) and [private azure containers](sources/azure.md#private-containers).

## when a download is refused

at a terminal, a command refused for want of an account offers to sign in, then tries once more. a key that went with the request and was refused, as an old `HF_TOKEN`, is named by where it comes from, never by its value. [auth](commands/auth.md#when-a-download-is-refused) shows both.

in a script or CI there is nobody to ask: set the source's variable from the CI system's secrets, as [CI](ci.md#keys) says, or pipe the key to `fetchloom auth login <account> --with-token`, as [auth](commands/auth.md#scripts-and-ci) shows.

## where a key goes

a key goes over https only, to its own host on port 443, and never after a redirect leaves that host. a dataverse key saved for an installation on another port, as `--host dv.example.edu:8443`, goes to that port only. a `.netrc` login also answers on later hops to its own host, as curl sends it, which earthdata needs: its archives send you to `urs.earthdata.nasa.gov` to sign in. an osf key also reaches `osf.io` and `files.osf.io`, where osf's downloads go. a github key goes to `api.github.com` only, never to github.com, where release files are.

no key appears in data.toml, data.lock, the cache, logs or `--json` output. recognized secrets in a link, such as `token=`, are masked wherever the link is shown and refused in a project's references. a key holding a character no request can carry, such as a line break, is left out of every request, and [`fetchloom auth`](commands/auth.md#status) warns about it.
