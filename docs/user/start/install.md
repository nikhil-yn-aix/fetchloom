# Install

no release has been published yet, so the one-line installers and `pip install fetchloom` do not work. until then, build fetchloom from its source code. you do this once and it takes a few minutes.

## open a terminal

a terminal is a window where you type a command and press enter to run it. type each command below on its own line.

- windows: open the start menu, type `powershell` and open windows powershell.
- macos: press cmd and space, type `terminal` and press enter.
- linux: press ctrl, alt and t, or open terminal from your applications.

with a screen reader, run `export FETCHLOOM_ACCESSIBLE=1` first, or `$env:FETCHLOOM_ACCESSIBLE = '1'` in powershell. fetchloom then redraws nothing: progress comes as plain lines, a question such as whether to download a file over 1 GiB is a line you answer with y or n, and words such as `iris` list the matching datasets by number, so you type the number of the one to add. `export TERM=dumb`, or `$env:TERM = 'dumb'`, drops colors and symbols such as `·` too.

## 1. install rust

rust is the language fetchloom is written in. its installer, rustup, also gives you `cargo`, the command that builds fetchloom.

windows: download `rustup-init.exe` from [rustup.rs](https://rustup.rs) and run it. when it offers to install visual studio's C++ build tools, accept; fetchloom needs them. then press enter to take the default rust install.

macos: install apple's command line tools, then rust:

```sh
xcode-select --install
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

linux: install a C compiler, then rust. on debian or ubuntu:

```sh
sudo apt install build-essential curl
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

on fedora, use `sudo dnf install gcc curl` for the first line.

close the terminal and open a new one, so it finds `cargo`.

## 2. build and install fetchloom

```sh
cargo install --locked --git https://github.com/nikhil-yn-aix/fetchloom
fetchloom --version
```

the second line prints `fetchloom` and its version. if `cargo install` says your rust is too old, run `rustup update` and try again.

cargo installs `fetchloom` alone, while the installers also make `fl`, a short name for it. on linux and macos, this line makes that link in cargo's folder; skip it when `command -v fl` prints another program:

```sh
ln -s fetchloom ~/.cargo/bin/fl
```

in powershell, `fl` means `Format-List`; [windows](../guides/windows.md#short-name) shows how to get the short name there.

fetchloom runs on windows 10 or later, macos 11 or later and linux, on x86_64 and arm64; [compatibility](../reference/compatibility.md) has the details. the [python helper](../reference/python-helper.md) says how to build the python package, which adds a python api to the same program. [shell completions](../commands/completions.md) finish the setup.

## windows

no release has been published yet, so build fetchloom from source, as [getting started](install.md) shows.

after the first release, the installer in the [readme](https://github.com/nikhil-yn-aix/fetchloom#install) puts fetchloom in `%LOCALAPPDATA%\Programs\fetchloom\bin`, which needs no administrator. the release executables are not signed. [Microsoft Defender SmartScreen](https://learn.microsoft.com/en-us/windows/security/operating-system-security/virus-and-threat-protection/microsoft-defender-smartscreen/) checks programs downloaded from the internet and warns about one without an established reputation, as a new unsigned release can be, with a "Windows protected your PC" window. a browser marks the files it saves as downloaded from the internet. for a release archive saved from a browser, compare its SHA-256 with the line for it in the release's `SHA256SUMS`, then remove the mark before unpacking it, so the program inside carries none:

```powershell
Get-FileHash .\fetchloom-*.zip
Unblock-File .\fetchloom-*.zip
```

[`Unblock-File`](https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.utility/unblock-file) removes the mark, the file's `Zone.Identifier` stream. if the warning already shows, "More info" and then "Run anyway" starts the program.

## macos

no release has been published yet, so build from source: `xcode-select --install` provides the command line tools, then follow [getting started](install.md).

after the first release, the installer in the [readme](https://github.com/nikhil-yn-aix/fetchloom#install) picks the x86_64 or arm64 build. a terminal running under rosetta on apple silicon still gets the arm64 build. it installs into `~/.local/bin` and, when PATH lacks that folder, adds it to `~/.zshrc` as well as `~/.profile`, since zsh is the default shell and never reads `~/.profile`.

the release executables are not signed by a developer account. `curl`, the installer and `pip` do not mark what they download, so macos runs it without asking. an archive saved from a browser is marked as quarantined; after unpacking it, clear the mark before the first run:

```sh
xattr -d com.apple.quarantine fetchloom
```

## uninstall

1. to delete the keys you saved with fetchloom, `fetchloom auth` lists them and `fetchloom auth logout <account>` removes one. keys you saved with a source's own tools, such as `~/.kaggle` or the hugging face token, are shared with those tools.
2. `fetchloom -q cache` prints the cache folder; delete it if you no longer need its bytes. it follows `FETCHLOOM_CACHE` and, on linux, an absolute `XDG_CACHE_HOME`; [cache locations](../reference/cache-folder.md#location) lists the defaults.
3. remove the program, the way you installed it:

| installed with | remove with |
|---|---|
| `cargo install` | `cargo uninstall fetchloom`, and `rm ~/.cargo/bin/fl` if you made that link |
| `pip install fetchloom` | `pip uninstall fetchloom` |
| `install.sh` | `rm ~/.local/bin/fetchloom ~/.local/bin/fl`, and the line marked `# added by the fetchloom installer` in your shell profiles. if the installer said it left `fl` alone, that `fl` is another program: remove only `fetchloom` |
| `install.ps1` | delete `%LOCALAPPDATA%\Programs\fetchloom`, which holds `fl.exe` too, and that folder's `bin` from your user PATH in the environment variables settings |

projects keep their `data.toml`, `data.lock` and `data/`; delete them with the project.
