# style

the reader is a researcher who wants their data, or a contributor opening the code for the first time.

## prose

- lowercase, headings too. capitals only for acronyms (DOI, URL, HPC, JSON), single letters like R, and code.
- write to one reader and call them "you".
- lead with the point or the command, then explain only what the reader needs.
- short sentences, active voice, plain words: "use" over "utilize", "lets you" over "enables you to".
- one idea per paragraph. use a table when several things share the same fields.
- every sentence adds something new. cut what repeats the heading, the previous paragraph or the obvious.
- no meta writing: never "this document describes", "in this section", "as mentioned above" or "in conclusion".
- no hype. a claim carries a number, a command or a link.
- links name where they go: "see [formats](formats.md)", never "click here".

## what we cut

| cut | why |
|---|---|
| em dashes | a known machine-writing habit. use a comma, a colon or a new sentence |
| "not X but Y" and "it's not just X, it's Y" | they stage a contrast instead of stating the fact |
| lists of three by habit | use as many items as the facts have |
| delve, leverage, seamless, robust, crucial, pivotal, comprehensive, streamline, empower, utilize, showcase, foster, underscore, testament, tapestry, vibrant | inflated words that carry no information |
| "serves as", "stands as", "acts as" | say "is" |
| simply, just, easily, obviously | they blame the reader when it isn't simple |
| bold labels at the start of list items, bold sprinkled through text | formatting by rule |
| emoji, exclamation marks, "let's", "happy coding" | noise |
| "it's worth noting", "arguably", "might potentially" | say it plainly or cut it |

## code comments

a comment says what the code cannot: why it is written this way, a rule that comes from outside (a server quirk, an OS behavior, a spec), or a trap waiting for the next reader. when a comment would only repeat the code, rename something instead.

- lowercase sentences with normal punctuation, one or two of them, written the way you would explain it to a colleague.
- no banners, no ASCII art, no decorative symbols, no labels like `NOTE:` or `IMPORTANT:`.
- no commented-out code and no `TODO`. open an issue instead.

```rust
// windows refuses to rename over a read-only file, so placing an object is
// one rename that fails when the name exists, never a replace.
```

```rust
// NFS leaves a .nfs file beside any file deleted while still open.
// it belongs to the NFS client, so gc walks past it.
```

these say nothing the code doesn't, so they don't exist:

```rust
// increment the retry counter
retries += 1;
```

## docstrings

- every public item gets one line saying what it is or does, from the caller's side: `/// path of the cached object for this digest.`
- add `# Errors` when a function returns `Result` and the failure cases are not obvious from its name.
- an example belongs only where it shows why you would call the item. examples run as tests.
- python follows the same rules: one imperative line, then `Args:` and `Returns:` only when they add something.

```python
def path(name: str) -> Path:
    """return the local path of a dataset, syncing it first if needed."""
```

## names

- whole words: `download`, `object`, `location`. common short forms are fine: `url`, `id`, `len`, `io`.
- booleans read as statements: `is_offline`, `has_checksum`.
- no `utils`, `helpers`, `manager` or `common` modules. a module is named for what it owns.

## commits

- one line, lowercase, imperative, at most 60 characters, no closing period.
- say what changed for someone reading `git log`: `add resumable downloads`, `refuse archives with escaping links`.
- no prefixes like `feat:`, no emoji, no trailers, no co-author lines, no mention of tools or assistants.
- one logical change per commit, and the tree builds and passes tests after each one.

## pull requests

the title says what landed. the body is two or three plain sentences on what changed and why. a milestone's pull request also carries its plan and its measured numbers, as [plan](plan.md) describes. no checklists, no footers.

## how it is enforced

- `typos` checks spelling in code and docs.
- a `vale` style holds the cut list and runs on every markdown file.
- a ripgrep check rejects em dashes and the cut words in comments and docstrings.
- `clippy` requires docs on public items.
- examples in docs and docstrings run as tests, so they cannot go stale.

## before and after

| before | after |
|---|---|
| fetchloom is a powerful, seamless tool that leverages rust to streamline research data management | fetchloom puts the exact data a project needs onto any machine with one command |
| Error: Failed to process the request due to an unexpected issue. | could not reach zenodo.org after 5 tries. check your connection, or run `fetchloom sync --offline` to use what is cached |
| `Updated download logic to improve robustness` | `retry downloads that stall mid-file` |
