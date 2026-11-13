# auth

`auth` signs you in to sources that need an account, such as for restricted data or kaggle competitions. [signing in](../signing-in.md) says which do, and how private buckets get their keys.

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

`<account>` is `hf`, `kaggle`, `zenodo`, `figshare`, `osf`, `dryad`, `github`, `gitlab`, `s2`, `dataverse`, `earthdata`, `physionet` or a plain host name such as `data.example.edu`, never a link. `--host` names a dataverse installation, with its port when that is not 443, as `--host dv.example.edu:8443`; harvard's is the default. a host name, there and as `<account>`, holds only letters, digits, dots, hyphens and underscores; `--host` also takes an IPv6 address in brackets. an argument that is refused is shown with any secret in it masked.

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

it asks before replacing a saved key; `--yes` answers both questions yes, and a no to either changes nothing and exits 1, with `Cancelled signing in to hugging face, so nothing changed`. after [a refused download](#when-a-download-is-refused), a key you do not save serves only that command's next try. a key the source refuses is never saved. zenodo, github, s2 and earthdata check a key without naming whose it is, and a login for any other host is not checked. a source that gives no answer, as a host this machine cannot reach, leaves a key unchecked: it is saved all the same, and the last line says `Saved a key for <source> to <file>, not checked` instead of `Signed in`, with a hint that `fetchloom auth` checks it once the host answers.

| account | where the key goes | also read by |
|---|---|---|
| hugging face | `$HF_TOKEN_PATH`, else `token` in `$HF_HOME`, else `huggingface/token` in `$XDG_CACHE_HOME`, else `~/.cache/huggingface/token` | the `hf` command, `datasets`, `transformers` |
| kaggle | `~/.kaggle/access_token` for a key starting `KGAT_`; a pasted `kaggle.json` is saved where the kaggle command reads it: `KAGGLE_CONFIG_DIR`, else `~/.kaggle`, else on linux `kaggle` in `XDG_CONFIG_HOME` | the `kaggle` command, `kagglehub` |
| nasa earthdata, physionet, any host | `.netrc` in your home folder, or the file `NETRC` names; `_netrc` on windows when it already exists | curl, wget, python's requests |
| zenodo, figshare, osf, dryad, github, gitlab, s2, dataverse | the system's credential store: windows credential manager, the macos keychain or the linux secret service | nothing else; these sources have no shared file |

a machine without a credential store, such as a server reached over ssh, asks before saving the key in `credentials.json` in fetchloom's settings folder, readable by you only on linux and macos: `%APPDATA%\fetchloom` on windows, `~/Library/Application Support/fetchloom` on macos, and `$XDG_CONFIG_HOME/fetchloom` or `~/.config/fetchloom` on linux. answering no keeps it out of files; set the source's variable in your environment instead. `FETCHLOOM_KEYRING=false`, or `0`, skips the credential store.

[where a key goes](../signing-in.md#where-a-key-goes) lists the hosts each key is sent to.

## scripts and CI

```sh
echo "$HF_TOKEN" | fetchloom auth login hf --with-token
echo "$EARTHDATA_USER:$EARTHDATA_PASSWORD" | fetchloom auth login earthdata --with-token
```

`--with-token` reads the key from stdin and saves it without asking; a login reads `username:password`. typed or pasted at a terminal, it reads one line, hidden as an interactive sign-in keeps it, and enter ends it. a source's variable, such as `HF_TOKEN`, works without signing in at all. without a terminal and without `--with-token`, `auth login` fails with `usage` and names the scripted form.

## when a download is refused

at a terminal, a command refused for want of an account offers to sign in, then runs once more. the offer comes before any `error:`, its first line naming what was refused, and defaults to no: enter alone starts no sign-in, and a no ends the run with one `Cancelled` line and the refusal's exit code, 40. hugging face and kaggle answer a name that does not exist as they answer a private one, so for those the offer first says to check the name, and where:

```text
    Refused huggingface.co has no dataset acme-lab/field-notes, or it is private
      Check the name first at https://huggingface.co/datasets/acme-lab/field-notes
            hugging face answers alike for a name that does not exist and for a private one
sign in to hugging face now? [y/N] n
  Cancelled signing in to hugging face
```

a command that is refused again once you signed in ends with that refusal as an error, and so does any refusal while a key for the source is on this machine: signing in again is no remedy, so its help says the key was refused or has no access, and that `fetchloom auth` shows where the key comes from and whose it is; only terms to accept are still offered. a server that asks for a login over plain http gets no offer at all, since fetchloom sends a key only over https. `-q` prints the error above the question instead of the `Refused` line.

`add` first asks the source, once, for the public datasets of the same owner, signed in or not. when one is a typo away, signing in could not help, so no sign-in is offered, the error is `reference.unresolved`, exit 10, rather than `auth.required`, and the help gives your command with the name fixed; signed in, the error above it reads `hf:datasets/scikit-learn/irsi does not exist, or is not public`. a source that is busy, or whose rate limit is used up, gets no second try, so the error comes at once with no suggestion. an offline run asks nothing more, so it keeps `auth.required`, exit 40. with `--json` the suggestion is in `help` and `fix` stays null, since a guess is never a command to run:

```text
error: huggingface.co has no dataset scikit-learn/irsi, or it is private
  help: did you mean `fetchloom add hf:datasets/scikit-learn/iris`?
```

a gated hugging face dataset, a restricted zenodo record or a kaggle competition also needs you to accept its terms on its page. without a key on this machine, the offer says that first, as `First accept the terms at https://huggingface.co/datasets/bigcode/the-stack while signed in there`, then asks whether to sign in; with a key, it shows that link and asks whether to try again once you have. a key cannot accept terms for you. scripts and `--json` never get these questions: the error names the command to run.

a key that went with the request and was refused, as an old or wrong `HF_TOKEN` is, is named by where it comes from, never by its value, with how to replace it:

```text
error: huggingface.co refused the key in HF_TOKEN for hf:datasets/bigcode/the-stack
  huggingface.co says: Access to dataset bigcode/the-stack is restricted. You must have access to it and be authenticated to access it. Please log in.
  help: the key may be old or wrong: set HF_TOKEN to a current key from https://huggingface.co/settings/tokens, or unset HF_TOKEN to use the key `fetchloom auth login hf` saves
```

a key saved by signing in is named by its file, below your home folder as `~/…`, and the help says that `fetchloom auth` shows where it comes from and whose it is. no sign-in is offered for a key from a variable, which would still win over the key a sign-in saves.

## status

```sh
fetchloom auth
```

it lists each account with a key, where the key came from and, online, who the source says it belongs to. accounts without a key share one line:

```text
  Signed in to hf as ada · ~/.cache/huggingface/token
  Signed in to none of kaggle, zenodo, figshare, osf, dryad, github, gitlab, s2, earthdata, physionet, aws, google, azure
```

`aws`, `google` and `azure` are the keys [private buckets](../signing-in.md#private-buckets-and-folders), containers and drive folders are read with. `auth` names them from the files and variables alone: it never runs a program, asks a server or shows a secret.

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

a key the source refuses is marked `Refused` with the command to sign in again, and a key whose source gives no answer, as a host this machine cannot reach, ends its line with `not checked, as <host> gave no answer`. `--offline` skips asking the sources. a key is never printed, in any output. a key that holds a character no request can carry, such as a line break, is never sent, and `auth` and the offer to sign in after a refusal warn about it, naming where the key is. on linux and macos, `auth` also warns when other users can open the `.netrc` it reads.

## signing out

`fetchloom auth logout <account>` removes the saved key. when the source's own tools read the same file, it asks first, since they are signed out too, and a no keeps the key and exits 1. `--yes` answers that question yes. where nobody can answer, as in a script, the key stays and the run exits 1 with `unconfirmed`, as any unanswered question does, so the file goes only with `--yes`. a key from an environment variable cannot be removed this way; unset the variable.

## JSON

`auth` returns `result.accounts`, each with `account`; `signed_in`, true when a key was found, even a refused one; `from` and `user`, or null; and `valid`, true or false after a source checked the key, else null. the rows `aws`, or `s3:<endpoint>` for keys your AWS settings send to another endpoint, `google`, and `azure`, or `azure:<account>` for a SAS or a key kept to one account, add `kind`: `keys`, `credential_process`, `sso` or `role` for AWS, `service_account` or `authorized_user` for google, whose `user` is a service account's email, and `sas`, `key`, `az_cli` or `managed_identity` for azure, whose `from` is the variable or the az cli's path and whose `user` is the az cli's user or a user-assigned identity's client id. nothing checks them, so their `valid` is null, and every other account's `kind` is null. `auth login` returns `account`, `user` and `saved_to`, and `auth logout` returns `account` and `removed_from`, null when nothing was saved or removed. keys never appear.
