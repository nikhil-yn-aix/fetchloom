//! the program the tests run: the fetchloom binary.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::paths::relocated;

/// the fetchloom binary cargo built for these tests.
pub(crate) fn executable() -> &'static Path {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        relocated(
            std::env::var_os("CARGO_BIN_EXE_fetchloom"),
            env!("CARGO_BIN_EXE_fetchloom"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_paths_replace_the_build_machines_paths() {
        let runtime = PathBuf::from("relocated").join("program");
        assert_eq!(
            relocated(Some(runtime.clone().into_os_string()), "build/program"),
            runtime
        );
    }

    #[test]
    fn cargo_test_keeps_the_compiled_path_when_no_runtime_path_is_set() {
        assert_eq!(relocated(None, "build/program"), Path::new("build/program"));
    }
}
