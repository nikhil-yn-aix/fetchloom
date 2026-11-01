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

- `--json` turns stdout into one [document](reference/json-output.md), with no colors and no questions, whatever the terminal.
- `errors[0].kind` and the [exit code](reference/exit-codes.md) say what failed; `errors[0].fix` is the command to run next, when there is one.
- `fetchloom help --json` describes every command, option and example in one call.
- without a terminal fetchloom never asks: it refuses to guess between search results, needs `--yes` for downloads over 1 GiB, and names `fetchloom auth login` instead of asking for a key.
- an agent that runs commands in a real terminal is treated the same way when it says so: `VSCODE_AGENT`, `CLAUDECODE`, `AI_AGENT`, `CODEX_SANDBOX`, `GEMINI_CLI`, `CURSOR_AGENT` and a few others turn off questions, colors and animated progress. a harness that sets none of them can set `AI_AGENT=1`.
- keys stay out of the agent's hands. fetchloom reads the keys the source's own tools saved, and signing in happens in your terminal, not the agent's.

## why there is no MCP server

an agent with a shell does better calling a CLI it can read about than a separate server whose tool list fills its context, and fetchloom's commands already return typed results. an MCP server would only help chat apps with no shell, and fetchloom does not ship one for now.
