//! the command line as clap reads it. every heading and placeholder is lowercase, like the docs.

use std::path::PathBuf;

use clap::error::ErrorKind;
use clap::{ArgAction, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};

use crate::digest::Sha256;
use crate::output::{ColorChoice, Style};

const TEMPLATE: &str = "{about}\n\nusage: {usage}\n\n{all-args}{after-help}";

/// what each command's help ends with: command lines to copy, then what the command changes on
/// disk.
mod examples {
    pub(super) const ADD: &str = "examples:
  fetchloom add iris    # at a terminal, pick one from a list
  fetchloom add https://huggingface.co/datasets/scikit-learn/iris
  fetchloom add https://zenodo.org/records/3608990 --as flowers
  fetchloom add hf:datasets/scikit-learn/iris --select \"*.csv\" --dry-run

on disk: writes data.toml, data.lock and data/<name>, and adds data/ to a git project's .gitignore. --dry-run writes nothing";

    pub(super) const REMOVE: &str = "examples:
  fetchloom remove iris
  fetchloom remove iris --dry-run
  fetchloom remove flowers --json

on disk: edits data.toml and data.lock, and deletes the unchanged files it placed in data/. --dry-run writes nothing";

    pub(super) const SYNC: &str = "examples:
  fetchloom sync
  fetchloom sync iris
  fetchloom sync --locked --verify
  fetchloom sync --force iris --dry-run

on disk: updates the data folder from data.lock, writing data.lock first if needed unless --locked, and adds data/ to a git project's .gitignore. given names, it changes only those datasets' entries and files. reports files it cannot replace without --force. --dry-run writes nothing";

    pub(super) const LOCK: &str = "examples:
  fetchloom lock
  fetchloom lock --check
  fetchloom lock --upgrade iris
  fetchloom lock --theirs iris

on disk: writes data.lock and may add files to the cache. after a git merge, it merges data.toml and data.lock without downloading. --check and --dry-run write nothing";

    pub(super) const STATUS: &str = "examples:
  fetchloom status
  fetchloom status --verify
  fetchloom status --json

on disk: nothing. it reads data.toml, data.lock and data/";

    pub(super) const LIST: &str = "examples:
  fetchloom list
  fetchloom list iris
  fetchloom list --json

on disk: nothing. it reads data.toml and data.lock";

    pub(super) const EXPORT: &str = "examples:
  fetchloom export > data.sha256
  fetchloom export --format links iris
  fetchloom export --format aria2 -o sources.aria2

on disk: nothing, or the one file -o names, which it never replaces without --force. it reads data.toml, data.lock and what the cache recorded of each archive";

    pub(super) const PATH: &str = "examples:
  fetchloom path iris
  fetchloom path iris/Iris.csv
  fetchloom path iris -q
  fetchloom path iris --locked

on disk: nothing when the dataset is ready. otherwise it syncs first, as `fetchloom sync` does, or with --locked fails rather than change data.lock";

    pub(super) const SETTINGS: &str = "examples:
  fetchloom settings
  fetchloom settings --json

on disk: nothing. it reads the FETCHLOOM_* variables and the settings file";

    pub(super) const CACHE: &str = "examples:
  fetchloom cache
  fetchloom cache gc
  fetchloom cache gc --now
  fetchloom cache verify

on disk: `cache` reports usage. `cache gc` deletes unused cached files; `cache verify` notes changed ones, which are downloaded again when needed. neither changes data/";

    pub(super) const GC: &str = "examples:
  fetchloom cache gc
  fetchloom cache gc --now
  fetchloom cache gc --now --dry-run
  fetchloom cache gc --json

on disk: deletes cached files no project needs once they are a day old, or at once with --now. data/ stays as it is. --dry-run deletes nothing";

    pub(super) const VERIFY: &str = "examples:
  fetchloom cache verify
  fetchloom cache verify --json

on disk: notes changed cached files, which are downloaded again when next needed. data/ stays as it is";

    pub(super) const GET: &str = "examples:
  fetchloom get iris
  fetchloom get https://example.org/iris.csv -o iris.csv
  fetchloom get zenodo:3608990 figshare:34744689@1 --dir downloads
  fetchloom get -i links.txt

on disk: writes one folder for each link, or with -o the one file it names, through the cache. does not create data.toml or data.lock";

    pub(super) const SEARCH: &str = "examples:
  fetchloom search iris
  fetchloom search fashion mnist
  fetchloom search iris --source kaggle,zenodo --limit 5
  fetchloom search iris --json

on disk: nothing. prints the exact reference of each match, such as zenodo:3608990, and downloads nothing";

    pub(super) const AUTH: &str = "examples:
  fetchloom auth
  fetchloom auth login hf
  fetchloom auth logout zenodo

on disk: a key is saved where the source's own tools read it, so one key works in all of them";

    pub(super) const LOGIN: &str = "examples:
  fetchloom auth login hf
  fetchloom auth login dataverse --host demo.dataverse.org
  fetchloom auth login earthdata
  fetchloom auth login hf --with-token

on disk: hugging face keys go where `hf auth login` puts them, kaggle keys in ~/.kaggle, logins in .netrc for curl and wget too, and other keys in the system's credential store. asks before saving";

    pub(super) const LOGOUT: &str = "examples:
  fetchloom auth logout zenodo
  fetchloom auth logout hf

on disk: removes the saved key. asks first when other tools read it too";

    pub(super) const HELP: &str = "examples:
  fetchloom help concepts
  fetchloom help exit-codes
  fetchloom help add
  fetchloom help cache gc

on disk: nothing";

    pub(super) const COMPLETIONS: &str = "examples:
  fetchloom completions bash > ~/.local/share/bash-completion/completions/fetchloom
  fetchloom completions zsh > ~/.zfunc/_fetchloom
  fetchloom completions fish > ~/.config/fish/completions/fetchloom.fish
  fetchloom completions powershell | Out-String | Invoke-Expression

on disk: nothing. it prints the script on stdout, for a file your shell loads";
}

/// what a beginner meets first, on the first run and at the top of `fetchloom --help`: what
/// fetchloom does, each thing by its own name with `pin` explained where it first appears,
/// commands to copy and where to learn more. the analogy belongs to `help concepts`.
pub(crate) const INTRO: &str =
    "fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml  the datasets you want
  pinning    recording a dataset's exact files and their SHA-256 in data.lock
  data.lock  the list of pinned files, so every machine gets the same bytes
  the cache  one folder that keeps each file once, for all your projects

  fetchloom add iris    choose an iris dataset, put it in data/ and pin it
  fetchloom get iris    download it once, without a project
  fetchloom sync        fill data/ in a project you cloned

a name is looked up on hugging face, kaggle and zenodo. a link works too:
  fetchloom add https://huggingface.co/datasets/scikit-learn/iris

learn more with `fetchloom help concepts` and `fetchloom <command> --help`";

/// put the exact data a project needs onto any machine with one command
#[derive(Debug, Parser)]
#[command(
    name = "fetchloom",
    about = INTRO,
    long_about = None,
    version,
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    subcommand_help_heading = "commands",
    subcommand_value_name = "command",
    override_usage = "fetchloom [options] <command>",
    help_template = TEMPLATE
)]
pub(crate) struct Args {
    #[command(subcommand)]
    pub(crate) command: Option<Command>,

    #[command(flatten)]
    pub(crate) global: Global,

    /// print help
    #[arg(short, long, action = ArgAction::Help, global = true, help_heading = "options")]
    help: Option<bool>,

    /// print the version
    #[arg(short = 'V', long, help_heading = "options")]
    pub(crate) version: bool,
}

pub(super) fn read() -> Result<Args, clap::Error> {
    let mut command = command();
    let mut line: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if let Some(topic) = json_help(&command, &line) {
        line = std::iter::once("fetchloom".into())
            .chain(std::iter::once("help".into()))
            .chain(topic.into_iter().map(Into::into))
            .chain(std::iter::once("--json".into()))
            .collect();
    }
    let mut matches = match command.try_get_matches_from_mut(line.clone()) {
        Ok(matches) => matches,
        // a mistake elsewhere on a line that asks for help still gets the help it asked for
        Err(error)
            if !matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let Some(topic) = help_topic(&command, &line) else {
                return Err(error);
            };
            let asked = std::iter::once("fetchloom".to_owned())
                .chain(topic)
                .chain(std::iter::once("--help".to_owned()));
            return Err(match command.try_get_matches_from_mut(asked) {
                Err(help) if help.kind() == ErrorKind::DisplayHelp => help,
                _ => error,
            });
        }
        Err(error) => return Err(error),
    };
    Args::from_arg_matches_mut(&mut matches).map_err(|error| error.format(&mut command))
}

/// `--help` beside `--json` asks for the help document, which stdout promises under `--json`.
/// returns the command it asks about, as `help` takes it.
fn json_help(command: &clap::Command, line: &[std::ffi::OsString]) -> Option<Vec<String>> {
    let json = line
        .iter()
        .skip(1)
        .take_while(|word| *word != "--")
        .any(|word| word == "--json");
    if !json {
        return None;
    }
    help_topic(command, line)
}

/// the command a line asking for `-h` or `--help` is about, as `help` takes it, or nothing when
/// it asks for no help.
fn help_topic(command: &clap::Command, line: &[std::ffi::OsString]) -> Option<Vec<String>> {
    let mut topic = Vec::new();
    let mut current = command;
    let mut asked = false;
    let mut value_next = false;
    let mut names = true;
    for word in line.iter().skip(1).take_while(|word| *word != "--") {
        asked |= word == "--help" || word == "-h";
        if std::mem::take(&mut value_next) {
            continue;
        }
        let option = word.to_string_lossy();
        if option.starts_with('-') {
            let (help, value) = help_option(command, current, &option);
            asked |= help;
            value_next = value;
            continue;
        }
        let Some(word) = word.to_str() else {
            names = false;
            continue;
        };
        if names {
            match current.find_subcommand(word) {
                Some(sub) if sub.get_name() != "help" => {
                    topic.push(sub.get_name().to_owned());
                    current = sub;
                }
                _ => names = false,
            }
        }
    }
    asked.then_some(topic)
}

/// whether an option asks for help or consumes the next word. a value inside a short bundle,
/// such as the folder in `-qChome`, is neither another option nor a command name.
fn help_option(root: &clap::Command, current: &clap::Command, word: &str) -> (bool, bool) {
    let arguments = || current.get_arguments().chain(root.get_arguments());
    if let Some(long) = word.strip_prefix("--") {
        let next = !long.contains('=')
            && arguments()
                .any(|arg| arg.get_long() == Some(long) && arg.get_action().takes_values());
        return (word == "--help", next);
    }
    let mut shorts = word.trim_start_matches('-').chars();
    let mut asked = false;
    while let Some(short) = shorts.next() {
        let Some(arg) = arguments().find(|arg| arg.get_short() == Some(short)) else {
            break;
        };
        asked |= matches!(arg.get_action(), ArgAction::Help);
        if arg.get_action().takes_values() {
            return (asked, shorts.as_str().is_empty());
        }
    }
    (asked, false)
}

pub(super) fn command() -> clap::Command {
    // help is wrapped as it is printed, which keeps command lines in the examples whole
    linked(Args::command())
        .term_width(0)
        .color(if Style::stdout_color(color()) {
            clap::ColorChoice::Always
        } else {
            clap::ColorChoice::Never
        })
}

const SITE: &str = "https://nikhil-yn-aix.github.io/fetchloom";

/// every command's help ends with its page on the docs site, and the top one with where to
/// report a bug.
fn linked(root: clap::Command) -> clap::Command {
    let page = |name: &str| match name {
        "help" => format!("{SITE}/user/commands/README.html"),
        "cache gc" => format!("{SITE}/user/commands/cache.html#gc"),
        "cache verify" => format!("{SITE}/user/commands/cache.html#verify"),
        "auth login" | "auth logout" => format!("{SITE}/user/commands/auth.html"),
        _ => format!("{SITE}/user/commands/{name}.html"),
    };
    let append = |command: clap::Command, line: String| {
        let before = command
            .get_after_help()
            .map(ToString::to_string)
            .unwrap_or_default();
        let text = if before.is_empty() {
            line
        } else {
            format!("{before}\n\n{line}")
        };
        command.after_help(text)
    };
    let names: Vec<(String, Vec<String>)> = root
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| {
            let inner = command
                .get_subcommands()
                .filter(|inner| !inner.is_hide_set())
                .map(|inner| inner.get_name().to_owned())
                .collect();
            (command.get_name().to_owned(), inner)
        })
        .collect();
    let mut root = append(
        root,
        format!("docs: {SITE}\nreport a bug: https://github.com/nikhil-yn-aix/fetchloom/issues"),
    );
    for (name, inner) in names {
        root = root.mut_subcommand(&name, |mut command| {
            for sub in &inner {
                let docs = format!("docs: {}", page(&format!("{name} {sub}")));
                command = command.mut_subcommand(sub, |sub| append(sub, docs));
            }
            append(command, format!("docs: {}", page(&name)))
        });
    }
    root
}

/// help can arrive before clap parses --color, including flags after --help.
pub(super) fn color() -> ColorChoice {
    let mut choice = ColorChoice::Auto;
    let mut arguments = std::env::args_os().skip(1).take_while(|arg| arg != "--");
    while let Some(argument) = arguments.next() {
        let value = match argument.to_str() {
            Some("--color") => arguments.next().and_then(|value| value.into_string().ok()),
            Some(argument) => argument.strip_prefix("--color=").map(str::to_owned),
            None => None,
        };
        if let Some(value) = value.and_then(|value| ColorChoice::from_str(&value, false).ok()) {
            choice = value;
        }
    }
    choice
}

/// options every command takes.
#[derive(Clone, Debug, clap::Args)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a yes or no on the command line"
)]
pub(crate) struct Global {
    /// print only the answer, warnings and errors
    #[arg(
        short,
        long,
        global = true,
        conflicts_with = "verbose",
        help_heading = "options"
    )]
    pub(crate) quiet: bool,

    /// say more: -v adds detail and counters, -vv adds debug logs
    #[arg(short, long, action = ArgAction::Count, global = true, help_heading = "options")]
    pub(crate) verbose: u8,

    /// print one JSON document on stdout, not the usual output
    #[arg(long, global = true, help_heading = "options")]
    pub(crate) json: bool,

    /// use only what is already on this machine
    #[arg(long, global = true, help_heading = "options")]
    pub(crate) offline: bool,

    /// answer yes to questions, but never pick a dataset for you
    #[arg(short, long, global = true, help_heading = "options")]
    pub(crate) yes: bool,

    /// when to use color: auto, the default, always or never
    #[arg(long, value_enum, value_name = "when", default_value_t = ColorChoice::Auto, hide_default_value = true, hide_possible_values = true, global = true, help_heading = "options")]
    pub(crate) color: ColorChoice,

    /// run as if fetchloom started in <dir>
    #[arg(
        short = 'C',
        value_name = "dir",
        global = true,
        help_heading = "options"
    )]
    pub(crate) directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum Command {
    /// add a dataset to the project and download it into the data folder
    #[command(override_usage = "fetchloom add [options] <link>", help_template = TEMPLATE, after_help = examples::ADD)]
    Add(Add),

    /// remove a dataset and the unchanged files fetchloom placed for it
    #[command(override_usage = "fetchloom remove [options] <name>", help_template = TEMPLATE, mut_arg("dry_run", |arg| arg.help(LOCAL_DRY_RUN)), after_help = examples::REMOVE)]
    Remove(Remove),

    /// put the files data.lock lists into the data folder
    #[command(override_usage = "fetchloom sync [options] [name]...", help_template = TEMPLATE, after_help = examples::SYNC)]
    Sync(Sync),

    /// update data.lock from data.toml, without changing the data folder
    #[command(override_usage = "fetchloom lock [options]", help_template = TEMPLATE, after_help = examples::LOCK)]
    Lock(Lock),

    /// list files that are missing, changed locally or extra
    #[command(override_usage = "fetchloom status [options]", help_template = TEMPLATE, after_help = examples::STATUS)]
    Status(Status),

    /// list the project's datasets: source, size and folder
    #[command(override_usage = "fetchloom list [options] [name]", help_template = TEMPLATE, after_help = examples::LIST)]
    List(List),

    /// write the SHA-256 or link of each file, for other tools
    #[command(override_usage = "fetchloom export [options] [name]...", help_template = TEMPLATE, after_help = examples::EXPORT)]
    Export(Export),

    /// print where a dataset or a file in it is, syncing first if needed
    #[command(override_usage = "fetchloom path [options] <name>[/file]", help_template = TEMPLATE, after_help = examples::PATH)]
    Path(Path),

    /// show the cache, free what no project needs, or verify it
    #[command(override_usage = "fetchloom cache [options] [command]", help_template = TEMPLATE, subcommand_help_heading = "commands", subcommand_value_name = "command", after_help = examples::CACHE)]
    Cache(Cache),

    /// show each setting, its value and where it came from
    #[command(override_usage = "fetchloom settings [options]", help_template = TEMPLATE, after_help = examples::SETTINGS)]
    Settings(Settings),

    /// fetch something once, without a project
    #[command(override_usage = "fetchloom get [options] <link>...", help_template = TEMPLATE, after_help = examples::GET)]
    Get(Get),

    /// find datasets on hugging face, kaggle and zenodo
    #[command(override_usage = "fetchloom search [options] <words>...", help_template = TEMPLATE, after_help = examples::SEARCH)]
    Search(Search),

    /// sign in for private data, or data with terms to accept
    #[command(override_usage = "fetchloom auth [options] [command]", help_template = TEMPLATE, subcommand_help_heading = "commands", subcommand_value_name = "command", after_help = examples::AUTH)]
    Auth(Auth),
    /// print a script that completes fetchloom's commands in a shell
    #[command(override_usage = "fetchloom completions [options] <shell>", help_template = TEMPLATE, after_help = examples::COMPLETIONS)]
    Completions(Completions),

    /// explain the ideas behind fetchloom, or one command
    #[command(override_usage = "fetchloom help [topic]", help_template = TEMPLATE, after_help = examples::HELP)]
    Help(Help),
}

impl Command {
    /// the command's name, as `--json` reports it.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Command::Add(_) => "add",
            Command::Remove(_) => "remove",
            Command::Sync(_) => "sync",
            Command::Lock(_) => "lock",
            Command::Status(_) => "status",
            Command::List(_) => "list",
            Command::Export(_) => "export",
            Command::Path(_) => "path",
            Command::Cache(Cache { action: None }) => "cache",
            Command::Cache(Cache {
                action: Some(CacheAction::Gc(_)),
            }) => "cache gc",
            Command::Cache(Cache {
                action: Some(CacheAction::Verify),
            }) => "cache verify",
            Command::Settings(_) => "settings",
            Command::Get(_) => "get",
            Command::Search(_) => "search",
            Command::Auth(Auth { action: None }) => "auth",
            Command::Auth(Auth {
                action: Some(AuthAction::Login(_)),
            }) => "auth login",
            Command::Auth(Auth {
                action: Some(AuthAction::Logout(_)),
            }) => "auth logout",
            Command::Completions(_) => "completions",
            Command::Help(_) => "help",
        }
    }
}

/// show the cache, free what no project needs, or verify it
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Cache {
    #[command(subcommand)]
    pub(crate) action: Option<CacheAction>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum CacheAction {
    /// free what no project needs once it is a day old, or at once with --now
    #[command(override_usage = "fetchloom cache gc [options]", help_template = TEMPLATE, mut_arg("dry_run", |arg| arg.help(LOCAL_DRY_RUN)), after_help = examples::GC)]
    Gc(Gc),

    /// check cached files, so a changed one is downloaded again
    #[command(override_usage = "fetchloom cache verify [options]", help_template = TEMPLATE, after_help = examples::VERIFY)]
    Verify,
}

/// options of `cache gc`.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Gc {
    /// also free files no project needs that are less than a day old, such as a `get` from
    /// today. downloads still running keep what they have so far
    #[arg(long, help_heading = "options")]
    pub(crate) now: bool,

    #[command(flatten)]
    pub(crate) dry: DryRun,
}

/// `auth`: which accounts are signed in, or a change to one.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Auth {
    #[command(subcommand)]
    pub(crate) action: Option<AuthAction>,
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum AuthAction {
    /// sign in once; the key also works in the source's own tools
    #[command(override_usage = "fetchloom auth login [options] <account>", help_template = TEMPLATE, after_help = examples::LOGIN)]
    Login(Login),

    /// remove a key fetchloom or the source's tools saved
    #[command(override_usage = "fetchloom auth logout [options] <account>", help_template = TEMPLATE, after_help = examples::LOGOUT)]
    Logout(Logout),
}

/// options of `auth login`.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Login {
    /// hf, kaggle, zenodo, figshare, osf, dataverse, earthdata, physionet or a host name
    #[arg(value_name = "account", help_heading = "arguments")]
    pub(crate) account: String,

    /// the dataverse site, such as demo.dataverse.org. harvard's is the default
    #[arg(long, value_name = "host", help_heading = "options")]
    pub(crate) host: Option<String>,

    /// read the key from stdin instead of asking, for scripts and CI. a login reads
    /// `username:password`
    #[arg(long, help_heading = "options")]
    pub(crate) with_token: bool,
}

/// options of `auth logout`.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Logout {
    /// the account, as `auth login` takes it
    #[arg(value_name = "account", help_heading = "arguments")]
    pub(crate) account: String,

    /// the dataverse site
    #[arg(long, value_name = "host", help_heading = "options")]
    pub(crate) host: Option<String>,
}

/// words to look up in public dataset catalogs.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Search {
    /// dataset names or keywords, with or without quotes
    #[arg(value_name = "words", required = true, num_args = 1.., help_heading = "arguments")]
    pub(crate) words: Vec<String>,

    /// search only this site: hf, kaggle or zenodo. repeat it or use commas for more
    #[arg(
        long,
        value_name = "source",
        value_delimiter = ',',
        help_heading = "options"
    )]
    pub(crate) source: Vec<crate::source::search::Catalog>,

    /// keep up to this many matches from each site, 1 to 20
    #[arg(long, value_name = "n", default_value_t = 3, value_parser = clap::value_parser!(u8).range(1..=20), help_heading = "options")]
    pub(crate) limit: u8,
}

/// which files of a dataset to keep, for `add` and `get`.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Filter {
    /// keep only files matching this pattern, such as "**/*.csv". repeat it for more
    #[arg(long, value_name = "glob", help_heading = "options")]
    pub(crate) select: Vec<String>,

    /// leave out files matching this pattern. repeat it for more
    #[arg(long, value_name = "glob", help_heading = "options")]
    pub(crate) exclude: Vec<String>,
}

/// `--dry-run` help for commands that never ask a source anything.
const LOCAL_DRY_RUN: &str = "show what would change on disk without changing it";

/// `--dry-run`, for the commands that change files.
#[derive(Clone, Copy, Debug, clap::Args)]
pub(crate) struct DryRun {
    /// show what would change on disk without changing it. sources may still be asked what a
    /// link holds
    #[arg(short = 'n', long, help_heading = "options")]
    pub(crate) dry_run: bool,
}

/// add a dataset to the project and download it into the data folder
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Add {
    /// search words, a link, a DOI, or an id such as zenodo:3608990
    #[arg(value_name = "link", help_heading = "arguments")]
    pub(crate) link: String,

    /// the dataset's name in data.toml and its folder's name
    #[arg(long = "as", value_name = "name", help_heading = "options")]
    pub(crate) name: Option<String>,

    #[command(flatten)]
    pub(crate) filter: Filter,

    /// keep zip, 7z and tar archives packed, as extract = false in data.toml does
    #[arg(long, help_heading = "options")]
    pub(crate) no_extract: bool,

    #[command(flatten)]
    pub(crate) dry: DryRun,
}

/// remove a dataset and the unchanged files fetchloom placed for it
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Remove {
    /// the dataset's name in data.toml
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) name: String,

    #[command(flatten)]
    pub(crate) dry: DryRun,
}

/// put the files data.lock lists into the data folder
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Sync {
    /// the datasets to sync, by their names in data.toml; all of them when none is named. the
    /// others keep their entries in data.lock and their files as they are
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) names: Vec<String>,

    /// replace edited files, and files fetchloom did not place, at the paths data.lock lists,
    /// naming each. it finds changes by size and time; add --verify for changes that kept both
    #[arg(long, help_heading = "options")]
    pub(crate) force: bool,

    /// fail instead of updating data.lock when it is out of date with data.toml, for CI
    #[arg(long, help_heading = "options")]
    pub(crate) locked: bool,

    #[command(flatten)]
    pub(crate) dry: DryRun,

    /// check the SHA-256 of every placed file, and put back those that changed though their
    /// size and time did not
    #[arg(long, help_heading = "options")]
    pub(crate) verify: bool,
}

/// list files that are missing, changed locally or extra
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Status {
    /// check the SHA-256 of every placed file, so a change that kept its size and time shows
    /// too
    #[arg(long, help_heading = "options")]
    pub(crate) verify: bool,
}

/// bring data.lock in line with data.toml
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Lock {
    /// check sources again for newer data, keeping any version data.toml names
    #[arg(long, value_name = "name", num_args = 0.., help_heading = "options")]
    pub(crate) upgrade: Option<Vec<String>>,

    /// change nothing, and fail when data.lock is out of date, for CI
    #[arg(long, conflicts_with = "upgrade", help_heading = "options")]
    pub(crate) check: bool,

    /// after a git merge, keep the checked-out branch's side of a dataset the two sides
    /// changed each their own way. repeat it for more
    #[arg(long, value_name = "name", conflicts_with_all = ["check", "upgrade"], help_heading = "options")]
    pub(crate) ours: Vec<String>,

    /// after a git merge, keep the merged branch's side of such a dataset. repeat it for more
    #[arg(long, value_name = "name", conflicts_with_all = ["check", "upgrade"], help_heading = "options")]
    pub(crate) theirs: Vec<String>,

    #[command(flatten)]
    pub(crate) dry: DryRun,
}

/// show each setting, its value and where it came from
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Settings {}

/// list the project's datasets, or the files of one
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct List {
    /// a dataset's name, to list the files data.lock has for it
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) name: Option<String>,
}

/// write the SHA-256 or link of each file, for other tools
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Export {
    /// the datasets to export, by their names in data.toml; all of them when none is named
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) names: Vec<String>,

    /// sha256 for each file in the data folder, as `sha256sum -c` reads them; links for each
    /// file downloaded, with its SHA-256; aria2 for an `aria2c -i` input file
    #[arg(long, value_enum, value_name = "format", default_value_t = Format::Sha256, hide_default_value = true, hide_possible_values = true, help_heading = "options")]
    pub(crate) format: Format,

    /// write to <file> instead of stdout. a file already there is kept
    #[arg(short, long, value_name = "file", help_heading = "options")]
    pub(crate) output: Option<PathBuf>,

    /// replace the file -o names when it exists
    #[arg(long, requires = "output", help_heading = "options")]
    pub(crate) force: bool,
}

/// what `export` writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(crate) enum Format {
    /// `<sha256>  <path>` for each file in the data folder, archive members included.
    Sha256,
    /// `<link> <sha256>` for each file downloaded.
    Links,
    /// each file downloaded, as an aria2c input file with `out=` and `checksum=`.
    Aria2,
}

/// print where a dataset or a file in it is
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Path {
    /// a dataset's name, or a file inside it as name/path
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) target: String,

    /// fail instead of updating data.lock when it is out of date with data.toml, for CI
    #[arg(long, help_heading = "options")]
    pub(crate) locked: bool,
}

/// fetch something once, without a project
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Get {
    /// what to download: a link, a DOI, an id such as zenodo:3608990, or search words. several
    /// links download together, but search words go alone, in quotes
    #[arg(
        value_name = "link",
        required_unless_present = "input_file",
        help_heading = "arguments"
    )]
    pub(crate) links: Vec<String>,

    /// write the downloaded file to <file> as it came, never unpacked. needs one link to one
    /// file
    #[arg(
        short,
        long,
        value_name = "file",
        conflicts_with = "dir",
        help_heading = "options"
    )]
    pub(crate) output: Option<PathBuf>,

    /// replace a file already at <file> that holds other bytes. it stays whole until the new
    /// file takes its name
    #[arg(
        long,
        requires = "output",
        conflicts_with = "dir",
        help_heading = "options"
    )]
    pub(crate) force: bool,

    /// put the download in <folder> instead of a new folder named after it. with several
    /// links, each gets its own folder inside <folder>
    #[arg(short, long, value_name = "folder", help_heading = "options")]
    pub(crate) dir: Option<PathBuf>,

    /// read links from <file>, one per line, or from stdin for -. blank lines and lines
    /// starting with # are skipped
    #[arg(short, long, value_name = "file", help_heading = "options")]
    pub(crate) input_file: Option<PathBuf>,

    /// check that the file has this SHA-256 before it takes its name or goes in the folder.
    /// needs one link to one file
    #[arg(long, value_name = "hex", value_parser = sha256, help_heading = "options")]
    pub(crate) sha256: Option<Sha256>,

    #[command(flatten)]
    pub(crate) filter: Filter,

    /// keep zip, 7z and tar archives packed instead of unpacking them
    #[arg(long, help_heading = "options")]
    pub(crate) no_extract: bool,
}

/// reads `--sha256`: 64 hex digits, in either case.
fn sha256(text: &str) -> Result<Sha256, String> {
    Sha256::from_hex(text.trim()).ok_or_else(|| "a SHA-256 is 64 hex digits".to_owned())
}

/// the shell a completion script is for.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Completions {
    /// the shell that loads the script
    #[arg(value_name = "shell", value_enum, help_heading = "arguments")]
    pub(crate) shell: clap_complete::Shell,
}

/// explain the ideas behind fetchloom, or one command
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Help {
    /// concepts, environment, exit-codes, or a command such as add or cache gc
    #[arg(value_name = "topic", help_heading = "arguments")]
    pub(crate) topic: Vec<String>,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::Args;

    #[test]
    fn force_help_names_files_fetchloom_did_not_place() -> Result<(), &'static str> {
        let help = Args::command()
            .find_subcommand_mut("sync")
            .ok_or("sync command is missing")?
            .render_help()
            .to_string();
        assert!(
            help.contains("replace edited files, and files fetchloom did not place"),
            "{help}"
        );
        Ok(())
    }

    #[test]
    fn cache_verify_and_auth_each_have_one_name() {
        for line in [
            &["fetchloom", "cache", "check"][..],
            &["fetchloom", "auth", "status"][..],
        ] {
            assert_eq!(
                Args::try_parse_from(line).unwrap_err().kind(),
                clap::error::ErrorKind::InvalidSubcommand
            );
        }
        assert_eq!(
            Args::try_parse_from(["fetchloom", "auth"])
                .unwrap()
                .command
                .unwrap()
                .name(),
            "auth"
        );
        assert_eq!(
            Args::try_parse_from(["fetchloom", "cache", "verify"])
                .unwrap()
                .command
                .unwrap()
                .name(),
            "cache verify"
        );
    }

    #[test]
    fn bundled_help_does_not_read_an_option_value_as_flags() {
        let root = Args::command();
        let get = root.find_subcommand("get").unwrap();
        for (word, expected) in [
            ("-qh", (true, false)),
            ("-qC", (false, true)),
            ("-qCcache", (false, false)),
            ("-qCh", (false, false)),
            ("-dh", (false, false)),
            ("--dir=--help", (false, false)),
        ] {
            assert_eq!(super::help_option(&root, get, word), expected, "{word}");
        }
    }

    /// -o writes one file and --dir fills a folder, so a get takes one of them, and --force
    /// replaces only the file -o names.
    #[test]
    fn output_and_dir_exclude_each_other_and_force_needs_output() {
        for (line, kind) in [
            (
                &["fetchloom", "get", "zenodo:1", "-o", "a.csv", "--dir", "a"][..],
                clap::error::ErrorKind::ArgumentConflict,
            ),
            (
                &["fetchloom", "get", "zenodo:1", "--force"],
                clap::error::ErrorKind::MissingRequiredArgument,
            ),
            (
                &["fetchloom", "get", "zenodo:1", "--dir", "a", "--force"],
                clap::error::ErrorKind::ArgumentConflict,
            ),
        ] {
            assert_eq!(
                Args::try_parse_from(line).unwrap_err().kind(),
                kind,
                "{line:?}"
            );
        }
        assert!(
            Args::try_parse_from(["fetchloom", "get", "zenodo:1", "-o", "a.csv", "--force"])
                .is_ok()
        );
    }

    #[test]
    fn upgrade_help_keeps_the_versions_data_toml_names() -> Result<(), &'static str> {
        let help = Args::command()
            .find_subcommand_mut("lock")
            .ok_or("lock command is missing")?
            .render_help()
            .to_string();
        assert!(
            help.contains("check sources again for newer data, keeping any version data.toml"),
            "{help}"
        );
        Ok(())
    }

    /// cache help says what verify means for a changed file, without the note it leaves inside
    /// the cache to get there.
    #[test]
    fn cache_help_says_a_changed_file_is_downloaded_again() {
        let help = every_help();
        let verify = help
            .iter()
            .find(|(name, _)| name == "cache verify")
            .unwrap();
        assert!(
            verify
                .1
                .contains("check cached files, so a changed one is downloaded again"),
            "{}",
            verify.1
        );
        assert!(!verify.1.contains("deletes cached files"));
        let cache = help.iter().find(|(name, _)| name == "cache").unwrap();
        // help wraps to the terminal's width, so a phrase may cross a line
        let flat = cache.1.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("`cache verify` notes changed ones, which are downloaded again"),
            "{flat}"
        );
        for (name, text) in [verify, cache] {
            assert!(!text.contains("mark"), "{name}: {text}");
        }
    }

    /// the words a shell makes of `line`, where double quotes keep a pattern such as "*.csv"
    /// whole.
    fn words(line: &str) -> Vec<String> {
        let mut words = Vec::new();
        let mut word: Option<String> = None;
        let mut quoted = false;
        for c in line.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    word.get_or_insert_default();
                }
                c if c.is_whitespace() && !quoted => words.extend(word.take()),
                c => word.get_or_insert_default().push(c),
            }
        }
        words.extend(word);
        words
    }

    /// the help of every command and subcommand, as `fetchloom <command> --help` prints it.
    fn every_help() -> Vec<(String, String)> {
        let root = Args::command();
        let mut names: Vec<Vec<String>> = Vec::new();
        for command in root
            .get_subcommands()
            .filter(|command| !command.is_hide_set())
        {
            names.push(vec![command.get_name().to_owned()]);
            for inner in command
                .get_subcommands()
                .filter(|inner| !inner.is_hide_set())
            {
                names.push(vec![
                    command.get_name().to_owned(),
                    inner.get_name().to_owned(),
                ]);
            }
        }
        names
            .into_iter()
            .map(|name| {
                let line = std::iter::once("fetchloom".to_owned())
                    .chain(name.iter().cloned())
                    .chain(std::iter::once("--help".to_owned()));
                let help = match Args::command().try_get_matches_from(line) {
                    Ok(_) => String::new(),
                    Err(err) => err.render().to_string(),
                };
                (name.join(" "), help)
            })
            .collect()
    }

    /// help wraps at the terminal's width between words: no width from 40 to 100 columns, the
    /// range help uses, splits a word or loses one.
    #[test]
    fn help_wraps_between_words_and_never_inside_one() {
        let mut screens = every_help();
        let top = match Args::command().try_get_matches_from(["fetchloom", "--help"]) {
            Ok(_) => String::new(),
            Err(err) => err.render().to_string(),
        };
        screens.push(("fetchloom".to_owned(), top));
        for (name, help) in screens {
            let words: Vec<&str> = help.split_whitespace().collect();
            assert!(words.len() > 20, "{name}\n{help}");
            for width in 40..=100 {
                let wrapped = crate::output::wrap_help(&help, width);
                assert_eq!(
                    wrapped.split_whitespace().collect::<Vec<_>>(),
                    words,
                    "{name} at {width} columns\n{wrapped}"
                );
            }
        }
    }

    /// help is read by people whose first language is not english: a screen that says "pin"
    /// explains it, and no screen uses the kitchen analogy, which belongs to `help concepts`, or
    /// other words that stand for a thing instead of naming it.
    #[test]
    fn help_screens_name_things_plainly() {
        let mut screens = every_help();
        let top = match Args::command().try_get_matches_from(["fetchloom", "--help"]) {
            Ok(_) => String::new(),
            Err(err) => err.render().to_string(),
        };
        screens.push(("fetchloom".to_owned(), top));
        for (name, help) in &screens {
            let flat = help.split_whitespace().collect::<Vec<_>>().join(" ");
            if flat.contains(" pin") {
                assert!(
                    flat.contains(
                        "pinning recording a dataset's exact files and their SHA-256 in data.lock"
                    ),
                    "{name} says pin without explaining it\n{help}"
                );
            }
            for word in [
                "shopping list",
                "receipt",
                "pantry",
                "restocking",
                "fingerprint",
                "the story",
                "approve",
                "catalog",
                "canonical",
                "gated",
                "untracked",
                "locked paths",
                "partial files",
                "one a line",
            ] {
                assert!(!flat.contains(word), "{name} says {word}\n{help}");
            }
        }
        let top = &screens.last().unwrap().1;
        let flat = top.split_whitespace().collect::<Vec<_>>().join(" ");
        for option in [
            "--json print one JSON document on stdout, not the usual output",
            "-y, --yes answer yes to questions, but never pick a dataset for you",
        ] {
            assert!(flat.contains(option), "{option}\n{top}");
        }
    }

    #[test]
    fn every_command_shows_examples_to_copy_and_what_it_changes_on_disk() {
        for (name, help) in every_help() {
            let examples: Vec<&str> = help
                .split_once("\nexamples:\n")
                .map(|(_, rest)| {
                    rest.lines()
                        .take_while(|line| line.starts_with("  "))
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                (2..=4).contains(&examples.len()),
                "{name} shows {} examples\n{help}",
                examples.len()
            );
            for example in examples {
                assert!(
                    example.starts_with(&format!("  fetchloom {name}")),
                    "{name}: {example}"
                );
                // a redirect, a pipe or a comment belongs to the shell, not to fetchloom
                let command = example.split(['>', '|', '#']).next().unwrap_or(example);
                let words = words(command);
                if let Err(err) = Args::try_parse_from(&words) {
                    panic!("{name}: `{example}` does not parse: {err}");
                }
            }
            let disk: Vec<&str> = help
                .lines()
                .filter(|line| line.starts_with("on disk: "))
                .collect();
            assert_eq!(disk.len(), 1, "{name}\n{help}");
        }
    }
}
