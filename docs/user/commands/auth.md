# auth

`auth` signs you in to sources for private, restricted and gated data. public data needs no account.

```sh
fetchloom auth
fetchloom auth login hf
fetchloom auth logout zenodo
```

one key works everywhere. if you already signed in with a source's own tools, fetchloom uses that key and asks for nothing. if you sign in through fetchloom, the key is saved where those tools read it, so it works in them too.

## usage

```sh
fetchloom auth [options]
fetchloom auth login [options] <account>
fetchloom auth logout [options] <account>
```

`<account>` is `hf`, `kaggle`, `zenodo`, `figshare`, `osf`, `dataverse`, `earthdata`, `physionet` or a host name such as `data.example.edu`. `--host` names a dataverse installation; harvard's is the default.

## signing in

`fetchloom auth login hf` shows the page where you make a key, as a link you can click in most terminals, and what to pick there. paste the key; it stays hidden until you press enter, then only its first and last four characters and its length show so you can tell it arrived, or only stars for a key under 24 characters. fetchloom asks the source whose key it is, then asks before saving it:

```text
    Sign in hugging face
         1. open https://huggingface.co/settings/tokens
         2. create a token of the read type, or a fine-grained one that can read the repositories you need
        key hf_A...x9Qz · 37 characters
    Checked the key works for ada
save it to /home/ada/.cache/huggingface/token, where the hf command and the hugging face python libraries read it? [Y/n] yes
  Signed in to hugging face as ada · saved to /home/ada/.cache/huggingface/token
```

answering no keeps the key for that command only. a key the source refuses is never saved. zenodo and earthdata check a key without naming whose it is, and a login for any other host is not checked.

| account | where the key goes | also read by |
|---|---|---|
| hugging face | `$HF_TOKEN_PATH`, else `token` in `$HF_HOME`, else `huggingface/token` in `$XDG_CACHE_HOME`, else `~/.cache/huggingface/token` | the `hf` command, `datasets`, `transformers` |
| kaggle | `~/.kaggle/access_token` for a key starting `KGAT_`; a pasted `kaggle.json` is saved where the kaggle command reads it: `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME` | the `kaggle` command, `kagglehub` |
| nasa earthdata, physionet, any host | `.netrc` in your home folder, or the file `NETRC` names; `_netrc` on windows when it already exists | curl, wget, python's requests |
| zenodo, figshare, osf, dataverse | the system's credential store: windows credential manager, the macos keychain or the linux secret service | nothing else; these sources have no shared file |

a machine without a credential store, such as a server reached over ssh, asks before saving the key in `credentials.json` in fetchloom's settings folder, readable only by you: `%APPDATA%\fetchloom` on windows, `~/Library/Application Support/fetchloom` on macos, and `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom` on linux. answering no keeps it out of files; set the source's variable in your environment instead. `FETCHLOOM_KEYRING=0` skips the credential store.

each key is sent over https to its own host only, never after a redirect leaves that host. a login from `.netrc` is the exception curl makes too: earthdata's archives send you to `urs.earthdata.nasa.gov` and back, and the login answers there.

## scripts and CI

```sh
echo "$HF_TOKEN" | fetchloom auth login hf --with-token
echo "$EARTHDATA_USER:$EARTHDATA_PASSWORD" | fetchloom auth login earthdata --with-token
```

`--with-token` reads the key from stdin and saves it without asking. a login reads `username:password`. setting a source's variable, such as `HF_TOKEN`, works without signing in at all. without a terminal and without `--with-token`, `auth login` fails with `usage` and names the scripted form.

## when a download is refused

at a terminal, a command refused for want of an account offers to sign in, then runs once more:

```text
error: huggingface.co has no dataset org/private, or it is private
  help: check the name at https://huggingface.co/datasets/org/private. a private repository needs an account that can read it: sign in with `fetchloom auth login hf`
sign in to hugging face now? [Y/n]
```

a gated hugging face dataset, a restricted zenodo record or a kaggle competition also needs you to accept its terms on its page; fetchloom shows that link and asks whether to try again once you have. a key cannot accept terms for you. scripts and `--json` never get these questions: the error names the command to run.

## status

```sh
fetchloom auth
```

`fetchloom auth status` does the same. it lists each account with a key, where the key came from and, online, who the source says it belongs to. accounts without a key share one line:

```text
  Signed in hugging face as ada · /home/ada/.cache/huggingface/token
 Signed out kaggle, zenodo, figshare, osf, dataverse at dataverse.harvard.edu, nasa earthdata, physionet
```

a key the source refuses is marked `Refused` with the command to sign in again. `--offline` skips asking the sources. the key itself is never printed, in any output.

## signing out

`fetchloom auth logout <account>` removes the saved key. when the source's own tools read the same file, it asks first, since they are signed out too; where nobody can answer, as in a script, it removes the file only with `--yes`. a key from an environment variable cannot be removed this way; unset the variable.

## JSON

`auth` returns `result.accounts`, each with `account`, `signed_in`, `from`, `user` and `valid`. `auth login` returns `account`, `user` and `saved_to`. `auth logout` returns `account` and `removed_from`. secrets never appear.
