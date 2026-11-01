# completions

`completions` prints a script that lets your shell complete fetchloom's commands, options and option values when you press tab.

```sh
fetchloom completions zsh
```

## usage

```sh
fetchloom completions [options] <shell>
```

`<shell>` is `bash`, `zsh`, `fish`, `powershell` or `elvish`. [global options](README.md#global-options) are accepted, except that `--json` is a usage error: the script is text for the shell, not a document.

## loading the script

save the script where your shell looks for completions, or have your shell profile run fetchloom on each start. a saved script describes the version that wrote it, so write it again after upgrading fetchloom.

| shell | setup |
|---|---|
| bash with bash-completion 2 | `mkdir -p ~/.local/share/bash-completion/completions`, then `fetchloom completions bash > ~/.local/share/bash-completion/completions/fetchloom`. it loads the first time you complete fetchloom |
| bash without bash-completion, such as the bash 3.2 macos ships | add `eval "$(fetchloom completions bash)"` to `~/.bashrc`, or to `~/.bash_profile` on macos |
| zsh | `mkdir -p ~/.zfunc`, then `fetchloom completions zsh > ~/.zfunc/_fetchloom`. in `~/.zshrc`, `fpath=(~/.zfunc $fpath)` must come before `autoload -U compinit && compinit` |
| fish | `fetchloom completions fish > ~/.config/fish/completions/fetchloom.fish`. fish loads it on demand |
| powershell | add `fetchloom completions powershell \| Out-String \| Invoke-Expression` to the file `$PROFILE` names. `New-Item -Force $PROFILE` creates it when it is missing |
| elvish | add `eval (fetchloom completions elvish \| slurp)` to `~/.config/elvish/rc.elv` |

open a new terminal afterwards. typing `fetchloom ca` and pressing tab then completes `cache`, and `fetchloom cache ` offers `gc` and `verify`.

## what it completes

the scripts complete command and subcommand names, options, the fixed values of `--color` and of `completions` itself, and folders after options such as `-C` and `-o`. they are written once from fetchloom's command line, so they do not complete dataset names or references.

## settings it ignores

a shell profile runs `completions` on every start, so it runs before fetchloom reads `-C`, `FETCHLOOM_JOBS`, `FETCHLOOM_THREADS` or `FETCHLOOM_RATE`. a value that would stop another command, or a missing folder after `-C`, never stops a shell from starting.

## exit codes

0 means the script was printed, including when a reader such as `head` stopped reading early. an unknown shell and `--json` are usage errors with exit 2. [exit codes](../reference/exit-codes.md) lists the rest.
