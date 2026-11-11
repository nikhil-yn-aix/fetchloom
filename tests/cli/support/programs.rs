use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use super::Result;
use super::paths::{relocated, workspace};

pub(crate) fn executable() -> &'static Path {
    static PROGRAM: OnceLock<PathBuf> = OnceLock::new();
    PROGRAM.get_or_init(|| {
        relocated(
            std::env::var_os("CARGO_BIN_EXE_fetchloom"),
            env!("CARGO_BIN_EXE_fetchloom"),
        )
    })
}

pub(crate) fn recipe_step() -> Result<PathBuf> {
    static BUILT: OnceLock<std::result::Result<PathBuf, String>> = OnceLock::new();
    BUILT
        .get_or_init(|| {
            build_recipe(
                executable(),
                workspace(),
                std::env::var_os("CARGO").as_deref(),
                std::env::var_os("NEXTEST").is_some(),
            )
        })
        .clone()
        .map_err(Into::into)
}

fn build_recipe(
    executable: &Path,
    workspace: &Path,
    cargo: Option<&OsStr>,
    reuse: bool,
) -> std::result::Result<PathBuf, String> {
    let program = executable
        .with_file_name("examples")
        .join(format!("recipe_step{}", std::env::consts::EXE_SUFFIX));
    if !reuse && let Some(cargo) = cargo {
        let mut build = Command::new(cargo);
        build
            .args(["build", "--locked", "--quiet", "--example", "recipe_step"])
            .current_dir(workspace);
        if executable.parent().and_then(Path::file_name) == Some("release".as_ref()) {
            build.arg("--release");
        }
        let output = build.output().map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
    }
    if program.is_file() {
        Ok(program)
    } else {
        Err(format!(
            "{} is missing: run `cargo build --example recipe_step`",
            program.display()
        ))
    }
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

    #[test]
    fn a_prebuilt_recipe_step_never_starts_cargo() -> Result {
        let folder = tempfile::tempdir()?;
        let executable = folder.path().join("debug").join("fetchloom");
        let program = executable
            .with_file_name("examples")
            .join(format!("recipe_step{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(program.parent().ok_or("no example directory")?)?;
        std::fs::write(&program, b"already built")?;
        assert_eq!(
            build_recipe(
                &executable,
                folder.path(),
                Some(OsStr::new("cargo-must-not-run-here")),
                true,
            )?,
            program
        );
        Ok(())
    }

    #[test]
    fn cargo_test_refreshes_a_recipe_step_that_already_exists() -> Result {
        let folder = tempfile::tempdir()?;
        let executable = folder.path().join("debug").join("fetchloom");
        let program = executable
            .with_file_name("examples")
            .join(format!("recipe_step{}", std::env::consts::EXE_SUFFIX));
        std::fs::create_dir_all(program.parent().ok_or("no example directory")?)?;
        std::fs::write(&program, b"already built")?;
        assert!(
            build_recipe(
                &executable,
                folder.path(),
                Some(OsStr::new("cargo-that-would-refresh-the-example")),
                false
            )
            .is_err()
        );
        Ok(())
    }
}
