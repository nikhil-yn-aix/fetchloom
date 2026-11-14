# AI agents

coding agents such as claude code, github copilot, codex and cursor run fetchloom the way a person does, through the shell. nothing extra needs to run.

## give your agent the skill

[skills/fetchloom](https://github.com/nikhil-yn-aix/fetchloom/blob/main/skills/fetchloom/SKILL.md) is an [agent skill](https://agentskills.io): a short page an agent loads only when a task needs a dataset. it says which command fits which task, which flags keep a run safe, and how to read a failure. copy the folder where your agent looks:

| agent | folder |
|---|---|
| claude code, every project | `~/.claude/skills/fetchloom` |
| claude code, one project | `.claude/skills/fetchloom` in the repository |
| github copilot in vs code and copilot cli | `.github/skills/fetchloom` in the repository, or `~/.copilot/skills/fetchloom` |
| openai codex | `.agents/skills/fetchloom` in the repository, or `~/.agents/skills/fetchloom` |

agents that read skills from another folder take the same `SKILL.md`.

## what agents rely on

- `--json` turns stdout into one [document](../reference/json-output.md), with no colors and no questions, whatever the terminal.
- `errors[0].kind` and the [exit code](../reference/exit-codes.md) say what failed; `errors[0].fix` is the command to run next, when there is one. it never deletes, overwrites or repins data, runs a build, signs in or says yes, nor picks one of the ways a `help` offers with `or`, so a null `fix` leaves the next step to the person.
- `fetchloom help --json` describes every command, option and example in one call.
- without a terminal fetchloom never asks: it refuses to guess between search results, needs `--yes` for downloads over 1 GiB by default, and names `fetchloom auth login` instead of asking for a key.
- an agent that runs commands in a real terminal is treated the same way when it says so: `VSCODE_AGENT`, `CLAUDECODE`, `AI_AGENT`, `CODEX_SANDBOX`, `GEMINI_CLI`, `CURSOR_AGENT` and a few others turn off questions, automatic colors and animated progress. a harness that sets none of them can set `AI_AGENT=1`.
- keep keys out of the agent's conversation. fetchloom reads keys saved by the source's tools; sign in in your own terminal rather than pasting a key into chat.

## why there is no MCP server

fetchloom's CLI already returns structured JSON to an agent with a shell. fetchloom ships no MCP server, so a chat application without shell access needs another way to run the executable.
