# google drive

google drive references fetch shared files or walk shared folders while pinning fixed file bytes. a file or folder shared with your google account or a service account, rather than with anyone who has the link, is read through drive's api once you list it: see [private files and folders](#private-files-and-folders).

```sh
fetchloom get drive:15bpMH3clXwBJf9cdGpXIOgwKwHrg4T--
fetchloom add drive:15bpMH3clXwBJf9cdGpXIOgwKwHrg4T--
```

## references

| spelling | behavior |
|---|---|
| `drive:` followed by a file id | fetch the shared file |
| `drive:folders/` followed by a folder id | walk the shared folder |
| a shared file or folder link | preserve its id and resource key when supplied |
| a shared `open?id=` link | resolve it to a file or folder reference |

## pins

resolved references retain the id and any `resourcekey`. file URLs use `https://drive.google.com/uc` with the id and resource key. confirmation-form fields are not written to the lock.

transfers start at `https://drive.usercontent.google.com/download` with `id`, `export=download` and `confirm=t`, plus the resource key when needed. a confirmation page can supply the next request without changing the lock URL.

folder listings give no file sizes. downloads supply sizes for the lock. [approval](../concepts.md#approval) checks a server's stated length, or received bytes when no length is available.

## access and limits

use links that grant access to the shared files. fetchloom follows download confirmations and reports quota, access and missing-file errors. a resource key is retained when the link supplies one. a file or folder shared only with some people fails with `auth.required`, unless `drive_folders` lists it, as [private files and folders](#private-files-and-folders) explains.

google docs exports can change between downloads. select and exclude patterns also match these entries. a doc that remains selected stops the command; the error names the selected docs and explains how to exclude them.

exclude those paths, or ask the owner to upload fixed files. `add` saves exclusions in `data.toml`; an existing project needs that setting edited there. every select pattern must match before exclusions, and some fixed files must remain. empty folders and empty selections are refused.

shared links can change their contents. file hashes pin bytes after resolution, and a later `get` can accept changed bytes. use the project lock when the input must stay fixed.

## private files and folders

a file or folder shared with some people, not with anyone who has the link, is read through drive's api with google's [application default credentials](../reference/environment.md#google-application-default-credentials): a service account's key, or a login of yours. a token that reads drive reads every file its account can see, so fetchloom sends it only where you said, never because data.toml or data.lock names something:

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

   google warns that it has not verified the app. it is your own, so go on. `cloud-platform` keeps the same login working for [private cloud storage buckets](buckets.md#private-cloud-storage-buckets) and other google cloud tools.

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
