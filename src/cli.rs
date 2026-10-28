//! the command line: read the arguments, run the command, return the exit code.

mod args;
mod cache;
mod fetch;
mod get;
mod logs;
mod project;
mod status;
mod sync;

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{ArgMatches, CommandFactory, Parser};
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
    /// a context with every setting at its default, printing through `output`.
    fn new(output: Output) -> Context {
        Context {
            output: Arc::new(output),
            offline: false,
            yes: false,
            jobs: JOBS,
            work: Arc::default(),
            stop: Arc::default(),
        }
    }

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
        Error::io(
            format_args!("could not open the cache at {}", root.display()),
            &err,
        )
        .or_help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

pub(crate) fn run() -> ExitCode {
    let started = Instant::now();
    let args = match args::Args::try_parse() {
        Ok(args) => args,
        Err(err) => return usage(&err, started),
    };
    let global = args.global;
    let level = level(global.quiet, global.verbose);
    let var = |name: &str| std::env::var_os(name);
    let style = Style::detect(global.color);
    let logs = logs::start(level, style.color, &var);
    let mut context = Context::new(Output::new(style, level, global.json, logs.on_stderr()));
    context.offline = global.offline || var("FETCHLOOM_OFFLINE").is_some_and(|value| value == "1");
    context.yes = global.yes;
    if args.version {
        return about(
            &context,
            &format!("fetchloom {}", env!("CARGO_PKG_VERSION")),
            started,
        );
    }
    let name = args.command.as_ref().map(args::Command::name);
    if let Some(folder) = &global.directory
        && let Err(err) = std::env::set_current_dir(folder)
    {
        let error = Error::new(
            Kind::Usage,
            format!("cannot work in {}: {err}", folder.display()),
        )
        .help("give -C a folder that exists and that you may open");
        return finish::<()>(&context, name, Err(error), started);
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
            &format!("fetchloom {}\n\n{WELCOME}", env!("CARGO_PKG_VERSION")),
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

/// what `fetchloom` alone prints outside a project, below the version. it ends with a command to
/// copy.
const WELCOME: &str = "  fetchloom add <link>    add a dataset, pin it and download it into data/
  fetchloom sync          make data/ match data.lock on this machine
  fetchloom status        see what changed
  fetchloom path <name>   print where a dataset is, for scripts
  fetchloom get <link>    fetch something once, without a project

<link> can be a DOI, a zenodo, figshare, dataverse, osf, hugging face or kaggle id, or any link you would paste in a browser.

get a first dataset into this folder:

  fetchloom add zenodo:3608990";

/// how much of the story `-q` and `-v` ask for.
fn level(quiet: bool, verbose: u8) -> Level {
    match (quiet, verbose) {
        (true, _) => Level::Quiet,
        (false, 0) => Level::Normal,
        (false, 1) => Level::Verbose,
        (false, _) => Level::Debug,
    }
}

/// help goes to stdout as clap writes it, even with `--json`, since it is for people. a mistake
/// on the command line becomes one of our own errors, printed like every other failure: in the
/// document `--json` asks for, or with the detail and colors the options clap read ask for.
fn usage(err: &clap::Error, started: Instant) -> ExitCode {
    if err.kind() == ErrorKind::DisplayHelp {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }
    let read = args::Args::command()
        .ignore_errors(true)
        .try_get_matches()
        .ok();
    let (level, color) = read
        .as_ref()
        .map_or((Level::Normal, ColorChoice::Auto), |read| {
            let quiet = read.get_flag("quiet");
            let color = read.get_one("color").copied().unwrap_or_default();
            (level(quiet, read.get_count("verbose")), color)
        });
    let output = Output::new(Style::detect(color), level, asks_for_json(), false);
    let command = read.as_ref().and_then(named_command);
    finish::<()>(
        &Context::new(output),
        command.as_deref(),
        Err(describe(err)),
        started,
    )
}

/// whether the command line asks for `--json`, read from the arguments themselves, since clap
/// stops at the first mistake and `--json` may come after it.
fn asks_for_json() -> bool {
    std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json")
}

/// the command `read` names, such as `cache` in `fetchloom cache gx`.
fn named_command(read: &ArgMatches) -> Option<String> {
    let mut names = Vec::new();
    let mut current = read;
    while let Some((name, inner)) = current.subcommand() {
        names.push(name);
        current = inner;
    }
    (!names.is_empty()).then(|| names.join(" "))
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
