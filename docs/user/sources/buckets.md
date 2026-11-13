# cloud buckets

fetchloom lists and downloads S3 and google cloud storage buckets over HTTPS, without an AWS or google SDK. a public bucket needs no account; a private S3 bucket you list in the `s3_buckets` setting is read with your AWS keys, which a profile may also get from a program or, for a sign-in or a role, from the AWS CLI, and a private cloud storage bucket you list in the `gcs_buckets` setting with your google application default credentials. azure blob containers have [a page of their own](azure.md).

```sh
fetchloom add s3://openneuro.org/ds000001/
fetchloom get gs://cloud-samples-data/bigquery/us-states/
FETCHLOOM_S3_BUCKETS=s3://lab-raw fetchloom add s3://lab-raw/2026/
FETCHLOOM_GCS_BUCKETS=gs://lab-private fetchloom add gs://lab-private/raw/
fetchloom add s3:https://minio.lab.example:9000/scans/raw/
```

## references

| written form | stored as |
|---|---|
| `s3://<bucket>/<prefix>/` | the same |
| `gs://<bucket>/<prefix>/` | the same |
| `https://<bucket>.s3.amazonaws.com/<prefix>/`, also with `s3.<region>` or `s3-<region>` in the host | `s3://<bucket>/<prefix>/` |
| `https://s3.amazonaws.com/<bucket>/<prefix>/` or `https://s3.<region>.amazonaws.com/<bucket>/<prefix>/` | `s3://<bucket>/<prefix>/` |
| `https://storage.googleapis.com/<bucket>/<prefix>/` or `https://<bucket>.storage.googleapis.com/<prefix>/` | `gs://<bucket>/<prefix>/` |
| `s3:https://<host>[:<port>]/<bucket>/<prefix>/`, or `s3:http://…` | the same, with the host in lowercase and a usual port left out |

a bucket at another service that speaks S3's API, such as minio, cloudflare r2, backblaze b2, wasabi or ceph, is written `s3:` followed by the service's own link, its host and port, then the bucket and prefix. it may hold no login, query or fragment. the endpoint written on the command line may be on this machine or your network for that run, at its own origin; one only data.toml or data.lock names reaches such a server when [`allowed_origins`](../reference/environment.md) lists its origin, as for any link.

a pasted HTTPS link names a prefix only when its path ends in `/`, or names the bucket alone. a link to one object stays a plain [URL](urls-and-dois.md) and downloads that object. a link with a query string, such as a presigned link, also stays a plain URL, as do website, dualstack and other S3 endpoint hosts.

an `s3://` or `gs://` reference without a final `/` names the object with exactly that key, or else the folder of that name. keys are case sensitive. bucket names follow each service's rules: lowercase letters, digits, `-` and `.`, from 3 to 63 characters on S3, and also `_` and up to 222 characters on cloud storage.

`data.toml` keeps the `s3://`, `s3:https://` or `gs://` form, even for a pasted link. without `--as`, `add` names a one-object listing after that file. otherwise it uses the last segment of the prefix, so `s3://openneuro.org/ds000001/` becomes `ds000001`. it chooses from the unfiltered listing, so `--select` keeping one of several objects does not change that prefix-derived name; use `--as` to choose it. `get` chooses its folder from the selected files and uses the filename stem when one remains.

## listing

fetchloom lists a bucket page by page, and a bucket in another AWS region than it first asked is found there. a listing whose service sends a page token again, or 100 empty pages in a row, is broken and fails with `source.invalid`; an empty page partway through is fine. [sources](../../dev/architecture/sources.md#buckets) has how each store is listed.

zero-byte objects whose keys end in `/` mark folders and are skipped. a nonempty object ending in `/` cannot name a file on disk, so the listing fails with `source.invalid` before selection or downloads. ask the owner to rename it, or choose a prefix that leaves it out. the limit on how many files a dataset lists, [`FETCHLOOM_MOST_FILES`](../reference/environment.md), applies before `--select` and `--exclude`, so name a narrower prefix inside a large bucket. a prefix that holds no objects fails with `reference.unresolved`.

## pins

| store | `resolved` | file `url` | `md5` |
|---|---|---|---|
| S3 | the `s3://` reference | the object's HTTPS URL on the endpoint that listed it, on its region's endpoint after a signed listing, with `?versionId=<id>` when the bucket keeps versions | the ETag, when it holds no `-` and the listing went without a key |
| another S3 service | the `s3:https://` reference | the object's link on that service, `https://<host>[:<port>]/<bucket>/<key>`, with `?versionId=<id>` when the bucket keeps versions | none |
| cloud storage | the `gs://` reference | `https://storage.googleapis.com/<bucket>/<key>?generation=<generation>` | the listed `md5Hash` |

a stated MD5 is checked against the downloaded bytes; the other files are pinned by SHA-256 and size alone, since an ETag with a `-`, one from a bucket that needs a key, or one from another service is no MD5 of the bytes.

`lock --upgrade` lists the prefix again and downloads only the objects it has no proof are unchanged. an object without an MD5 counts as unchanged when the listing gives the ETag and size this machine's cache recorded at its last download of that link, and the cache still holds those bytes; the record lives in the cache, not in `data.lock`, so a teammate without it downloads the object again. this trusts the service to give an object a new ETag whenever its bytes change, as S3 does, as an answer that a file is unchanged since an ETag is trusted. a service that keeps the ETag and size while the bytes change keeps the old pin here, and the change shows wherever the bytes are downloaded, as in a teammate's `sync`, which refuses them with `integrity.mismatch`, exit 30, and places nothing from them.

an S3 bucket that keeps versions names each object's version, which the lock URL pins, so `sync` gets the same bytes after someone replaces the object; bytes from the cache keep the version an earlier download of that link recorded. a pinned version that is gone, or that the key may not read, as many public buckets refuse old versions to anyone without a key, is fetched from the object as it is now, and the locked SHA-256 decides: the same bytes are used, and other bytes fail with `integrity.mismatch`, exit 30, saying the version could not be read. a bucket that keeps no versions serves whatever the key holds now, and sync refuses bytes that differ from the locked SHA-256.

a cloud storage URL keeps the object's generation. a listing with a missing, empty or nonnumeric generation fails with `source.invalid`; fetchloom does not replace it with an unversioned URL. when someone replaces the object, that URL still names the old bytes while the bucket keeps old generations, and fails to download when it does not.

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

the credentials are google's [application default credentials](../reference/environment.md#google-application-default-credentials): a service account's JSON key that `GOOGLE_APPLICATION_CREDENTIALS` names, which signs a request for a token, else the login `gcloud auth application-default login` saved. a service account asks for the read-only storage scope, `devstorage.read_only`; a login's token carries the scopes you granted when you signed in, cloud storage among them by default. a token that expires or is refused with 401 is replaced. messages name the identity, such as `the service account loader@lab-project.iam.gserviceaccount.com in /home/ada/keys/lab.json`, never what it signs with.

the same credentials read the [private google drive folders and files](google-drive.md#private-files-and-folders) that `drive_folders` lists, with a scope of their own.

## access and limits

a refused listing (401 or 403, such as S3's `AccessDenied`) fails with `auth.required` and exit code 40, saying whether a key went with the request and what to do. a bucket `s3_buckets` does not list:

```text
error: s3://lab-raw/2026/ is private: S3 refused to list it without a key
  help: if you trust s3://lab-raw with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS
```

a listed bucket with no keys on this machine:

```text
error: s3://lab-raw/2026/ is private: S3 refused to list it without a key, and no AWS keys were found
  help: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile with keys in ~/.aws/credentials, a credential_process, or sso or a role through the AWS CLI. see https://nikhil-yn-aix.github.io/fetchloom/user/sources/buckets.html#private-s3-buckets
```

cloud storage says the same of a bucket `gcs_buckets` does not list, `gs://lab-private/raw/ is private: google cloud storage refused to list it without credentials`, with help to add it to `gcs_buckets` or `FETCHLOOM_GCS_BUCKETS`, and of a listed one with none on this machine, `... and no google application default credentials were found`, with help to run `gcloud auth application-default login` or set `GOOGLE_APPLICATION_CREDENTIALS` to a service account's key file.

a login google no longer takes, here one that was revoked, fails before any download:

```text
error: google refused the google login in /home/ada/.config/gcloud/application_default_credentials.json
  google said invalid_grant: Token has been expired or revoked.
  help: sign in again with `gcloud auth application-default login`
```

a request with a token cloud storage still refuses says `google cloud storage refused the service account … for gs://lab-private/raw/` and asks you to have the owner let that account list and read the bucket. a token refused for lacking the scope that reads cloud storage, which google says in a `WWW-Authenticate` header with `error="insufficient_scope"` or in its message, is not replaced: the error says so, and to sign in again with `gcloud auth application-default login` with no `--scopes`, or with `https://www.googleapis.com/auth/cloud-platform` among them. a signed request S3 still refuses says `S3 refused the AWS key …WXYZ from AWS_ACCESS_KEY_ID for s3://lab-raw/2026/` and asks the same for that key, with a line under each error that repeats what S3 said, such as `AccessDenied: Access Denied`. a download refused the same way fails with the same kind, naming its link. a service that wants another region than the request was signed for names it, and a missing bucket fails with `reference.unresolved`.

requester-pays buckets bill the reader for each request, and fetchloom never agrees to pay: they fail with `auth.required`, with or without a key or a token, since fetchloom never names a project to bill. use the AWS CLI with `--request-payer requester`, or `gcloud storage cp` with `--billing-project`, for them. a bucket that allows reading objects but not listing them still works one object at a time through plain object links, `https://storage.googleapis.com/<bucket>/<key>` for cloud storage.

AWS keys that an instance's or a container's role gives, or that `AWS_ROLE_ARN` and `AWS_WEB_IDENTITY_TOKEN_FILE` lead to outside a profile, are not read; an object link that needs no key still works as a plain URL.
