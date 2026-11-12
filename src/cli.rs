//! the command line: read the arguments, run the command, return the exit code.

mod add;
mod args;
mod auth;
mod build;
mod cache;
mod completions;
mod drop;
mod export;
mod fetch;
mod get;
mod help;
mod import;
mod links;
mod list;
mod logs;
mod path;
mod project;
mod push;
mod remove;
mod search;
mod settings;
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
use crate::machine::Machine;
use crate::net::{self, Net, Notice};
use crate::output::{Interrupt, Level, Output, Style, Warning};
use crate::settings::{File, Settings};
use crate::stop::{Heard, Stop};
use crate::threads::Threads;
use crate::units;
use crate::work::Work;

pub(crate) use project::command;

/// how long retries to one host stay quiet after the line that told one, so downloads that lost
/// the network together do not fill the screen with the same line.
const RETRY_LINES: Duration = Duration::from_secs(30);

/// what every command gets: where to print, the settings, the counters and the stop signal.
struct Context {
    output: Arc<Output>,
    offline: net::Offline,
    yes: bool,
    settings: Settings,
    rate: Option<Arc<net::Rate>>,
    threads: Threads,
    /// when a large download splits across streams, which `FETCHLOOM_SPLIT` sets.
    split: net::Splitting,
    work: Arc<Work>,
    stop: Arc<Stop>,
    /// a failed command's refusal that signing in may lift, kept so a person at the terminal
    /// can sign in and run it again.
    needed: std::sync::Mutex<Option<Refusal>>,
    /// a key signed in with during this command but not saved, which the run again still uses.
    unsaved: std::sync::Mutex<Vec<net::Credential>>,
    /// whether this run has already warned about keys it cannot use safely or at all.
    cautioned: std::sync::atomic::AtomicBool,
    /// the mirrors asked for pinned files before their sources, and what they answered.
    mirrors: fetch::mirrors::Mirrored,
    /// the project whose `fetchloom build` runs this one inside a step, from
    /// `FETCHLOOM_BUILDING`, which may not wait for that project's lock the build holds.
    building: Option<std::path::PathBuf>,
    /// the parts of file lists this run wrote anew into a project's data.lock.d, with the
    /// project's folder, so the run can end by naming those git does not have.
    lists: std::sync::Mutex<Vec<(std::path::PathBuf, String)>>,
}

/// a refusal that signing in may lift, which a person at the terminal is offered to sign in for
/// once the command ends.
struct Refusal {
    error: Error,
    /// whether the error was told already, as `-q` tells it, above the offer.
    told: bool,
}

impl Context {
    /// takes the refusal the command ended with, if signing in may lift it.
    fn refusal(&self) -> Option<Refusal> {
        self.needed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }

    /// a context with every setting at its default, printing through `output`.
    fn new(output: Output) -> Context {
        Context {
            output: Arc::new(output),
            offline: net::Offline::No,
            yes: false,
            settings: Settings::defaults(Machine::default()),
            rate: None,
            threads: Threads::default(),
            split: net::Splitting::WhenSlow,
            work: Arc::default(),
            stop: Arc::default(),
            needed: std::sync::Mutex::default(),
            unsaved: std::sync::Mutex::default(),
            cautioned: std::sync::atomic::AtomicBool::default(),
            mirrors: fetch::mirrors::Mirrored::default(),
            building: None,
            lists: std::sync::Mutex::default(),
        }
    }

    /// notes the parts of file lists `written` anew below the project at `root`.
    fn wrote_lists(&self, root: &std::path::Path, written: Vec<String>) {
        self.lists
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend(written.into_iter().map(|part| (root.to_path_buf(), part)));
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
            self.output.warn_with(
                Warning::AuthReadable,
                &format!(
                    "other users of this machine can open {}, which holds logins",
                    path.display()
                ),
                &format!("limit it to yourself with `chmod 600 {}`", path.display()),
            );
        }
        for credential in credentials {
            if reqwest::header::HeaderValue::from_str(&credential.value).is_err() {
                self.output.warn_with(
                    Warning::AuthKeyUnsendable,
                    &format!(
                        "a key for {} is never sent, since it holds a character no request header can carry, such as a line break",
                        credential.host
                    ),
                    "`fetchloom auth` shows where it is kept",
                );
            }
        }
    }

    /// the `allowed_origins` setting, with where a person changes it.
    fn allowed(&self) -> net::Allowed {
        let allowed = &self.settings.allowed_origins;
        net::Allowed {
            origins: allowed.value.clone(),
            by_variable: allowed.origin == crate::settings::Origin::Environment,
            file: self.settings.file.path.clone(),
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
                    output.warn(
                        Warning::NetworkRetry,
                        &format!(
                            "{reason}. trying again in {} (try {next} of {of})",
                            units::duration(wait)
                        ),
                    );
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
            jobs: self.settings.jobs.value,
            credentials,
            split: self.split,
        };
        let signing = crate::source::Signing::new(
            self.settings.s3_buckets.value.clone(),
            Arc::new(crate::auth::env),
        );
        let google = net::Google::new(
            self.settings.gcs_buckets.value.clone(),
            Arc::new(crate::auth::env),
        )
        .with_drive(self.settings.drive_folders.value.clone());
        #[cfg(debug_assertions)]
        let google = google.with_token_endpoint(net::test_token_endpoint());
        let azure = net::Azure::new(
            self.settings.azure_containers.value.clone(),
            Arc::new(crate::auth::env),
        );
        let net = Net::new(options, self.work.clone(), self.stop.clone(), notify)
            .with_memory(self.settings.memory.value)
            .with_rate(self.rate.clone())
            .with_allowed_origins(self.allowed())
            .with_signing(signing)
            .with_google(google)
            .with_azure(azure)
            .with_retries(self.settings.retries.value)
            .with_host_jobs(self.settings.host_jobs.value)
            .with_timeout(self.settings.timeout.value);
        let net = if self.mirrors.mirrors.only {
            net.with_mirrors_only(self.mirrors.mirrors.web().cloned().collect())
        } else {
            net
        };
        // a person named these mirrors, as they type a link, so a lab server on their network
        // may be one. each mirror's origin is trusted, and no other port or scheme of its host
        net.trust(self.mirrors.mirrors.web().map(url::Url::origin));
        net
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
/// the project's folder `root` as a path from `here`, a folder inside it: `../` for each folder
/// between them, so a command a help line names works where the person ran fetchloom.
pub(crate) fn up_to(root: &std::path::Path, here: &std::path::Path) -> String {
    let depth = here
        .strip_prefix(root)
        .map_or(0, |inside| inside.components().count());
    "../".repeat(depth)
}

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

/// whether `FETCHLOOM_OFFLINE`, as `value` holds it, asks for offline mode: `true` or `1`, and
/// `false`, `0`, unset or empty for no, as the settings read a switch. any other value fails
/// rather than letting a run meant to be offline go online.
fn offline(value: Option<&std::ffi::OsStr>) -> Result<bool, Error> {
    switch(
        "FETCHLOOM_OFFLINE",
        value,
        "set FETCHLOOM_OFFLINE to true to work offline, or unset it to work online",
    )
}

/// reads the on/off variables a named command needs before its settings into `context`: offline
/// mode, which `flag`, `--offline`, also asks, how large downloads split, and whether keys may go
/// to the credential store. a value that is neither true nor false stops the run before any
/// request.
fn read_switches(
    context: &mut Context,
    flag: bool,
    var: crate::settings::Var<'_>,
) -> Result<(), Error> {
    let environment = offline(var("FETCHLOOM_OFFLINE").as_deref())?;
    context.offline = net::Offline::from_settings(flag, environment);
    context.split = splits(var("FETCHLOOM_SPLIT").as_deref())?;
    keyring(var("FETCHLOOM_KEYRING").as_deref())
}

/// the settings file this run reads, or leaves unread as `--no-config`, when `flag` says it was
/// given, or `FETCHLOOM_NO_CONFIG` asks.
fn settings_file(flag: bool, var: crate::settings::Var<'_>) -> Result<File, Error> {
    match no_config(flag, var("FETCHLOOM_NO_CONFIG").as_deref())? {
        Some(by) => Ok(File::bypassed(var, by)),
        None => File::read(var),
    }
}

/// what leaves the settings file unread for this run: `--no-config`, as `flag` says, else
/// `FETCHLOOM_NO_CONFIG`, which takes true or false as `FETCHLOOM_OFFLINE` does.
fn no_config(flag: bool, value: Option<&std::ffi::OsStr>) -> Result<Option<&'static str>, Error> {
    if flag {
        return Ok(Some("--no-config"));
    }
    let bypassed = switch(
        "FETCHLOOM_NO_CONFIG",
        value,
        "set FETCHLOOM_NO_CONFIG to true to leave the settings file unread, or unset it",
    )?;
    Ok(bypassed.then_some("FETCHLOOM_NO_CONFIG"))
}

/// the variable `name`'s `value` as true or false, read as every on/off variable is: unset or
/// empty is false, and anything but true, false, 1 or 0 is a usage error whose help is `help`.
fn switch(name: &str, value: Option<&std::ffi::OsStr>, help: &str) -> Result<bool, Error> {
    crate::settings::switched(name, value)
        .map(|on| on.unwrap_or(false))
        .map_err(|error| error.help(help))
}

/// when large downloads split, by `FETCHLOOM_SPLIT`: false never, true from the first answer,
/// and unset or empty once one stream proves slow; any other value is a usage error. splitting
/// from the start lost in the lab on lan and home, while it won on far, where one stream is slow.
fn splits(value: Option<&std::ffi::OsStr>) -> Result<net::Splitting, Error> {
    let split = crate::settings::switched("FETCHLOOM_SPLIT", value).map_err(|error| {
        error.help("set FETCHLOOM_SPLIT to true to split large downloads from the start or false to never split them, or unset it to split one once it proves slow")
    })?;
    Ok(match split {
        Some(true) => net::Splitting::Always,
        Some(false) => net::Splitting::Never,
        None => net::Splitting::WhenSlow,
    })
}

/// refuses a `FETCHLOOM_KEYRING` that is neither true nor false before any work, as a setting
/// that cannot be read stops every named command, though only `auth login` saves keys.
fn keyring(value: Option<&std::ffi::OsStr>) -> Result<(), Error> {
    switch(
        "FETCHLOOM_KEYRING",
        value,
        "set FETCHLOOM_KEYRING to false to keep keys out of the system's credential store, or unset it",
    )
    .map(|_| ())
}

/// the cache this machine uses, created on first use and held shared until the last clone is
/// dropped, so `cache gc` waits for this run.
fn open_cache() -> Result<Cache, Error> {
    let unset = Stop::default();
    let (output, stop) = match WAITS.get() {
        Some((output, stop)) => (Some(output.as_ref()), stop.as_ref()),
        None => (None, &unset),
    };
    let _setup = output.map(|output| output.setup("Opening", "the cache"));
    // counts its seconds on a terminal until the cache is held
    let waiting = std::sync::Mutex::new(None);
    hold_cache(open_unheld()?, Access::Shared, stop, &|| {
        if let Some(output) = output {
            *waiting
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(output.waiting(
                "Waiting",
                "for `fetchloom cache gc` to finish with the cache",
            ));
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
        moved_by_variable(
            Error::io(
                format_args!("could not lock the cache at {}", root.display()),
                &err,
            ),
            &err,
        )
    })
}

/// the settings file this run read, kept for the places that open the cache without a
/// [`Context`].
static SETTINGS_FILE: std::sync::OnceLock<File> = std::sync::OnceLock::new();

/// the help for a cache folder that could not be used. a user may not own the folder fetchloom
/// chose itself, as when a container gives an arbitrary user a home of `/`, so for that folder
/// the help names the variable that moves the cache as well as its permissions.
fn moved_by_variable(error: Error, err: &std::io::Error) -> Error {
    let chosen = std::env::var_os("FETCHLOOM_CACHE").is_some_and(|value| !value.is_empty());
    let filed = SETTINGS_FILE
        .get()
        .filter(|file| !chosen && file.cache().is_some())
        .and_then(|file| file.path.as_deref());
    if let Some(file) = filed {
        error.or_help(format!(
            "change cache in the settings file {}, or set FETCHLOOM_CACHE to a folder fetchloom may write to",
            file.display()
        ))
    } else if !chosen && err.kind() == std::io::ErrorKind::PermissionDenied {
        error.help("check its permissions, or set FETCHLOOM_CACHE to a folder fetchloom may write to, then run the command again")
    } else {
        error.or_help("set FETCHLOOM_CACHE to a folder fetchloom may write to")
    }
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
        moved_by_variable(
            Error::io(
                format_args!("could not open the cache at {}", root.display()),
                &err,
            ),
            &err,
        )
    })
}

/// the cache this machine uses, for a dry run: read as it is, with no folder or file created, so
/// a missing cache or cache folder stands for an empty one.
fn peek_cache() -> Result<Cache, Error> {
    let root = cache_root()?;
    Cache::unopened(root.clone()).map_err(|err| {
        moved_by_variable(
            Error::io(
                format_args!("could not open the cache at {}", root.display()),
                &err,
            ),
            &err,
        )
    })
}

/// the cache folder: `FETCHLOOM_CACHE`, else the settings file's `cache`, else the platform's.
fn cache_folder() -> Option<std::path::PathBuf> {
    let filed = SETTINGS_FILE.get().and_then(File::cache);
    Cache::locate(&|name| std::env::var_os(name), filed.as_deref())
}

fn cache_root() -> Result<std::path::PathBuf, Error> {
    cache_folder().ok_or_else(|| {
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

/// the words of this run's command line after the program's name, as typed, kept for help that
/// suggests a corrected command line. tests that never `run` leave it unset.
static TYPED: std::sync::OnceLock<Vec<std::ffi::OsString>> = std::sync::OnceLock::new();

pub(crate) fn run() -> ExitCode {
    let started = Instant::now();
    let _ = TYPED.set(std::env::args_os().skip(1).collect());
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
        .without_hints(crate::settings::is_on(&var, "FETCHLOOM_NO_HINTS"))
        .with_progress(global.progress)
        .fitted(crate::output::story_columns());
    let mut context = Context::new(output);
    if let Some(refused) = &logs.refused {
        context.output.warn(Warning::TraceUnwritable, refused);
    }
    context.yes = global.yes;
    context.building = var("FETCHLOOM_BUILDING")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from);
    if args.version {
        let mut text = format!("fetchloom {}", env!("CARGO_PKG_VERSION"));
        // what a bug report needs: which build, on which system
        if level >= Level::Verbose {
            let _ = write!(
                text,
                "\n{} {} \u{b7} commit {} \u{b7} cache {}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                commit().unwrap_or("unknown, built outside git"),
                // a broken settings file is told by the commands that use it, not by --version
                Cache::locate(
                    &var,
                    settings_file(global.no_config, &var)
                        .ok()
                        .and_then(|file| file.cache())
                        .as_deref()
                )
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
        return completions::print(&context, asked, started);
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
    settings::warn_misnamed(&context.output, std::env::vars_os().map(|(name, _)| name));
    settings::warn_unread(&context.output, &var);
    if let Err(error) = read_switches(&mut context, global.offline, &var) {
        return finish::<()>(&context, name, Err(error), started);
    }
    let file = match settings_file(global.no_config, &var) {
        Ok(file) => file,
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    };
    let _ = SETTINGS_FILE.set(file.clone());
    match Settings::read(&var, file, Machine::read()) {
        Ok(settings) => {
            if let Some(threads) = settings.threads {
                context.threads = context.threads.at_most(threads.value);
            }
            crate::cache::set_hash_batches(settings.hash_batches());
            crate::cache::set_hash_workers(context.threads.limit(settings.hash_workers()));
            crate::source::set_most_files(settings.most_files.value);
            crate::archive::set_limits(settings.archive_limits());
            context.settings = settings;
        }
        Err(error) => return finish::<()>(&context, name, Err(error), started),
    }
    match fetch::mirrors::read(&global.mirror, &context.settings) {
        Ok(mirrors) => context.mirrors = fetch::mirrors::Mirrored::new(mirrors),
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
    if let Some(refusal) = context.refusal() {
        let offered = match (&refusal.error.need, command) {
            (Some(need), Some(command)) => {
                let lead = (!refusal.told).then_some(&refusal.error);
                Some((
                    block_on(&context, auth::offer(&context, need, lead)),
                    command,
                ))
            }
            _ => None,
        };
        match offered {
            Some((Ok(true), command)) => {
                outcome = dispatch(&context, command, Instant::now());
                // signed in, the command is not offered a sign-in again; a key kept out of files,
                // which `fetchloom auth` cannot show, is the one this refusal came with
                if let Some(again) = context.refusal().filter(|again| !again.told) {
                    let error = with_key(again.error, "the key you signed in with", None);
                    context.output.error(&error);
                }
            }
            Some((Ok(false), _)) => {}
            Some((Err(error), _)) => context.output.error(&error),
            None if refusal.told => {}
            None => context.output.error(&refusal.error),
        }
    }
    drop(entered);
    end_runtime(runtime, EXIT_WAIT);
    outcome
}

/// how long the end of a run waits for the runtime's threads to have exited. each of 1,920 normal
/// and settled runs measured let go of them within 10.1 ms, the slowest on two cores each shared
/// with three busy loops; a stopped run whose scan still runs, as `status --verify`'s, ends this
/// much after `Stopped`.
const EXIT_WAIT: Duration = Duration::from_millis(25);

/// ends `runtime`, waiting at most `wait` until its threads have exited, their thread-local
/// destructors included. on windows, returning from main ends every other thread wherever it is
/// and then runs main's destructors, which take the same locks as theirs, as `-vv`'s logging does,
/// so a runtime thread ended inside one left the run hanging after its last line. a blocking task
/// still running after `wait`, as a scan a stop left behind, ends with the process.
fn end_runtime(runtime: tokio::runtime::Runtime, wait: Duration) {
    runtime.shutdown_timeout(wait);
}

/// whether `command`, as help quotes it, is a whole command line fetchloom would accept, so a
/// script handed it as the next step can run it. `fetchloom add` alone names a command and is not one.
pub(crate) fn runs(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .is_some_and(|words| args::Args::try_parse_from(words).is_ok())
}

/// fetchloom's commands and options as clap defines them, for tests that walk every one.
#[cfg(test)]
pub(crate) fn definition() -> clap::Command {
    <args::Args as clap::CommandFactory>::command()
}

/// whether `command`, as help quotes it, is one a script may run as the next step with no
/// person to ask, as [`args::Command::harmless`] says. `--yes` answers a question meant for a
/// person, so no command with it is.
pub(crate) fn harmless(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .and_then(|words| args::Args::try_parse_from(words).ok())
        .is_some_and(|args| {
            !args.global.yes && args.command.is_some_and(|command| command.harmless())
        })
}

/// whether `command`, as help quotes it, may write data.lock, which a run asked to leave it as it
/// is never hands a script as the next step.
pub(crate) fn changes_lock(command: &str) -> bool {
    use clap::Parser;
    command_words(command, cfg!(windows))
        .and_then(|words| args::Args::try_parse_from(words).ok())
        .is_some_and(|args| args.command.is_some_and(|command| command.writes_lock()))
}

/// reads the literal quoting in suggestions without interpreting expansions or shell operators.
fn command_words(command: &str, windows: bool) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(delimiter) = quote {
            let powershell_single = windows && delimiter == '\'';
            if c == delimiter || (powershell_single && is_single_quote(c)) {
                match chars.next_if(|&next| powershell_single && is_single_quote(next)) {
                    Some(next) => word.get_or_insert_default().push(next),
                    None => quote = None,
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
        args::Command::Drop(args) => !args.dry.dry_run,
        args::Command::Sync(args) => !args.dry.dry_run,
        args::Command::Get(_) => true,
        _ => false,
    }
}

fn dispatch(context: &Arc<Context>, command: args::Command, started: Instant) -> ExitCode {
    let name = Some(command.name());
    if command.locked() {
        context.output.leave_lock();
    }
    match command {
        args::Command::Add(args) => {
            let outcome = block_on(context, add::add(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Remove(args) => {
            let outcome = block_on(context, remove::remove(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Drop(args) => {
            let outcome = block_on(context, drop::run(context, args));
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
        args::Command::Build(args) => {
            let outcome = block_on(context, build::run(context, &args));
            finish(context, name, outcome, started)
        }
        args::Command::Status(args) => {
            let outcome = blocking(context, Effect::Reads, move |context| {
                status::run(context, args.verify, args.check)
            });
            finish(context, name, outcome, started)
        }
        args::Command::Path(args) => {
            let outcome = block_on(context, path::path(context, args));
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache { action: None }) => {
            let outcome = cache::info(context);
            finish(context, name, outcome, started)
        }
        args::Command::List(args) => {
            let outcome = blocking(context, Effect::Reads, move |context| {
                list::run(context, args.name.as_deref())
            });
            finish(context, name, outcome, started)
        }
        args::Command::Export(args) => {
            // with -o, export writes a file, and with --force moves the one there aside first
            let effect = if args.output.is_some() {
                Effect::Writes
            } else {
                Effect::Reads
            };
            let outcome = blocking(context, effect, move |context| export::run(context, &args));
            finish(context, name, outcome, started)
        }
        args::Command::Push(args) => {
            let outcome = blocking(context, Effect::Writes, move |context| {
                push::run(context, &args)
            });
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Gc(gc)),
        }) => {
            let outcome = cache::gc(context, gc.now, gc.inputs, gc.dry.dry_run);
            finish(context, name, outcome, started)
        }
        args::Command::Cache(args::Cache {
            action: Some(args::CacheAction::Verify),
        }) => {
            let outcome = cache::verify(context);
            finish(context, name, outcome, started)
        }
        args::Command::Settings(_) => {
            let outcome = Ok(settings::run(context));
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
        args::Command::Completions(asked) => completions::print(context, &asked, started),
        args::Command::Help(asked) => help::show(context, &asked.topic, started),
    }
}

/// what `--version` and `fetchloom` alone report in `--json`.
#[derive(Serialize)]
struct About {
    version: &'static str,
    commit: Option<&'static str>,
}

/// the commit this build of fetchloom came from, as build.rs found it, or none for a build
/// outside git.
fn commit() -> Option<&'static str> {
    option_env!("FETCHLOOM_COMMIT").filter(|commit| !commit.is_empty())
}

/// prints `text` for a person, after `banner`, or the version in the document `--json` asked for.
fn about(context: &Context, banner: &str, text: &str, started: Instant) -> ExitCode {
    context.output.answer(&format!(
        "{banner}{}",
        crate::output::help_text(text, Style::stdout_color(args::color()))
    ));
    let about = About {
        version: env!("CARGO_PKG_VERSION"),
        commit: commit(),
    };
    finish(context, None, Ok(about), started)
}

/// `fetchloom` alone: a short status inside a project, and the welcome outside one.
fn alone(context: &Arc<Context>, started: Instant) -> ExitCode {
    if let Ok(project) = project::find(&context.output) {
        let outcome = blocking(context, Effect::Reads, move |context| {
            status::brief(context, &project)
        });
        return finish(context, Some("status"), outcome, started);
    }
    let welcome = format!("fetchloom {}\n\n{}", env!("CARGO_PKG_VERSION"), args::INTRO);
    let Some(width) = context.output.answer_width() else {
        return about(context, "", &welcome, started);
    };
    let welcome = crate::output::wrap_help(&welcome, width, true);
    let rows = usize::from(console::Term::stdout().size().0);
    let banner = crate::output::wordmark(width, Style::stdout_color(args::color()));
    // the wordmark leads only where the whole welcome still fits below it
    let fits = banner.lines().count() + welcome.lines().count() < rows;
    about(context, if fits { &banner } else { "" }, &welcome, started)
}
/// asks before a large download, or one the disk holding the cache may lack room for, unless
/// `--yes` already said yes. without a terminal to ask on, it stops and names the flag. `room` is
/// the cache folder and the bytes the downloads need on its disk. says whether a person said yes
/// to going on without room, which says yes to the size as well.
fn confirm(
    output: &Output,
    yes: bool,
    above: u64,
    what: &str,
    bytes: u64,
    room: Option<(&std::path::Path, u64)>,
) -> Result<bool, Error> {
    confirm_doing(
        output,
        yes,
        above,
        what,
        bytes,
        room,
        &format!("downloading {what}"),
    )
}

/// [`confirm`], with `doing` naming what needs the room, as downloading and unpacking does.
fn confirm_doing(
    output: &Output,
    yes: bool,
    above: u64,
    what: &str,
    bytes: u64,
    room: Option<(&std::path::Path, u64)>,
    doing: &str,
) -> Result<bool, Error> {
    if let Some((cache, needed)) = room
        && !yes
    {
        let free = crate::space::free(cache);
        if crate::space::short(needed, free).is_some() {
            ask_room(output, doing, needed, free.unwrap_or_default(), cache)?;
            return Ok(true);
        }
    }
    if bytes <= above {
        return Ok(false);
    }
    ask(output, yes, what, &units::size(bytes)).map(|()| false)
}

/// asks whether to go on `doing` what needs at least `needed` bytes on the disk holding `cache`,
/// which has `free`. a disk that compresses what it stores can hold more than it tells, so a
/// person may say yes; without a terminal it stops and names the flag.
fn ask_room(
    output: &Output,
    doing: &str,
    needed: u64,
    free: u64,
    cache: &std::path::Path,
) -> Result<(), Error> {
    let (needs, has) = (units::size(needed), units::size(free));
    let cancel = format!("{doing} ({needs})");
    let shortage = format!(
        "{doing} needs at least {needs} on the disk holding the cache at {}, which has {has} free",
        units::path(cache)
    );
    match output
        .confirm(&format!("{shortage}. go on anyway?"))
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(Kind::Unconfirmed, format!("did not start {doing}")).cancelled(cancel)),
        None => Err(Error::new(Kind::Unconfirmed, shortage)
        .help("free space on that disk, or set FETCHLOOM_CACHE to a folder on a bigger one, then run the same command again. add --yes to go on anyway, as on a disk that compresses what it stores")),
    }
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
    let late = Arc::clone(&context.output);
    context.stop.on_late(move || late.finishing());
    let (heard, interrupted) = tokio::sync::mpsc::unbounded_channel();
    #[cfg(unix)]
    listen_for_terminate(&runtime, heard.clone());
    runtime.spawn(pass_on_ctrl_c(signal, already_stopped, heard));
    let stopping = Arc::clone(context);
    let quitting = Arc::clone(context);
    runtime.spawn(interrupts(
        interrupted,
        move |interrupt| {
            let heard = heard_first(&stopping.stop, &stopping.output, interrupt);
            // ctrl-c at a terminal reaches a step's process group too, while a SIGTERM reaches
            // fetchloom alone, and the SIGKILL a scheduler sends after it would leave the step
            if interrupt == Interrupt::Terminate && heard == Heard::Stop {
                crate::build::step::end_running();
            }
        },
        move || {
            // a step's process group ends with fetchloom, so nothing it started runs on
            crate::build::step::end_running();
            quitting.output.quit();
            quitting.stop.quitting();
            std::process::exit(130);
        },
    ));
    Ok(runtime)
}

/// passes each ctrl-c on to `heard`: the first, which `first` listens for from before the work
/// started, or at once when it came `already`, then each one after it.
async fn pass_on_ctrl_c(
    first: impl Future<Output = std::io::Result<()>>,
    already: bool,
    heard: tokio::sync::mpsc::UnboundedSender<Interrupt>,
) {
    if already || first.await.is_ok() {
        while heard.send(Interrupt::CtrlC).is_ok() && tokio::signal::ctrl_c().await.is_ok() {}
    }
}

/// passes each SIGTERM, which a scheduler sends a job it ends, on to `heard`, listening from
/// before the work starts. tokio keeps the handler for the rest of the process, so a SIGTERM no
/// longer ends it at once: the work stops as at a ctrl-c instead.
#[cfg(unix)]
fn listen_for_terminate(
    runtime: &tokio::runtime::Runtime,
    heard: tokio::sync::mpsc::UnboundedSender<Interrupt>,
) {
    use tokio::signal::unix::{SignalKind, signal};
    let _entered = runtime.enter();
    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            runtime.spawn(async move {
                while terminate.recv().await.is_some() && heard.send(Interrupt::Terminate).is_ok() {
                }
            });
        }
        Err(err) => tracing::debug!(%err, "could not listen for SIGTERM"),
    }
}

/// what a first ctrl-c does: stops the run, or lets it finish where it has marked its work as
/// finishing or only the disk is left, as [`Stop::hear`] decides, then `tell`s the person which,
/// so the work sees a stop by the time `Stopping` shows. says which it did.
fn first_ctrl_c(stop: &Stop, tell: impl FnOnce(Heard)) -> Heard {
    let heard = stop.hear();
    tell(heard);
    heard
}

/// a first `interrupt`, told on `output` in words that name a second one, while every other line
/// waits, so none that the stopping work tells comes before `Stopping`.
fn heard_first(stop: &Stop, output: &Output, interrupt: Interrupt) -> Heard {
    output.interrupted_by(interrupt);
    let mut alone = output.alone();
    first_ctrl_c(stop, |heard| match heard {
        Heard::Stop => alone.stopping(),
        Heard::Started => alone.finishing(),
        Heard::Downloaded(count) => alone.finishing_downloads(count),
    })
}

/// what ctrl-c and SIGTERM do: at the first interrupt `heard`, `stopping` asks the work to wind
/// down, told as that interrupt; at the next of either, `quitting` ends the run now.
async fn interrupts(
    mut heard: tokio::sync::mpsc::UnboundedReceiver<Interrupt>,
    stopping: impl FnOnce(Interrupt),
    quitting: impl FnOnce(),
) {
    if let Some(first) = heard.recv().await {
        stopping(first);
        if heard.recv().await.is_some() {
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
    // work a stop cut short may still end well, so it ends as stopped, unless the command marked
    // its work as finishing: it does that after its last look for a stop, so one asked after that
    // look, even before the mark, leaves the work done
    context.stop.check_unfinished()?;
    outcome
}

/// what a command on a blocking thread does to files, which decides what a stop does to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Effect {
    /// reads alone, so a stop returns at once and leaves the scan to end with the process.
    Reads,
    /// writes, so a stop waits for the work to see it and remove what it started. a second
    /// ctrl-c still ends the run at once.
    Writes,
}

fn blocking<T: Send + 'static>(
    context: &Arc<Context>,
    effect: Effect,
    command: impl FnOnce(&Context) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    block_on(context, on_its_thread(context, effect, command))
}

/// runs `command` on a thread that may block, as [`blocking`] says.
async fn on_its_thread<T: Send + 'static>(
    context: &Arc<Context>,
    effect: Effect,
    command: impl FnOnce(&Context) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    let owned = Arc::clone(context);
    let task = tokio::task::spawn_blocking(move || command(&owned));
    match effect {
        Effect::Writes => ended(task.await),
        Effect::Reads => tokio::select! {
            biased;
            () = context.stop.asked() => Err(Stop::stopped()),
            outcome = task => ended(outcome),
        },
    }
}

/// what a command on its own thread returned, or why it ended without returning.
fn ended<T>(outcome: Result<Result<T, Error>, tokio::task::JoinError>) -> Result<T, Error> {
    outcome.map_err(|err| {
        Error::new(
            Kind::LocalIo,
            format!("could not finish the command: {err}"),
        )
    })?
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
    let outcome = outcome
        .map_err(|error| stop_named(error, context.output.stopped_by()))
        .map_err(|error| with_key_here(error, &crate::auth::env));
    if outcome.is_ok() {
        project::remind_lists(context);
    }
    let printed = context
        .output
        .document(command, outcome.as_ref(), counts, elapsed);
    let outcome = outcome.and_then(|result| printed.map(|()| result));
    if outcome.is_ok() {
        context.output.retell();
    }
    if let Err(error) = &outcome {
        // a person who can answer the offer to sign in reads it first; `-q` tells the error
        // above it, as without the offer
        let offered = error.need.is_some() && context.output.can_ask();
        let told = !offered || !context.output.shows_items();
        if told {
            context.output.error(error);
        }
        if offered {
            *context
                .needed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Refusal {
                error: error.clone(),
                told,
            });
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

/// `error`, a stop that the SIGTERM a scheduler sends asked, saying so rather than that a person
/// asked it, as in `stopped by a SIGTERM while talking to zenodo.org`. any other error stays as
/// it is.
fn stop_named(mut error: Error, by: Option<Interrupt>) -> Error {
    if error.kind == Kind::Interrupted
        && by == Some(Interrupt::Terminate)
        && let Some(rest) = error
            .message
            .strip_prefix("stopped at your request")
            .or_else(|| error.message.strip_prefix("stopped"))
    {
        error.message = format!("stopped by a SIGTERM{rest}");
    }
    error
}

/// `error`, a refusal that signing in could lift, when the environment `var` gives a key for its
/// account already: signing in again is no remedy, so the help says the key has no access or was
/// refused, and how to check it, and only terms to accept are still offered. any other error
/// stays as it is.
fn with_key_here(error: Error, var: crate::source::Var<'_>) -> Error {
    let Some(need) = error.need.as_deref() else {
        return error;
    };
    let account = &need.account;
    let Some(found) = account.find(var).filter(|found| account.sendable(found)) else {
        return error;
    };
    let named = match &found.origin {
        crate::auth::Origin::Variable(name) => format!("the key in {name}"),
        origin => format!("the key saved in {}", origin.shown()),
    };
    // a detail that names the key already, as one saying it went with the request, is enough
    let key = if error.details.iter().any(|line| line.starts_with(&named)) {
        "the key".to_owned()
    } else {
        named
    };
    with_key(
        error,
        &key,
        Some("`fetchloom auth` shows where it comes from and whose it is"),
    )
}

/// `error`, a refusal that signing in could lift, refused with the key `key` went with: its help
/// says the key was refused or has no access, then `check`, how to check the key, and only a need
/// for terms to accept stays.
fn with_key(mut error: Error, key: &str, check: Option<&str>) -> Error {
    let Some(need) = error.need.as_deref() else {
        return error;
    };
    let check = check.map_or_else(String::new, |check| format!(": {check}"));
    let help = match (&need.maybe_missing, &need.terms) {
        (Some(page), _) => {
            format!("check the name at {page}. if it is right, {key} has no access to it{check}")
        }
        (None, Some(terms)) => {
            format!("accept the terms at {terms} with the account {key} belongs to{check}")
        }
        (None, None) => format!("{key} was refused, or has no access to it{check}"),
    };
    if need.terms.is_none() {
        error.need = None;
    }
    error.help(help)
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
pub(in crate::cli) fn shell_word(word: &str) -> Cow<'_, str> {
    shell_word_for(word, cfg!(windows))
}

/// [`shell_word`] for the shells of windows, powershell and cmd, or for bash, zsh and fish. a
/// word starting with `~` or `=`, which zsh expands, or `@`, which powershell splats, is quoted.
/// double quotes keep every other character literal in all of them, but for `$`, a backtick and
/// a double quote; on windows curly double quotes, which powershell reads as double quotes; and
/// elsewhere `!`, which bash and zsh expand from their history, and a backslash, an escape there.
/// a word holding any of those is single-quoted: powershell doubles each single quote inside,
/// curly ones too, and elsewhere a single quote and a backslash go outside the quotes, escaped,
/// since fish reads a backslash inside them as an escape too.
fn shell_word_for(word: &str, windows: bool) -> Cow<'_, str> {
    if !word.is_empty()
        && !word.starts_with(['~', '=', '@'])
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@%+=,~".contains(c))
    {
        return Cow::Borrowed(word);
    }
    let literal = |c: char| match c {
        '$' | '\u{60}' | '"' => false,
        '!' | '\\' => windows,
        '\u{201c}'..='\u{201e}' => !windows,
        _ => true,
    };
    if word.chars().all(literal) {
        return Cow::Owned(format!("\"{word}\""));
    }
    if windows {
        let mut quoted = String::with_capacity(word.len() + 2);
        quoted.push('\'');
        for c in word.chars() {
            if is_single_quote(c) {
                quoted.push(c);
            }
            quoted.push(c);
        }
        quoted.push('\'');
        return Cow::Owned(quoted);
    }
    let mut quoted = String::with_capacity(word.len() + 2);
    let mut open = false;
    for c in word.chars() {
        if matches!(c, '\'' | '\\') {
            if open {
                quoted.push('\'');
                open = false;
            }
            quoted.push('\\');
        } else if !open {
            quoted.push('\'');
            open = true;
        }
        quoted.push(c);
    }
    if open {
        quoted.push('\'');
    }
    Cow::Owned(quoted)
}

/// whether powershell reads `c` as a single quote, as it reads the curly ones.
fn is_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}'..='\u{201b}')
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
/// argument holds a link with a query, a fragment or a login, or is not unicode.
fn command_line(args: impl Iterator<Item = std::ffi::OsString>) -> Option<String> {
    let mut line = String::from("fetchloom");
    for arg in args {
        let arg = arg.into_string().ok()?;
        if may_hold_secret(&arg) {
            return None;
        }
        line.push(' ');
        line.push_str(&shell_word(&arg));
    }
    Some(line)
}

/// whether `arg` holds a link with a query, a fragment or a login, which may carry a secret under
/// a name no list knows, such as dropbox's `rlkey`: the whole word, an option's value after `=`,
/// a link attached to a short option, or a link pasted wholly percent-encoded.
fn may_hold_secret(arg: &str) -> bool {
    if matches!(net::mask_query(arg), Cow::Owned(_)) {
        return true;
    }
    let decoded;
    let arg = if !arg.contains(':') && arg.contains('%') {
        decoded = percent_encoding::percent_decode_str(arg).decode_utf8_lossy();
        decoded.as_ref()
    } else {
        arg
    };
    let value = arg
        .starts_with('-')
        .then(|| arg.find('=').map(|at| at + 1))
        .flatten();
    let schemes = arg.match_indices("://").map(|(at, _)| {
        let run = arg[..at]
            .char_indices()
            .rev()
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || "+-.".contains(*c)))
            .map_or(0, |(before, c)| before + c.len_utf8());
        // a scheme starts with a letter, so `-ihttps://` holds the link `ihttps://`
        arg[run..at]
            .find(|c: char| c.is_ascii_alphabetic())
            .map_or(at, |letter| run + letter)
    });
    std::iter::once(0).chain(value).chain(schemes).any(|start| {
        url::Url::parse(&arg[start..]).is_ok_and(|url| {
            url.query().is_some()
                || url.fragment().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
        })
    })
}

/// the command a person likely meant: the command line they typed with the words `typed`
/// changed to `meant`, every other word kept, so a flag such as `--dry-run` stays in what they
/// paste. `instead` names the command when that line is not known, lacks the words or holds a
/// secret no message may repeat.
pub(in crate::cli) fn meant(typed: &[&str], meant: &[&str], instead: String) -> String {
    TYPED
        .get()
        .and_then(|line| retyped(line, typed, meant))
        .unwrap_or(instead)
}

/// the command line `line` with the words `typed` changed to `meant`, ready to copy. the words
/// are found where they stand last, alone or beside an `=`, as in `--limt=5` or `--upgrade=irs`.
/// none when they stand nowhere, or the line cannot be repeated.
fn retyped(line: &[std::ffi::OsString], typed: &[&str], meant: &[&str]) -> Option<String> {
    let mut line = line.to_vec();
    let count = typed.len();
    let alone = (0..=line.len().checked_sub(count)?).rev().find(|&at| {
        line[at..at + count]
            .iter()
            .zip(typed)
            .all(|(arg, word)| arg == *word)
    });
    if let Some(at) = alone {
        line.splice(at..at + count, meant.iter().map(Into::into));
    } else {
        let ([word], [replacement]) = (typed, meant) else {
            return None;
        };
        let (at, changed) = line.iter().enumerate().rev().find_map(|(at, arg)| {
            let changed = match arg.to_str()?.split_once('=') {
                Some((option, value)) if option == *word && word.starts_with('-') => {
                    format!("{replacement}={value}")
                }
                Some((option, value)) if value == *word && option.starts_with('-') => {
                    format!("{option}={replacement}")
                }
                _ => return None,
            };
            Some((at, changed))
        })?;
        line[at] = changed.into();
    }
    command_line(line.into_iter())
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
                    accessible: false,
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

    /// the end of a run waits until the runtime's threads have exited, their thread-local
    /// destructors included. on windows, returning from main ends every other thread wherever it
    /// is, then runs main's destructors, which take the same locks as theirs: a runtime thread
    /// ended inside one left the run hanging after its last line.
    #[test]
    fn ending_the_runtime_waits_until_its_threads_have_exited() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static EXITED: AtomicUsize = AtomicUsize::new(0);
        struct Exiting;
        impl Drop for Exiting {
            fn drop(&mut self) {
                // a thread still in here when the process ends is what windows cuts short
                std::thread::sleep(Duration::from_millis(20));
                EXITED.fetch_add(1, Ordering::SeqCst);
            }
        }
        thread_local! {
            static EXITING: Exiting = const { Exiting };
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            tokio::spawn(async { EXITING.with(|_| ()) }).await.unwrap();
            tokio::task::spawn_blocking(|| EXITING.with(|_| ()))
                .await
                .unwrap();
        });
        end_runtime(runtime, Duration::from_secs(60));
        assert_eq!(
            EXITED.load(Ordering::SeqCst),
            2,
            "the run ended while a runtime thread was still exiting"
        );
    }

    /// a blocking task that never ends, as a scan a stop left running or `get -i`'s reader of a
    /// pipe kept open, still lets the run end once the wait is over.
    #[test]
    fn a_task_that_never_ends_still_lets_the_run_end() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        runtime.spawn_blocking(move || held.recv());
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            end_runtime(runtime, EXIT_WAIT);
            let _ = ended.send(());
        });
        end.recv_timeout(Duration::from_secs(60))
            .expect("the run waited for a task that never ends");
        drop(release);
    }

    /// a stop returns from work that only reads at once, while work that writes is waited for
    /// until it has seen the stop and removed what it started, since the process ends soon after
    /// and would leave its files behind, as a stopped push left its unfinished copies.
    #[test]
    fn a_stop_waits_for_work_that_writes_and_leaves_a_scan_behind() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        for (effect, waits) in [(Effect::Reads, false), (Effect::Writes, true)] {
            let context = Arc::new(Context::new(Output::new(
                Style {
                    color: false,
                    unicode: false,
                    interactive: false,
                    watched: false,
                    links: false,
                    accessible: false,
                },
                Level::Quiet,
                false,
                false,
            )));
            context.stop.ask();
            let (release, released) = std::sync::mpsc::channel::<()>();
            let removed = Arc::new(AtomicBool::new(false));
            let removing = Arc::clone(&removed);
            let mut running = std::pin::pin!(on_its_thread(&context, effect, move |_| {
                // the work sees the stop once the test lets it, then removes what it started
                let _ = released.recv();
                removing.store(true, Ordering::SeqCst);
                Err::<(), Error>(Stop::stopped())
            }));
            let returned = runtime.block_on(std::future::poll_fn(|cx| {
                std::task::Poll::Ready(running.as_mut().poll(cx).is_ready())
            }));
            assert_eq!(returned, !waits, "{effect:?}");
            drop(release);
            if waits {
                let outcome = runtime.block_on(running);
                assert!(
                    removed.load(Ordering::SeqCst),
                    "returned before the work ended"
                );
                assert_eq!(outcome.unwrap_err().kind, Kind::Interrupted);
            }
        }
    }

    /// a wait for a host's limit is told in whole seconds, rounded up, with the limit it stated.
    #[test]
    fn a_rate_limit_wait_names_the_limit_it_waits_for() {
        assert_eq!(
            rate_wait("127.0.0.1", Some(133), None, Duration::from_millis(600)),
            "1 s for 127.0.0.1, whose limit of 133 requests is used up"
        );
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
                accessible: false,
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

    /// the first interrupt, ctrl-c or SIGTERM, stops the work, told as the one it was, and the
    /// next of either quits.
    #[test]
    fn the_first_interrupt_stops_the_work_and_the_second_quits() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let wait = Duration::from_millis(20);
        for (first, next) in [
            (Interrupt::CtrlC, Interrupt::CtrlC),
            (Interrupt::Terminate, Interrupt::CtrlC),
        ] {
            let told = std::sync::Mutex::new(Vec::new());
            runtime.block_on(async {
                let (heard, interrupted) = tokio::sync::mpsc::unbounded_channel();
                let listening = interrupts(
                    interrupted,
                    |interrupt| told.lock().unwrap().push(format!("stopping {interrupt:?}")),
                    || told.lock().unwrap().push("quitting".to_owned()),
                );
                tokio::pin!(listening);
                assert!(tokio::time::timeout(wait, &mut listening).await.is_err());
                assert!(told.lock().unwrap().is_empty());
                heard.send(first).unwrap();
                assert!(tokio::time::timeout(wait, &mut listening).await.is_err());
                assert_eq!(*told.lock().unwrap(), [format!("stopping {first:?}")]);
                heard.send(next).unwrap();
                listening.await;
            });
            assert_eq!(
                *told.lock().unwrap(),
                [format!("stopping {first:?}"), "quitting".to_owned()]
            );
        }
    }

    /// a SIGTERM's stop names it, whatever the work said it stopped doing, since no person asked.
    #[test]
    fn a_stop_a_sigterm_asked_names_it() {
        for (said, named) in [
            (
                "stopped at your request, so data.lock and the data folder are as they were",
                "stopped by a SIGTERM, so data.lock and the data folder are as they were",
            ),
            (
                "stopped while talking to 127.0.0.1, so nothing changed",
                "stopped by a SIGTERM while talking to 127.0.0.1, so nothing changed",
            ),
            ("stopped", "stopped by a SIGTERM"),
        ] {
            let stopped = || Error::new(Kind::Interrupted, said);
            assert_eq!(
                stop_named(stopped(), Some(Interrupt::Terminate)).message,
                named
            );
            assert_eq!(stop_named(stopped(), Some(Interrupt::CtrlC)).message, said);
        }
    }

    /// a first ctrl-c asks the stop before it tells the person, so once `Stopping` shows, the
    /// work sees the stop: a test that waited for the line once found the work done anyway. a run
    /// past its mark is not stopped but told it finishes.
    #[test]
    fn a_first_ctrl_c_is_told_once_the_work_can_see_it() {
        let stop = Stop::default();
        let mut seen = Vec::new();
        first_ctrl_c(&stop, |heard| seen.push((heard, stop.is_asked())));
        assert_eq!(seen, [(Heard::Stop, true)]);
        let finished = Stop::default();
        finished.finish();
        first_ctrl_c(&finished, |heard| seen.push((heard, finished.is_asked())));
        assert_eq!(seen[1..], [(Heard::Started, false)]);
        assert!(!finished.is_asked());
    }

    /// work that tells its last line as soon as it sees a stop tells it after `Stopping`, since
    /// the stop is asked before that line while every other line waits for it.
    #[test]
    fn a_line_told_once_the_stop_is_seen_follows_stopping() {
        let output = Output::new(
            crate::output::Style {
                color: false,
                unicode: false,
                interactive: false,
                watched: false,
                links: false,
                accessible: false,
            },
            Level::Normal,
            false,
            false,
        );
        let stop = Stop::default();
        std::thread::scope(|scope| {
            let work = scope.spawn(|| {
                let waiting = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                waiting.block_on(stop.asked());
                output.say("    Stopped at your request");
            });
            assert_eq!(heard_first(&stop, &output, Interrupt::CtrlC), Heard::Stop);
            work.join().unwrap();
        });
        assert_eq!(
            output.said(),
            [
                "   Stopping (press ctrl-c again to quit at once)",
                "    Stopped at your request",
            ]
        );
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
                accessible: false,
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

    /// a refusal that signing in could lift, with a key here already, says the key was refused or
    /// has no access and how to check it, never to sign in, and offers to sign in no more: only
    /// terms to accept stay offered. with no key here, it stays as it was.
    #[test]
    fn a_refusal_with_a_key_here_says_to_check_the_key() {
        use crate::auth::Account;
        let key = |name: &str| (name == "ZENODO_TOKEN").then(|| "z".into());
        let none = |_: &str| None;
        let refused = || Error::new(Kind::AuthRequired, "zenodo:1 is restricted").help("sign in");
        let page = "https://zenodo.org/records/1".to_owned();
        let check = "`fetchloom auth` shows where it comes from and whose it is";
        let cases = [
            (
                refused().needs(Account::Zenodo, None),
                format!("the key in ZENODO_TOKEN was refused, or has no access to it: {check}"),
                false,
            ),
            (
                refused().needs(Account::Zenodo, Some(page.clone())),
                format!(
                    "accept the terms at {page} with the account the key in ZENODO_TOKEN belongs to: {check}"
                ),
                true,
            ),
            (
                refused()
                    .detail("the key in ZENODO_TOKEN went with the request")
                    .needs_or_missing(Account::Zenodo, page.clone()),
                format!(
                    "check the name at {page}. if it is right, the key has no access to it: {check}"
                ),
                false,
            ),
        ];
        for (error, help, offered) in cases {
            let told = with_key_here(error.clone(), &key);
            assert_eq!(told.help.as_deref(), Some(help.as_str()));
            assert_eq!(told.need.is_some(), offered, "{help}");
            let alone = with_key_here(error, &none);
            assert_eq!(alone.help.as_deref(), Some("sign in"));
            assert!(alone.need.is_some());
        }
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
    fn a_guess_keeps_every_other_word_of_the_command_line() {
        let line = |args: &[&str], typed: &[&str], meant: &[&str]| {
            let args: Vec<std::ffi::OsString> = args.iter().map(Into::into).collect();
            retyped(&args, typed, meant)
        };
        for (args, typed, meant, expected) in [
            (
                &["remove", "--dry-run", "irs"][..],
                &["irs"][..],
                &["iris"][..],
                "fetchloom remove --dry-run iris",
            ),
            (
                &["-C", "irs", "remove", "irs", "-n"],
                &["irs"],
                &["iris"],
                "fetchloom -C irs remove iris -n",
            ),
            (
                &["lock", "--upgrade=irs"],
                &["irs"],
                &["iris"],
                "fetchloom lock --upgrade=iris",
            ),
            (
                &["search", "iris", "--limt=5"],
                &["--limt"],
                &["--limit"],
                "fetchloom search iris --limit=5",
            ),
            (
                &["help", "cache", "gx", "--json"],
                &["cache", "gx"],
                &["cache", "gc"],
                "fetchloom help cache gc --json",
            ),
            (
                &["verify", "-q"],
                &["verify"],
                &["cache", "verify"],
                "fetchloom cache verify -q",
            ),
            (
                &["add", "fasion mnist", "--select", "*.csv"],
                &["fasion mnist"],
                &["fashion mnist"],
                "fetchloom add \"fashion mnist\" --select \"*.csv\"",
            ),
            (
                &["-q", "Search", "iris", "--source", "hf"],
                &["Search"],
                &["search"],
                "fetchloom -q search iris --source hf",
            ),
            (
                &["ad", "iris", "--dry-run"],
                &["ad"],
                &["add"],
                "fetchloom add iris --dry-run",
            ),
            (
                &["add", "--yse", "zenodo:3608990"],
                &["--yse"],
                &["--yes"],
                "fetchloom add --yes zenodo:3608990",
            ),
            (
                &["cache", "verfiy", "--json"],
                &["verfiy"],
                &["verify"],
                "fetchloom cache verify --json",
            ),
        ] {
            assert_eq!(
                line(args, typed, meant).as_deref(),
                Some(expected),
                "{args:?}"
            );
        }
        assert_eq!(line(&["remove", "iris"], &["irs"], &["iris"]), None);
        assert_eq!(
            line(
                &["serch", "https://example.org/a.csv?token=abc"],
                &["serch"],
                &["search"]
            ),
            None
        );
        assert_eq!(
            meant(&["irs"], &["iris"], "fetchloom sync iris".to_owned()),
            "fetchloom sync iris",
            "without the line typed, the command alone"
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
        // a name no list knows may hold a secret too, as dropbox's rlkey does, so any query,
        // fragment or login in a link keeps the line from being repeated
        for link in [
            "https://example.org/a.csv?rlkey=abc",
            "https://example.org/a.csv?download=1",
            "https://example.org/a.csv#abc",
            "s3://key:abc@bucket/a.csv",
            "https%3A%2F%2Fexample.org%2Fa.csv%3Fsv%3Dabc",
        ] {
            assert_eq!(line(&["get", link]), None, "{link}");
        }
        assert_eq!(
            line(&["--mirror=https://lab.example.org/m?code=abc", "sync"]),
            None,
            "a link given as an option's value"
        );
        assert_eq!(
            line(&["-ihttps://example.org/list.txt#abc", "get"]),
            None,
            "a link attached to a short option"
        );
        assert_eq!(
            line(&["get", "https://example.org/a.csv", "-o", "a#1.csv"]).as_deref(),
            Some("fetchloom get https://example.org/a.csv -o \"a#1.csv\"")
        );
        assert_eq!(
            line(&["add", "hf:org/name@main"]).as_deref(),
            Some("fetchloom add hf:org/name@main")
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

    /// words a shell would expand or split, as a suggested command quotes them for bash, zsh and
    /// fish, and on windows for powershell, and for cmd too but for `$`, a backtick and a double
    /// quote, which no quoting keeps literal in both. each reads back as it was.
    const QUOTED: [(&str, &str, &str); 13] = [
        ("hi!.csv", "'hi!.csv'", "\"hi!.csv\""),
        ("$notes", "'$notes'", "'$notes'"),
        ("a`b`", "'a`b`'", "'a`b`'"),
        ("ada's data", "\"ada's data\"", "\"ada's data\""),
        ("it's $5!", "'it'\\''s $5!'", "'it''s $5!'"),
        ("my data", "\"my data\"", "\"my data\""),
        ("données é.csv", "\"données é.csv\"", "\"données é.csv\""),
        (r"data\notes", r"'data'\\'notes'", r#""data\notes""#),
        ("a\"b", "'a\"b'", "'a\"b'"),
        ("~notes.csv", "\"~notes.csv\"", "\"~notes.csv\""),
        ("=notes.csv", "\"=notes.csv\"", "\"=notes.csv\""),
        ("@notes.csv", "\"@notes.csv\"", "\"@notes.csv\""),
        (
            "ada’s “best” data",
            "\"ada’s “best” data\"",
            "'ada’’s “best” data'",
        ),
    ];

    #[test]
    fn suggested_commands_keep_shell_metacharacters_literal() {
        for (word, posix, windows) in QUOTED {
            for (on_windows, quoted) in [(false, posix), (true, windows)] {
                assert_eq!(shell_word_for(word, on_windows), quoted, "{word}");
                let line = format!("fetchloom get {quoted}");
                assert_eq!(
                    command_words(&line, on_windows),
                    Some(vec![
                        "fetchloom".to_owned(),
                        "get".to_owned(),
                        word.to_owned()
                    ]),
                    "{line}"
                );
            }
        }
    }

    /// interactive bash and zsh, which expand `!` inside double quotes from their history, and
    /// every other shell here that a person may paste a suggested command into, take each quoted
    /// word as it was: powershell, which reads the same wherever it runs, as windows quotes it.
    #[cfg(unix)]
    #[test]
    fn a_quoted_word_reads_back_whole_in_each_shell() {
        let words: Vec<&str> = QUOTED.iter().map(|(word, _, _)| *word).collect();
        let printf = "printf '%s\\n'";
        for (shell, windows, print) in [
            (
                &["bash", "--norc", "--noprofile", "-i", "-s"][..],
                false,
                printf,
            ),
            (&["sh", "-s"], false, printf),
            (&["zsh", "-f", "-i", "-s"], false, printf),
            (&["fish", "--no-config"], false, printf),
            (
                &["pwsh", "-NoProfile", "-NonInteractive", "-Command", "-"],
                true,
                "Write-Output",
            ),
        ] {
            let line: Vec<String> = words
                .iter()
                .map(|word| shell_word_for(word, windows).into_owned())
                .collect();
            let script = format!("{print} {}\n", line.join(" "));
            let started = std::process::Command::new(shell[0])
                .args(&shell[1..])
                .env("HISTFILE", "/dev/null")
                .env("POWERSHELL_TELEMETRY_OPTOUT", "1")
                .env("TERM", "dumb")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn();
            let mut child = match started {
                Ok(child) => child,
                Err(error) => {
                    let reason = format!("{} cannot start: {error}", shell[0]);
                    crate::test_skips::skip(shell[0], &reason);
                    continue;
                }
            };
            std::io::Write::write_all(child.stdin.as_mut().unwrap(), script.as_bytes()).unwrap();
            drop(child.stdin.take());
            let output = child.wait_with_output().unwrap();
            let told = String::from_utf8_lossy(&output.stdout);
            assert_eq!(
                told.lines().collect::<Vec<_>>(),
                words,
                "{}: {script}",
                shell[0]
            );
        }
    }

    /// `FETCHLOOM_OFFLINE` reads true and false as `FETCHLOOM_MIRRORS_ONLY` does, and a value it
    /// cannot read is a usage error naming it, never a run that goes online.
    #[test]
    fn the_offline_variable_takes_true_or_false_and_refuses_the_rest() {
        let read = |value: Option<&str>| offline(value.map(std::ffi::OsStr::new));
        for (value, expected) in [
            (None, false),
            (Some(""), false),
            (Some("1"), true),
            (Some("true"), true),
            (Some(" TRUE "), true),
            (Some("0"), false),
            (Some("false"), false),
            (Some("False"), false),
        ] {
            assert_eq!(read(value).ok(), Some(expected), "{value:?}");
        }
        for value in ["yes", "2", "on", "1.0"] {
            let error = read(Some(value)).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{value}");
            assert_eq!(
                error.to_string(),
                format!("FETCHLOOM_OFFLINE is \"{value}\", which is not true or false")
            );
        }
    }

    /// `FETCHLOOM_SPLIT` reads true and false as every on/off variable does, splits once one
    /// stream proves slow when it is unset or empty, and refuses any other value.
    #[test]
    fn downloads_split_once_one_stream_is_slow_unless_told_otherwise() {
        use net::Splitting::{Always, Never, WhenSlow};
        let read = |value: Option<&str>| splits(value.map(std::ffi::OsStr::new));
        for (value, expected) in [
            (None, WhenSlow),
            (Some(""), WhenSlow),
            (Some("0"), Never),
            (Some(" False "), Never),
            (Some("1"), Always),
            (Some("TRUE"), Always),
        ] {
            assert_eq!(read(value).ok(), Some(expected), "{value:?}");
        }
        for value in ["yes", "2", "when_slow"] {
            let error = read(Some(value)).unwrap_err();
            assert_eq!(error.kind, Kind::Usage, "{value}");
            assert_eq!(
                error.to_string(),
                format!("FETCHLOOM_SPLIT is \"{value}\", which is not true or false")
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
                    accessible: false,
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
            end_runtime(runtime, EXIT_WAIT);
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

    #[test]
    fn a_help_reaches_the_project_from_the_folder_fetchloom_ran_in() {
        let root = std::path::Path::new("/work/project");
        assert_eq!(up_to(root, root), "");
        assert_eq!(up_to(root, &root.join("data").join("sample")), "../../");
        assert_eq!(
            up_to(root, std::path::Path::new("/elsewhere")),
            "",
            "from a folder not inside it, paths start at the project"
        );
    }
}
