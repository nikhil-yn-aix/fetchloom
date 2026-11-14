# Use private cloud storage

## private buckets and folders

a cloud store takes the keys its own tools keep, and fetchloom offers them only to the buckets, containers and folders you list in the [settings file](../reference/environment.md#settings-file), so a project you clone cannot make fetchloom hand your keys to a bucket of its choosing:

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
| azure blob storage | `azure_containers` | `az login`, a SAS in `AZURE_STORAGE_SAS_TOKEN` or in the container's entry, an account key in `AZURE_STORAGE_KEY` with `AZURE_STORAGE_ACCOUNT`, or a virtual machine's managed identity with `AZURE_TOKEN_CREDENTIALS=ManagedIdentityCredential` |

each request goes without a key first, so a public bucket never sees one, and the key follows only once the store refused. `fetchloom auth` shows the AWS keys, google credentials and azure credentials it would use, without running a program or asking a server. each store's page has the rest: [private S3 buckets](#private-s3-buckets), [private cloud storage buckets](#private-cloud-storage-buckets), [private drive files and folders](#private-google-drive-files) and [private azure containers](#private-containers).

## private S3 buckets

the `s3_buckets` setting lists the buckets fetchloom may sign for with your AWS keys, in the [settings file](../reference/environment.md#settings-file) or `FETCHLOOM_S3_BUCKETS`, separated by commas. an AWS bucket is `s3://<bucket>`, or the bare name; one at another service is `s3:https://<host>[:<port>]/<bucket>`. a bucket nobody listed never gets a key, so a cloned project cannot make fetchloom present your key to a bucket of its choosing.

```toml
s3_buckets = ["s3://lab-raw", "s3:https://minio.lab.example:9000/scans"]
```

each run asks a bucket without a key first, so a public bucket never sees your key. when S3 refuses a listed bucket, with 401 or 403, fetchloom asks once more with a signature and signs that bucket's later requests from the start. a link that carries its own signature, as a presigned link does, is never signed again, and a signature never follows a redirect; [network](../../dev/architecture/network.md#trust-boundaries) has the details.

keys come from where the AWS tools keep them, in this order:

1. `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`, with `AWS_SESSION_TOKEN` for temporary keys.
2. the profile `AWS_PROFILE` names, else `default`, in `~/.aws/credentials` and `~/.aws/config`, or the files `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE` name. the first of these the profile has gives its keys, as the AWS tools take them:
    1. a role or a sign-in: `role_arn`, `source_profile`, `web_identity_token_file` or any `sso_` setting, such as `sso_session`. fetchloom leaves these to the AWS CLI, version 2.9 or later, and runs `aws configure export-credentials --profile <name> --format process` for the keys.
    2. `aws_access_key_id` and `aws_secret_access_key` in `~/.aws/credentials`.
    3. `credential_process`, a program that prints the keys.
    4. `aws_access_key_id` and `aws_secret_access_key` in `~/.aws/config`.

a profile is never passed over for another identity: one that signs in or takes on a role on a machine without the AWS CLI fails with `auth.required`, exit 40, and says to install it. an `AWS_PROFILE` neither file has fails with `usage`, exit 2.

the region a request is signed for is the one its host names, as `s3.eu-west-1.amazonaws.com` does, else the one S3 named for the bucket, else `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile's `region`, else `us-east-1`.

keys reach another service only where your AWS settings send them: `AWS_ENDPOINT_URL_S3` or `AWS_ENDPOINT_URL` sends the keys above to that endpoint, and so does a profile's `endpoint_url`, set in the profile or for `s3` in the `services` section it names. a bucket there takes the keys of the one profile whose endpoint names it, so `AWS_PROFILE` can stay on your AWS profile. keys sent to an endpoint are never sent to AWS. `AWS_IGNORE_CONFIGURED_ENDPOINT_URLS=true` turns endpoint settings off. a plain `http` service is signed only when your settings name its `http` link.

the secret never leaves the machine: a request carries the key id and a signature made with the secret. messages show the key id by its last four characters, such as `…WXYZ`.

### keys from a program

a `credential_process`, or the AWS CLI for a sign-in or a role, runs only once a bucket `s3_buckets` lists has refused a request without a key, when its requests first need keys. nothing in data.toml or data.lock chooses a profile, a program or a command.

- the setting is split into words as the AWS CLI splits it, by shell quoting on linux and macos and by the windows rules on windows. no shell runs it, so `$HOME`, `~`, pipes and `;` reach the program as written. the program is a full path, or a name found in a folder on PATH. a folder PATH names relatively, such as `.`, is never searched, since it would be whatever folder fetchloom runs in, and a program named by a relative path fails with `usage`, exit 2. the AWS CLI is found the same way.
- the program prints the JSON AWS defines for `credential_process`: `Version` 1, `AccessKeyId`, `SecretAccessKey`, and `SessionToken` and `Expiration` for keys that expire. nothing it prints reaches a message, a log, `--json` output or a file, and the keys stay in memory for the run.
- keys with an `Expiration` are asked for again before they run out, and keys S3 calls expired are asked for once more; written keys have nothing to give new ones, so their refusal is the answer at once. [sources](../../dev/architecture/sources.md#signing-private-s3-buckets) has how the program runs and when it runs again.
- a program that fails, prints no keys fetchloom can read, gives keys that have expired or has given none within a minute fails the request with `auth.required`, exit 40, and every later request for that bucket fails the same way. no request goes without a key or with other keys instead. ctrl-c ends the program.

a profile whose sign-in has expired, as the AWS CLI reports with exit code 255:

```text
error: `aws configure export-credentials --profile lab` exited with code 255
  help: sign in again with `aws sso login --profile lab`, or run `aws configure export-credentials --profile lab` yourself to see what went wrong
```

fetchloom itself asks no instance or container metadata service for keys. a profile whose role names `credential_source = Ec2InstanceMetadata` gets its keys from the AWS CLI, which asks that service.

## private cloud storage buckets

the `gcs_buckets` setting lists the cloud storage buckets fetchloom may send your google credentials for, in the [settings file](../reference/environment.md#settings-file) or `FETCHLOOM_GCS_BUCKETS`, separated by commas: `gs://<bucket>`, or the bare name. a bucket nobody listed never gets them, so a cloned project cannot make fetchloom sign in to a bucket of its choosing.

```toml
gcs_buckets = ["gs://lab-private"]
```

each run asks a bucket without credentials first, so a public bucket never sees them. when cloud storage refuses a listed bucket, with 401 or 403, fetchloom gets an access token from google's token endpoint, `https://oauth2.googleapis.com/token`, asks once more with it and sends it with that bucket's later requests. the token goes only to `https://storage.googleapis.com`, with the first request and never after a redirect, and a link that carries its own signature, as a signed URL does, never gets one.

the credentials are google's [application default credentials](../reference/credentials.md#google-application-default-credentials): a service account's JSON key that `GOOGLE_APPLICATION_CREDENTIALS` names, which signs a request for a token, else the login `gcloud auth application-default login` saved. a service account asks for the read-only storage scope, `devstorage.read_only`; a login's token carries the scopes you granted when you signed in, cloud storage among them by default. a token that expires or is refused with 401 is replaced. messages name the identity, such as `the service account loader@lab-project.iam.gserviceaccount.com in /home/ada/keys/lab.json`, never what it signs with.

the same credentials read the [private google drive folders and files](#private-google-drive-files) that `drive_folders` lists, with a scope of their own.

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

with no SAS or key for a listed container, fetchloom asks the az cli, version 2.54 or later, for a token that reads azure storage: `az account get-access-token --resource https://storage.azure.com/ --output json`. sign in first with `az login`. the az cli is found in a folder PATH names by its full path, never one named relatively such as `.`, and runs as a `credential_process` does for S3, as [keys from a program](#keys-from-a-program) says, so nothing it prints reaches a message, a log or a file. the token reads the listed containers on which your login has a role that reads blobs, such as Storage Blob Data Reader.

an az cli without a login fails the request with `auth.required`, exit 40, and help to sign in with `az login`; a role azure refuses fails the same way and names the role to ask for. [`fetchloom auth`](../commands/auth.md#status) names the SAS, key or sign-in the environment gives, without running the az cli or asking for a token.

`AZURE_TOKEN_CREDENTIALS`, which azure's own libraries read, chooses the sign-in: unset, `AzureCliCredential` or `dev` take the az cli's login, `ManagedIdentityCredential` or `prod` take this machine's managed identity, and any other credential it names fails with `auth.required` once a container needs a token, as [environment](../reference/credentials.md#azure-credentials) lists.

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

## private Google Drive files

a file or folder shared with some people, not with anyone who has the link, is read through drive's api with google's [application default credentials](../reference/credentials.md#google-application-default-credentials): a service account's key, or a login of yours. a token that reads drive reads every file its account can see, so fetchloom sends it only where you said, never because data.toml or data.lock names something:

- the `drive_folders` setting, or `FETCHLOOM_DRIVE_FOLDERS`, lists the folders and files that may get the token, by id or by the link drive gives when you share them. a folder covers what is inside it.
- every request goes without a token first, as anyone with the link would ask. only once drive refuses it does the token go to drive's api, `https://www.googleapis.com/drive/v3/files`, and never to the download host, a redirect or another host.
- a file counts as inside a listed folder only when google's own listing of that folder, asked with the token in the same run, puts it there. a file a lock names elsewhere never gets the token.

```toml
drive_folders = ["https://drive.google.com/drive/folders/1AbCdEfGhIjKlMnOpQrStUvWxYz012345"]
```

```sh
fetchloom add drive:folders/1AbCdEfGhIjKlMnOpQrStUvWxYz012345 --as raw
```

the folder's page is refused, so drive's api lists it, with at most [`FETCHLOOM_MOST_FILES`](../reference/environment.md) files and folders, and each file is pinned with the size, SHA-256 and MD5 drive states. data.lock keeps the same `https://drive.google.com/uc?id=` link as for a shared file, so it says nothing about who may read it, and a sync on another machine finds the files again the same way, through that machine's own `drive_folders` and credentials. items in shared drives work too, and an account an item is shared with needs no resource key. [sources](../../dev/architecture/sources.md#google-credentials-for-private-drive-items) has how drive's api is asked.

google docs, sheets and slides in a private folder are listed as unavailable, as in a shared one, and so are shortcuts: add the item a shortcut points to by its own link.

### with a service account

a service account is an account for a program, with its own email. share the folder with it as you share with a person, and it reads that folder and nothing else of yours.

1. in the [google cloud console](https://console.cloud.google.com/), pick or make a project, and turn on the google drive api for it under apis and services, library.
2. under iam and admin, service accounts, make a service account, then under its keys make a JSON key and download it. an organization may forbid keys; ask its admin, or use your own login below.
3. in drive, share the folder with the service account's email, such as `lab-reader@lab-project.iam.gserviceaccount.com`, as a viewer.
4. set `GOOGLE_APPLICATION_CREDENTIALS` to the key file and list the folder in `drive_folders`.

fetchloom signs its own request for a token with the key, for the `drive.readonly` scope only. when drive refuses the service account, the error names its email and says to share the folder with it.

### with your own login

`gcloud auth application-default login` signs you in through gcloud's own oauth client, which google lets ask for cloud scopes only. asked for a drive scope, google answers "this app is blocked", as its [troubleshooting page for application default credentials](https://docs.cloud.google.com/docs/authentication/troubleshoot-adc) says. the way around, which [gcloud's reference](https://cloud.google.com/sdk/gcloud/reference/auth/application-default/login) names, is an oauth client of your own: a small app registration in your own google cloud project, which gcloud signs you in through instead, with the scopes you choose. rclone asks people to [make their own client id](https://rclone.org/drive/#making-your-own-client-id) for drive the same way. it takes about ten minutes, once:

1. in the [google cloud console](https://console.cloud.google.com/), pick or make a project, and turn on the google drive api for it under apis and services, library. drive counts your requests against this project.
2. under google auth platform, set up the consent screen: an app name and your email. for audience, an account of an organization can pick internal; anyone else picks external, then adds their own google account as a test user.
3. under data access, add the scope `https://www.googleapis.com/auth/drive.readonly`.
4. under clients, make a client of type desktop app, and download its JSON file.
5. sign in through it:

    ```sh
    gcloud auth application-default login --client-id-file=client_secret.json --scopes=https://www.googleapis.com/auth/drive.readonly,https://www.googleapis.com/auth/cloud-platform
    ```

    google warns that it has not verified the app. it is your own, so go on. `cloud-platform` keeps the same login working for [private cloud storage buckets](#private-cloud-storage-buckets) and other google cloud tools.

an external app left in testing gets logins that google ends after 7 days, by its [rules for refresh tokens](https://developers.google.com/identity/protocols/oauth2#expiration). to keep yours, publish the app under audience. google does not ask to [verify](https://support.google.com/cloud/answer/13464323) an app for personal use with fewer than 100 users; you only see the warning when you sign in. an internal app has neither limit.

a login made with gcloud's own client, without the drive scope, is told apart: the error says its token lacks the `drive.readonly` scope and gives the command above.

### refusals

| what you see | what it means |
|---|---|
| `... is not shared with anyone who has the link`, with help to add its id to `drive_folders` | no listed folder holds it. list it, or the folder it is in, if you trust it with your google credentials |
| `..., and no google application default credentials were found` | it is listed, but neither `GOOGLE_APPLICATION_CREDENTIALS` nor a gcloud login is there |
| `google drive has no file or folder ... that <account> can read` | the account has no access, or the item is gone: share it with that account |
| `... its token lacks the drive.readonly scope` | a login made without a drive scope, such as with gcloud's own client |
| `google drive's api is turned off in the google cloud project of ...` | turn on the google drive api in the project of the key or the oauth client |
| `google refused the google login in ...` | the login expired or was revoked, as one of an app in testing is after 7 days: sign in again through your oauth client |
