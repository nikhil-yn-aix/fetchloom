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

`data.toml` keeps the `s3://`, `s3:https://` or `gs://` form, even for a pasted link. without `--as`, `add` names a reference to one object after that object. a folder, whose reference ends in `/`, and a whole bucket are named by their path, even when they hold one object, so folders whose objects share a name stay apart: openalex's date folders each hold one `part_0000.gz`, and `s3://openalex/data/jsonl/works/updated_date=2016-06-24/` becomes `updated_date-2016-06-24`. the name is the last segment of the path, with the segments before it while it holds no letter, so `s3://openneuro.org/ds000001/` becomes `ds000001` and `gs://arxiv-dataset/arxiv/arxiv/pdf/2401/` becomes `pdf-2401`. `--select` keeping one of several objects does not change it; use `--as` to choose it. `get` names its folder the same way, and after the file without its file type for a reference to one object.

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

| source | `resolved` | file `url` |
|---|---|---|
| S3 bucket | `s3://<bucket>/<prefix>`, or `s3:https://<host>/<bucket>/<prefix>` at another service | the object's HTTPS URL on the endpoint that listed it, with `?versionId=<id>` when the bucket keeps versions |
| cloud storage bucket | `gs://<bucket>/<prefix>` | `https://storage.googleapis.com/<bucket>/<key>?generation=<generation>` |

## private S3 buckets

see [private S3 buckets](../guides/private-cloud-storage.md#private-s3-buckets).

## private cloud storage buckets

see [private cloud storage buckets](../guides/private-cloud-storage.md#private-cloud-storage-buckets).

## access and limits

a bucket that S3 or cloud storage lists only once a key or token goes with the request records `access = "login"` in [data.lock](../reference/data-lock.md), since a teammate needs one too, and [`push`](../commands/push.md#data-you-may-not-share) asks before copying it into a mirror. a bucket either lists to anyone records no `access`.

a refused listing (401 or 403, such as S3's `AccessDenied`) fails with `auth.required` and exit code 40, saying whether a key went with the request and what to do. a bucket `s3_buckets` does not list:

```text
error: s3://lab-raw/2026/ is private: S3 refused to list it without a key
  help: if you trust s3://lab-raw with your AWS keys, add it to s3_buckets in the settings file or FETCHLOOM_S3_BUCKETS
```

a listed bucket with no keys on this machine:

```text
error: s3://lab-raw/2026/ is private: S3 refused to list it without a key, and no AWS keys were found
  help: set AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY, or AWS_PROFILE to a profile with keys in ~/.aws/credentials, a credential_process, or sso or a role through the AWS CLI. see https://nikhil-yn-aix.github.io/fetchloom/user/guides/private-cloud-storage/#private-s3-buckets
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
