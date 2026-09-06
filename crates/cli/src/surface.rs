//! The command surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fetchloom_engine::compression::CompressionChoice;
use fetchloom_engine::limits::Bandwidth;
use fetchloom_engine::selection::Layout;

/// How progress is presented.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum DisplayMode {
    /// One aggregated progress line and the final result.
    Plain,
    /// A redrawn view of the run's internals.
    Live,
    /// No progress output.
    None,
}

/// When output carries color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ColorChoice {
    /// Color when the stream is a terminal that permits it.
    Auto,
    /// Color whatever the stream is.
    Always,
    /// Never color.
    Never,
}

/// A shell a completion script can be written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum Shell {
    /// The Bourne again shell.
    Bash,
    /// The Elvish shell.
    Elvish,
    /// The friendly interactive shell.
    Fish,
    /// PowerShell.
    Powershell,
    /// The Z shell.
    Zsh,
}

/// Flags that apply to every command.
#[derive(Args, Clone, Debug, Default)]
#[command(next_help_heading = "Everywhere")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag the contract names, and they are independent"
)]
pub struct GlobalFlags {
    /// Use this configuration file only.
    #[arg(hide_short_help = true, long, global = true, value_name = "path")]
    pub config: Option<PathBuf>,
    /// Ignore all configuration files.
    #[arg(hide_short_help = true, long, global = true)]
    pub no_config: bool,
    /// Forbid all network activity.
    #[arg(long, global = true)]
    pub offline: bool,
    /// Machine-readable result on stdout.
    #[arg(long, global = true)]
    pub json: bool,
    /// Newline-delimited event stream.
    #[arg(hide_short_help = true, long, global = true, value_name = "path|-")]
    pub events: Option<String>,
    /// Suppress progress.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Raise the log level one step. Repeatable.
    #[arg(hide_short_help = true, long, short, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// When output is colored.
    #[arg(
        hide_short_help = true,
        long,
        global = true,
        value_name = "auto|always|never"
    )]
    pub color: Option<ColorChoice>,
    /// Never print a hint.
    #[arg(hide_short_help = true, long, global = true)]
    pub no_hints: bool,
    /// Progress presentation.
    #[arg(
        hide_short_help = true,
        long,
        global = true,
        value_name = "plain|live|none"
    )]
    pub display: Option<DisplayMode>,
    /// Disable redrawing.
    #[arg(hide_short_help = true, long, global = true)]
    pub no_animation: bool,
    /// Ceiling on threads used for processor work.
    #[arg(hide_short_help = true, long, global = true, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub threads: Option<u32>,
    /// Where the cache is.
    #[arg(hide_short_help = true, long, global = true, value_name = "path")]
    pub cache_dir: Option<PathBuf>,
    /// Answer every confirmation with yes.
    #[arg(hide_short_help = true, long, global = true)]
    pub yes: bool,
    /// How cached objects are stored.
    #[arg(
        hide_short_help = true,
        long,
        global = true,
        value_name = "auto|none|zstd:1..19"
    )]
    pub compress: Option<CompressArg>,
}

/// How far a write is pushed before publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub(crate) enum DurabilityChoice {
    /// Flush the file and its containing directory to the device.
    Strict,
    /// Flush the file.
    Normal,
    /// Flush nothing and rely on the atomic rename alone.
    Fast,
}

/// What a cache hit is checked against before it is reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum VerifyChoice {
    /// Reread and rehash the whole object.
    Always,
    /// Trust the object when its recorded filesystem fingerprint matches.
    Fingerprint,
    /// Trust the object unconditionally, which makes the result unverified.
    Never,
}

/// Which write path a run takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub(crate) enum IoChoice {
    /// Let the volume's own capabilities decide.
    Auto,
    /// Write through the operating system's page cache.
    Buffered,
    /// Write without leaving the bytes in the operating system's page cache.
    Uncached,
}

/// The value `--compress` was given, parsed into how objects are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompressArg(pub CompressionChoice);

impl std::str::FromStr for CompressArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// The value `--bandwidth` was given, parsed into a ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RateArg(pub Bandwidth);

impl std::str::FromStr for RateArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// The value `--timeout` was given, parsed into a span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DurationArg(pub std::time::Duration);

impl std::str::FromStr for DurationArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let refusal = || format!("{text} is not a duration such as 30s, 500ms, or 2m");
        let (digits, unit) = text.split_at(
            text.find(|letter: char| !letter.is_ascii_digit())
                .ok_or_else(refusal)?,
        );
        let count: u64 = digits.parse().map_err(|_| refusal())?;
        let span = match unit {
            "ms" => std::time::Duration::from_millis(count),
            "s" => std::time::Duration::from_secs(count),
            "m" => std::time::Duration::from_secs(count.saturating_mul(60)),
            "h" => std::time::Duration::from_secs(count.saturating_mul(3600)),
            _ => return Err(refusal()),
        };
        if span.is_zero() {
            return Err(refusal());
        }
        Ok(Self(span))
    }
}

/// The value `--layout` was given, parsed into what selection acts on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LayoutArg(pub Layout);

impl std::str::FromStr for LayoutArg {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self)
    }
}

/// Flags for the commands that materialize bytes.
#[derive(Args, Clone, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a flag the contract names, and they are independent"
)]
pub struct TransferFlags {
    /// Destination directory.
    #[arg(help_heading = "Where it lands", long, short, value_name = "path")]
    pub output: Option<PathBuf>,
    /// Include members. Repeatable.
    #[arg(
        help_heading = "Choosing what to take",
        long = "select",
        value_name = "glob"
    )]
    pub select: Vec<String>,
    /// Exclude members. Repeatable.
    #[arg(
        help_heading = "Choosing what to take",
        long = "exclude",
        value_name = "glob"
    )]
    pub exclude: Vec<String>,
    /// Path rewriting.
    #[arg(help_heading = "Where it lands", long, value_name = "keep|flatten:n")]
    pub(crate) layout: Option<LayoutArg>,
    /// How far a write is pushed before publication.
    #[arg(
        help_heading = "Proving what you got",
        long,
        value_name = "strict|normal|fast"
    )]
    pub(crate) durability: Option<DurabilityChoice>,
    /// Lock file location.
    #[arg(help_heading = "Proving what you got", long, value_name = "path")]
    pub lock: Option<PathBuf>,
    /// Fail when resolution differs from the lock.
    #[arg(help_heading = "Proving what you got", long)]
    pub locked: bool,
    /// Bypass the cache for this operation.
    #[arg(help_heading = "Proving what you got", long)]
    pub(crate) no_cache: bool,
    /// Keep a recognized archive as a file rather than extracting it.
    #[arg(help_heading = "Where it lands", long)]
    pub(crate) no_extract: bool,
    /// What a cache hit is checked against before it is reused.
    #[arg(
        help_heading = "Proving what you got",
        long,
        value_name = "always|fingerprint|never"
    )]
    pub(crate) verify: Option<VerifyChoice>,
    /// Overwrite modified destination entries and remove foreign ones.
    #[arg(help_heading = "Where it lands", long)]
    pub force: bool,
    /// Accept current destination contents as correct.
    #[arg(help_heading = "Where it lands", long)]
    pub adopt: bool,
    /// Ceiling on transfers in flight across every host.
    #[arg(help_heading = "Speed and politeness", hide_short_help = true, long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub concurrency: Option<u32>,
    /// Ceiling on transfers in flight for one host.
    #[arg(help_heading = "Speed and politeness", hide_short_help = true, long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub per_host: Option<u32>,
    /// Ceiling on how fast the run may transfer.
    #[arg(
        help_heading = "Speed and politeness",
        hide_short_help = true,
        long,
        value_name = "rate"
    )]
    pub(crate) bandwidth: Option<RateArg>,
    /// Attempts per transient failure.
    #[arg(help_heading = "Speed and politeness", hide_short_help = true, long, value_name = "n", value_parser = clap::value_parser!(u32).range(1..))]
    pub retries: Option<u32>,
    /// Idle timeout per connection.
    #[arg(
        help_heading = "Speed and politeness",
        hide_short_help = true,
        long,
        value_name = "duration"
    )]
    pub(crate) timeout: Option<DurationArg>,
    /// Which write path a run takes.
    #[arg(
        help_heading = "Speed and politeness",
        hide_short_help = true,
        long,
        value_name = "auto|buffered|uncached"
    )]
    pub(crate) io: Option<IoChoice>,
    /// Raise politeness ceilings.
    #[arg(help_heading = "Speed and politeness", hide_short_help = true, long)]
    pub aggressive: bool,
    /// Disable adaptation, so two runs do identical work.
    #[arg(help_heading = "Speed and politeness", hide_short_help = true, long)]
    pub(crate) deterministic_io: bool,
}

/// What to do with the cache.
#[derive(Subcommand, Clone, Debug)]
pub enum CacheCommand {
    /// Report what the cache holds.
    Status,
    /// List the objects the cache holds.
    Ls,
    /// Reread and rehash every object, quarantining each mismatch.
    Verify,
    /// Keep an object from ever being pruned.
    Pin {
        /// The content digest to pin.
        #[arg(value_name = "digest")]
        digest: String,
    },
    /// Remove the mark that kept an object from being pruned.
    Unpin {
        /// The content digest to unpin.
        #[arg(value_name = "digest")]
        digest: String,
    },
    /// Write every object the cache holds into a bundle.
    Export {
        /// Where the bundle is written.
        #[arg(value_name = "bundle")]
        bundle: PathBuf,
    },
    /// Read the objects a bundle holds into the cache.
    Import {
        /// The bundle to read.
        #[arg(value_name = "bundle")]
        bundle: PathBuf,
    },
    /// Rebuild the derived data the cache can regenerate from what it holds.
    Repair,
    /// Mark what nothing refers to, then sweep what has been marked longest.
    Prune,
    /// Rewrite packs against a dictionary trained over what each one holds.
    Compact,
    /// Remove every object.
    Clear,
}

/// What Fetchloom was asked to do.
#[derive(Subcommand, Clone, Debug)]
pub enum Command {
    /// Fetch something, check every byte of it, and unpack it.
    #[command(
        long_about = "Fetch something, check every byte of it, and unpack it.\n\n\
            Give it a web address, a folder on this machine, a manifest file, or a name \
            you have set up a source for. It works out which of those you meant, so you \
            do not have to tell it.\n\n\
            What you get back is a directory. If what it fetched was a zip or a tarball, \
            it unpacks it for you, after the checks pass and never before. A digest is \
            just a short fingerprint of the bytes: if one byte changed, the fingerprint \
            changes, and the run stops instead of handing you something quietly wrong.\n\n\
            It writes a lock file beside you recording exactly what it gave you. Commit \
            that file and a later run, on any machine, can tell you whether the source \
            still serves the same thing.\n\n\
            Running it twice is safe. The second run reuses what is already on disk and \
            tells you nothing changed. If you edited a file in the destination, it stops \
            and names the file rather than overwriting your work.",
        after_help = "Examples:\n  \
            fetchloom get https://example.org/data.tar.gz\n      \
            fetch it, check it, unpack it into ./data.tar.gz\n\n  \
            fetchloom get https://example.org/data.tar.gz -o corpus\n      \
            same, but put it in ./corpus\n\n  \
            fetchloom get ./dataset.yaml\n      \
            fetch everything a manifest names\n\n  \
            fetchloom get https://example.org/big.zip --select 'train/*'\n      \
            take only the members you want out of an archive\n\n\
            Not sure what a reference will do? Run `fetchloom plan <ref>` first. It moves \
            no bytes.\n\n\
            There are more flags for speed, politeness and durability. Every one of them \
            is measured for you and you should not need to touch any, so `-h` leaves them \
            out. `fetchloom get --help` lists all of them."
    )]
    Get {
        /// What to fetch.
        #[arg(value_name = "ref")]
        reference: String,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Write a manifest for data that does not have one.
    #[command(
        long_about = "Write a manifest for data that does not have one.\n\n\
            Most published data ships no checksums at all, so there is nothing to check a \
            download against. Point init at a folder or at a listing and it reads every \
            file, records what each one hashes to, and prints a manifest you can publish \
            or commit.\n\n\
            Every digest in that manifest is one it saw for itself. It never copies a \
            checksum out of a web header, because a header is a claim about a file and \
            not a fingerprint of it.\n\n\
            If the data already ships something usable, a checksums file, a Croissant \
            record, a Frictionless package, a pooch registry or a BagIt manifest, init \
            reads that instead of making you retype it.",
        after_help = "Examples:\n  \
            fetchloom init ./my-dataset\n      \
            print a manifest for a folder\n\n  \
            fetchloom init ./my-dataset -o dataset.yaml\n      \
            write it to a file instead\n\n  \
            fetchloom init https://example.org/files/\n      \
            walk a listing and record what it holds"
    )]
    Init {
        /// What to infer a manifest for.
        #[arg(value_name = "url|dir")]
        reference: String,
        /// Where the manifest is written, rather than to standard output.
        #[arg(long, short, value_name = "path")]
        output: Option<PathBuf>,
        /// Overwrite the file the manifest is written to.
        #[arg(long)]
        force: bool,
    },
    /// See what a run would do, without moving a single byte.
    #[command(
        long_about = "See what a run would do, without moving a single byte.\n\n\
            It resolves the reference, works out which files it would fetch, how much \
            disk each place would need, which hosts it would talk to, whether any of them \
            need a credential, and how far you could trust the result. Then it stops.\n\n\
            The plan is a file, not just something on your screen. Make it on a machine \
            with a network, carry it to one without, and run it there with apply. That is \
            what makes air-gapped work practical.\n\n\
            A plan never promises a size or a duration the source could not tell it. \
            Unknown stays unknown.",
        after_help = "Examples:\n  \
            fetchloom plan https://example.org/data.tar.gz\n      \
            see what would happen\n\n  \
            fetchloom plan ./dataset.yaml > run.plan\n      \
            save it to carry somewhere else\n\n  \
            fetchloom plan ./dataset.yaml --json\n      \
            the same answer as one JSON object"
    )]
    Plan {
        /// What to plan.
        #[arg(value_name = "ref")]
        reference: String,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Run a plan that was made earlier, or somewhere else.
    #[command(
        long_about = "Run a plan that was made earlier, or somewhere else.\n\n\
            Apply re-checks the digests the plan states rather than trusting them, so a \
            source that has started serving different bytes fails on integrity instead of \
            quietly giving you something else.\n\n\
            With the objects already in the cache, usually put there by `cache import`, \
            apply needs no network at all.",
        after_help = "Examples:\n  \
            fetchloom apply run.plan\n      \
            run it here\n\n  \
            fetchloom apply run.plan --offline\n      \
            run it with the network forbidden, and fail loudly if it was needed\n\n  \
            fetchloom apply run.plan -o elsewhere\n      \
            put the result somewhere other than the plan named"
    )]
    Apply {
        /// The plan to execute.
        #[arg(value_name = "plan")]
        plan: PathBuf,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Fix a cached object that went bad, without refetching all of it.
    #[command(
        long_about = "Fix a cached object that went bad, without refetching all of it.\n\n\
            Disks corrupt files. When one of the objects fetchloom is holding stops \
            matching its own fingerprint, repair works out which byte ranges are actually \
            damaged and asks the source for those ranges only. A bad megabyte inside a \
            fifty gigabyte file costs a megabyte to fix.\n\n\
            If the source cannot serve ranges, or the damage is spread too wide to be \
            worth it, repair fetches the whole object and says so rather than doing it \
            silently.",
        after_help = "Example:\n  \
            fetchloom repair https://example.org/big.tar\n\n\
            Find out whether anything needs repairing: `fetchloom cache verify`"
    )]
    Repair {
        /// What to repair.
        #[arg(value_name = "ref")]
        reference: String,
        /// The flags that control materialization.
        #[command(flatten)]
        transfer: Box<TransferFlags>,
    },
    /// Check a directory against the record of what was written there.
    #[command(
        long_about = "Check a directory against the record of what was written there.\n\n\
            Verify rehashes everything in the directory, folds it into one value for the \
            whole tree, and compares that against what the run which produced it recorded. \
            Same value, nothing has changed. Different value, it names both so you can see \
            what you are dealing with.\n\n\
            The value is the same on Windows and on Linux for the same content, so you can \
            compare across machines.",
        after_help = "Example:\n  \
            fetchloom verify ./corpus"
    )]
    Verify {
        /// The path to verify.
        #[arg(value_name = "path")]
        target: String,
    },
    /// Say which entries of a directory differ from the record of what was written there.
    #[command(
        long_about = "Say which entries of a directory differ from the record of what was \
            written there.\n\n\
            Every entry is one of four things: unchanged, modified, deleted, or added. \
            Status prints a line for each one that is not unchanged and nothing at all when \
            the directory is exactly what the run left.\n\n\
            It changes nothing, and it does not go near the network. Where the record still \
            describes the file on disk, size and timestamp answer the question and the bytes \
            are not read again.",
        after_help = "Example:\n  \
            fetchloom status ./corpus\n\n\
            To see what the difference actually is: `fetchloom diff ./corpus`"
    )]
    Status {
        /// The directory to compare against its record.
        #[arg(value_name = "path")]
        target: String,
        /// What an entry is checked against.
        #[arg(long, value_name = "always|fingerprint|never")]
        verify: Option<VerifyChoice>,
    },
    /// Show what each changed entry was and what it is now.
    #[command(
        long_about = "Show what each changed entry was and what it is now.\n\n\
            The same four states status reports, with the digest and the length the record \
            holds and the digest and the length the directory holds now.\n\n\
            It never compares the inside of a file. A parquet file and a JPEG have no lines, \
            so an entry is the smallest thing that can differ.",
        after_help = "Example:\n  \
            fetchloom diff ./corpus"
    )]
    Diff {
        /// The directory to compare against its record.
        #[arg(value_name = "path")]
        target: String,
        /// What an entry is checked against.
        #[arg(long, value_name = "always|fingerprint|never")]
        verify: Option<VerifyChoice>,
    },
    /// Put back what the record says was there.
    #[command(
        long_about = "Put back what the record says was there.\n\n\
            Name entries to restore those, or name none and every changed entry goes back. \
            An entry you edited is rewritten, one you deleted comes back, and one you added \
            is removed.\n\n\
            The bytes come out of the cache, so this needs no network at all. If the cache \
            no longer holds the object an entry came from, revert fails naming what is \
            missing and the command that would bring it back, rather than fetching it for \
            you.",
        after_help = "Examples:\n  \
            fetchloom revert ./corpus\n      \
            put the whole directory back\n\n  \
            fetchloom revert ./corpus train/labels.csv\n      \
            put one entry back and leave the rest of your edits alone"
    )]
    Revert {
        /// The directory to restore.
        #[arg(value_name = "path")]
        target: String,
        /// The entries to restore, or none for all of them.
        #[arg(value_name = "entry")]
        entries: Vec<String>,
        /// What an entry is checked against.
        #[arg(long, value_name = "always|fingerprint|never")]
        verify: Option<VerifyChoice>,
    },
    /// Make the directory as it stands a dataset of its own.
    #[command(
        long_about = "Make the directory as it stands a dataset of its own.\n\n\
            Promote reads every file, keeps the bytes in the cache, and writes a manifest \
            and a lock describing exactly what is there. Your edited copy becomes something \
            another person can fetch and get byte for byte.\n\n\
            The manifest records what it was derived from: the dataset, the manifest and the \
            tree the record names. A promoted dataset that forgets where it came from is \
            worth less than one that remembers.\n\n\
            Promote pins the bytes; it does not publish them. `cache export` is how the \
            objects travel to someone else.",
        after_help = "Examples:\n  \
            fetchloom promote ./corpus\n      \
            print the manifest\n\n  \
            fetchloom promote ./corpus -o corpus.yaml\n      \
            write it to a file"
    )]
    Promote {
        /// The directory to promote.
        #[arg(value_name = "path")]
        target: String,
        /// Where the manifest is written, rather than to standard output.
        #[arg(long, short, value_name = "path")]
        output: Option<PathBuf>,
        /// Overwrite the file the manifest is written to.
        #[arg(long)]
        force: bool,
        /// Where the lock is written.
        #[arg(long, value_name = "path", default_value = "fetchloom.lock")]
        lock: PathBuf,
    },
    /// Watch a run as it happens, or replay one that already did.
    #[command(
        long_about = "Watch a run as it happens, or replay one that already did.\n\n\
            Every run can write a stream of events, one JSON object per line, with \
            `--events <path>`. Watch renders that stream: live if you point it at a run in \
            progress, or after the fact if you point it at a file one left behind.\n\n\
            This is also how you keep a record of what a run did without scraping its \
            progress output.",
        after_help = "Examples:\n  \
            fetchloom get <ref> --events run.ndjson\n      \
            record what happened\n\n  \
            fetchloom watch run.ndjson\n      \
            replay it\n\n  \
            fetchloom get <ref> --events - | fetchloom watch -\n      \
            watch it live"
    )]
    Watch {
        /// The event stream to render, or `-` for standard input.
        #[arg(id = "watched", value_name = "events")]
        stream: String,
    },
    /// Print a script so your shell can complete fetchloom commands.
    #[command(
        long_about = "Print a script so your shell can complete fetchloom commands.\n\n\
            The script goes to standard output. Where you put it depends on your shell, \
            and your shell's own documentation is the authority on that.",
        after_help = "Examples:\n  \
            fetchloom completions bash > /etc/bash_completion.d/fetchloom\n  \
            fetchloom completions powershell | Out-String | Invoke-Expression"
    )]
    Completions {
        /// The shell to write a script for.
        shell: Shell,
    },
    /// Look at, and change, what is kept between runs.
    #[command(
        long_about = "Look at, and change, what is kept between runs.\n\n\
            The cache is where fetchloom keeps bytes it has already fetched and checked, \
            so the next run does not fetch them again. It is shared between every project \
            on this machine and it is addressed by content, so two datasets holding the \
            same file hold it once.\n\n\
            Nothing in it is precious. Everything in it can be fetched again, or rebuilt \
            from a lock file or a bundle. If you are short of disk, clearing it costs you \
            only the time to fetch again.",
        after_help = "Common ones:\n  \
            fetchloom cache status      how much is in there\n  \
            fetchloom cache verify      reread everything and quarantine anything bad\n  \
            fetchloom cache prune       remove what nothing refers to\n  \
            fetchloom cache export b.tar / cache import b.tar\n      \
            carry the bytes to a machine with no network"
    )]
    Cache {
        /// What to do with the cache.
        #[command(subcommand)]
        command: CacheCommand,
    },
    /// Check this machine, and change nothing.
    #[command(
        long_about = "Check this machine, and change nothing.\n\n\
            Doctor looks at your configuration, whether the places it needs to write are \
            writable, how much disk is free, whether the cache is healthy, whether the \
            certificates it needs are present, and whether the credentials you have set up \
            look right.\n\n\
            It makes no network request, prints no secret, and removes every temporary \
            file it creates. Run it first when something is not working.",
        after_help = "Example:\n  \
            fetchloom doctor\n  \
            fetchloom doctor --json"
    )]
    Doctor,
    /// Explain a reference: what it is, where it came from, how far to trust it.
    #[command(
        long_about = "Explain a reference: what it is, where it came from, how far to \
            trust it.\n\n\
            Why answers the questions you would otherwise have to guess at. What kind of \
            reference is this. Which source would be used and why that one rather than \
            another. Is there a lock entry for it. What trust class would the result carry, \
            and what would have to be true for it to carry a stronger one.\n\n\
            It reaches the network only as far as answering those questions needs.",
        after_help = "Example:\n  \
            fetchloom why https://example.org/data.tar.gz"
    )]
    Why {
        /// The reference to explain.
        #[arg(value_name = "ref")]
        reference: String,
    },
    /// Show every setting, and where each value came from.
    #[command(
        long_about = "Show every setting, and where each value came from.\n\n\
            A value can come from a flag you typed, an environment variable, a project \
            configuration file, your user configuration file, something fetchloom measured \
            about this machine, or a built-in default. Explain says which, for every \
            setting, so you never have to work out why a number is what it is.\n\n\
            Give it one setting name to see just that one.",
        after_help = "Examples:\n  \
            fetchloom explain\n  \
            fetchloom explain concurrency\n  \
            fetchloom explain --json"
    )]
    Explain {
        /// One setting to report, or every setting when absent.
        key: Option<String>,
    },
}

/// The whole command line.
#[derive(Parser, Debug)]
#[command(
    name = "fetchloom",
    version,
    about = "Turns a dataset reference into exact, verified local files.",
    long_about = "Turns a dataset reference into exact, verified local files.\n\n\
        Point it at a URL, a folder, a manifest, or a bare name. It works out what that \
        is, fetches what you do not already have, checks every byte against a digest, \
        unpacks it, and writes down exactly what it gave you so the next run can prove \
        it is the same thing.\n\n\
        You do not have to tune anything. Every number it needs, it measures.",
    after_help = "Try this first:\n  \
        fetchloom get https://ftp.gnu.org/gnu/hello/hello-2.12.tar.gz -o hello\n\n\
        Then:\n  \
        fetchloom plan <ref>       see what a run would do, without moving a byte\n  \
        fetchloom why <ref>        see how a reference resolved and how far to trust it\n  \
        fetchloom doctor           check this machine, changing nothing\n  \
        fetchloom explain          see every setting and where it came from\n\n\
        Full help for any command: fetchloom <command> --help",
    disable_help_subcommand = true
)]
pub struct CommandLine {
    /// The command that was asked for.
    #[command(subcommand)]
    pub command: Command,
    /// The flags that apply to every command.
    #[command(flatten)]
    pub global: GlobalFlags,
}
