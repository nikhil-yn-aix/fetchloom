# Privacy

## what leaves your machine

fetchloom sends nothing on its own: it has no telemetry, no account and no update check. a request goes out only for work a command does:

| what | goes to |
|---|---|
| search words, given to `search`, `add` or `get` in place of a link | the search of hugging face, kaggle, zenodo, GEO and openneuro, or of the sites `search --source` names, ENA and dandi among them |
| a reference | the site it names: a DOI to doi.org, which says where it points, an id such as `zenodo:3608990` to that site, a link to its host |
| a download | the file's host, or a [mirror](../guides/mirrors.md#privacy) you named, which is asked by SHA-256 alone |
| a key | its own host only, over https, as [where a key goes](#where-a-key-goes) says |

each request names fetchloom and its version, as `fetchloom/0.1.0`, in its `User-Agent` header. a path on this machine never leaves it: `add` copies the files a path names into the cache with no request, `get` and `search` refuse a path before any request, and data.lock records no path for an import. what your repository shares is `data.toml` and `data.lock`: each dataset's reference, links, file names, sizes and SHA-256. a `-vv` log names local paths and every link fetchloom asked for, so read it before you share it.

## where a key goes

a key goes over https, to its own host on port 443, and never after a redirect leaves that host. plain http would show it to every network on the way, so a key goes over plain http only to this machine, `localhost` or a loopback address, or to an origin the `allowed_origins` setting lists, such as a lab server on a network you trust, and only when no proxy carries the request, since a proxy reads a plain http request whole: with `HTTP_PROXY` set, list the host in `NO_PROXY` to send it straight there. that holds for a `.netrc` login too, and a link to any other plain http host goes without one. a dataverse key saved for an installation on another port, as `--host dv.example.edu:8443`, goes to that port only. a `.netrc` login also answers on later hops to its own host, as curl sends it, which earthdata needs: its archives send you to `urs.earthdata.nasa.gov` to sign in. an osf key also reaches `osf.io` and `files.osf.io`, where osf's downloads go. a github key goes to `api.github.com` only, never to github.com, where release files are. a semantic scholar key goes to `api.semanticscholar.org` only, never to the links on S3 it gives for dataset files.

a website's token, header or cookies go to its own host and port alone, and only while a request stays on the origin it started on: once a redirect leaves it, no later hop gets it, not even one back on the same host. a cookie the site sets in return, as a renewed session, goes by the same rules, so it never reaches another port or plain http, a proxy, or another host its domain covers. a site that refuses a plain http link where no key may go is told so, `fetchloom sends a key over plain http only to this machine or to an origin allowed_origins lists`, rather than offered a sign-in that could not help. data.toml and data.lock can never add a host, a header, a token or a cookie, since a cloned project could point them anywhere: every key comes from you, through `auth login`, a variable or the settings file.

no key appears in data.toml, data.lock, the cache, logs or `--json` output. wherever a link is shown, its query and fragment values are masked unless their names are known to be harmless, such as `dl` or `version`, and recognized secrets, such as `token=`, are refused in a project's references. a key holding a character no request can carry, such as a line break, is left out of every request, and [`fetchloom auth`](../commands/auth.md#status) warns about it.
