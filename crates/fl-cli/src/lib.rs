//! The `fl` command line of fetchloom: arguments, configuration, the order of work in each command, and rendering.

mod args;
mod ask;
mod clock;
mod commands;
mod config;
mod engine;
mod failure;
mod observe;
mod progress;
mod project;
mod report;
mod term;

use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use fl_model::document::Work;
use fl_model::error::ErrorKind;
use fl_model::event::Event;

use crate::args::{Cli, Command};
use crate::ask::{Ask, Prompt, Refuse};
use crate::commands::Context;
use crate::config::Config;
use crate::engine::Engine;
use crate::failure::Failure;
use crate::progress::Bars;
use crate::report::{Human, Json, Outcome, Output};
use crate::term::{Style, Terminal, Tone};

const WELCOME: &str = concat!(
    "fl ",
    env!("CARGO_PKG_VERSION"),
    ", uv for research data
",
    "
",
    "Get a first dataset into this folder with:
",
    "
",
    "  fl add zenodo:3608990
",
    "
",
    "The commands that matter:
",
    "  fl add REF      add a dataset, lock it and download it into data/
",
    "  fl sync         make data/ match data.lock
",
    "  fl status       see what differs from data.lock
",
    "  fl path NAME    print where a dataset is, for scripts
",
    "  fl get REF      fetch something once, without a project
",
    "
",
    "REF is a DOI, a link, zenodo:ID, hf:datasets/ORG/NAME, a folder, or search words.
",
    "`fl --help` lists every command.
",
    "
",
);

const MAKER: &str = "fetchloom by KairosLab";

/// Runs `fl` with the arguments of this process and returns its exit code.
///
/// `--help` and `--version` print their text and give 0. A usage error prints the usage to stderr
/// and gives 2. With no arguments, inside a project it runs `fl status`, and elsewhere the welcome
/// is written to stdout. A command gives the exit code of its error's kind, the code of the most
/// severe failure when it did part of its work, or 0.
#[must_use]
pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(parse) => {
            return parse
                .print()
                .map_or(ExitCode::FAILURE, |()| exit_code(parse.exit_code()));
        }
    };
    let global = cli.global;
    let alone = cli.command.is_none();
    let Some(command) = cli
        .command
        .or_else(|| in_project(&global).then(Command::status))
    else {
        let style = Terminal::detect(global.color).style;
        let mut out = anstream::AutoStream::auto(io::stdout().lock());
        return welcome(&mut out, style).map_or(ExitCode::FAILURE, |()| ExitCode::SUCCESS);
    };
    let terminal = Terminal::detect(global.color);
    let output = if global.json {
        Output::Json(Arc::new(Json::default()))
    } else {
        let style = terminal.style;
        let bars = draws_progress(terminal, &global)
            .then(|| Bars::stderr(style, usize::from(console::Term::stderr().size().0)));
        Output::Human(Arc::new(Human::new(
            global.verbose,
            global.quiet,
            style,
            bars,
        )))
    };
    let name = command_name(&command);
    observe::panic_hook(global.log_file.clone());
    let observed = observe::install(&observe::Options {
        verbose: global.verbose,
        log_file: global.log_file.clone(),
        trace_file: global.trace_file.clone(),
        filter: std::env::var("FETCHLOOM_LOG").ok(),
    });
    let guard = match observed {
        Ok(guard) => guard,
        Err(error) => {
            let failure = Failure::new(error.kind(), error.to_string());
            return finish_failed(&output, name, failure, &Work::default());
        }
    };
    let ask: Box<dyn Ask> = if asks(terminal, &global) {
        Box::new(Prompt::new(output.shared()))
    } else {
        Box::new(Refuse)
    };
    let (result, work) = execute(&global, &command, &output, ask.as_ref());
    if alone && result.as_ref().is_ok_and(|outcome| outcome.exit.is_none()) {
        output.reporter().emit(Event::Hint(
            "`fl add REF` adds a dataset, `fl --help` lists every command".to_owned(),
        ));
    }
    let code = match result {
        Ok(outcome) => {
            let code = outcome.exit.map_or(0, ErrorKind::exit_code);
            match output.finish(name, outcome, &work) {
                Ok(()) => ExitCode::from(code),
                Err(_) => ExitCode::FAILURE,
            }
        }
        Err(failure) => finish_failed(&output, name, failure, &work),
    };
    drop(guard);
    code
}

/// Whether progress is drawn: on a terminal, unless turned off or `-vv` debug lines would tear it.
fn draws_progress(terminal: Terminal, global: &args::Global) -> bool {
    terminal.stderr && !global.no_progress && global.verbose < 2
}

/// Whether questions are asked: only of someone at a terminal, and never with `--json`.
fn asks(terminal: Terminal, global: &args::Global) -> bool {
    terminal.stderr && terminal.stdin && !global.json
}

/// Whether `fl` runs inside a project, so that `fl` alone shows its status.
fn in_project(global: &args::Global) -> bool {
    std::env::current_dir()
        .map(|cwd| match &global.directory {
            Some(dir) => cwd.join(dir),
            None => cwd,
        })
        .is_ok_and(|dir| project::Project::find(&dir).is_ok())
}

fn settings(global: &args::Global) -> Result<Config, Failure> {
    let cwd = std::env::current_dir().map_err(|error| {
        Failure::new(
            ErrorKind::LocalIo,
            format!("the working directory cannot be read: {error}"),
        )
    })?;
    Config::new(
        global,
        &cwd,
        |name| std::env::var(name).ok(),
        &config::read_optional,
    )
}

fn execute(
    global: &args::Global,
    command: &Command,
    output: &Output,
    ask: &dyn Ask,
) -> (Result<Outcome, Failure>, Work) {
    let span = tracing::info_span!("command", name = command_name(command));
    let _entered = span.enter();
    let prepared = settings(global).and_then(|config| match command {
        Command::Init {
            interactive: true, ..
        } => {
            commands::init::refuse_existing(&config.cwd)?;
            let answers = commands::init::interview(ask, &config.cwd, &config.store.value)?;
            match (&answers.store, config.settings.as_ref()) {
                (Some(store), Some(dir)) => {
                    commands::init::save_store(&dir.join("config.toml"), store)?;
                    Ok((settings(global)?, answers))
                }
                _ => Ok((config, answers)),
            }
        }
        _ => Ok((config, commands::init::Answers::default())),
    });
    let (config, answers) = match prepared {
        Ok(prepared) => prepared,
        Err(failure) => return (Err(failure), Work::default()),
    };
    for (key, value, from) in config.shown() {
        output.reporter().emit(Event::Detail {
            label: "Setting".to_owned(),
            text: format!("{key} = {value} from {from}"),
        });
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .thread_name("fl-worker")
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            return (
                Err(Failure::new(ErrorKind::LocalIo, error.to_string())),
                Work::default(),
            );
        }
    };
    let base = project::Project::find(&config.cwd).map_or_else(
        |_| config.cwd.clone(),
        |project| project.root().to_path_buf(),
    );
    let engine = match runtime.block_on(async { Engine::new(&config, &base, output.shared()) }) {
        Ok(engine) => engine,
        Err(failure) => return (Err(failure), Work::default()),
    };
    let context = Context {
        engine: &engine,
        reporter: output.reporter(),
        cwd: &config.cwd,
        ask,
        yes: global.yes,
        offline: config.offline.value,
        link: config.link.value,
    };
    let result = runtime.block_on(async {
        match command {
            Command::Init { name, .. } => {
                let name = answers.name.as_deref().or(name.as_deref());
                commands::init::run(&context, name, answers.first.as_deref()).await
            }
            Command::Add(add) => commands::add::run(&context, add).await,
            Command::Remove { names } => commands::remove::run(&context, names).await,
            Command::Lock { upgrade, check } => {
                commands::lock::run(&context, upgrade.as_deref(), *check).await
            }
            Command::Sync(sync) => commands::sync::run(&context, sync).await,
            Command::Status { names, check } => commands::status::run(&context, names, *check),
            Command::Restore { paths } => commands::restore::run(&context, paths),
            Command::Path { name, no_sync } => commands::path::run(&context, name, *no_sync).await,
            Command::Get(get) => commands::get::run(&context, get).await,
        }
    });
    for (host, done) in engine.slowest(3) {
        output.reporter().emit(Event::Detail {
            label: "Host".to_owned(),
            text: format!(
                "{host}: {}, {} in {}",
                fl_model::units::count(done.files, "file", "files"),
                fl_model::units::size(done.bytes),
                fl_model::units::duration(done.spent)
            ),
        });
    }
    (result, engine.work())
}

fn finish_failed(output: &Output, name: &str, failure: Failure, work: &Work) -> ExitCode {
    let code = failure.kind().exit_code();
    let _ignored = output.fail(name, failure, work);
    ExitCode::from(code)
}

const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init { .. } => "init",
        Command::Add(_) => "add",
        Command::Remove { .. } => "remove",
        Command::Lock { .. } => "lock",
        Command::Sync(_) => "sync",
        Command::Status { .. } => "status",
        Command::Restore { .. } => "restore",
        Command::Path { .. } => "path",
        Command::Get(_) => "get",
    }
}

fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

fn welcome(out: &mut impl Write, style: Style) -> io::Result<()> {
    out.write_all(WELCOME.as_bytes())?;
    writeln!(out, "{}", style.paint(Tone::Dim, MAKER))?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Failing {
        on_write: bool,
    }

    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.on_write {
                Err(io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn welcome_writes_version_commands_and_maker() {
        let mut out = Vec::new();
        welcome(&mut out, Style::PLAIN).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with(&format!(
            "fl {}, uv for research data\n",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(text.contains(
            "
  fl add zenodo:3608990
"
        ));
        assert!(text.ends_with("\nfetchloom by KairosLab\n"));
    }

    #[test]
    fn welcome_returns_the_write_error() {
        let err = welcome(&mut Failing { on_write: true }, Style::PLAIN).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn welcome_returns_the_flush_error() {
        let err = welcome(&mut Failing { on_write: false }, Style::PLAIN).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn progress_is_drawn_on_a_terminal_unless_turned_off_or_at_debug() {
        let at = |stderr| Terminal {
            stderr,
            stdin: false,
            style: Style::PLAIN,
        };
        let global = |verbose, no_progress| args::Global {
            verbose,
            no_progress,
            ..args::Global::default()
        };
        assert!(draws_progress(at(true), &global(0, false)));
        assert!(draws_progress(at(true), &global(1, false)));
        assert!(!draws_progress(at(true), &global(2, false)));
        assert!(!draws_progress(at(true), &global(3, false)));
        assert!(!draws_progress(at(true), &global(0, true)));
        assert!(!draws_progress(at(false), &global(0, false)));
    }

    #[test]
    fn questions_are_asked_only_at_a_terminal_without_json() {
        let at = |stderr, stdin| Terminal {
            stderr,
            stdin,
            style: Style::PLAIN,
        };
        let json = args::Global {
            json: true,
            ..args::Global::default()
        };
        let plain = args::Global::default();
        assert!(asks(at(true, true), &plain));
        assert!(!asks(at(true, true), &json));
        assert!(!asks(at(true, false), &plain));
        assert!(!asks(at(false, true), &plain));
    }

    #[test]
    fn exit_code_keeps_codes_that_fit_and_fails_the_rest() {
        assert_eq!(exit_code(0), ExitCode::SUCCESS);
        assert_eq!(exit_code(2), ExitCode::from(2));
        assert_eq!(exit_code(-1), ExitCode::FAILURE);
        assert_eq!(exit_code(256), ExitCode::FAILURE);
    }
}
