# R package

the R package in `r/` runs the fetchloom program with `--json --progress` and returns paths, as the [python helper](python-helper.md) does. it needs R 4.1 or later, the jsonlite package, and fetchloom [installed](../start/install.md); no CRAN package is published yet.

```r
remotes::install_github("nikhil-yn-aix/fetchloom", subdir = "r")

project <- fetchloom::sync(".", yes = TRUE)
iris <- read.csv(fetchloom::path("iris/iris.csv"))
table <- read.csv(fetchloom::download("https://example.org/iris.csv", "data.csv"))
```

| function | return |
|---|---|
| `fetchloom::sync(project = ".", names = character(), yes = FALSE, offline = FALSE, strict = FALSE, locked = FALSE, verify = FALSE)` | absolute project root |
| `fetchloom::path(name, project = ".", yes = FALSE, offline = FALSE, locked = FALSE)` | dataset, file or subfolder |
| `fetchloom::download(link, path = NULL, folder = NULL, sha256 = NULL, force = FALSE, yes = FALSE, offline = FALSE)` | with `path`, the file written there; otherwise the folder, `folder` or a new one named after the download |

the arguments mean what the python helper's keywords mean, and `download()` is the python helper's `get()`: an R function called `get` would hide `base::get` once the package is attached, which R's own check and many scripts call.

a failure stops with a condition of class `fetchloom_error`, whose `kind`, `message`, `details`, `help`, `code` and `result` are those of fetchloom's JSON error, as [exit codes](exit-codes.md) lists them; its printed text is the message, then each detail and the help on a line of its own, with the commands help suggests written as you type them at a terminal. `kind` is `NA` when fetchloom gave no JSON answer. control characters fetchloom's text holds, as a name from a cloned project can, are written as escapes, as the command line writes them. each warning is an R warning of class `fetchloom_warning`, and the progress lines reach the console as they come.

```r
tryCatch(
  fetchloom::path("irs"),
  fetchloom_error = function(error) message(error$kind, ": ", error$help)
)
```

the package runs the first `fetchloom` in an absolute folder on PATH, else the one the installers put in `~/.local/bin`, or `%LOCALAPPDATA%\Programs\fetchloom\bin` on windows, where only `fetchloom.exe` counts. without one, each function stops with `the fetchloom program is missing: install it as https://github.com/nikhil-yn-aix/fetchloom#install says, or put it on PATH`.
