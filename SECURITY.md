# security

fetchloom reads cloud keys and tokens and writes what it downloads into your projects, so report a weakness privately, before it is public.

## report a vulnerability

use github's private vulnerability reporting: [report a vulnerability](https://github.com/nikhil-yn-aix/fetchloom/security/advisories/new) opens an advisory only the maintainers can read. please include:

- what an attacker can do, and what they need for it, such as a data.toml in a repository you clone, a server's answer or an archive
- the output of `fetchloom --version -v` and your system
- the smallest steps that show it

please do not open a public issue, discussion or pull request about it until a fix is out. [what leaves your machine](https://nikhil-yn-aix.github.io/fetchloom/user/concepts/privacy/#what-leaves-your-machine) says what fetchloom sends and to whom.

## supported versions

| version | security fixes |
|---|---|
| the latest release | yes |
| older releases | no; upgrade to the latest |

no release has been published yet; until the first one, fixes go to the main branch.
