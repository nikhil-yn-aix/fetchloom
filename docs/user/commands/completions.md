# completions

`completions` prints a script that lets your shell complete fetchloom's commands, options, option values and the names of your datasets when you press tab.

```sh
fetchloom completions zsh
```

## usage

```sh
fetchloom completions [options] <shell>
```

`<shell>` is `bash`, `zsh`, `fish`, `powershell` or `elvish`. [global options](README.md#global-options) are accepted, except that `--json` is a usage error: the script is text for the shell, not a document.

## loading the script

the steps below make completions permanent: each saves the script where the shell looks, or adds one line to the shell's startup file. a saved script describes the version that wrote it, so write it again after upgrading fetchloom. a script an older version wrote keeps completing commands and options, but completes dataset names only once it is written again.

powershell, in the file `$PROFILE` names. the first line creates the file only when it is missing:

```powershell
if (-not (Test-Path $PROFILE)) { New-Item -ItemType File -Path $PROFILE -Force | Out-Null }
Add-Content -Path $PROFILE -Value 'fetchloom completions powershell | Out-String | Invoke-Expression'
```

if scripts are blocked, `Set-ExecutionPolicy -Scope CurrentUser RemoteSigned` lets the profile run.

bash with bash-completion 2, which loads the file the first time you complete fetchloom:

```sh
mkdir -p ~/.local/share/bash-completion/completions
fetchloom completions bash > ~/.local/share/bash-completion/completions/fetchloom
```

bash without bash-completion, such as the bash 3.2 macos ships. use `~/.bash_profile` instead of `~/.bashrc` on macos:

```sh
echo 'eval "$(fetchloom completions bash)"' >> ~/.bashrc
```

zsh:

```sh
mkdir -p ~/.zfunc
fetchloom completions zsh > ~/.zfunc/_fetchloom
```

then in `~/.zshrc`, `fpath=(~/.zfunc $fpath)` must come before `autoload -Uz compinit && compinit`. add both lines if they are not there yet.

fish, which loads the file on demand:

```sh
mkdir -p ~/.config/fish/completions
fetchloom completions fish > ~/.config/fish/completions/fetchloom.fish
```

elvish: add `eval (fetchloom completions elvish | slurp)` to `~/.config/elvish/rc.elv`.

open a new terminal afterwards. typing `fetchloom ca` and pressing tab then completes `cache`, and `fetchloom cache ` offers `gc` and `verify`. in a project whose data.toml has `iris`, `fetchloom path ir` completes `iris`.

## completing fl

each script also completes `fl`, the short name the installers make for fetchloom, the same way. zsh, powershell and elvish register it as they load the script. bash-completion and fish load a script the first time you complete the command it is named after, so `fl` completes only once `fetchloom` has, unless the saved script has a second name. where `fl` runs fetchloom, give it one:

```sh
ln -s fetchloom ~/.local/share/bash-completion/completions/fl
ln -s fetchloom.fish ~/.config/fish/completions/fl.fish
```

zsh keeps the commands each script completes in `~/.zcompdump` and reads them again only when the number of scripts changes. after replacing a saved `_fetchloom`, delete `~/.zcompdump` once and open a new terminal. in powershell, `fl` is the alias of `Format-List`, which powershell completes as its own command.

## what it completes

the scripts complete command and subcommand names, options, the fixed values of `--color` and of `completions` itself, and paths after options such as `-C`, `-o` and `--dir`. these come from fetchloom's command line as it was when the script was written.

after a command that takes a dataset's name, they also complete the names data.toml has: after `path`, `sync`, `remove`, `build`, `list`, `export` and `push`, and after `lock --upgrade`, `--ours` and `--theirs`. for those words the script asks fetchloom, which reads the data.toml of the project the current folder is in, or of the one `-C` names on the line you are completing, `~` included. it reads nothing else and writes nothing: no network, no settings, no data.lock. a name fetchloom would refuse is never offered, so a cloned data.toml cannot put text such as `$(…)` on your command line. each script also takes the names it gets as plain words: none is expanded while you complete, and one that holds a character your shell would read as code goes on the line quoted. outside a project, or for a word that is no name, such as an option or the file in `iris/train.csv`, the script completes as it would without names. `add --as` names a new dataset, so it gets no names, and references are never completed.

asking runs fetchloom once per tab, and only after those commands. it reads data.toml's `[datasets.<name>]` lines, the way fetchloom writes them, instead of parsing the whole file; a data.toml written another way is parsed whole.

## settings it ignores

a shell profile runs `completions` on every start, so it runs before fetchloom reads `-C`, `FETCHLOOM_JOBS`, `FETCHLOOM_THREADS` or `FETCHLOOM_RATE`. a value that would stop another command, or a missing folder after `-C`, never stops a shell from starting. completing a dataset's name reads no settings either, so a broken settings file never stops it.

## exit codes

0 means the script was printed, including when a reader such as `head` stopped reading early. an unknown shell and `--json` are usage errors with exit 2. [exit codes](../reference/exit-codes.md) lists the rest.
