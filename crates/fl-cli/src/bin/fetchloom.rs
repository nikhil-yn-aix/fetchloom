//! The `fetchloom` executable, the same program as `fl` under a name no shell aliases.

use std::process::ExitCode;

fn main() -> ExitCode {
    fl_cli::run()
}
