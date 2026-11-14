# auth

`auth` signs you in to sources that need an account, such as for restricted data or kaggle competitions. [signing in](../guides/sign-in.md) says which do, and how private buckets get their keys.

```sh
fetchloom auth
fetchloom auth login hf
fetchloom auth logout zenodo
```

fetchloom reads the keys a source's own tools saved, and a sign-in saves where those tools read: hugging face's and kaggle's files and `.netrc`. keys for the other sources go in the system's credential store, as the table below shows.

## usage

```sh
fetchloom auth [options]
fetchloom auth login [options] <account>
fetchloom auth logout [options] <account>
```

`<account>` is `hf`, `kaggle`, `zenodo`, `figshare`, `osf`, `dryad`, `github`, `gitlab`, `s2`, `dataverse`, `earthdata`, `physionet` or a plain host name such as `data.example.edu`, never a link. a host on another port than 443 carries it, as `data.example.edu:8443` or `localhost:8080`, and what is saved for it goes to that port alone. `--host` names a dataverse installation, with its port when that is not 443, as `--host dv.example.edu:8443`; harvard's is the default. a host name, there and as `<account>`, holds only letters, digits, dots, hyphens and underscores; `--host` also takes an IPv6 address in brackets. an argument that is refused is shown with any secret in it masked.

## signing in

`fetchloom auth login hf` shows the steps to a key: make a free account if you have none, open the page where you make a key, what to pick there, and for a gated dataset or a competition, accept its terms on its own page with the same account. the key you paste stays hidden, then shows as its first and last four characters and its length, so you can tell it arrived. fetchloom asks the source whose key it is, then asks before saving it. a file below your home folder reads as `~/…`, here and in `fetchloom auth`. stderr from a loopback check with a made-up key:

```text
    Sign in hugging face
         1. make a free account at huggingface.co if you have none, and sign in
         2. open https://huggingface.co/settings/tokens
         3. create a token of the read type, or a fine-grained one that can read the repositories you need
         4. a gated dataset also needs its terms accepted on its own page, with the same account
        key hf_A...x9Qz · 37 characters
    Checked the key works for ada
save it to ~/.cache/huggingface/token? [Y/n] yes
  Signed in to hf as ada · saved to ~/.cache/huggingface/token
  hint: the hf command and the hugging face python libraries use this key too
```

it asks before replacing a saved key; `--yes` answers both questions yes, and a no to either changes nothing and exits 1, with `Cancelled signing in to hugging face, so nothing changed`. after [a refused download](../guides/sign-in.md#when-a-download-is-refused), a key you do not save serves only that command's next try. a key the source refuses is never saved. zenodo, github, s2 and earthdata check a key without naming whose it is, and a login for any other host is not checked. a source that gives no answer, as a host this machine cannot reach, leaves a key unchecked: it is saved all the same, and the last line says `Saved a key for <source> to <file>, not checked` instead of `Signed in`, with a hint that `fetchloom auth` checks it once the host answers.

| account | where the key goes | also read by |
|---|---|---|
| hugging face | `$HF_TOKEN_PATH`, else `token` in `$HF_HOME`, else `huggingface/token` in `$XDG_CACHE_HOME`, else `~/.cache/huggingface/token` | the `hf` command, `datasets`, `transformers` |
| kaggle | `~/.kaggle/access_token` for a key starting `KGAT_`; a pasted `kaggle.json` is saved where the kaggle command reads it: `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME` | the `kaggle` command, `kagglehub` |
| nasa earthdata, physionet, any host | `.netrc` in your home folder, or the file `NETRC` names; `_netrc` on windows when it already exists | curl, wget, python's requests |
| zenodo, figshare, osf, dryad, github, gitlab, s2, dataverse | the system's credential store: windows credential manager, the macos keychain or the linux secret service | nothing else; these sources have no shared file |

a machine without a credential store, such as a server reached over ssh, asks before saving the key in `credentials.json` in fetchloom's settings folder, readable by you only on linux and macos: `%APPDATA%\fetchloom` on windows, `~/Library/Application Support/fetchloom` on macos, and `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom` on linux. answering no keeps it out of files; set the source's variable in your environment instead. `FETCHLOOM_KEYRING=false`, or `0`, skips the credential store.

[where a key goes](../concepts/privacy.md#where-a-key-goes) lists the hosts each key is sent to.

## any website

a website fetchloom has no account for takes its own credential, saved for its host alone. [any website](../guides/sign-in.md#any-website) says which to pick.

```sh
fetchloom auth login data.example.org --token
fetchloom auth login api.example.org --header X-API-Key
fetchloom auth login data.example.org --browser firefox
fetchloom auth login data.example.org --cookies cookies.txt
echo "$LAB_TOKEN" | fetchloom auth login data.example.org --token --with-token
```

`--token` saves a token the site gave you, sent as `Authorization: Bearer <token>`. `--header <name>` saves a key the site reads from the header `<name>`, as an api that wants `X-API-Key` asks; it refuses `Host`, `Content-Length`, `Transfer-Encoding`, `Connection`, the other headers that belong to one connection, `Cookie` and the headers fetchloom sets itself, such as `Range`. either asks for the key without showing it, or reads it from stdin with `--with-token`; nothing checks it, since every site checks keys its own way. it is kept in the system's credential store, or with your yes in `credentials.json` where the machine has none, as the keys of zenodo and the other sources without a file of their own are, and replaces what was saved for the host before, after a question. `fetchloom auth logout <host>` removes it, and a `.netrc` login for the same host stays until you run it again.

`--cookies <file>` reads a cookies.txt file, in the netscape format curl's `-b` and yt-dlp read and browser extensions export, and keeps the cookies whose domain covers the host, each with its path, secure flag and expiry. it says which it found and which it left out, by name and domain, never by value, then asks before saving them; `--yes` answers. stderr from a run with a file exported from a browser, for `data.example.org`, on a machine without a credential store:

```text
      Found 2 cookies for data.example.org in cookies.txt
            session · .example.org · expires 2033-05-18
            csrf · data.example.org · ends with the browser's session
   Left out 3 cookies for other hosts: sid, sso_csrf of sso.example.org; ad of .tracker.example
   Left out 1 cookie that expired: old of data.example.org
warning: this machine has no credential store, so they went in /home/ada/.config/fetchloom/credentials.json, readable only by you
  Signed in to data.example.org with 2 cookies from a cookies file, the soonest expiring 2033-05-18 · saved to ~/.config/fetchloom/credentials.json
```

a file with no cookie for the host fails with `usage` and names the domains it holds cookies for; one whose cookies for the host all expired fails with `auth.required`.

`--browser <name>` reads the cookies from your browser's profile once you signed in to the site there, so a single sign-on page, a second step or a captcha the site asked of you counts. `<name>` is `firefox`, `chrome`, `chromium`, `brave`, `edge`, `vivaldi` or `opera`. `firefox` reads the profile firefox used last, in its folder on linux, the XDG one firefox 147 moved new installs to, flatpak's, snap's, `~/Library/Application Support/Firefox` on macos and `%APPDATA%\Mozilla\Firefox` on windows; `firefox:<profile>` names another, by its name in `profiles.ini` or its folder's name, as `default-release`, which fetchloom looks up only under firefox's own folders, never in the folder you run it in, where a cloned project could hold one of that name; a folder anywhere else takes its absolute path. fetchloom reads `cookies.sqlite` and the log beside it without opening the database or locking it, so firefox may keep running, and leaves out the cookies firefox keeps apart for a container, a private window or another site's frames. it shows the host's cookies as `--cookies` does, never another site's, then asks before saving them; `--yes` answers, and where nobody can answer the run exits 1 with `unconfirmed`.

the other names read a chromium browser's profile on linux and macos, the one it used last, as `Default`, or the one `chrome:<profile>` names by its folder under the browser's own, as `chrome:"Profile 1"`, or by an absolute path. chromium seals every cookie it keeps, so one kept plain, as the browser never writes it, is left out by name. chromium encrypts each cookie with a key it keeps in the system's credential store, which fetchloom reads only to open the host's cookies: on linux from the secret service, which gnome keyring and KDE wallet 5.97 and later answer, or chromium's fixed key when the browser runs without a keyring; on macos from the keychain, which asks whether fetchloom may use "Chrome Safe Storage", or the browser's own entry, and a yes there is expected. `FETCHLOOM_KEYRING=false` keeps fetchloom out of the credential store, so on linux only cookies under chromium's fixed key open. a cookie no key opens is left out by name, and when none of the host's opens the run fails with `auth.required` and says how to let fetchloom read the key. on windows the chromium browsers are not read: chrome seals cookies with app-bound encryption since version 127, which only chrome's own program may open, and the others' would need windows' DPAPI, which fetchloom does not call; use `--browser firefox` or `--cookies` there.

a key the host refuses with 401 or 403 ends the command with `auth.required`, exit 40, naming what was refused and the command that saves a new one, as `fetchloom auth login data.example.org --token`; saved cookies that expired are named so. a web page where a file was expected, once a key went, ends it the same way, since a site sends its sign-in page once a session ends; `--web-page`, or `web_page = true` in data.toml, says a page the link itself answers with is the data, and so is a page a redirect leads to on the same site, at a link that names no sign-in page; any other page a redirect leads to is refused, and the help names the link it is at.

## scripts and CI

```sh
echo "$HF_TOKEN" | fetchloom auth login hf --with-token
echo "$EARTHDATA_USER:$EARTHDATA_PASSWORD" | fetchloom auth login earthdata --with-token
```

`--with-token` reads the key from stdin and saves it without asking; a login reads `username:password`. typed or pasted at a terminal, it reads one line, hidden as an interactive sign-in keeps it, and enter ends it. a source's variable, such as `HF_TOKEN`, works without signing in at all. without a terminal and without `--with-token`, `auth login` fails with `usage` and names the scripted form.

## status

```sh
fetchloom auth
```

it lists each account with a key, where the key came from and, online, who the source says it belongs to. accounts without a key share one line:

```text
  Signed in to hf as ada · ~/.cache/huggingface/token
  Signed in to none of kaggle, zenodo, figshare, osf, dryad, github, gitlab, s2, earthdata, physionet, aws, google, azure
```

`aws`, `google` and `azure` are the keys [private buckets](../guides/private-cloud-storage.md#private-buckets-and-folders), containers and drive folders are read with. `auth` names them from the files and variables alone: it never runs a program, asks a server or shows a secret. each signs in with the cloud's own tool, which fetchloom reads, so `fetchloom auth login aws` says so with `usage`, exit 2, and the tool's command, as `aws signs in with the AWS CLI, and fetchloom reads its profile` with the help ``run `aws sso login` or `aws configure`, then list your private buckets in the s3_buckets setting``; google names `gcloud auth application-default login` and azure `az login`.

| row | what it names |
|---|---|
| `aws` | the keys in `AWS_ACCESS_KEY_ID`, or a profile's keys in `~/.aws/credentials` or `~/.aws/config`, each by the last four characters of its id; else the program that would give them, which runs only once a bucket needs keys: the profile's `credential_process`, or the AWS CLI for a profile that signs in to IAM identity center or takes on a role |
| `google` | the application default credentials file, as a service account with its email or a person's login |
| `azure` | the SAS in `AZURE_STORAGE_SAS_TOKEN` or the key in `AZURE_STORAGE_KEY`, by its variable; else the az cli, by its path and the user its saved login names, once `az login` signed it in; or this machine's managed identity, when `AZURE_TOKEN_CREDENTIALS` asks for it |

keys your AWS settings send to another endpoint are listed for it, as `s3 at https://minio.lab.example:9000`, and `aws` then has none. a SAS or a key kept to the account `AZURE_STORAGE_ACCOUNT` names is listed for that account, as `azure account lab`, and `azure` then names what other accounts' containers get. settings fetchloom cannot use, such as an `AWS_PROFILE` neither AWS file has, get a warning with what would fix them:

```text
  Signed in to aws with key …MPLE · profile default in ~/.aws/credentials
  Signed in to google as loader@lab-project.iam.gserviceaccount.com · ~/keys/lab.json
  Signed in to azure account lab with its key · AZURE_STORAGE_KEY
  Signed in to azure as ada@lab.example · the az cli at /usr/bin/az, which runs when a container needs a token
  Signed in to none of hf, kaggle, zenodo, figshare, osf, dryad, github, gitlab, s2, earthdata, physionet
```

a key the source refuses is marked `Refused`, named by where it comes from, with how to replace it: a saved key by signing in again, and a key in a variable, which wins over a saved one, by setting the variable to a current key or unsetting it:

```text
    Refused the key in HF_TOKEN for hf; set HF_TOKEN to a current key, or unset HF_TOKEN to use the key `fetchloom auth login hf` saves
```

a key whose source gives no answer, as a host this machine cannot reach, ends its line with `not checked, as <host> gave no answer`. `--offline` skips asking the sources. a key is never printed, in any output. a key that holds a character no request can carry, such as a line break, is never sent, and `auth` and the offer to sign in after a refusal warn about it, naming where the key is. on linux and macos, `auth` also warns when other users can open the `.netrc` it reads.

## signing out

`fetchloom auth logout <account>` removes the saved key. when the source's own tools read the same file, it asks first, since they are signed out too, and a no keeps the key and exits 1. `--yes` answers that question yes. where nobody can answer, as in a script, the key stays and the run exits 1 with `unconfirmed`, as any unanswered question does, so the file goes only with `--yes`. a key from an environment variable cannot be removed this way; unset the variable.

## JSON

`auth` returns `result.accounts`, each with `account`; `signed_in`, true when a key was found, even a refused one; `from` and `user`, or null; and `valid`, true or false after a source checked the key, else null. a website's saved credential is listed as `site:<host>`, with `kind` `token`, `header` or `cookies`, `from` where it is kept and, for cookies with an expiry, `expires`, when the soonest expires as an RFC 3339 time; nothing checks it, so `valid` is null. the rows `aws`, or `s3:<endpoint>` for keys your AWS settings send to another endpoint, `google`, and `azure`, or `azure:<account>` for a SAS or a key kept to one account, add `kind`: `keys`, `credential_process`, `sso` or `role` for AWS, `service_account` or `authorized_user` for google, whose `user` is a service account's email, and `sas`, `key`, `az_cli` or `managed_identity` for azure, whose `from` is the variable or the az cli's path and whose `user` is the az cli's user or a user-assigned identity's client id. nothing checks them, so their `valid` is null, and every other account's `kind` is null. `auth login` returns `account`, `user` and `saved_to`, and `auth logout` returns `account` and `removed_from`, null when nothing was saved or removed; a website's is `site:<host>`. keys never appear.
