# style

the reader is a researcher who wants their data, or a contributor opening the code for the first time.

## prose

- use lowercase prose and headings. preserve acronyms, code, literal program output, personal names and legal names.
- write to one reader and call them "you".
- lead with the point or command. use short sentences, active voice and plain words.
- give each paragraph one job. use a table when entries share fields.
- every sentence adds a fact or an instruction. cut what repeats the heading, the previous paragraph or the obvious.
- describe current behavior. mark unbuilt work as pending and put its status in [plan](plan.md).
- replace a vague promise with an exact example, a number with its measurement conditions, or a link to the implementation.
- keep each fact in the doc that owns it. link from the others.
- links name their destination: [formats](formats.md), never "click here".

## what we cut

| cut | use instead |
|---|---|
| em dashes | a comma, colon or new sentence |
| "not X but Y" contrasts and one-line dramatic closers | the fact itself |
| lists of three by habit | as many entries as the facts need |
| delve, leverage, seamless, robust, crucial, pivotal, comprehensive, streamline, empower, utilize, showcase, foster, underscore, testament, tapestry, vibrant, powerful | a concrete verb or noun |
| "serves as", "stands as", "acts as" | "is" |
| simply, just, easily, obviously | the instruction without judging its difficulty |
| "this document describes", "in this section", "as mentioned above", "in conclusion" | the information |
| bold labels, decorative headings, emoji and exclamation marks | plain text |
| "it's worth noting", "arguably", "might potentially" | the supported claim, with one qualification if needed |

## examples and claims

copy program output from a real run or a matching snapshot. do not invent verbs, spacing, prompts, hashes or error text. keep placeholders outside transcripts, or label them as placeholders. a snapshot's `[time]` or `[port]` is a test filter, not text the program prints.

show the command and only the output needed to teach its behavior. say when an excerpt is stderr, when a result depends on cached data, and when a method depends on the filesystem. do not promise zero requests when only file-body bytes were avoided.

examples of `data.toml`, `data.lock` and JSON must use accepted keys and values. show complete hashes in valid file examples. do not silently turn a known limitation into a guarantee.

## terminal output

keep step and completion verbs right-aligned in 11 columns, with no status glyphs. use a clear completion verb such as `Added`, `Fetched`, `Synced` or `Ok`. completed work is bold green, ongoing work cyan, attention yellow and failures red. details after `·` are dim.

errors use `error:`, indented details and an optional `help:` line. warnings use `warning:`; hints stay dim, with commands in bold cyan. keep backticks in plain text. headings in welcome and help are bold and commands cyan; preserve the kitchen analogy.

progress keeps the filename when space is tight. colored bars use `━`, with the remainder dimmed; uncolored terminal bars use `#` and `-`. pipes have no bars. never rely on color to convey meaning. see [output and errors](cli.md#output-and-errors) for color controls and stream behavior.

## code comments

a comment explains a reason, an outside constraint or a trap the code cannot express. if it repeats the code, rename the item or remove the comment.

- use lowercase sentences with normal punctuation.
- keep comments to one or two sentences where possible.
- no banners, decorative symbols or labels such as `NOTE:`.
- no commented-out code or `TODO` notes. track unfinished work in an issue.

```rust
// a second ctrl-c means now
```

this explains why the second signal bypasses cleanup. a comment saying "increment the retry counter" adds nothing beside an increment.

## docstrings and names

public items need a short description from the caller's side. add `# Errors` for non-obvious failure cases. rust doc examples can run through `cargo test --doc`; use them when a call teaches more than a signature.

use whole words for names. `url`, `id`, `len` and `io` are familiar exceptions. booleans read as statements, such as `is_offline`. name a module for what it owns, not `utils`, `helpers`, `manager` or `common`.

## commits and pull requests

commit messages are one line, lowercase and imperative, fewer than 60 characters, with no closing period. say what changed: `retry downloads that stall mid-file`.

no prefixes, emoji, trailers, co-author lines or mention of assistants or tools. keep one logical change per commit. check the behavior affected by that change before committing.

a pull request title says what changed. its body explains what and why in two or three plain sentences. include measurements when the change claims a performance improvement. do not add stock checklists or footers.

## checks in place

cargo configures rust and clippy lints, including missing public documentation warnings. one CLI test compares the scope doc's option sentence with command help. snapshots check the program's output separately.

typos, vale and comment-prose checks are not configured. markdown transcripts are not run as tests. review their wording and examples against the code; [engineering](engineering.md#checks) lists the checks available now.
