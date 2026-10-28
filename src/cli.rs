//! the command line: read the arguments, run the command, return the exit code.

mod args;
mod cache;
mod fetch;
mod get;
mod logs;
mod project;
mod status;
mod sync;

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use clap::error::{ContextKind, ContextValue, ErrorKind};
use serde::Serialize;

use crate::cache::Cache;
use crate::error::{Error, Kind};
use crate::net::{self, Net, Notice};
use crate::output::{ColorChoice, Level, Output, Style};
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
    offline: bool,
    yes: bool,
    jobs: usize,
    work: Arc<Work>,
    stop: Arc<Stop>,
}

impl Context {
    /// the network, with credentials from where each source keeps them, and retries reported as
    /// warnings.
    fn net(&self) -> Result<Net, Error> {
        let output = self.output.clone();
        let notify: net::Notify = Arc::new(move |notice| match notice {
            Notice::Retrying { reason, wait, next } => {
                output.warn(&format!(
                    "{reason}. trying again in {} (try {next} of {})",
                    units::duration(wait),
                    net::ATTEMPTS
                ));
            }
        });
        let options = net::Options {
            offline: self.offline,
            jobs: self.jobs,
            credentials: source::credentials(&|name| std::env::var_os(name)),
        };
        Net::new(options, self.work.clone(), self.stop.clone(), notify)
    }
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
            .help(format!("unset it to use the default of {JOBS}"))
        })
}

/// the cache this machine uses, created on first use.
fn open_cache() -> Result<Cache, Error> {
    let root = Cache::locate(&|name| std::env::var_os(name)).ok_or_else(|| {
        Error::new(Kind::LocalIo, "could not find a folder for the cache")
            .help("set FETCHLOOM_CACHE to a folder fetchloom may use")
    })?;
    Cache::open(root.clone()).map_err(|err| {
        Error::new(
            Kind::LocalIo,
            format!("could not open the cache at {}: {err}", root.display()),
        )
        .help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

pub(crate) fn run() -> ExitCode {
    let started = Instant::now();
    let args = match args::Args::try_parse() {
        Ok(args) => args,
        Err(err) => return usage(&err),
    };
    let Some(command) = args.command else {
        let _ = writeln!(std::io::stdout().lock(), "{}", welcome());
        return ExitCode::SUCCESS;
    };
    let global = args.global;
    let level = match (global.quiet, global.verbose) {
        (true, _) => Level::Quiet,
        (false, 0) => Level::Normal,
        (false, 1) => Level::Verbose,
        (false, _) => Level::Debug,
    };
    let var = |name: &str| std::env::var_os(name);
    let style = Style::detect(global.color);
    let logs = logs::start(level, style.color, &var);
    let output = Arc::new(Output::new(style, level, global.json, logs.on_stderr()));
    let name = command.name();
    let context = match jobs(&var) {
        Ok(jobs) => Context {
            output,
            offline: global.offline || var("FETCHLOOM_OFFLINE").is_some_and(|value| value == "1"),
            yes: global.yes,
            jobs,
            work: Arc::default(),
            stop: Arc::default(),
        },
        Err(error) => {
            let context = Context {
                output,
                offline: false,
                yes: false,
                jobs: JOBS,
                work: Arc::default(),
                stop: Arc::default(),
            };
            return finish::<()>(&context, name, Err(error), started);
        }
    };
    if let Some(folder) = &global.directory
        && let Err(err) = std::env::set_current_dir(folder)
    {
        let error = Error::new(
            Kind::Usage,
            format!("cannot work in {}: {err}", folder.display()),
        );
        return finish::<()>(&context, name, Err(error), started);
    }
    match command {
        args::Command::Add(args) => {
            let outcome = block_on(&context, project::add(&context, args));
            finish(&context, name, outcome, started)
        }
        args::Command::Remove(args) => {
            let outcome = block_on(&context, project::remove(&context, args));
            finish(&context, name, outcome, started)
        }
        args::Command::Sync(args) => {
            let outcome = block_on(&context, project::sync(&context, args));
            finish(&context, name, outcome, started)
        }
        args::Command::Lock(args) => {
            let outcome = block_on(&context, project::lock(&context, args));
            finish(&context, name, outcome, started)
        }
        args::Command::Status => {
            let outcome = status::run(&context);
            finish(&context, name, outcome, started)
        }
        args::Command::Path(args) => {
            let outcome = block_on(&context, project::path(&context, args));
            finish(&context, name, outcome, started)
        }
        args::Command::Cache(args::Cache { action: None }) => {
            let outcome = cache::info(&context);
            finish(&context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Gc),
        }) => {
            let outcome = cache::gc(&context);
            finish(&context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Verify),
        }) => {
            let outcome = cache::verify(&context);
            finish(&context, name, outcome, started)
        }
        args::Command::Get(args) => {
            let outcome = block_on(&context, get::run(&context, args));
            finish(&context, name, outcome, started)
        }
    }
}

/// asks before a large download, unless `--yes` already said yes. without a terminal to ask on,
/// it stops and names the flag.
fn confirm(output: &Output, yes: bool, what: &str, bytes: u64) -> Result<(), Error> {
    if yes || bytes <= ASK_ABOVE {
        return Ok(());
    }
    let size = units::size(bytes);
    match output.confirm(&format!("download {what} ({size})?")) {
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
        .map_err(|err| Error::new(Kind::LocalIo, format!("could not start: {err}")))?;
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
    command: &str,
    outcome: Result<T, Error>,
    started: Instant,
) -> ExitCode {
    let elapsed = started.elapsed();
    let counts = context.work.counts();
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

fn welcome() -> String {
    format!(
        "fetchloom {}\n\nput the exact data a project needs onto any machine with one command.\n\n  fetchloom get <link>    fetch something once, without a project\n\n`fetchloom --help` lists every option.",
        env!("CARGO_PKG_VERSION")
    )
}

/// help and version go to stdout as clap writes them. a mistake on the command line becomes one
/// of our own errors, so it reads like every other failure.
fn usage(err: &clap::Error) -> ExitCode {
    if matches!(
        err.kind(),
        ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
    ) {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }
    let error = describe(err);
    Output::new(
        Style::detect(ColorChoice::Auto),
        Level::Normal,
        false,
        false,
    )
    .error(&error);
    ExitCode::from(error.kind.exit_code())
}

fn describe(err: &clap::Error) -> Error {
    let named = |context| match err.get(context) {
        Some(ContextValue::String(value)) => Some(value.clone()),
        Some(ContextValue::Strings(values)) => values.first().cloned(),
        _ => None,
    };
    let message = match (
        err.kind(),
        named(ContextKind::InvalidSubcommand),
        named(ContextKind::InvalidArg),
    ) {
        (ErrorKind::InvalidSubcommand, Some(command), _) => {
            format!("there is no command called \"{command}\"")
        }
        (ErrorKind::UnknownArgument, _, Some(arg)) => {
            format!("there is no option called \"{arg}\"")
        }
        (ErrorKind::MissingRequiredArgument, _, Some(arg)) => format!("{arg} is missing"),
        _ => first_line(err),
    };
    // a missing or wrong value is easiest to fix next to the usage line
    let usage = match err.get(ContextKind::Usage) {
        Some(ContextValue::StyledStr(text))
            if matches!(
                err.kind(),
                ErrorKind::MissingRequiredArgument | ErrorKind::InvalidValue
            ) =>
        {
            let text = text.to_string();
            let text = text.trim();
            Some(
                text.strip_prefix("Usage:")
                    .map_or(text, str::trim)
                    .to_owned(),
            )
        }
        _ => None,
    };
    let help = if let Some(command) = named(ContextKind::SuggestedSubcommand) {
        format!("did you mean `fetchloom {command}`?")
    } else if let Some(arg) = named(ContextKind::SuggestedArg) {
        format!("did you mean `{arg}`?")
    } else if let Some(ContextValue::Strings(values)) = err.get(ContextKind::ValidValue) {
        format!("use one of: {}", values.join(", "))
    } else if let Some(usage) = &usage {
        let command: Vec<&str> = usage
            .split_whitespace()
            .take_while(|word| !word.starts_with(['[', '<']))
            .collect();
        format!("`{} --help` says what it takes", command.join(" "))
    } else {
        "`fetchloom --help` lists every command and option".to_owned()
    };
    let error = Error::new(Kind::Usage, message);
    match usage {
        Some(usage) => error.detail(format!("usage: {usage}")),
        None => error,
    }
    .help(help)
}

fn first_line(err: &clap::Error) -> String {
    let text = err.to_string();
    let line = text.lines().next().unwrap_or_default();
    line.strip_prefix("error: ").unwrap_or(line).to_owned()
}
