//! The `fl` command line of fetchloom: arguments, configuration, the order of work in each command, and rendering.

mod args;
mod clock;
mod commands;
mod config;
mod engine;
mod failure;
mod observe;
mod project;
mod report;

use std::io::{self, Write};
use std::process::ExitCode;

use clap::Parser;
use fl_model::document::Work;
use fl_model::error::ErrorKind;
use fl_model::event::Event;

use crate::args::{Cli, Command};
use crate::commands::Context;
use crate::config::Config;
use crate::engine::Engine;
use crate::failure::Failure;
use crate::report::{Human, Json, Outcome, Output};

const WELCOME: &str = concat!(
    "fl ",
    env!("CARGO_PKG_VERSION"),
    ", uv for research data\n",
    "\n",
    "  fl add zenodo:3608990     add a dataset, lock it and download it into data/\n",
    "  fl sync                   make data/ match data.lock\n",
    "  fl status                 see what differs from data.lock\n",
    "  fl path NAME              print where a dataset is, for scripts\n",
    "  fl get URL                fetch something once, without a project\n",
    "\n",
    "fetchloom by KairosLab\n"
);

/// Runs `fl` with the arguments of this process and returns its exit code.
///
/// `--help` and `--version` print their text and give 0. A usage error prints the usage to stderr
/// and gives 2. With no arguments the welcome is written to stdout. A command gives the exit code
/// of its error's kind, or 0.
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
    let Some(command) = cli.command else {
        return welcome(&mut io::stdout().lock()).map_or(ExitCode::FAILURE, |()| ExitCode::SUCCESS);
    };
    let global = cli.global;
    let output = if global.json {
        Output::Json(Json::default())
    } else {
        Output::Human(Human::new(global.verbose, global.quiet))
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
    let (result, work) = execute(&global, &command, &output);
    let code = match result {
        Ok(outcome) => match output.finish(name, outcome, &work) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        },
        Err(failure) => finish_failed(&output, name, failure, &work),
    };
    drop(guard);
    code
}

fn execute(
    global: &args::Global,
    command: &Command,
    output: &Output,
) -> (Result<Outcome, Failure>, Work) {
    let span = tracing::info_span!("command", name = command_name(command));
    let _entered = span.enter();
    let prepared = std::env::current_dir()
        .map_err(|error| {
            Failure::new(
                ErrorKind::LocalIo,
                format!("the working directory cannot be read: {error}"),
            )
        })
        .and_then(|cwd| {
            Config::new(
                global,
                &cwd,
                |name| std::env::var(name).ok(),
                &config::read_optional,
            )
        });
    let config = match prepared {
        Ok(config) => config,
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
    let engine = match runtime.block_on(async { Engine::new(&config) }) {
        Ok(engine) => engine,
        Err(failure) => return (Err(failure), Work::default()),
    };
    let context = Context {
        engine: &engine,
        reporter: output.reporter(),
        cwd: &config.cwd,
    };
    let result = runtime.block_on(async {
        match command {
            Command::Init { name } => commands::init::run(&context, name.as_deref()),
            Command::Add(add) => commands::add::run(&context, add).await,
            Command::Lock => commands::lock(&context).await,
            Command::Sync => commands::sync(&context).await,
            Command::Get { reference, output } => {
                commands::get::run(&context, reference, output.as_deref()).await
            }
            Command::Path { name, no_sync } => commands::path::run(&context, name, *no_sync).await,
            Command::Status => commands::status::run(&context),
        }
    });
    (result, engine.work())
}

fn finish_failed(output: &Output, name: &str, failure: Failure, work: &Work) -> ExitCode {
    let code = failure.kind().exit_code();
    let _ignored = output.fail(name, failure.0, work);
    ExitCode::from(code)
}

const fn command_name(command: &Command) -> &'static str {
    match command {
        Command::Init { .. } => "init",
        Command::Add(_) => "add",
        Command::Lock => "lock",
        Command::Sync => "sync",
        Command::Get { .. } => "get",
        Command::Path { .. } => "path",
        Command::Status => "status",
    }
}

fn exit_code(code: i32) -> ExitCode {
    u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from)
}

fn welcome(out: &mut impl Write) -> io::Result<()> {
    out.write_all(WELCOME.as_bytes())?;
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
        welcome(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with(&format!(
            "fl {}, uv for research data\n",
            env!("CARGO_PKG_VERSION")
        )));
        assert!(text.contains("fl add zenodo:3608990"));
        assert!(text.ends_with("\nfetchloom by KairosLab\n"));
    }

    #[test]
    fn welcome_returns_the_write_error() {
        let err = welcome(&mut Failing { on_write: true }).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn welcome_returns_the_flush_error() {
        let err = welcome(&mut Failing { on_write: false }).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn exit_code_keeps_codes_that_fit_and_fails_the_rest() {
        assert_eq!(exit_code(0), ExitCode::SUCCESS);
        assert_eq!(exit_code(2), ExitCode::from(2));
        assert_eq!(exit_code(-1), ExitCode::FAILURE);
        assert_eq!(exit_code(256), ExitCode::FAILURE);
    }
}
