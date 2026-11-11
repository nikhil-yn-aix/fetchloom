# auth

`auth` signs you in to sources that require an account, including restricted data and kaggle competitions.

```sh
fetchloom auth
fetchloom auth login hf
fetchloom auth logout zenodo
```

fetchloom reuses supported credentials saved by a source's tools. signing in with fetchloom shares credential files for hugging face, kaggle and `.netrc` logins. other source keys use fetchloom's credential store, as the table below shows.

## usage

```sh
fetchloom auth [options]
fetchloom auth login [options] <account>
fetchloom auth logout [options] <account>
```

`<account>` is `hf`, `kaggle`, `zenodo`, `figshare`, `osf`, `dataverse`, `earthdata`, `physionet` or a host name such as `data.example.edu`. `--host` names a dataverse installation; harvard's is the default. it takes a host name, with its port when that is not 443, as `--host dv.example.edu:8443`, and refuses a login, path or query.

an account name must be a source name or a plain host name, not a URL, query or fragment. rejected arguments use the same masking as URL output, including percent-encoded URLs and syntax the URL parser would normalize. `--json` and `-vv` keep that masking. malformed hosts in the account index or `.netrc` are ignored.

## signing in

`fetchloom auth login hf` shows the steps to a key: make a free account if you have none, open the page where you make a key, as a link you can click in most terminals, and what to pick there. hugging face and kaggle add the step a gated dataset or a competition needs: accept its terms or rules on its own page, with the same account. paste the key; it stays hidden until you press enter, then only its first and last four characters and its length show so you can tell it arrived, or only stars for a key under 24 characters. fetchloom asks the source whose key it is, then asks before saving it. stderr excerpt from a loopback check with a synthetic key and the home path shortened:

```text
    Sign in hugging face
         1. make a free account at huggingface.co if you have none, and sign in
         2. open https://huggingface.co/settings/tokens
         3. create a token of the read type, or a fine-grained one that can read the repositories you need
         4. a gated dataset also needs its terms accepted on its own page, with the same account
        key hf_A...x9Qz · 37 characters
    Checked the key works for ada
save it to /home/ada/.cache/huggingface/token? [Y/n] yes
  Signed in to hugging face as ada · saved to /home/ada/.cache/huggingface/token
  hint: the hf command and the hugging face python libraries use this key too
```

when a key is already saved, it asks before replacing it; `--yes` answers both questions yes. a no to either question changes nothing: `auth login` ends with `Cancelled signing in to hugging face, so nothing changed` and exit 1. after [a refused download](#when-a-download-is-refused), a key you do not save serves only that command's next try. a key the source refuses is never saved. zenodo and earthdata check a key without naming whose it is, and a login for any other host is not checked.

| account | where the key goes | also read by |
|---|---|---|
| hugging face | `$HF_TOKEN_PATH`, else `token` in `$HF_HOME`, else `huggingface/token` in `$XDG_CACHE_HOME`, else `~/.cache/huggingface/token` | the `hf` command, `datasets`, `transformers` |
| kaggle | `~/.kaggle/access_token` for a key starting `KGAT_`; a pasted `kaggle.json` is saved where the kaggle command reads it: `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME` | the `kaggle` command, `kagglehub` |
| nasa earthdata, physionet, any host | `.netrc` in your home folder, or the file `NETRC` names; `_netrc` on windows when it already exists | curl, wget, python's requests |
| zenodo, figshare, osf, dataverse | the system's credential store: windows credential manager, the macos keychain or the linux secret service | nothing else; these sources have no shared file |

a machine without a credential store, such as a server reached over ssh, asks before saving the key in `credentials.json` in fetchloom's settings folder: `%APPDATA%\fetchloom` on windows, `~/Library/Application Support/fetchloom` on macos, and `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom` on linux. credential files start with mode `0600` on unix. on windows they inherit the folder's permissions, so keep any custom credential folder private. answering no keeps it out of files; set the source's variable in your environment instead. `FETCHLOOM_KEYRING=0` skips the credential store.

each key is sent over https to its own host only, on port 443 unless it was saved for another port, never after a redirect leaves that host. a login from `.netrc` is the exception curl makes too: earthdata's archives send you to `urs.earthdata.nasa.gov` and back, and the login answers there. an osf key also goes to `osf.io` and `files.osf.io` on any hop, since osf's download links start on the one and send you to the other.

## scripts and CI

```sh
echo "$HF_TOKEN" | fetchloom auth login hf --with-token
echo "$EARTHDATA_USER:$EARTHDATA_PASSWORD" | fetchloom auth login earthdata --with-token
```

`--with-token` reads the key from stdin and saves it without asking. a login reads `username:password`. setting a source's variable, such as `HF_TOKEN`, works without signing in at all. without a terminal and without `--with-token`, `auth login` fails with `usage` and names the scripted form.

## when a download is refused

at a terminal, a command refused for want of an account offers to sign in, then runs once more. the question defaults to no, so enter alone starts no sign-in. hugging face and kaggle answer a name that does not exist as they answer a private one, so for those refusals the offer first says to check the name:

```text
error: huggingface.co has no dataset scikit-learn/irs, or it is private
  help: check the name at https://huggingface.co/datasets/scikit-learn/irs. a private repository needs an account that can read it: sign in with `fetchloom auth login hf`
      Check the name first: hugging face answers alike for a name that does not exist and for a private one
sign in to hugging face now? [y/N]
```

a gated hugging face dataset, a restricted zenodo record or a kaggle competition also needs you to accept its terms on its page. without a key on this machine, the offer says that first, as `First accept the terms at https://huggingface.co/datasets/bigcode/the-stack while signed in there`, then asks whether to sign in; with a key, it shows that link and asks whether to try again once you have. a key cannot accept terms for you. scripts and `--json` never get these questions: the error names the command to run.

a key that went with the request and was refused, as an old or wrong `HF_TOKEN` is, is named by where it comes from, never by its value, with how to replace it:

```text
error: huggingface.co refused the key in HF_TOKEN for hf:datasets/bigcode/the-stack
  huggingface.co says: Access to dataset bigcode/the-stack is restricted. You must have access to it and be authenticated to access it. Please log in.
  help: the key may be old or wrong: set HF_TOKEN to a current key from https://huggingface.co/settings/tokens, or unset HF_TOKEN to use the key `fetchloom auth login hf` saves
```

a key saved by signing in is named by its file, and the help says to sign in again. no sign-in is offered for a key from a variable, which would still win over the key a sign-in saves.

## status

```sh
fetchloom auth
```

it lists each account with a key, where the key came from and, online, who the source says it belongs to. accounts without a key share one line:

```text
  Signed in hugging face as ada · /home/ada/.cache/huggingface/token
 Signed out kaggle, zenodo, figshare, osf, nasa earthdata, physionet
```

a key the source refuses is marked `Refused` with the command to sign in again. `--offline` skips asking the sources. the key itself is never printed, in any output.

a key that holds a character no request header can carry, such as a line break, is never sent. `auth` and the offer to sign in after a refusal warn about it, naming where the key is but not the key. on linux and macos, `auth` also warns when other users can open the `.netrc` fetchloom reads, the file python's netrc module refuses to read then.

## signing out

`fetchloom auth logout <account>` removes the saved key. when the source's own tools read the same file, it asks first, since they are signed out too, and a no keeps the key and exits 1. `--yes` answers that question yes, and where nobody can answer, as in a script, the file goes only with `--yes`. a key from an environment variable cannot be removed this way; unset the variable.

## JSON

`auth` returns `result.accounts`. `account` is a string id; `signed_in` is true when a key was found, even if it is refused. `from` and `user` are strings or null. `valid` is true or false after a source check, or null when no check was made, including offline or for an unsendable key.

`auth login` returns `account`, nullable `user` and nullable `saved_to`. `auth logout` returns `account` and nullable `removed_from`. a null destination means no key was saved or removed. secrets never appear.
