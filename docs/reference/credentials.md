# Credentials

Fetchloom never asks for a token at startup and has no setup step. It asks at
the moment a token changes the outcome, and it says what the change is.

## Where a token goes

Three places are looked in, in this order, for the host a reference names. The
first match wins, and the run says which one it used without printing the secret.

| Order | Where | How to put one there |
|---|---|---|
| 1 | An environment variable named `FETCHLOOM_TOKEN_<HOST>`, or the `FETCHLOOM_ACCESS_KEY_<HOST>` set | Set it in your shell |
| 2 | This platform's own credential store | See below |
| 3 | The provider's own helper | For a source that signs, the `AWS_` variables you already have |

`<HOST>` is the endpoint's host in capital letters, with every character that is
not a letter or a digit replaced by an underscore. For `storage.googleapis.com`
that is `FETCHLOOM_TOKEN_STORAGE_GOOGLEAPIS_COM`. For `minio.example.org:9000`
it is `FETCHLOOM_TOKEN_MINIO_EXAMPLE_ORG_9000`.

**The credential store on Windows** is the Windows Credential Manager. Open
Windows PowerShell and type this, replacing the host and the token:

```
cmdkey /generic:fetchloom:storage.googleapis.com /user:fetchloom /pass:THE-TOKEN
```

**The credential store on Linux** is a file beside your configuration, at
`$XDG_CONFIG_HOME/fetchloom/credentials` or `~/.config/fetchloom/credentials`.
One host per line:

```
storage.googleapis.com = "THE-TOKEN"
```

Then restrict it, because Fetchloom refuses to read a credential file any other
user on the machine can read, and names the mode it found:

```
chmod 600 ~/.config/fetchloom/credentials
```

The Secret Service is not used. It is a desktop daemon reached over a session
bus, and Fetchloom's Linux users are on clusters, in containers and in
continuous integration, where there is no session bus and no daemon.

A credential is bound to the host it was resolved for. It is dropped on any
redirect to a different host, and the drop is reported.

## When Fetchloom asks

A run tries without a credential first. Only when a source refuses for want of
authorization does the run stop, with exit code 40 and
`policy.credential_missing`, and print the provider's numbered steps. A run that
cannot prompt never blocks: it fails immediately with those steps on stderr.

An expired, revoked or too narrowly scoped credential fails
`policy.credential_invalid`, naming the provider and how to renew or widen it. A
bare authorization failure is never shown on its own.

## What is never written down

Never, in any log, event, receipt, plan or error message: bearer tokens, API
keys, passwords, the `Authorization` and `Cookie` headers, the userinfo
component of a URL, and the value of every query parameter. Query values are
redacted whatever the parameter is called, because a list of known-sensitive
names is a thing that can be incomplete. The replacement is the fixed text
`[redacted]`.

That means a presigned link is safe to pass on the command line. Its signature
never reaches a file Fetchloom writes.

## The providers

Each record below is fixed. Fetchloom never improvises the wording.

### Google Cloud Storage

**Unlocks.** Reading a bucket that is not open to everyone. Without a token,
only buckets whose contents are public can be fetched.

**Steps.**

1. Install the Google Cloud CLI from https://cloud.google.com/sdk/docs/install
   by following the instructions there for your operating system. It is a
   program called `gcloud` that you run in a terminal.
2. Open a terminal. On Windows that is the program called Windows PowerShell or
   Terminal. On Linux it is the program called Terminal.
3. Type `gcloud auth login` and press Enter. A web browser opens. Sign in with
   the Google account that can read the bucket, and choose Allow.
4. Back in the terminal, type `gcloud auth print-access-token` and press Enter.
   It prints one long line. That line is the token. Select it and copy it.
5. Put it in `FETCHLOOM_TOKEN_STORAGE_GOOGLEAPIS_COM` or in the credential
   store, as above.

**Verification.** `fetchloom plan https://storage.googleapis.com/<bucket>/`
reads the listing and moves no bytes.

**Scope.** Read access to the one bucket, granted by the role Storage Object
Viewer on that bucket. Not a project-wide role.

An access token from `gcloud` expires, usually within an hour. Repeat step 4 and
replace the value when it does.

### Azure Blob Storage

**Unlocks.** Reading a container that is not open to everyone.

**Steps.**

1. Install the Azure CLI from
   https://learn.microsoft.com/cli/azure/install-azure-cli by following the
   instructions there for your operating system. It is a program called `az`
   that you run in a terminal.
2. Open a terminal. On Windows that is Windows PowerShell or Terminal. On Linux
   it is Terminal.
3. Type `az login` and press Enter. A web browser opens. Sign in with the
   account that can read the container.
4. Type
   `az account get-access-token --resource https://storage.azure.com/ --query accessToken --output tsv`
   and press Enter. It prints one long line. That is the token.
5. Put it in `FETCHLOOM_TOKEN_<ACCOUNT>_BLOB_CORE_WINDOWS_NET` or in the
   credential store under `fetchloom:<account>.blob.core.windows.net`.

**Verification.**
`fetchloom plan https://<account>.blob.core.windows.net/<container>/`

**Scope.** The role Storage Blob Data Reader, assigned on the one container
rather than on the storage account or the subscription.

An access token from `az` expires, usually within an hour.

### Amazon S3

**Unlocks.** Nothing this build can use. Amazon S3 authenticates a request by
signing it with an access key and a secret key, which is not a token that can be
sent, and this build sends a token. A private Amazon S3 bucket cannot be fetched
by name here.

**Steps.**

1. If the bucket is public, no token is needed. Fetch it by its endpoint, for
   example `https://s3.amazonaws.com/<bucket>/<prefix>/`.
2. If the bucket is private, ask whoever owns it for a presigned link to each
   object you need. A presigned link is an ordinary web address that already
   carries its own authorization and expires after a set time.
3. Fetch each presigned link directly. Quote the address, because it contains
   characters a terminal would otherwise read as instructions.

**Scope.** For a presigned link, one object, for the shortest time that covers
the transfer.

Signed request support is not in this build. It is the second credential shape
Fetchloom will carry.

### Any other object storage endpoint

**Unlocks.** Reading a bucket or prefix that is not open to everyone.

**Steps.**

1. Ask whoever runs the storage endpoint for a bearer token that can read the
   bucket you want. A bearer token is one line of letters and numbers that a
   program sends with every request to prove it is allowed to read.
2. If they offer an access key and a secret key instead, this build cannot use
   them. Ask for a presigned link to each object you need instead.
3. Put the token in `FETCHLOOM_TOKEN_<HOST>` or in the credential store, as
   above.

**Verification.** `fetchloom plan https://<endpoint>/<bucket>/`

**Scope.** Read access to the one bucket or prefix you name, and no write access
of any kind.

## Terms

A manifest recording `requires_acceptance` refuses to transfer until you assert
acceptance, with `--yes` or by answering the confirmation. The assertion is
recorded in the receipt. Fetchloom makes no legal determination about what the
terms mean.

A run that cannot prompt and was not given `--yes` fails with
`policy.terms_required` and exit code 40, before a byte moves.

## Two shapes of credential

A credential is one of exactly two things, and which one a host needs is decided
by the adapter that serves it rather than by you.

A **bearer token** is one opaque value that is sent with the request. Google
Cloud Storage, Azure Blob Storage and most lab endpoints take one.

A **signing key pair** is an access key, a secret key and a region. The secret is
never sent: it derives a key that signs the request, and only the signature
travels. Amazon S3 and the endpoints that speak its protocol need this.

For a signing credential the host-scoped variables are:

```
FETCHLOOM_ACCESS_KEY_<HOST>
FETCHLOOM_SECRET_KEY_<HOST>
FETCHLOOM_REGION_<HOST>
FETCHLOOM_SESSION_TOKEN_<HOST>    only for a temporary credential
```

If you already have a working AWS setup, you do not need to restate it. The
third tier reads `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION` and
`AWS_SESSION_TOKEN`, and is asked only after the two tiers above answered
nothing. A per-host Fetchloom variable therefore always wins.

An access key set without a region fails and says so, naming the variable to
set. It is not guessed: a signature is computed over a region, and a wrong one
is refused by the source with an error you cannot act on.
