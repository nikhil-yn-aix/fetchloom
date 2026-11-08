//! the command line: read the arguments, run the command, return the exit code.

mod args;
mod auth;
mod cache;
mod completions;
mod fetch;
mod get;
mod help;
mod list;
mod logs;
mod project;
mod search;
mod status;
mod sync;
mod usage;

use std::borrow::Cow;
use std::fmt::Write as _;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cache::{Access, Cache};
use crate::error::{Error, Kind};
use crate::net::{self, Net, Notice};
use crate::output::{Level, Output, Style};
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;
use crate::work::Work;

/// downloads running at once when `FETCHLOOM_JOBS` says nothing. downloads wait on the network,
/// not the processor, so the number does not follow the core count.
const JOBS: usize = 8;

/// a download larger than this asks first.
const ASK_ABOVE: u64 = 1024 * 1024 * 1024;

/// how long retries to one host stay quiet after the line that told one, so downloads that lost
/// the network together do not fill the screen with the same line.
const RETRY_LINES: Duration = Duration::from_secs(30);

/// what every command gets: where to print, the settings, the counters and the stop signal.
struct Context {
    output: Arc<Output>,
    offline: net::Offline,
    yes: bool,
    jobs: usize,
    /// tries after the first that a stalled request gets, which `FETCHLOOM_RETRIES` sets.
    retries: u32,
    rate: Option<Arc<net::Rate>>,
    threads: Threads,
    /// when a large download splits across streams, which `FETCHLOOM_SPLIT` sets.
    split: net::Splitting,
    work: Arc<Work>,
    stop: Arc<Stop>,
    /// the account a failed command needed, kept so a person at the terminal can sign in and
    /// run it again.
    needed: std::sync::Mutex<Option<crate::auth::Need>>,
    /// a key signed in with during this command but not saved, which the run again still uses.
    unsaved: std::sync::Mutex<Vec<net::Credential>>,
    /// whether this run has already warned about keys it cannot use safely or at all.
    cautioned: std::sync::atomic::AtomicBool,
}

impl Context {
    /// a context with every setting at its default, printing through `output`.
    fn new(output: Output) -> Context {
        Context {
            output: Arc::new(output),
            offline: net::Offline::No,
            yes: false,
            jobs: JOBS,
            retries: net::RETRIES,
            rate: None,
            threads: Threads::default(),
            split: net::Splitting::WhenSlow,
            work: Arc::default(),
            stop: Arc::default(),
            needed: std::sync::Mutex::default(),
            unsaved: std::sync::Mutex::default(),
            cautioned: std::sync::atomic::AtomicBool::default(),
        }
    }

    /// the network, with credentials from where each source keeps them, and retries reported as
    /// warnings.
    fn net(&self) -> Net {
        let mut credentials = self
            .unsaved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        credentials.extend(crate::auth::credentials(&crate::auth::env));
        self.caution(&credentials);
        self.net_with(credentials)
    }

    /// warns once per run about a `.netrc` other users can open and about keys no request header
    /// can carry, so a command that quietly signs in without them says why.
    fn caution(&self, credentials: &[net::Credential]) {
        if self
            .cautioned
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return;
        }
        if let Some(path) = crate::auth::netrc::path(&crate::auth::env)
            .filter(|path| crate::auth::netrc::too_open(path))
        {
            self.output.warn(&format!(
                "other users of this machine can open {}, which holds logins; limit it to yourself with `chmod 600 {}`",
                path.display(),
                path.display()
            ));
        }
        for credential in credentials {
            if reqwest::header::HeaderValue::from_str(&credential.value).is_err() {
                self.output.warn(&format!(
                    "a key for {} is never sent, since it holds a character no request header can carry, such as a line break; `fetchloom auth` shows where it is kept",
                    credential.host
                ));
            }
        }
    }

    /// the network with only `credentials`, as checking one key needs.
    fn net_with(&self, credentials: Vec<net::Credential>) -> Net {
        let output = self.output.clone();
        let lines = RetryLines::default();
        let notify: net::Notify = Arc::new(move |notice| match notice {
            Notice::Retrying {
                host,
                reason,
                wait,
                next,
                of,
            } => {
                if lines.tell(&host, Instant::now()) {
                    output.warn(&format!(
                        "{reason}. trying again in {} (try {next} of {of})",
                        units::duration(wait)
                    ));
                }
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
            credentials,
            split: self.split,
        };
        Net::new(options, self.work.clone(), self.stop.clone(), notify)
            .with_rate(self.rate.clone())
            .with_retries(self.retries)
    }
}

/// when each host's retries last got a line.
#[derive(Default)]
struct RetryLines {
    told: std::sync::Mutex<std::collections::HashMap<String, Instant>>,
}

impl RetryLines {
    /// whether a retry to `host` at `now` gets a line: the first does, and then one every
    /// [`RETRY_LINES`] while the trouble lasts.
    fn tell(&self, host: &str, now: Instant) -> bool {
        let mut told = self
            .told
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if told
            .get(host)
            .is_some_and(|at| now.saturating_duration_since(*at) < RETRY_LINES)
        {
            return false;
        }
        told.insert(host.to_owned(), now);
        true
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

/// when large downloads split, by `FETCHLOOM_SPLIT`: `0` never, `1` from the first answer, and
/// otherwise once one stream proves slow. splitting from the start lost in the lab on lan and
/// home, while it won on far, where one stream is slow.
fn splits(value: Option<std::ffi::OsString>) -> net::Splitting {
    match value {
        Some(value) if value == "0" => net::Splitting::Never,
        Some(value) if value == "1" => net::Splitting::Always,
        _ => net::Splitting::WhenSlow,
    }
}

/// `FETCHLOOM_JOBS`, or the default.
fn jobs(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Result<usize, Error> {
    number(var, "FETCHLOOM_JOBS", 1..=256, JOBS)
}

/// `FETCHLOOM_RETRIES`, or the default.
fn retries(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Result<u32, Error> {
    number(
        var,
        "FETCHLOOM_RETRIES",
        0..=net::MOST_RETRIES,
        net::RETRIES,
    )
}

/// the whole number the setting `name` holds, within `range`, or `default` when it is unset or
/// empty. surrounding whitespace is accepted.
fn number<T: std::str::FromStr + PartialOrd + std::fmt::Display>(
    var: &dyn Fn(&str) -> Option<std::ffi::OsString>,
    name: &str,
    range: std::ops::RangeInclusive<T>,
    default: T,
) -> Result<T, Error> {
    let Some(value) = var(name).filter(|value| !value.is_empty()) else {
        return Ok(default);
    };
    let (low, high) = (range.start(), range.end());
    value
        .to_str()
        .and_then(|text| text.trim().parse::<T>().ok())
        .filter(|number| range.contains(number))
        .ok_or_else(|| {
            Error::new(
                Kind::Usage,
                format!(
                    "{name} is \"{}\", which is not a number from {low} to {high}",
                    value.display()
                ),
            )
            .help(format!(
                "unset {name} to use the default of {default}, or set it to a number from {low} to {high}"
            ))
        })
}

/// the cache this machine uses, created on first use and held shared until the last clone is
/// dropped, so `cache gc` waits for this run.
fn open_cache() -> Result<Cache, Error> {
    let unset = Stop::default();
    let (output, stop) = match WAITS.get() {
        Some((output, stop)) => (Some(output.as_ref()), stop.as_ref()),
        None => (None, &unset),
    };
    hold_cache(open_unheld()?, Access::Shared, stop, &|| {
        if let Some(output) = output {
            output.step(
                "Waiting",
                "for `fetchloom cache gc` to finish with the cache",
            );
        }
    })
}

/// what a run that waits for the cache tells and listens to: the command's output and stop
/// signal, once they exist. `open_cache` is called where neither is at hand.
static WAITS: std::sync::OnceLock<(Arc<Output>, Arc<Stop>)> = std::sync::OnceLock::new();

/// holds `cache` for `access`, calling `waiting` once if another run makes this one wait.
fn hold_cache(
    cache: Cache,
    access: Access,
    stop: &Stop,
    waiting: &dyn Fn(),
) -> Result<Cache, Error> {
    let root = cache.root().to_path_buf();
    cache.hold(access, stop, waiting).map_err(|err| {
        if err.kind() == std::io::ErrorKind::Interrupted {
            return Error::new(
                Kind::Interrupted,
                "stopped while waiting for another fetchloom",
            );
        }
        Error::io(
            format_args!("could not lock the cache at {}", root.display()),
            &err,
        )
        .or_help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

/// the cache this machine uses, created on first use, without the cache-wide lock.
fn open_unheld() -> Result<Cache, Error> {
    let root = cache_root()?;
    if root.exists() && !root.is_dir() {
        return Err(Error::new(
            Kind::LocalIo,
            format!(
                "could not open the cache at {}: it is a file, not a folder",
                root.display()
            ),
        )
        .help("set FETCHLOOM_CACHE to a folder fetchloom may write to"));
    }
    Cache::open(root.clone()).map_err(|err| {
        Error::io(
            format_args!("could not open the cache at {}", root.display()),
            &err,
        )
        .or_help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

/// the cache this machine uses, for a dry run: read as it is, with no folder or file created, so
/// a missing cache or cache folder stands for an empty one.
fn peek_cache() -> Result<Cache, Error> {
    let root = cache_root()?;
    Cache::unopened(root.clone()).map_err(|err| {
        Error::io(
            format_args!("could not open the cache at {}", root.display()),
            &err,
        )
        .or_help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

fn cache_root() -> Result<std::path::PathBuf, Error> {
    Cache::locate(&|name| std::env::var_os(name)).ok_or_else(|| {
        Error::new(Kind::LocalIo, "could not find a folder for the cache")
            .help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    })
}

/// why fetchloom cannot start in `folder`, as `-C` asks. windows lets a program work in no folder
/// whose path passes 258 characters, so a project deeper than that is named with its length
/// before anything is written.
fn unusable(folder: &std::path::Path, err: &std::io::Error) -> Error {
    if err.kind() != std::io::ErrorKind::InvalidFilename {
        return Error::new(
            Kind::Usage,
            format!(
                "cannot work in {}: {}",
                folder.display(),
                crate::error::said(err)
            ),
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
    .help(match moved_command(std::env::args_os().skip(1)) {
        Some(command) => {
            format!("move the project to a folder with a shorter path, then run `{command}` there")
        }
        None => {
            "move the project to a folder with a shorter path, then run fetchloom from that folder"
                .to_owned()
        }
    })
}

pub(crate) fn run() -> ExitCode {
    let started = Instant::now();
    let args = match args::read() {
        Ok(args) => args,
        Err(err) => return usage::answer(&err, started),
    };
    let global = args.global;
    let level = level(global.quiet, global.verbose);
    let var = |name: &str| std::env::var_os(name);
    let style = Style::detect(global.color);
    let logs = logs::start(level, style.color, &var);
    let output = Output::new(style, level, global.json, logs.on_stderr())
        .without_hints(var("FETCHLOOM_NO_HINTS").is_some_and(|value| value == "1"));
    let mut context = Context::new(output);
    if let Some(refused) = &logs.refused {
        context.output.warn(refused);
    }
    context.offline = net::Offline::from_settings(
        global.offline,
        var("FETCHLOOM_OFFLINE").is_some_and(|value| value == "1"),
    );
    context.yes = global.yes;
    context.split = splits(var("FETCHLOOM_SPLIT"));
    if args.version {
        let mut text = format!("fetchloom {}", env!("CARGO_PKG_VERSION"));
        // what a bug report needs: which build, on which system
        if level >= Level::Verbose {
            let _ = write!(
                text,
                "\n{} {} \u{b7} commit {} \u{b7} cache {}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                option_env!("FETCHLOOM_COMMIT").unwrap_or("unknown"),
                crate::cache::Cache::locate(&var)
                    .map_or_else(|| "unknown".to_owned(), |root| root.display().to_string())
            );
        }
        return about(&context, "", &text, started);
    }
    if let Some(args::Command::Help(asked)) = &args.command {
        return help::show(&context, &asked.topic, started);
    }
    // a shell profile loads the script on every start, so no setting fetchloom reads for its
    // work may stop it
    if let Some(args::Command::Completions(asked)) = &args.command {
        return completions::print(&context, asked.shell, started);
    }
    let name = args.command.as_ref().map(args::Command::name);
    let _command = tracing::debug_span!("command", name).entered();
    if let Some(folder) = &global.directory
        && let Err(err) = std::env::set_current_dir(folder)
    {
        return finish::<()>(&context, name, Err(unusable(folder, &err)), started);
    }
    match Threads::read(&var) {
        Ok(threads) => context.threads = threads,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    }
    let Some(command) = args.command else {
        return execute(context, None, started);
    };
    match jobs(&var) {
        Ok(jobs) => context.jobs = jobs,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    }
    match retries(&var) {
        Ok(retries) => context.retries = retries,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    }
    match net::Rate::read(&var) {
        Ok(rate) => context.rate = rate,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    }
    execute(context, Some(command), started)
}

fn execute(context: Context, command: Option<args::Command>, started: Instant) -> ExitCode {
    let name = command.as_ref().map(args::Command::name);
    let context = Arc::new(context);
    let runtime = match command_runtime(&context) {
        Ok(runtime) => runtime,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    };
    let entered = runtime.enter();
    let _ = WAITS.set((Arc::clone(&context.output), Arc::clone(&context.stop)));
    let mut outcome = match command.clone() {
        Some(command) => dispatch(&context, command, started),
        None => alone(&context, started),
    };
    if outcome == ExitCode::SUCCESS && command.as_ref().is_some_and(reminds) {
        cache::remind(&context);
    }
    // refused for want of an account, with a person at the terminal: offer to sign in, then run
    // the command once more with the new key
    let needed = context
        .needed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let (Some(need), Some(command)) = (needed, command) {
        match block_on(&context, auth::offer(&context, &need)) {
            Ok(true) => outcome = dispatch(&context, command, Instant::now()),
            Ok(false) => {}
            Err(error) => context.output.error(&error),
        }
    }
    drop(entered);
    // status is read-only, so an interrupted scan need not delay process exit.
    runtime.shutdown_background();
    outcome
}

/// whether `command`, as help quotes it, is a whole command line fetchloom would accept, so a
/// script handed it as the next step can run it. `fetchloom add` alone names a command and is not one.
pub(crate) fn runs(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .is_some_and(|words| args::Args::try_parse_from(words).is_ok())
}

/// reads the literal quoting in suggestions without interpreting expansions or shell operators.
fn command_words(command: &str, windows: bool) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(delimiter) = quote {
            if c == delimiter {
                if windows && c == '\'' && chars.peek() == Some(&'\'') {
                    chars.next();
                    word.get_or_insert_default().push(c);
                } else {
                    quote = None;
                }
            } else if delimiter == '"' && c == '\\' && !windows {
                let next = *chars.peek()?;
                if matches!(next, '"' | '\\' | '$' | '\u{60}') {
                    chars.next();
                    word.get_or_insert_default().push(next);
                } else {
                    word.get_or_insert_default().push(c);
                }
            } else if delimiter == '"' && matches!(c, '$' | '\u{60}') {
                return None;
            } else {
                word.get_or_insert_default().push(c);
            }
        } else {
            match c {
                '\'' | '"' => {
                    quote = Some(c);
                    word.get_or_insert_default();
                }
                '\n' | '\r' | '|' | '&' | ';' | '<' | '>' | '$' | '\u{60}' | '(' | ')' | '*'
                | '?' | '[' | ']' => return None,
                c if c.is_whitespace() => words.extend(word.take()),
                '\\' if !windows => word.get_or_insert_default().push(chars.next()?),
                _ => word.get_or_insert_default().push(c),
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    words.extend(word);
    Some(words)
}

/// commands after which the cache may remind a person what no project needs: they change what
/// projects need, and a dry run changes nothing at all.
fn reminds(command: &args::Command) -> bool {
    match command {
        args::Command::Add(args) => !args.dry.dry_run,
        args::Command::Remove(args) => !args.dry.dry_run,
        args::Command::Sync(args) => !args.dry.dry_run,
        args::Command::Get(_) => true,
        _ => false,
    }
}

fn dispatch(context: &Arc<Context>, command: args::Command, started: Instant) -> ExitCode {
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
        args::Command::Status(args) => {
            let outcome = blocking(context, move |context| status::run(context, args.verify));
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
        args::Command::List(args) => {
            let outcome = blocking(context, move |context| {
                list::run(context, args.name.as_deref())
            });
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Gc(gc)),
        }) => {
            let outcome = cache::gc(context, gc.now, gc.dry.dry_run);
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
        args::Command::Search(args) => {
            let outcome = block_on(context, search::run(context, &args));
            finish(context, name, outcome, started)
        }
        args::Command::Auth(args) => {
            let outcome = block_on(context, auth::run(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Completions(asked) => completions::print(context, asked.shell, started),
        args::Command::Help(asked) => help::show(context, &asked.topic, started),
    }
}

/// what `--version` and `fetchloom` alone report in `--json`.
#[derive(Serialize)]
struct About {
    version: &'static str,
}

/// prints `text` for a person, after `banner`, or the version in the document `--json` asked for.
fn about(context: &Context, banner: &str, text: &str, started: Instant) -> ExitCode {
    context.output.answer(&format!(
        "{banner}{}",
        crate::output::help_text(text, Style::stdout_color(args::color()))
    ));
    let about = About {
        version: env!("CARGO_PKG_VERSION"),
    };
    finish(context, None, Ok(about), started)
}

/// `fetchloom` alone: a short status inside a project, and the welcome outside one.
fn alone(context: &Arc<Context>, started: Instant) -> ExitCode {
    if let Ok(project) = project::find(&context.output) {
        let outcome = blocking(context, move |context| status::brief(context, &project));
        return finish(context, Some("status"), outcome, started);
    }
    let banner = context
        .output
        .answer_width()
        .map_or_else(String::new, |width| {
            crate::output::wordmark(width, Style::stdout_color(args::color()))
        });
    about(
        context,
        &banner,
        &format!("fetchloom {}\n\n{}", env!("CARGO_PKG_VERSION"), args::INTRO),
        started,
    )
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
    let cancel = format!("the download of {what} ({size})");
    match output
        .confirm(&format!("download {what} ({size})?"))
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => {
            Err(Error::new(Kind::Unconfirmed, format!("did not download {what}")).cancelled(cancel))
        }
        None => Err(Error::new(
            Kind::Unconfirmed,
            format!("downloading {what} needs {size}, and fetchloom cannot ask you here"),
        )
        .help("add --yes to download it without being asked")),
    }
}

/// installs interruption handling before either synchronous or asynchronous work starts.
fn command_runtime(context: &Arc<Context>) -> Result<tokio::runtime::Runtime, Error> {
    let workers = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    let runtime = crate::threads::start_runtime(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(context.threads.limit(workers))
            .enable_all()
            .build()
    })
    .map_err(|err| Error::io("could not start", &err))?;
    let mut signal = Box::pin(tokio::signal::ctrl_c());
    // polling once installs the OS listener before a synchronous scan can occupy this thread.
    let already_stopped = runtime
        .block_on(std::future::poll_fn(|cx| {
            std::task::Poll::Ready(match signal.as_mut().poll(cx) {
                std::task::Poll::Pending => Ok(false),
                std::task::Poll::Ready(result) => result.map(|()| true),
            })
        }))
        .map_err(|err| Error::io("could not listen for ctrl-c", &err))?;
    if already_stopped {
        context.stop.ask();
    }
    let stopping = Arc::clone(context);
    let quitting = Arc::clone(context);
    runtime.spawn(interrupts(
        signal,
        already_stopped,
        tokio::signal::ctrl_c(),
        move || {
            stopping.stop.ask();
            stopping.output.stopping();
        },
        move || {
            quitting.output.abandon();
            std::process::exit(130);
        },
    ));
    Ok(runtime)
}

/// what ctrl-c does: at the first press, or at once when one came `already`, `stopping` asks the
/// work to wind down; at the `next`, `quitting` ends the run now.
async fn interrupts(
    first: impl Future<Output = std::io::Result<()>>,
    already: bool,
    next: impl Future<Output = std::io::Result<()>>,
    stopping: impl FnOnce(),
    quitting: impl FnOnce(),
) {
    if already || first.await.is_ok() {
        stopping();
        if next.await.is_ok() {
            quitting();
        }
    }
}

fn block_on<T>(
    context: &Context,
    command: impl Future<Output = Result<T, Error>>,
) -> Result<T, Error> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|err| Error::new(Kind::LocalIo, format!("cannot run the command: {err}")))?;
    let outcome = runtime.block_on(command);
    // a command that stopped says itself what it left behind
    if outcome
        .as_ref()
        .is_err_and(|error| error.kind == Kind::Interrupted)
    {
        return outcome;
    }
    // work a stop cut short may still end well, so it ends as stopped, unless the command had
    // finished it before the stop was asked
    context.stop.check_unfinished()?;
    outcome
}

fn blocking<T: Send + 'static>(
    context: &Arc<Context>,
    command: impl FnOnce(&Context) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let owned = Arc::clone(context);
    block_on(context, async {
        let task = tokio::task::spawn_blocking(move || command(&owned));
        tokio::select! {
            biased;
            () = context.stop.asked() => Err(Stop::stopped()),
            outcome = task => outcome.map_err(|err| Error::new(Kind::LocalIo, format!("could not finish the scan: {err}")))?,
        }
    })
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
    let printed = context
        .output
        .document(command, outcome.as_ref(), counts, elapsed);
    let outcome = outcome.and_then(|result| printed.map(|()| result));
    if let Err(error) = &outcome {
        context.output.error(error);
        if let Some(need) = &error.need
            && context.output.can_ask()
        {
            *context
                .needed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((**need).clone());
        }
    }
    // a run that did no work, such as `--version`, has no counters worth a line
    if command.is_some() || counts != crate::work::Counts::default() {
        context.output.summary(counts, elapsed);
    }
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

/// quotes a word so suggested commands preserve spaces, patterns and shell metacharacters.
fn shell_word(word: &str) -> Cow<'_, str> {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,~".contains(c))
    {
        Cow::Borrowed(word)
    } else if word.contains(['\'', '"', '$', '\u{60}', '\\']) {
        let quoted = if cfg!(windows) {
            word.replace('\'', "''")
        } else {
            word.replace('\'', "'\"'\"'")
        };
        Cow::Owned(format!("'{quoted}'"))
    } else {
        Cow::Owned(format!("\"{word}\""))
    }
}

/// retry from the moved project, without the old working directory or its attached forms.
fn moved_command(mut args: impl Iterator<Item = std::ffi::OsString>) -> Option<String> {
    let command = <args::Args as clap::CommandFactory>::command();
    let mut kept = Vec::new();
    let mut positional = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            positional = true;
        }
        if !positional
            && let Some(word) = arg.to_str()
            && word.starts_with('-')
            && !word.starts_with("--")
            && let Some(at) = word.find('C')
            && word[1..at].chars().all(|short| {
                command
                    .get_arguments()
                    .any(|arg| arg.get_short() == Some(short) && !arg.get_action().takes_values())
            })
        {
            if at > 1 {
                kept.push(std::ffi::OsString::from(&word[..at]));
            }
            if at + 1 == word.len() {
                args.next();
            }
        } else {
            kept.push(arg);
        }
    }
    command_line(kept.into_iter())
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

    /// a stop asked once a command marked its work as finishing, as by ctrl-c while it prints its
    /// last lines, leaves it finished, while one asked before the mark ends it as stopped.
    #[test]
    fn a_stop_after_a_command_finished_its_work_leaves_it_finished() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let quiet = || {
            Context::new(Output::new(
                crate::output::Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                },
                Level::Quiet,
                false,
                false,
            ))
        };
        let finished = quiet();
        let outcome = block_on(&finished, async {
            finished.stop.finish();
            finished.stop.ask();
            Ok(())
        });
        assert!(outcome.is_ok());
        let unfinished = quiet();
        let outcome = block_on(&unfinished, async {
            unfinished.stop.ask();
            Ok(())
        });
        assert_eq!(outcome.unwrap_err().kind, Kind::Interrupted);
    }

    #[test]
    fn a_key_no_header_can_carry_is_told_once_per_run() {
        let context = Context::new(Output::new(
            crate::output::Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            true,
            false,
        ));
        context.unsaved.lock().unwrap().push(net::Credential {
            host: "lab.example.org".to_owned(),
            header: "authorization",
            value: "Bearer abc\ndef".to_owned(),
            every_hop: false,
        });
        let _ = context.net();
        let _ = context.net();
        let told = context.output.told();
        let about = told
            .iter()
            .filter(|text| text.contains("a key for lab.example.org is never sent"))
            .count();
        assert_eq!(about, 1, "{told:?}");
        assert!(!told.iter().any(|text| text.contains("abc")), "{told:?}");
    }

    #[test]
    fn the_first_ctrl_c_stops_the_work_and_the_second_quits() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wait = Duration::from_millis(20);
        let told = std::sync::Mutex::new(Vec::new());
        let tell = |what: &'static str| told.lock().unwrap().push(what);
        runtime.block_on(async {
            let (first, pressed_first) = tokio::sync::oneshot::channel::<()>();
            let (next, pressed_next) = tokio::sync::oneshot::channel::<()>();
            let listening = interrupts(
                async { pressed_first.await.map_err(std::io::Error::other) },
                false,
                async { pressed_next.await.map_err(std::io::Error::other) },
                || tell("stopping"),
                || tell("quitting"),
            );
            tokio::pin!(listening);
            assert!(tokio::time::timeout(wait, &mut listening).await.is_err());
            assert!(told.lock().unwrap().is_empty());
            first.send(()).unwrap();
            assert!(tokio::time::timeout(wait, &mut listening).await.is_err());
            assert_eq!(*told.lock().unwrap(), ["stopping"]);
            next.send(()).unwrap();
            listening.await;
        });
        assert_eq!(*told.lock().unwrap(), ["stopping", "quitting"]);

        told.lock().unwrap().clear();
        runtime.block_on(interrupts(
            std::future::pending(),
            true,
            async { Ok(()) },
            || tell("stopping"),
            || tell("quitting"),
        ));
        assert_eq!(*told.lock().unwrap(), ["stopping", "quitting"]);
    }

    #[test]
    fn a_key_kept_out_of_files_still_serves_the_command() {
        let context = Context::new(Output::new(
            crate::output::Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
            },
            Level::Quiet,
            false,
            false,
        ));
        context.unsaved.lock().unwrap().push(net::Credential {
            host: "unsaved.example.org".to_owned(),
            header: "authorization",
            value: "Bearer k".to_owned(),
            every_hop: false,
        });
        assert!(
            context
                .net()
                .credential_hosts()
                .contains(&"unsaved.example.org")
        );
    }

    #[test]
    fn moved_commands_keep_other_option_values_and_literal_arguments() {
        let line = |args: &[&str]| moved_command(args.iter().map(std::ffi::OsString::from));
        assert_eq!(
            line(&[
                "-C",
                "old",
                "get",
                "https://example.org/data.csv",
                "-oCaseStudy"
            ])
            .as_deref(),
            Some("fetchloom get https://example.org/data.csv -oCaseStudy")
        );
        assert_eq!(
            line(&["-qvCold", "sync", "--offline"]).as_deref(),
            Some("fetchloom -qv sync --offline")
        );
        assert_eq!(
            line(&["-C", "old", "get", "--", "-CaseStudy"]).as_deref(),
            Some("fetchloom get -- -CaseStudy")
        );
        assert_eq!(
            line(&[
                "-C",
                "old",
                "get",
                "https://example.org/data.csv?token=secret"
            ]),
            None
        );
    }

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
    fn suggested_commands_keep_shell_metacharacters_literal() {
        assert_eq!(shell_word("$notes"), "'$notes'");
        assert_eq!(shell_word("a\"b"), "'a\"b'");
        assert_eq!(shell_word(r"data\notes"), r"'data\notes'");
        assert_eq!(
            shell_word("a'b"),
            if cfg!(windows) {
                "'a''b'"
            } else {
                "'a'\"'\"'b'"
            }
        );
    }

    #[test]
    fn downloads_split_once_one_stream_is_slow_unless_told_otherwise() {
        use net::Splitting::{Always, Never, WhenSlow};
        assert_eq!(splits(None), WhenSlow);
        assert_eq!(splits(Some("0".into())), Never);
        assert_eq!(
            splits(Some("1".into())),
            Always,
            "kept to measure splitting from the start"
        );
        for other in ["", "2", "yes", " 1"] {
            assert_eq!(splits(Some(other.into())), WhenSlow, "{other:?}");
        }
    }

    #[test]
    fn retries_come_from_the_setting_within_its_range() {
        let setting = |value: &'static str| {
            move |name: &str| (name == "FETCHLOOM_RETRIES").then(|| value.into())
        };
        assert_eq!(retries(&|_| None).unwrap(), net::RETRIES);
        assert_eq!(retries(&setting("")).unwrap(), net::RETRIES);
        assert_eq!(retries(&setting("0")).unwrap(), 0);
        assert_eq!(retries(&setting(" 12 ")).unwrap(), 12);
        assert_eq!(retries(&setting("20")).unwrap(), net::MOST_RETRIES);
        for bad in ["21", "-1", "many", "1.5"] {
            let error = retries(&setting(bad)).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{bad}");
            assert_eq!(
                error.message,
                format!("FETCHLOOM_RETRIES is \"{bad}\", which is not a number from 0 to 20")
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "unset FETCHLOOM_RETRIES to use the default of 4, or set it to a number from 0 to 20"
                )
            );
        }
    }

    #[test]
    fn retries_get_one_line_per_host_every_half_minute_at_most() {
        let lines = RetryLines::default();
        let start = Instant::now();
        let at = |seconds| start + Duration::from_secs(seconds);
        assert!(lines.tell("huggingface.co", at(0)));
        assert!(
            !lines.tell("huggingface.co", at(3)),
            "a second download's retry"
        );
        assert!(
            lines.tell("zenodo.org", at(3)),
            "another host has its own line"
        );
        assert!(!lines.tell("huggingface.co", at(29)));
        assert!(
            lines.tell("huggingface.co", at(30)),
            "a long outage is told again"
        );
    }

    #[test]
    fn list_obeys_an_existing_stop() {
        const CHILD: &str = "FETCHLOOM_TEST_CLI_STOPPED_LIST";
        if let Some(root) = std::env::var_os(CHILD) {
            std::env::set_current_dir(root).unwrap();
            let context = Arc::new(Context::new(Output::new(
                Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                },
                Level::Quiet,
                true,
                false,
            )));
            let runtime = command_runtime(&context).unwrap();
            let entered = runtime.enter();
            context.stop.ask();
            let code = dispatch(
                &context,
                args::Command::List(args::List { name: None }),
                Instant::now(),
            );
            assert_eq!(code, ExitCode::from(130));
            drop(entered);
            runtime.shutdown_background();
            return;
        }
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(folder.path().join("data.toml"), "[datasets]\n").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::tests::list_obeys_an_existing_stop"])
            .env(CHILD, folder.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[test]
    fn quoted_retry_commands_are_runnable() {
        for command in [
            r#"fetchloom -C "my project" status"#,
            "fetchloom -C 'my project' list",
            "fetchloom status",
        ] {
            assert!(runs(command), "{command}");
        }
        assert!(!runs("fetchloom -C 'an unfinished quote status"));
    }
    #[test]
    fn retry_words_preserve_literal_quoting() {
        for windows in [false, true] {
            assert_eq!(
                command_words(r#"fetchloom -C "my project" status"#, windows).unwrap(),
                ["fetchloom", "-C", "my project", "status"],
            );
            assert_eq!(
                command_words("fetchloom -C '' status", windows).unwrap(),
                ["fetchloom", "-C", "", "status"],
            );
            for command in [
                "fetchloom get a | fetchloom get b",
                "fetchloom get $dataset",
                "fetchloom get a; fetchloom get b",
                "fetchloom get a\nfetchloom get b",
                "fetchloom -C \"$directory\" status",
            ] {
                assert!(command_words(command, windows).is_none(), "{command}");
            }
        }
        assert_eq!(
            command_words(r#"fetchloom -C 'ada'"'"'s project' status"#, false).unwrap(),
            ["fetchloom", "-C", "ada's project", "status"],
        );
        assert_eq!(
            command_words("fetchloom -C 'ada''s project' status", true).unwrap(),
            ["fetchloom", "-C", "ada's project", "status"],
        );
    }
}
