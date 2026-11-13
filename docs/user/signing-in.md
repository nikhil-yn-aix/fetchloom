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
| `s2` | semantic scholar's dataset files, and a rate of your own for papers and search | `S2_API_KEY` |
| `dataverse` | restricted files, on the installation `--host` names | `DATAVERSE_API_TOKEN`, for the installation `DATAVERSE_URL` names |
| `earthdata`, `physionet` or a host name | a server that asks for a username and password | a `machine` line in `.netrc` |

you may be signed in already: fetchloom reads the keys the hf and kaggle commands saved, and a `.netrc` that curl reads, and `auth login` saves to the same places, so those tools share the key. a variable wins over a saved key. [environment](reference/environment.md#credentials) gives the order fetchloom looks in.

a gated dataset or a competition also needs its terms accepted on its own page, with the same account; a key cannot accept them for you.

## any website

a site fetchloom has no account for may still want you signed in. `auth login` with the site's host name saves what the site gives you, for that host alone:

| the site gives you | sign in with | each request to the host carries |
|---|---|---|
| a username and password | `fetchloom auth login data.example.org` | the login, from `.netrc`, which curl and wget read too |
| a token | `fetchloom auth login data.example.org --token` | `Authorization: Bearer <token>` |
| a key it reads from a header, as many apis ask | `fetchloom auth login api.example.org --header X-API-Key` | `X-API-Key: <key>` |
| a session in your browser, once you signed in there | `fetchloom auth login data.example.org --browser firefox` | the session's cookies, by cookie rules |
| the same session, exported by a browser extension | `fetchloom auth login data.example.org --cookies cookies.txt` | the session's cookies, by cookie rules |

a token or a header's key is typed without showing, or piped in with `--with-token`, and kept in the system's credential store, as the keys of sources without a file of their own are. `fetchloom auth` lists each host with its kind and where it is kept, and `fetchloom auth logout data.example.org` removes it. a host on another port than 443 is named with its port, as `data.example.org:8443`, and so is a server on this machine, as `localhost:8080`.

a session is what a site keeps of your sign-in in your browser, so whatever the site asked of you there, a single sign-on page, a second step or a captcha, counts once you finish it. `--browser firefox` reads the session's cookies from the firefox profile you used last, or the one `--browser firefox:<profile>` names, and asks before saving them; `chrome`, `chromium`, `brave`, `edge`, `vivaldi` and `opera` read those browsers' profiles on linux and macos, where macos asks whether fetchloom may use the browser's key in your keychain, while on windows only firefox and cookies files work, since chrome seals cookies there so that only chrome may open them; `--cookies` reads a cookies.txt file, the format curl's `-b` and yt-dlp read, which a browser extension exports while you are signed in. fetchloom keeps only the cookies whose domain covers the host, each with its path, secure flag and expiry, and names those it left out by name and domain, never by value. a cookie goes only to the host it was saved for, then by cookie rules: a path that matches, https for a secure one, and not once it expired. a session ends when the site says so, often within days; `fetchloom auth` shows when the soonest cookie expires, and a refusal names the command that takes new ones.

`--header` takes a header a key can travel in, such as `X-API-Key` or `Authorization`. it refuses the headers that frame a request or belong to one connection, such as `Host`, `Content-Length`, `Transfer-Encoding` and `Connection`, and those fetchloom sets itself to resume and check downloads, such as `Range`.

### what still cannot work

- a site that hands out a file only through javascript, such as a button whose script builds the link: fetchloom runs no page's scripts, so it never sees the link. click it in your browser, copy the address the download came from, and give fetchloom that link, with the site's cookies saved when it wants them.
- an oauth "allow" window, where a site asks whether an app may use your account: only a service that registered an app with that site can show one, as fetchloom has with none. sign in with what the site hands you instead, a token, a key or your browser's session.
- a session the site ties to the browser it began in, by its address or a key the browser holds: its cookies are refused anywhere else, so use a token from the site's settings where it offers one.
- chrome, edge and the other chromium browsers on windows seal their cookies so that only the browser itself can open them, which chrome calls app-bound encryption, since chrome 127. export the cookies with a browser extension and use `--cookies`, or sign in with firefox.
- chromium browsers on linux whose key is kept only in kwallet, and safari, are not read. export the cookies with an extension and use `--cookies`.
- windows' credential manager keeps at most 2,560 bytes for one entry, about 1,280 characters, and a site's cookies often take more, so saving them there fails; its help says to set `FETCHLOOM_KEYRING=false`, which keeps them in fetchloom's own file, readable only by you.

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
| azure blob storage | `azure_containers` | `az login`, a SAS in `AZURE_STORAGE_SAS_TOKEN` or in the container's entry, an account key in `AZURE_STORAGE_KEY` with `AZURE_STORAGE_ACCOUNT`, or a virtual machine's managed identity with `AZURE_TOKEN_CREDENTIALS=ManagedIdentityCredential` |

each request goes without a key first, so a public bucket never sees one, and the key follows only once the store refused. `fetchloom auth` shows the AWS keys, google credentials and azure credentials it would use, without running a program or asking a server. each store's page has the rest: [private S3 buckets](sources/buckets.md#private-s3-buckets), [private cloud storage buckets](sources/buckets.md#private-cloud-storage-buckets), [private drive files and folders](sources/google-drive.md#private-files-and-folders) and [private azure containers](sources/azure.md#private-containers).

## when a download is refused

at a terminal, a command refused for want of an account offers to sign in, then tries once more. a key that went with the request and was refused, as an old `HF_TOKEN`, is named by where it comes from, never by its value. [auth](commands/auth.md#when-a-download-is-refused) shows both.

a website with nothing saved for it that answers 401 or 403 gets the same offer for its host and port. its help names `fetchloom auth login <host>` with the ways a site lets you in, and at a terminal you choose one: a browser's cookies, a cookies.txt file, a token, a header or a username and password. a site whose answer asks for a username and password, as a basic challenge does, is asked for those alone, and one that asks for a token, as a bearer challenge does, has its help name `--token`. once something is saved for the host, a 401 or 403 says what was refused and that it may have expired, and so does a web page where a file was expected, as a site sends its sign-in page once a session ends; both give the command that saves a new one. a page you asked for with `--web-page`, or one at a link ending in `.html`, is the data rather than a sign-in page as long as the link itself answers with it: a page the site sends you on to, at another path or site, is refused either way, since that is how a sign-in page arrives.

in a script or CI there is nobody to ask: set the source's variable from the CI system's secrets, as [CI](ci.md#keys) says, or pipe the key to `fetchloom auth login <account> --with-token`, as [auth](commands/auth.md#scripts-and-ci) shows.

## where a key goes

a key goes over https, to its own host on port 443, and never after a redirect leaves that host. plain http would show it to every network on the way, so a key goes over plain http only to this machine, `localhost` or a loopback address, or to an origin the `allowed_origins` setting lists, such as a lab server on a network you trust, and only when no proxy carries the request, since a proxy reads a plain http request whole: with `HTTP_PROXY` set, list the host in `NO_PROXY` to send it straight there. that holds for a `.netrc` login too, and a link to any other plain http host goes without one. a dataverse key saved for an installation on another port, as `--host dv.example.edu:8443`, goes to that port only. a `.netrc` login also answers on later hops to its own host, as curl sends it, which earthdata needs: its archives send you to `urs.earthdata.nasa.gov` to sign in. an osf key also reaches `osf.io` and `files.osf.io`, where osf's downloads go. a github key goes to `api.github.com` only, never to github.com, where release files are. a semantic scholar key goes to `api.semanticscholar.org` only, never to the links on S3 it gives for dataset files.

a website's token, header or cookies go to its own host and port alone, and only while a request stays on the origin it started on: once a redirect leaves it, no later hop gets it, not even one back on the same host. a site that refuses a plain http link where no key may go is told so, `fetchloom sends a key over plain http only to this machine or to an origin allowed_origins lists`, rather than offered a sign-in that could not help. data.toml and data.lock can never add a host, a header, a token or a cookie, since a cloned project could point them anywhere: every key comes from you, through `auth login`, a variable or the settings file.

no key appears in data.toml, data.lock, the cache, logs or `--json` output. wherever a link is shown, its query and fragment values are masked unless their names are known to be harmless, such as `dl` or `version`, and recognized secrets, such as `token=`, are refused in a project's references. a key holding a character no request can carry, such as a line break, is left out of every request, and [`fetchloom auth`](commands/auth.md#status) warns about it.
