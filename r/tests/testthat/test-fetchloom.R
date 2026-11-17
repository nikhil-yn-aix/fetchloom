# a stand-in for fetchloom on unix: it keeps the arguments it was given beside itself, answers
# `answer` on stdout and exits with `code`
stand_in <- function(answer, code = 0L, env = parent.frame()) {
  folder <- withr::local_tempdir(.local_envir = env)
  writeLines(c(
    "#!/bin/sh",
    "printf '%s\\n' \"$@\" > \"$(dirname \"$0\")/arguments\"",
    "cat <<'ANSWER'",
    answer,
    "ANSWER",
    paste("exit", code)
  ), file.path(folder, "fetchloom"))
  Sys.chmod(file.path(folder, "fetchloom"), "755")
  withr::local_envvar(PATH = paste(folder, Sys.getenv("PATH"), sep = ":"), .local_envir = env)
  folder
}

# a project whose dataset iris is a folder on this machine, made by the fetchloom on PATH, with a
# cache of its own
project_with_iris <- function(env = parent.frame()) {
  found <- tryCatch(program(), error = function(error) "")
  if (!nzchar(found)) {
    skip("fetchloom is not on PATH")
  }
  root <- withr::local_tempdir(.local_envir = env)
  withr::local_envvar(
    FETCHLOOM_CACHE = file.path(root, "cache"), FETCHLOOM_KEYRING = "0", .local_envir = env
  )
  source <- file.path(root, "iris-source")
  dir.create(source)
  writeLines(c("sepal_length,species", "5.1,setosa"), file.path(source, "iris.csv"))
  project <- file.path(root, "project")
  dir.create(project)
  words <- c("--json", paste0("-C=", project), "add", source, "--as", "iris")
  added <- system2(found, command_words(words), stdout = FALSE, stderr = FALSE, stdin = nullfile())
  expect_equal(added, 0)
  normalizePath(project)
}

test_that("a missing program is one plain error that says how to install fetchloom", {
  empty <- withr::local_tempdir()
  withr::local_envvar(PATH = empty, HOME = empty, LOCALAPPDATA = empty)
  expect_error(
    path("iris"),
    paste0(
      "the fetchloom program is missing: install it as ",
      "https://github.com/nikhil-yn-aix/fetchloom#install says, or put it on PATH"
    ),
    fixed = TRUE
  )
})

test_that("path() returns the folder a dataset is placed in", {
  project <- project_with_iris()
  withr::local_dir(project)
  expect_identical(normalizePath(path("iris")), file.path(project, "data", "iris"))
  expect_identical(readLines(path("iris/iris.csv")), c("sepal_length,species", "5.1,setosa"))
})

test_that("sync() places the project's data again and returns its root", {
  project <- project_with_iris()
  unlink(file.path(project, "data"), recursive = TRUE)
  expect_identical(normalizePath(sync(project, names = "iris", locked = TRUE)), project)
  expect_true(file.exists(file.path(project, "data", "iris", "iris.csv")))
})

test_that("a mistyped name stops with fetchloom's kind, help and exit code", {
  project <- project_with_iris()
  error <- tryCatch(path("irs", project), fetchloom_error = function(error) error)
  expect_s3_class(error, "fetchloom_error")
  expect_identical(error$kind, "usage")
  expect_identical(error$code, 2L)
  expect_match(error$help, "did you mean `fetchloom -C=", fixed = TRUE)
  expect_match(conditionMessage(error), "usage: data.toml has no dataset called", fixed = TRUE)
})

test_that("download() gives fetchloom's link and options and returns its error", {
  project_with_iris()
  error <- tryCatch(
    download("https://example.org/x.csv", offline = TRUE),
    fetchloom_error = function(error) error
  )
  expect_identical(error$kind, "offline.missing")
  expect_identical(error$code, 20L)
})

test_that("download() takes a path or a folder, and force needs a path", {
  expect_error(download("https://e.org/x.csv", "x.csv", folder = "f"), class = "fetchloom_error")
  error <- tryCatch(download("https://e.org/x.csv", force = TRUE), fetchloom_error = function(e) e)
  expect_identical(error$kind, "usage")
  expect_true(is.na(error$code))
})

test_that("a project folder starting with a dash stays the folder", {
  skip_on_os("windows")
  folder <- stand_in('{"schema":1,"status":"ok","result":{"path":"/p/data/iris"},"warnings":[],"errors":[]}')
  expect_identical(path("iris", "-p", yes = TRUE), "/p/data/iris")
  expect_identical(
    readLines(file.path(folder, "arguments")),
    c("--json", "--progress", "-C=-p", "path", "--yes", "--", "iris")
  )
})

test_that("a name, a link and a path a shell would act on reach fetchloom as they are", {
  skip_on_os("windows")
  folder <- stand_in('{"schema":1,"status":"ok","result":{"path":"p","file":"f","folder":"d"},"warnings":[],"errors":[]}')
  work <- withr::local_tempdir()
  withr::local_dir(work)
  for (word in c("$(touch ran)", "`touch ran`", "a;b|c&d", ">ran", "x' 'y", "x\" \"y", "%PATH%")) {
    path(word, paste("p", word))
    expect_identical(
      readLines(file.path(folder, "arguments")),
      c("--json", "--progress", paste0("-C=p ", word), "path", "--", word)
    )
    download(paste0("https://e.org/", word), paste("o", word))
    expect_identical(readLines(file.path(folder, "arguments")), c(
      "--json", "--progress", "-C=.", "get", paste0("--output=o ", word), "--",
      paste0("https://e.org/", word)
    ))
  }
  expect_identical(list.files(work, all.files = TRUE, no.. = TRUE), character(0))
})

test_that("a fetchloom in the working folder or a relative folder of PATH never runs", {
  skip_on_os("windows")
  work <- withr::local_tempdir()
  empty <- withr::local_tempdir()
  for (folder in c(work, file.path(work, "bin"))) {
    dir.create(folder, showWarnings = FALSE)
    writeLines(
      c("#!/bin/sh", paste0(": > '", file.path(empty, "ran"), "'")),
      file.path(folder, "fetchloom")
    )
    Sys.chmod(file.path(folder, "fetchloom"), "755")
  }
  withr::local_dir(work)
  withr::local_envvar(PATH = paste(".", "", "bin", empty, sep = ":"), HOME = empty, LOCALAPPDATA = empty)
  expect_error(path("iris"), "the fetchloom program is missing", fixed = TRUE)
  expect_false(file.exists(file.path(empty, "ran")))
})

test_that("download() names its path, hash and switches as fetchloom takes them", {
  skip_on_os("windows")
  folder <- stand_in('{"schema":1,"status":"ok","result":{"file":"/w/y.csv","folder":null},"warnings":[],"errors":[]}')
  expect_identical(
    download("https://e.org/y.csv", "~/y.csv", sha256 = "ab", force = TRUE),
    "/w/y.csv"
  )
  expect_identical(readLines(file.path(folder, "arguments")), c(
    "--json", "--progress", "-C=.", "get", paste0("--output=", path.expand("~/y.csv")),
    "--sha256=ab", "--force", "--", "https://e.org/y.csv"
  ))
})

test_that("an error and a warning hold fetchloom's text with control characters escaped", {
  skip_on_os("windows")
  stand_in(paste0(
    '{"schema":1,"status":"error","result":null,',
    '"warnings":[{"kind":"cache.slow","message":"slow \\u001b[31mdisk","details":[],"help":null}],',
    '"errors":[{"kind":"usage","message":"no dataset called \\"irs\\u202e\\"",',
    '"details":["iris holds \\u001b]0;title\\u0007"],',
    '"help":"did you mean `fetchloom --json --progress -C=. path -- iris`?"}]}'
  ), code = 2L)
  expect_warning(
    error <- tryCatch(path("irs"), fetchloom_error = function(error) error),
    "slow \\u{1b}[31mdisk",
    fixed = TRUE,
    class = "fetchloom_warning"
  )
  expect_identical(error$message, "no dataset called \"irs\\u{202e}\"")
  expect_identical(error$details, "iris holds \\u{1b}]0;title\\u{7}")
  expect_identical(error$help, "did you mean `fetchloom -C=. path -- iris`?")
  expect_identical(error$code, 2L)
  expect_identical(conditionMessage(error), paste0(
    "usage: no dataset called \"irs\\u{202e}\"\n",
    "  iris holds \\u{1b}]0;title\\u{7}\n",
    "  help: did you mean `fetchloom -C=. path -- iris`?"
  ))
})

test_that("an answer that is no JSON names the exit code", {
  skip_on_os("windows")
  stand_in("not json", code = 101L)
  error <- tryCatch(sync(), fetchloom_error = function(error) error)
  expect_true(is.na(error$kind))
  expect_identical(error$code, 101L)
  expect_identical(conditionMessage(error), "fetchloom exited with code 101 without a JSON answer")
})

test_that("only the switches that are on are given", {
  expect_identical(switches(yes = FALSE, offline = FALSE), character(0))
  expect_identical(switches(yes = TRUE, offline = FALSE, locked = TRUE), c("--yes", "--locked"))
})

test_that("a word reaches a windows program whole", {
  expect_identical(windows_word("iris"), "iris")
  expect_identical(windows_word("my data"), "\"my data\"")
  expect_identical(windows_word("C:\\my data\\"), "\"C:\\my data\\\\\"")
  expect_identical(windows_word("say \"hi\""), "\"say \\\"hi\\\"\"")
  expect_identical(windows_word("a\\\"b"), "\"a\\\\\\\"b\"")
  expect_identical(windows_word(""), "\"\"")
})

test_that("only an absolute folder of PATH is searched", {
  expect_true(absolute("/usr/bin", FALSE))
  expect_false(absolute("bin", FALSE))
  expect_false(absolute("", FALSE))
  expect_true(absolute("C:\\tools", TRUE))
  expect_true(absolute("\\\\lab\\share", TRUE))
  expect_false(absolute("\\tools", TRUE))
  expect_false(absolute("C:tools", TRUE))
})

test_that("control and bidi characters are written as escapes, as the cli writes them", {
  expect_identical(escaped("a\u001b]0;x\u0007b"), "a\\u{1b}]0;x\\u{7}b")
  expect_identical(escaped("\u202eexe.txt"), "\\u{202e}exe.txt")
  expect_identical(escaped("one\ttwo\nthree\u0085"), "one\\ttwo\\nthree\\u{85}")
  expect_identical(escaped("na\u00efve \u65e5"), "na\u00efve \u65e5")
})
