//! the command line: read the arguments, run the command, return the exit code.

mod args;
mod cache;
mod fetch;
mod get;
mod help;
mod logs;
mod project;
mod status;
mod sync;
mod usage;

use std::borrow::Cow;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use serde::Serialize;

use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::net::{self, Net, Notice};
use crate::output::{Level, Output, Style};
use crate::source;
use crate::stop::Stop;
use crate::units;
use crate::work::Work;

/// downloads running at once when `FETCHLOOM_JOBS` says nothing. downloads wait on the network,
/// not the processor, so the number does not follow the core count.
const JOBS: usize = 8;

/// a download larger than this asks first.
const ASK_ABOVE: u64 = 1024 * 1024 * 1024;

/// what every command gets: where to print, the settings, the counters and the stop signal.
struct Context {
    output: Arc<Output>,
    offline: net::Offline,
    yes: bool,
    jobs: usize,
    /// whether a large download may be split across streams, which `FETCHLOOM_SPLIT=1` asks
    /// for.
    split: bool,
    work: Arc<Work>,
    stop: Arc<Stop>,
}

impl Context {
    /// a context with every setting at its default, printing through `output`.
    fn new(output: Output) -> Context {
        Context {
            output: Arc::new(output),
            offline: net::Offline::No,
            yes: false,
            jobs: JOBS,
            split: false,
            work: Arc::default(),
            stop: Arc::default(),
        }
    }

    /// the network, with credentials from where each source keeps them, and retries reported as
    /// warnings.
    fn net(&self) -> Net {
        let output = self.output.clone();
        let notify: net::Notify = Arc::new(move |notice| match notice {
            Notice::Retrying { reason, wait, next } => {
                output.warn(&format!(
                    "{reason}. trying again in {} (try {next} of {})",
                    units::duration(wait),
                    net::ATTEMPTS
                ));
            }
            Notice::Waiting {
                host,
                quota,
                window,
                wait,
            } => output.step("Waiting", &rate_wait(&host, quota, window, wait)),
        });
        let options = net::Options {
            offline: self.offline,
            jobs: self.jobs,
            credentials: source::credentials(&|name| std::env::var_os(name)),
            split: self.split,
        };
        Net::new(options, self.work.clone(), self.stop.clone(), notify)
    }
}

/// why requests wait for a host: `20 s for zenodo.org, whose limit of 133 requests is used up`.
fn rate_wait(host: &str, quota: Option<u64>, window: Option<Duration>, wait: Duration) -> String {
    let wait = units::seconds(Duration::from_secs(
        wait.as_secs() + u64::from(wait.subsec_nanos() > 0),
    ));
    let Some(quota) = quota else {
        return format!("{wait} for {host}, which asked for a pause");
    };
    let every = window.map_or_else(String::new, |window| {
        let seconds = window.as_secs();
        match seconds {
            60 => " every minute".to_owned(),
            3600 => " every hour".to_owned(),
            86_400 => " every day".to_owned(),
            _ if seconds % 60 == 0 => format!(" every {} minutes", seconds / 60),
            _ => format!(" every {seconds} s"),
        }
    });
    format!(
        "{wait} for {host}, whose limit of {}{every} is used up",
        units::count(quota, "request", "requests")
    )
}

/// whether large downloads may split: only when `FETCHLOOM_SPLIT=1` asks, since they lost on lan
/// and home in the lab while they won on far.
fn splits(value: Option<std::ffi::OsString>) -> bool {
    value.is_some_and(|value| value == "1")
}

/// `FETCHLOOM_JOBS`, or the default.
fn jobs(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Result<usize, Error> {
    let Some(value) = var("FETCHLOOM_JOBS").filter(|value| !value.is_empty()) else {
        return Ok(JOBS);
    };
    value
        .to_str()
        .and_then(|text| text.trim().parse::<usize>().ok())
        .filter(|jobs| (1..=256).contains(jobs))
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!(
                    "FETCHLOOM_JOBS is \"{}\", which is not a number from 1 to 256",
                    value.display()
                ),
            )
            .help(format!(
                "unset FETCHLOOM_JOBS to use the default of {JOBS}, or set it to a number from 1 to 256"
            ))
        })
}

/// the cache this machine uses, created on first use.
fn open_cache() -> Result<Cache, Error> {
    let root = Cache::locate(&|name| std::env::var_os(name)).ok_or_else(|| {
        Error::new(Kind::LocalIo, "could not find a folder for the cache")
            .help("set FETCHLOOM_CACHE to a folder fetchloom may use")
    })?;
    Cache::open(root.clone()).map_err(|err| {
        Error::io(
            format_args!("could not open the cache at {}", root.display()),
            &err,
        )
        .or_help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

/// why fetchloom cannot start in `folder`, as `-C` asks. windows lets a program work in no folder
/// whose path passes 258 characters, so a project deeper than that is named with its length
/// before anything is written.
fn unusable(folder: &std::path::Path, err: &std::io::Error) -> Error {
    if err.kind() != std::io::ErrorKind::InvalidFilename {
        return Error::new(
            Kind::Usage,
            format!("cannot work in {}: {err}", folder.display()),
        )
        .help("give -C a folder that exists and that you may open");
    }
    let path = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
    let length = path.as_os_str().to_string_lossy().chars().count();
    Error::new(
        Kind::Usage,
        format!(
            "cannot work in {}: at {length} characters, its path is too long for a working folder",
            path.display()
        ),
    )
    .help("move the project to a folder with a shorter path, then run the same command there")
}

pub(crate) fn run() -> ExitCode {
    let started = Instant::now();
    let args = match args::Args::try_parse() {
        Ok(args) => args,
        Err(err) => return usage::answer(&err, started),
    };
    let global = args.global;
    let level = level(global.quiet, global.verbose);
    let var = |name: &str| std::env::var_os(name);
    let style = Style::detect(global.color);
    let logs = logs::start(level, style.color, &var);
    let mut context = Context::new(Output::new(style, level, global.json, logs.on_stderr()));
    context.offline = net::Offline::from_settings(
        global.offline,
        var("FETCHLOOM_OFFLINE").is_some_and(|value| value == "1"),
    );
    context.yes = global.yes;
    context.split = splits(var("FETCHLOOM_SPLIT"));
    if args.version {
        return about(
            &context,
            &format!("fetchloom {}", env!("CARGO_PKG_VERSION")),
            started,
        );
    }
    if let Some(args::Command::Help(asked)) = &args.command {
        return help::show(&context, &asked.topic, started);
    }
    let name = args.command.as_ref().map(args::Command::name);
    let _command = tracing::debug_span!("command", name).entered();
    if let Some(folder) = &global.directory
        && let Err(err) = std::env::set_current_dir(folder)
    {
        return finish::<()>(&context, name, Err(unusable(folder, &err)), started);
    }
    let Some(command) = args.command else {
        return alone(&context, started);
    };
    match jobs(&var) {
        Ok(jobs) => context.jobs = jobs,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    }
    dispatch(&context, command, started)
}

/// runs a command and turns how it ended into an exit code.
fn dispatch(context: &Context, command: args::Command, started: Instant) -> ExitCode {
    let name = Some(command.name());
    match command {
        args::Command::Add(args) => {
            let outcome = block_on(context, project::add(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Remove(args) => {
            let outcome = block_on(context, project::remove(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Sync(args) => {
            let outcome = block_on(context, project::sync(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Lock(args) => {
            let outcome = block_on(context, project::lock(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Status => {
            let outcome = status::run(context);
            finish(context, name, outcome, started)
        }
        args::Command::Path(args) => {
            let outcome = block_on(context, project::path(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache { action: None }) => {
            let outcome = cache::info(context);
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Gc),
        }) => {
            let outcome = cache::gc(context);
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Verify),
        }) => {
            let outcome = cache::verify(context);
            finish(context, name, outcome, started)
        }
        args::Command::Get(args) => {
            let outcome = block_on(context, get::run(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Help(asked) => help::show(context, &asked.topic, started),
    }
}

/// what `--version` and `fetchloom` alone report in `--json`.
#[derive(Serialize)]
struct About {
    version: &'static str,
}

/// prints `text` for a person, or the version in the document `--json` asked for.
fn about(context: &Context, text: &str, started: Instant) -> ExitCode {
    context.output.answer(text);
    let about = About {
        version: env!("CARGO_PKG_VERSION"),
    };
    finish(context, None, Ok(about), started)
}

/// `fetchloom` alone: a short status inside a project, and the welcome outside one.
fn alone(context: &Context, started: Instant) -> ExitCode {
    match project::find() {
        Ok(project) => {
            let outcome = status::brief(context, &project);
            finish(context, Some("status"), outcome, started)
        }
        Err(_) => about(
            context,
            &format!("fetchloom {}\n\n{}", env!("CARGO_PKG_VERSION"), args::INTRO),
            started,
        ),
    }
}
/// asks before a large download, unless `--yes` already said yes. without a terminal to ask on,
/// it stops and names the flag.
fn confirm(output: &Output, yes: bool, what: &str, bytes: u64) -> Result<(), Error> {
    if bytes <= ASK_ABOVE {
        return Ok(());
    }
    ask(output, yes, what, &units::size(bytes))
}

/// asks whether to download `what`, which needs `size`, as [`confirm`] does.
fn ask(output: &Output, yes: bool, what: &str, size: &str) -> Result<(), Error> {
    if yes {
        return Ok(());
    }
    match output.confirm(&format!("download {what} ({size})?"))? {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!("did not download {what}"),
        )),
        None => Err(Error::new(
            Kind::Unconfirmed,
            format!("downloading {what} needs {size}, and there is no terminal to ask first"),
        )
        .help("add --yes to download anyway")),
    }
}

/// runs a command that needs the network, with ctrl-c asking it to stop cleanly.
fn block_on<T>(
    context: &Context,
    command: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .map_err(|err| Error::io("could not start", &err))?;
    let stop = context.stop.clone();
    runtime.spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            stop.ask();
            // a second ctrl-c means now
            if tokio::signal::ctrl_c().await.is_ok() {
                std::process::exit(130);
            }
        }
    });
    runtime.block_on(command)
}

/// prints how the run ended and turns it into an exit code.
fn finish<T: Serialize>(
    context: &Context,
    command: Option<&str>,
    outcome: Result<T, Error>,
    started: Instant,
) -> ExitCode {
    let elapsed = started.elapsed();
    let counts = context.work.counts();
    let outcome = outcome.map_err(|error| match command_line(std::env::args_os().skip(1)) {
        Some(line) => error.naming(&line),
        None => error,
    });
    context
        .output
        .document(command, outcome.as_ref(), counts, elapsed);
    if let Err(error) = &outcome {
        context.output.error(error);
    }
    context.output.summary(counts, elapsed);
    match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => ExitCode::from(error.kind.exit_code()),
    }
}

/// how much of the story `-q` and `-v` ask for.
fn level(quiet: bool, verbose: u8) -> Level {
    match (quiet, verbose) {
        (true, _) => Level::Quiet,
        (false, 0) => Level::Normal,
        (false, 1) => Level::Verbose,
        (false, _) => Level::Debug,
    }
}

/// `word` as a shell reads it back whole: in double quotes when it holds more than letters,
/// digits and the marks links use, such as `?` or `&`.
fn shell_word(word: &str) -> Cow<'_, str> {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,~".contains(c))
    {
        Cow::Borrowed(word)
    } else {
        Cow::Owned(format!("\"{word}\""))
    }
}

/// the command line as typed, ready to copy, for help that says to run it again. none when an
/// argument holds a secret no message may repeat, such as a token in a link, or is not unicode.
fn command_line(args: impl Iterator<Item = std::ffi::OsString>) -> Option<String> {
    let mut line = String::from("fetchloom");
    for arg in args {
        let arg = arg.into_string().ok()?;
        let login = url::Url::parse(&arg)
            .is_ok_and(|url| !url.username().is_empty() || url.password().is_some());
        if login || matches!(net::mask_query(&arg), Cow::Owned(_)) {
            return None;
        }
        line.push(' ');
        line.push_str(&shell_word(&arg));
    }
    Some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_is_named_as_typed_unless_it_holds_a_secret() {
        let line = |args: &[&str]| command_line(args.iter().map(std::ffi::OsString::from));
        assert_eq!(
            line(&["add", "zenodo:3608990", "--select", "*.csv"]).as_deref(),
            Some("fetchloom add zenodo:3608990 --select \"*.csv\"")
        );
        assert_eq!(
            line(&["-C", "my project", "sync"]).as_deref(),
            Some("fetchloom -C \"my project\" sync")
        );
        assert_eq!(line(&["get", "https://example.org/a.csv?token=abc"]), None);
        assert_eq!(
            line(&["get", "https://user:secret@example.org/a.csv"]),
            None
        );
    }

    #[test]
    fn a_word_a_shell_would_split_or_expand_is_quoted() {
        assert_eq!(shell_word("zenodo:3608990"), "zenodo:3608990");
        assert_eq!(
            shell_word("https://example.org/a.csv?download=1&v=2"),
            "\"https://example.org/a.csv?download=1&v=2\""
        );
        assert_eq!(shell_word("*.csv"), "\"*.csv\"");
        assert_eq!(shell_word("my data"), "\"my data\"");
    }

    #[test]
    fn downloads_split_only_when_asked() {
        assert!(
            !splits(None),
            "split downloads lost in the lab on lan and home"
        );
        assert!(splits(Some("1".into())));
        assert!(!splits(Some("0".into())));
        assert!(!splits(Some(String::new().into())));
    }
}
