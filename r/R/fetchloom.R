# each helper runs `fetchloom --json --progress` and returns a path, or stops with a condition of
# class fetchloom_error that holds the kind, details, help and exit code fetchloom gave. each
# warning goes on as an R warning, and the progress lines a long download prints reach the console
# as they come.

path <- function(name, project = ".", yes = FALSE, offline = FALSE, locked = FALSE) {
  name <- one_string(name, "name")
  arguments <- c("path", switches(yes = yes, offline = offline, locked = locked), "--", name)
  run(arguments, project)$path
}

sync <- function(project = ".", names = character(), yes = FALSE, offline = FALSE,
                 strict = FALSE, locked = FALSE, verify = FALSE) {
  arguments <- c(
    "sync",
    switches(yes = yes, offline = offline, locked = locked, verify = verify, strict = strict)
  )
  names <- as.character(names)
  if (length(names) > 0L) {
    arguments <- c(arguments, "--", names)
  }
  run(arguments, project)$project
}

download <- function(link, path = NULL, folder = NULL, sha256 = NULL, decompress = FALSE,
                     force = FALSE, yes = FALSE, offline = FALSE) {
  link <- one_string(link, "link")
  if (!is.null(path) && !is.null(folder)) {
    stop(fetchloom_error(
      "usage",
      "download writes one file at path or fills a folder, so it takes path or folder, not both"
    ))
  }
  if (isTRUE(force) && is.null(path)) {
    stop(fetchloom_error("usage", "force = TRUE replaces only the file at path, so it needs path"))
  }
  arguments <- "get"
  if (!is.null(path)) {
    arguments <- c(arguments, paste0("--output=", local_path(path)))
  }
  if (!is.null(folder)) {
    arguments <- c(arguments, paste0("--dir=", local_path(folder)))
  }
  if (!is.null(sha256)) {
    arguments <- c(arguments, paste0("--sha256=", one_string(sha256, "sha256")))
  }
  arguments <- c(
    arguments,
    switches(decompress = decompress, force = force, yes = yes, offline = offline),
    "--", link
  )
  result <- run(arguments, ".")
  if (is.null(path)) result$folder else result$file
}

run <- function(arguments, project) {
  program <- program()
  words <- c("--json", "--progress", paste0("-C=", local_path(project)), arguments)
  said <- suppressWarnings(system2(
    program, command_words(words),
    stdout = TRUE, stderr = "", stdin = nullfile()
  ))
  code <- attr(said, "status")
  code <- if (is.null(code)) 0L else as.integer(code)
  said <- as.character(said)
  Encoding(said) <- "UTF-8"
  answer <- tryCatch(
    parse_json(paste(said, collapse = "\n"), simplifyVector = FALSE),
    error = function(error) NULL
  )
  if (!is.list(answer) || is.null(answer$status)) {
    stop(fetchloom_error(
      NA_character_,
      sprintf("fetchloom exited with code %d without a JSON answer", code),
      code = code
    ))
  }
  for (warned in answer$warnings) {
    told <- shown(c(warned$message, unlist(warned$details), warned$help))
    warning(fetchloom_warning(warned$kind, paste(told, collapse = "; ")))
  }
  if (!identical(answer$status, "ok")) {
    failed <- answer$errors[[1]]
    help <- if (is.null(failed$help)) NULL else shown(failed$help)
    stop(fetchloom_error(
      failed$kind, shown(failed$message), shown(unlist(failed$details)), help, code, answer$result
    ))
  }
  answer$result
}

# the fetchloom program in the first absolute folder of PATH that holds it, else where the
# installers put it. on windows only fetchloom.exe counts: windows runs a .cmd or .bat through
# cmd.exe, which would read a link or a path as commands
program <- function() {
  windows <- .Platform$OS.type == "windows"
  name <- if (windows) "fetchloom.exe" else "fetchloom"
  folders <- strsplit(Sys.getenv("PATH"), .Platform$path.sep, fixed = TRUE)[[1]]
  home <- Sys.getenv(if (windows) "LOCALAPPDATA" else "HOME")
  if (nzchar(home)) {
    installed <- if (windows) {
      file.path(home, "Programs", "fetchloom", "bin")
    } else {
      file.path(home, ".local", "bin")
    }
    folders <- c(folders, installed)
  }
  for (folder in folders) {
    candidate <- file.path(folder, name)
    if (absolute(folder, windows) && file.exists(candidate) && !dir.exists(candidate) &&
      file.access(candidate, 1L) == 0L) {
      return(candidate)
    }
  }
  stop(
    "the fetchloom program is missing: install it as ",
    "https://github.com/nikhil-yn-aix/fetchloom#install says, or put it on PATH",
    call. = FALSE
  )
}

# a relative folder, an empty entry among them, depends on the working folder, where a cloned
# project could hold a program of that name
absolute <- function(folder, windows) {
  if (windows) {
    grepl("^([A-Za-z]:[\\\\/]|[\\\\/]{2})", folder)
  } else {
    startsWith(folder, "/")
  }
}

# system2 pastes its arguments into one command line: sh reads it on unix, and on windows the
# program splits it by the c runtime's rules
command_words <- function(words, windows = .Platform$OS.type == "windows") {
  if (windows) {
    vapply(words, windows_word, character(1), USE.NAMES = FALSE)
  } else {
    shQuote(words, type = "sh")
  }
}

windows_word <- function(word) {
  if (nzchar(word) && !grepl("[[:space:]\"]", word)) {
    return(word)
  }
  quoted <- ""
  backslashes <- 0L
  for (letter in strsplit(word, "", fixed = TRUE)[[1]]) {
    if (letter == "\\") {
      backslashes <- backslashes + 1L
      next
    }
    if (letter == "\"") {
      quoted <- paste0(quoted, strrep("\\", 2L * backslashes + 1L), "\"")
    } else {
      quoted <- paste0(quoted, strrep("\\", backslashes), letter)
    }
    backslashes <- 0L
  }
  paste0("\"", quoted, strrep("\\", 2L * backslashes), "\"")
}

# fetchloom's text as a person reads it at a terminal: the commands it suggests without the flags
# this package adds, and control and bidi characters written as escapes, as the cli writes them,
# so a name from a cloned project or a server cannot recolor the console or reorder its words
shown <- function(text) {
  escaped(gsub("`fetchloom --json --progress ", "`fetchloom ", as.character(text), fixed = TRUE))
}

escaped <- function(text) {
  vapply(as.character(text), escaped_one, character(1), USE.NAMES = FALSE)
}

escaped_one <- function(text) {
  if (is.na(text)) {
    return(text)
  }
  points <- utf8ToInt(enc2utf8(text))
  if (anyNA(points)) {
    return(text)
  }
  control <- points < 32L | (points >= 127L & points <= 159L) |
    points %in% c(0x61cL, 0x200eL, 0x200fL) |
    (points >= 0x202aL & points <= 0x202eL) | (points >= 0x2066L & points <= 0x2069L)
  if (!any(control)) {
    return(text)
  }
  parts <- vapply(seq_along(points), function(at) {
    point <- points[[at]]
    if (!control[[at]]) {
      return(intToUtf8(point))
    }
    switch(as.character(point),
      "9" = "\\t",
      "10" = "\\n",
      "13" = "\\r",
      sprintf("\\u{%x}", point)
    )
  }, character(1))
  paste(parts, collapse = "")
}

one_string <- function(value, what) {
  if (!is.character(value) || length(value) != 1L || is.na(value)) {
    stop(fetchloom_error("usage", paste0(what, " must be one string")))
  }
  value
}

local_path <- function(path) {
  path.expand(one_string(as.character(path), "a path"))
}

switches <- function(...) {
  wanted <- list(...)
  on <- vapply(wanted, isTRUE, logical(1))
  sprintf("--%s", names(wanted)[on])
}

fetchloom_error <- function(kind, message, details = character(), help = NULL,
                            code = NA_integer_, result = NULL) {
  structure(
    class = c("fetchloom_error", "error", "condition"),
    list(
      message = message, call = NULL, kind = kind, details = details, help = help,
      code = code, result = result
    )
  )
}

fetchloom_warning <- function(kind, message) {
  structure(
    class = c("fetchloom_warning", "warning", "condition"),
    list(message = message, call = NULL, kind = kind)
  )
}

# reads like the command line's error: the message, then each detail and the help on a line of
# its own
conditionMessage.fetchloom_error <- function(c) {
  error <- c
  first <- if (is.na(error$kind)) error$message else paste0(error$kind, ": ", error$message)
  lines <- c(first, if (length(error$details) > 0L) paste0("  ", error$details))
  if (!is.null(error$help)) {
    lines <- c(lines, paste0("  help: ", error$help))
  }
  paste(lines, collapse = "\n")
}
