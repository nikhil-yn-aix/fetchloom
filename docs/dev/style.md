# style

prose, comments and commits use plain language and claims a reader can check.

## prose

write for a researcher retrieving data or a contributor opening the code for the first time. explain why a constraint exists before giving its details; define an unfamiliar term when you first use it.

- use lowercase prose and headings; preserve acronyms, code, literal output, personal names and legal names.
- give each paragraph one job. use short sentences, active voice and tables when entries share fields.
- open each page with one plain sentence about its subject. cut announcements about the document itself.
- describe current behavior. keep pending code and publishing work in [roadmap](roadmap.md).
- keep facts with their owning component and link from other pages. link text names its destination.
- attach conditions to measurements and link claims to source, tests or recorded results.

| cut | use |
|---|---|
| em dashes, dramatic fragments and repeated closing lines | a sentence that adds a fact |
| forced contrasts and lists of three by habit | the actual distinction or number of items |
| delve, leverage, seamless, robust, crucial, pivotal, comprehensive, streamline, empower, utilize, showcase, foster, underscore, testament, tapestry, vibrant, powerful | a concrete verb or noun |
| "serves as", "stands as", "acts as" | "is" |
| simply, just, easily, obviously | the instruction without judging its difficulty |
| "this document describes", "in this section", "in conclusion" | the subject itself |
| bold labels, decorative headings, emoji and exclamation marks | plain text |
| filler, speculative qualifiers, placeholders and unfinished notes | supported detail or an explicit limit of the evidence |

## examples and diagrams

copy output from a real run or matching snapshot. do not invent verbs, spacing, hashes or errors. a snapshot's `[time]` or `[port]` is a test filter, not literal output. say whether an excerpt is stderr and whether the result depends on cached data or a filesystem capability. avoiding file-body downloads does not necessarily mean zero requests.

configuration and JSON examples must use accepted fields and values; valid file examples need complete hashes. keep mermaid diagrams small, usually fewer than 15 nodes, and quote labels with special characters. arrows should express a checked call, dependency or data flow.

## terminal output

verbs are right-aligned in 11 columns without status glyphs. completed work is bold green, ongoing work cyan, attention yellow and failures red; details after `·` are dim. completion verbs include `Added`, `Fetched`, `Synced` and `Ok`.

errors use `error:`, indented details and optional `help:`. warnings use `warning:`. hints are dim with commands in bold cyan; plain output keeps backticks. welcome and help headings are bold and commands cyan; preserve the kitchen analogy.

progress keeps the filename when space is tight. colored bars use `━` with a dim remainder; uncolored terminal bars use `#` and `-`. pipes have no bars. never rely on color for meaning. [output](architecture/output.md) owns stream and color policy.

## code comments and names

a comment explains a reason, outside constraint or trap the code cannot express. if it repeats the operation, improve the name or remove the comment. use lowercase sentences, normally one or two, without banners, decorative labels or commented-out code. track unfinished work in an issue.

```rust
// a second ctrl-c means now
```

public items need a short caller-facing description and `# Errors` for non-obvious failure cases. useful rust examples can run under `cargo test --doc`. prefer whole-word names; `url`, `id`, `len` and `io` are familiar exceptions. booleans read as statements, such as `is_offline`. name modules for what they own rather than `utils` or `manager`.

## commits and pull requests

commit messages are one line, lowercase and imperative, under 60 characters, without a closing period. name the change, for example `retry downloads that stall mid-file`. use no prefixes, emoji, trailers, co-author lines or references to assistants or tools. keep commits small and check the affected behavior before committing.

a pull request title says what changed. its body explains what and why in two or three plain sentences. include measurements for performance claims; omit stock checklists and footers.

## existing checks

cargo configures rust and clippy lints, including missing public-documentation warnings. CLI tests compare an option sentence with help and snapshot program output. markdown transcripts are not otherwise executed. typos, vale and comment-prose checks are not configured. [engineering](engineering/README.md) lists the available checks.
