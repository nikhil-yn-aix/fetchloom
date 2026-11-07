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
  fetchloom sync --offline
  fetchloom sync --locked
  fetchloom sync --force --dry-run

on disk: updates the data folder from data.lock, writing data.lock first if needed unless --locked. reports files it cannot replace without --force. --dry-run writes nothing";

    pub(super) const LOCK: &str = "examples:
  fetchloom lock
  fetchloom lock --check
  fetchloom lock --upgrade iris

on disk: writes data.lock and may add files to the cache. --check writes nothing";

    pub(super) const STATUS: &str = "examples:
  fetchloom status
  fetchloom status --json

on disk: nothing. it reads data.toml, data.lock and data/";

    pub(super) const LIST: &str = "examples:
  fetchloom list
  fetchloom list iris
  fetchloom list --json

on disk: nothing. it reads data.toml and data.lock";

    pub(super) const PATH: &str = "examples:
  fetchloom path iris
  fetchloom path iris/Iris.csv
  fetchloom path iris -q

on disk: nothing when the dataset is ready. otherwise it syncs first, as `fetchloom sync` does";

    pub(super) const CACHE: &str = "examples:
  fetchloom cache
  fetchloom cache gc
  fetchloom cache gc --now
  fetchloom cache verify

on disk: `cache` reports usage. `cache gc` deletes unused cached files; `cache verify` marks changed files for replacement. neither changes data/";

    pub(super) const GC: &str = "examples:
  fetchloom cache gc
  fetchloom cache gc --now
  fetchloom cache gc --now --dry-run
  fetchloom cache gc --json

on disk: deletes cached files no project needs once they are a day old, or at once with --now. data/ stays as it is. --dry-run deletes nothing";

    pub(super) const VERIFY: &str = "examples:
  fetchloom cache verify
  fetchloom cache verify --json

on disk: marks changed cached files for replacement on the next download. data/ stays as it is";

    pub(super) const GET: &str = "examples:
  fetchloom get iris
  fetchloom get https://huggingface.co/datasets/scikit-learn/iris -o iris
  fetchloom get zenodo:3608990 figshare:34744689@1 -o downloads
  fetchloom get -i links.txt

on disk: writes one output folder for each link through the cache. does not create project files";

    pub(super) const SEARCH: &str = "examples:
  fetchloom search iris
  fetchloom search fashion mnist
  fetchloom search iris --source kaggle,zenodo --limit 5
  fetchloom search iris --json

on disk: nothing. prints canonical references without downloading a dataset";

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

on disk: hugging face keys go where `hf auth login` puts them, kaggle keys in ~/.kaggle, logins in
.netrc for curl and wget too, and other keys in the system's credential store. asks before saving.
--with-token reads the key from stdin, as in `echo $HF_TOKEN | fetchloom auth login hf --with-token`";

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
/// fetchloom does, the analogy, commands to copy and where to learn more.
pub(crate) const INTRO: &str =
    "fetchloom puts the exact research data a project needs into its data/ folder.

  data.toml, the shopping list: which datasets you want
  data.lock, the receipt: the exact files and their fingerprints
  the cache, the pantry: shared by projects using it
  sync, restocking: fill data/ from the receipt

  fetchloom add iris    pick an iris dataset, put it in data/ and pin it
  fetchloom get iris    just download it, without a project
  fetchloom sync        fill data/ in a project you cloned

a name searches hugging face, kaggle and zenodo. a link works as well:
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
    let words: Vec<_> = line
        .iter()
        .skip(1)
        .take_while(|word| *word != "--")
        .collect();
    if !words.iter().any(|word| *word == "--help" || *word == "-h") {
        return None;
    }
    let mut topic = Vec::new();
    let mut current = command;
    // the value of an option such as -C or --color is no command name
    let takes_value = |owner: &clap::Command, word: &str| {
        owner.get_arguments().any(|arg| {
            arg.get_action().takes_values()
                && (word
                    .strip_prefix("--")
                    .is_some_and(|long| arg.get_long() == Some(long))
                    || (word.len() == 2
                        && word
                            .strip_prefix('-')
                            .and_then(|short| short.chars().next())
                            == arg.get_short()
                        && arg.get_short().is_some()))
        })
    };
    let mut value_next = false;
    for word in words.into_iter().map_while(|word| word.to_str()) {
        if std::mem::take(&mut value_next) {
            continue;
        }
        if word.starts_with('-') {
            value_next =
                !word.contains('=') && (takes_value(command, word) || takes_value(current, word));
            continue;
        }
        match current.find_subcommand(word) {
            Some(sub) if sub.get_name() != "help" => {
                topic.push(sub.get_name().to_owned());
                current = sub;
            }
            _ => break,
        }
    }
    Some(topic)
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

    /// print one JSON document on stdout instead of the story
    #[arg(long, global = true, help_heading = "options")]
    pub(crate) json: bool,

    /// use only what is already on this machine
    #[arg(long, global = true, help_heading = "options")]
    pub(crate) offline: bool,

    /// approve confirmations, but never choose a dataset for you
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
    /// add a dataset, pin it and download it into the data folder
    #[command(override_usage = "fetchloom add [options] <link>", help_template = TEMPLATE, after_help = examples::ADD)]
    Add(Add),

    /// remove a dataset and the unchanged files fetchloom placed for it
    #[command(override_usage = "fetchloom remove [options] <name>", help_template = TEMPLATE, mut_arg("dry_run", |arg| arg.help(LOCAL_DRY_RUN)), after_help = examples::REMOVE)]
    Remove(Remove),

    /// place the files pinned in data.lock on this machine
    #[command(override_usage = "fetchloom sync [options]", help_template = TEMPLATE, after_help = examples::SYNC)]
    Sync(Sync),

    /// update data.lock from data.toml without placing any files
    #[command(override_usage = "fetchloom lock [options]", help_template = TEMPLATE, after_help = examples::LOCK)]
    Lock(Lock),

    /// list files that are missing, changed locally or extra
    #[command(override_usage = "fetchloom status [options]", help_template = TEMPLATE, after_help = examples::STATUS)]
    Status,

    /// list the project's datasets: source, size and folder
    #[command(override_usage = "fetchloom list [options] [name]", help_template = TEMPLATE, after_help = examples::LIST)]
    List(List),

    /// print where a dataset or a file in it is, syncing first if needed
    #[command(override_usage = "fetchloom path [options] <name>[/file]", help_template = TEMPLATE, after_help = examples::PATH)]
    Path(Path),

    /// show the cache, free what no project needs, or verify it
    #[command(override_usage = "fetchloom cache [options] [command]", help_template = TEMPLATE, subcommand_help_heading = "commands", subcommand_value_name = "command", after_help = examples::CACHE)]
    Cache(Cache),

    /// fetch something once, without a project
    #[command(override_usage = "fetchloom get [options] <link>...", help_template = TEMPLATE, after_help = examples::GET)]
    Get(Get),

    /// find datasets on hugging face, kaggle and zenodo
    #[command(override_usage = "fetchloom search [options] <words>...", help_template = TEMPLATE, after_help = examples::SEARCH)]
    Search(Search),

    /// sign in to sources for private and gated data
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
            Command::Status => "status",
            Command::List(_) => "list",
            Command::Path(_) => "path",
            Command::Cache(Cache { action: None }) => "cache",
            Command::Cache(Cache {
                action: Some(CacheAction::Gc(_)),
            }) => "cache gc",
            Command::Cache(Cache {
                action: Some(CacheAction::Verify),
            }) => "cache verify",
            Command::Get(_) => "get",
            Command::Search(_) => "search",
            Command::Auth(Auth {
                action: None | Some(AuthAction::Status),
            }) => "auth",
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

    /// check cached files and mark changed files for replacement
    #[command(alias = "check", override_usage = "fetchloom cache verify [options]", help_template = TEMPLATE, after_help = examples::VERIFY)]
    Verify,
}

/// options of `cache gc`.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Gc {
    /// also free files no project needs that are less than a day old, such as a `get` from
    /// today. downloads still running keep their partial files
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

    /// list each account, as plain `fetchloom auth` does
    #[command(hide = true, override_usage = "fetchloom auth [options]", help_template = TEMPLATE)]
    Status,
}

/// options of `auth login`.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Login {
    /// hf, kaggle, zenodo, figshare, osf, dataverse, earthdata, physionet or a host name
    #[arg(value_name = "account", help_heading = "arguments")]
    pub(crate) account: String,

    /// the dataverse installation, such as demo.dataverse.org. harvard's is the default
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

    /// the dataverse installation
    #[arg(long, value_name = "host", help_heading = "options")]
    pub(crate) host: Option<String>,
}

/// words to look up in public dataset catalogs.
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Search {
    /// dataset names or keywords, with or without quotes
    #[arg(value_name = "words", required = true, num_args = 1.., help_heading = "arguments")]
    pub(crate) words: Vec<String>,

    /// ask only this catalog: hf, kaggle or zenodo. repeat it or use commas for more
    #[arg(
        long,
        value_name = "source",
        value_delimiter = ',',
        help_heading = "options"
    )]
    pub(crate) source: Vec<crate::source::search::Catalog>,

    /// keep up to this many matches from each catalog, 1 to 20
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

/// add a dataset, pin it and download it into the data folder
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

    /// keep zip and tar archives packed, as extract = false in data.toml does
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

/// place the files pinned in data.lock on this machine
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Sync {
    /// replace conflicting files at locked paths, including untracked files
    #[arg(long, help_heading = "options")]
    pub(crate) force: bool,

    /// fail instead of updating data.lock when it is out of date with data.toml, for CI
    #[arg(long, help_heading = "options")]
    pub(crate) locked: bool,

    #[command(flatten)]
    pub(crate) dry: DryRun,
}

/// bring data.lock in line with data.toml
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Lock {
    /// check sources again, keeping explicitly pinned versions
    #[arg(long, value_name = "name", num_args = 0.., help_heading = "options")]
    pub(crate) upgrade: Option<Vec<String>>,

    /// change nothing, and fail when data.lock is out of date, for CI
    #[arg(long, conflicts_with = "upgrade", help_heading = "options")]
    pub(crate) check: bool,
}

/// list the project's datasets, or the files of one
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct List {
    /// a dataset's name, to list its locked files
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) name: Option<String>,
}

/// print where a dataset or a file in it is
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Path {
    /// a dataset's name, or a file inside it as name/path
    #[arg(value_name = "name", help_heading = "arguments")]
    pub(crate) target: String,
}

/// fetch something once, without a project
#[derive(Clone, Debug, clap::Args)]
pub(crate) struct Get {
    /// what to download: search words, a link, a DOI, or an id such as zenodo:3608990. several
    /// links download together, so search words in quotes go alone
    #[arg(
        value_name = "link",
        required_unless_present = "input_file",
        help_heading = "arguments"
    )]
    pub(crate) links: Vec<String>,

    /// put the download in <dir> instead of a new folder named after it. with several links,
    /// each gets its own folder inside <dir>
    #[arg(short, long, value_name = "dir", help_heading = "options")]
    pub(crate) output: Option<PathBuf>,

    /// read links from <file>, one a line, or from stdin for -. blank lines and lines starting
    /// with # are skipped
    #[arg(short, long, value_name = "file", help_heading = "options")]
    pub(crate) input_file: Option<PathBuf>,

    /// check the file's SHA-256 before placing it. needs one link to one file
    #[arg(long, value_name = "hex", value_parser = sha256, help_heading = "options")]
    pub(crate) sha256: Option<Sha256>,

    #[command(flatten)]
    pub(crate) filter: Filter,

    /// keep zip and tar archives packed instead of unpacking them
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
    fn force_help_names_untracked_conflicts() -> Result<(), &'static str> {
        let help = Args::command()
            .find_subcommand_mut("sync")
            .ok_or("sync command is missing")?
            .render_help()
            .to_string();
        assert!(
            help.contains("replace conflicting files at locked paths, including untracked files"),
            "{help}"
        );
        Ok(())
    }

    #[test]
    fn upgrade_help_keeps_explicit_pins() -> Result<(), &'static str> {
        let help = Args::command()
            .find_subcommand_mut("lock")
            .ok_or("lock command is missing")?
            .render_help()
            .to_string();
        assert!(
            help.contains("check sources again, keeping explicitly pinned versions"),
            "{help}"
        );
        Ok(())
    }

    #[test]
    fn cache_help_describes_marking_changed_files_for_replacement() {
        let help = every_help();
        let verify = help
            .iter()
            .find(|(name, _)| name == "cache verify")
            .unwrap();
        assert!(verify.1.contains("mark changed files for replacement"));
        assert!(!verify.1.contains("deletes cached files"));
        let cache = help.iter().find(|(name, _)| name == "cache").unwrap();
        // help wraps to the terminal's width, so a phrase may cross a line
        let flat = cache.1.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            flat.contains("`cache verify` marks changed files for replacement"),
            "{flat}"
        );
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
