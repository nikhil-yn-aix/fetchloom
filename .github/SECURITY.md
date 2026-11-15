# security

fetchloom reads cloud keys and tokens and writes what it downloads into your projects, so please report a weakness privately, before it is public.

## how to report

use github's private vulnerability reporting: [report a vulnerability](https://github.com/nikhil-yn-aix/fetchloom/security/advisories/new) opens an advisory only the maintainer can read. include what an attacker can do and what they need for it, the output of `fetchloom --version -v` and your system, and the smallest steps that show it.

please don't open a public issue, discussion or pull request about it until a fix is out.

## what is in scope

- the `fetchloom` command and its python helper, including how they handle untrusted input: a `data.toml` in a repository you clone, a server's answer, an archive or a cookie database
- how they keep keys and tokens, and what they send where. [what leaves your machine](https://nikhil-yn-aix.github.io/fetchloom/user/concepts/privacy/#what-leaves-your-machine) lists it
- the release files, installers, debian packages and apt repository, and the workflows that build them

out of scope: flaws in the services fetchloom talks to, such as zenodo or hugging face (tell them instead), and attacks that need someone who already controls your account or machine.

## how fast you hear back

you get a first answer within seven days. a confirmed weakness gets a fix and a published advisory.

## supported versions

| version | security fixes |
|---|---|
| the latest release | yes |
| older releases | no; upgrade to the latest |

no release has been published yet; until the first one, fixes go to the main branch.
