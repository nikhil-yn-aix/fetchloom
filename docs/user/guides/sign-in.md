# Sign in to a source

most data is public and needs no account. a key is needed for the rest: a private, restricted or gated dataset, a kaggle competition, any download from dryad, and a private bucket, container or drive folder. a key is kept on your machine, never in data.toml or data.lock, so everyone who clones a project signs in on their own.

## sources with an account

```sh
fetchloom auth login hf
fetchloom auth
```

`auth login` shows the steps to make a key, takes it without showing it, checks it with the source and saves it. `fetchloom auth` then lists each account with where its key comes from and, online, whose it is. [auth](../commands/auth.md) has the details.

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

you may be signed in already: fetchloom reads the keys the hf and kaggle commands saved, and a `.netrc` that curl reads, and `auth login` saves to the same places, so those tools share the key. a variable wins over a saved key. [environment](../reference/credentials.md) gives the order fetchloom looks in.

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
- windows' credential manager keeps at most 2,560 bytes for one entry, about 1,280 characters, and a site's cookies often take more, as can a long token or header, so saving them there fails; its help says to set `FETCHLOOM_KEYRING=false`, which keeps them in fetchloom's own file, readable only by you.

## when a download is refused

at a terminal, a command refused for want of an account offers to sign in, then tries once more. a key that went with the request and was refused, as an old `HF_TOKEN`, is named by where it comes from, never by its value. [auth](#when-a-download-is-refused) shows both.

a website with nothing saved for it that answers 401 or 403 gets the same offer for its host and port. its help names `fetchloom auth login <host>` with the ways a site lets you in, and at a terminal you choose one: a browser's cookies, a cookies.txt file, a token, a header or a username and password. a site whose answer asks for a username and password, as a basic challenge does, is asked for those alone, and one that asks for a token, as a bearer challenge does, has its help name `--token`. once something is saved for the host, a 401 or 403 says what was refused and that it may have expired, and so does a web page where a file was expected, as a site sends its sign-in page once a session ends; both give the command that saves a new one. a page you asked for with `--web-page`, or one at a link ending in `.html`, is the data rather than a sign-in page when the link itself answers with it. a page the site sends you on to, at another path, counts only when you asked for it, it stays on the site, as a move to `www.` or to a newer version's path does, and its link names no sign-in page, as `/login` or a `?next=` way back does; anything else is refused, since that is how a sign-in page arrives, and the help names the link the page is at, which you can ask for with `--web-page`.

in a script or CI there is nobody to ask: set the source's variable from the CI system's secrets, as [CI](ci.md#keys) says, or pipe the key to `fetchloom auth login <account> --with-token`, as [auth](../commands/auth.md#scripts-and-ci) shows.

at a terminal, a command refused for want of an account offers to sign in, then runs once more. the offer comes before any `error:`, its first line naming what was refused, and defaults to no: enter alone starts no sign-in, and a no ends the run with one `Cancelled` line and the refusal's exit code, 40. hugging face and kaggle answer a name that does not exist as they answer a private one, so for those the offer first says to check the name, and where:

```text
    Refused huggingface.co has no dataset acme-lab/field-notes, or it is private
      Check the name first at https://huggingface.co/datasets/acme-lab/field-notes
            hugging face answers alike for a name that does not exist and for a private one
sign in to hugging face now? [y/N] n
  Cancelled signing in to hugging face
```

a command that is refused again once you signed in ends with that refusal as an error, and so does any refusal while a key for the source is on this machine: signing in again is no remedy, so its help says the key was refused or has no access, and that `fetchloom auth` shows where the key comes from and whose it is; only terms to accept are still offered. a server that asks for a login over plain http gets no offer unless it is on this machine or at an origin `allowed_origins` lists, since a key goes over plain http only there; anywhere else the help says so and asks for an https link. `-q` prints the error above the question instead of the `Refused` line.

`add` first asks the source, once, for the public datasets of the same owner, signed in or not. when one is a typo away, signing in could not help, so no sign-in is offered, the error is `reference.unresolved`, exit 10, rather than `auth.required`, and the help gives your command with the name fixed; signed in, the error above it reads `hf:datasets/scikit-learn/irsi does not exist, or is not public`. a source that is busy, or whose rate limit is used up, gets no second try, so the error comes at once with no suggestion. an offline run asks nothing more, so it keeps `auth.required`, exit 40. with `--json` the suggestion is in `help` and `fix` stays null, since a guess is never a command to run:

```text
error: huggingface.co has no dataset scikit-learn/irsi, or it is private
  help: did you mean `fetchloom add hf:datasets/scikit-learn/iris`?
```

a website fetchloom has no account for, with nothing saved for it, gets the offer when it answers 401 or 403 to the link itself: a picker asks how the site lets you in, then signs you in that way, as `auth login <host>` with `--browser`, `--cookies`, `--token`, `--header` or a username and password would, and runs the command once more. q or esc ends the run as a no. a site whose answer asks for a username and password, as a basic challenge does, gets the question above and the login's prompts instead. a host a redirect led to is offered nothing, since a website's key never follows a redirect, unless it asks for a `.netrc` login, which does. from a real run against a server on this machine, choosing 3:

```text
    Refused 127.0.0.1 refused access to http://127.0.0.1:19371/open.csv (403)
  sign in to 127.0.0.1:19371 with  ↑↓ move · 1-9 or enter choose · q cancel
  › 1 browser  the cookies of a browser signed in there
    2 cookies  a cookies.txt file a browser extension saved
    3 token    a token the site gave you
    4 header   a key the site sends in a header of its own
    5 login    a username and password
    Sign in 127.0.0.1:19371
         1. sign in at 127.0.0.1:19371 in your browser
         2. make a token or an api key where its settings or profile offer one
         3. paste it below; it is sent in Authorization: Bearer
      token ******** · 8 characters
no credential store here, so it would go in a file only you can read
save it to ~/.config/fetchloom/credentials.json? [y/N] y
  Signed in to 127.0.0.1:19371 with a token · saved to ~/.config/fetchloom/credentials.json
 Downloaded open.csv · 8 B in 1 ms
      Saved open.csv
```

the picker clears itself once you choose, so the screen keeps the lines from `Sign in` on. with `--json`, or in a script, the help names the command and each way instead, and `fix` stays null.

a gated hugging face dataset, a restricted zenodo record or a kaggle competition also needs you to accept its terms on its page. without a key on this machine, the offer says that first, as `First accept the terms at https://huggingface.co/datasets/bigcode/the-stack while signed in there`, then asks whether to sign in; with a key, it shows that link and asks whether to try again once you have. a key cannot accept terms for you. scripts and `--json` never get these questions: the error names the command to run.

a key that went with the request and was refused, as an old or wrong `HF_TOKEN` is, is named by where it comes from, never by its value, with how to replace it:

```text
error: huggingface.co refused the key in HF_TOKEN for hf:datasets/bigcode/the-stack
  huggingface.co says: Access to dataset bigcode/the-stack is restricted. You must have access to it and be authenticated to access it. Please log in.
  help: the key may be old or wrong: set HF_TOKEN to a current key from https://huggingface.co/settings/tokens, or unset HF_TOKEN to use the key `fetchloom auth login hf` saves
```

a key saved by signing in is named by its file, below your home folder as `~/…`, and the help says that `fetchloom auth` shows where it comes from and whose it is. no sign-in is offered for a key from a variable, which would still win over the key a sign-in saves.
