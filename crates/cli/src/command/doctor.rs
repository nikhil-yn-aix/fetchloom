//! The `doctor` command: checking the environment and changing nothing.

use fetchloom_cli::settings::ProcessEnvironment;
use fetchloom_cli::{config, policy, run, settings};
use fetchloom_engine::outcome::ExitCode;
use std::io::Write;

pub(crate) fn run_doctor(
    resolved: &settings::Settings,
    discovered: &config::Discovered,
    json: bool,
) -> ExitCode {
    let root = match run::resolve_path(&resolved.cache_dir.value) {
        Ok(root) => root,
        Err(error) => {
            eprintln!("{}", error.next_action());
            return ExitCode::Usage;
        }
    };
    let environment = ProcessEnvironment;
    let report = fetchloom_cli::doctor::run(
        discovered,
        &root,
        &environment,
        &policy::NativeCredentialStore,
    );
    let mut stdout = std::io::stdout().lock();
    if json {
        match serde_json::to_string(&report) {
            Ok(written) => {
                let _ = writeln!(stdout, "{written}");
            }
            Err(reason) => {
                eprintln!("the report could not be written: {reason}");
                return ExitCode::Usage;
            }
        }
    } else {
        let _ = write!(stdout, "{}", report.render());
    }
    let _ = stdout.flush();
    report.exit_code()
}
