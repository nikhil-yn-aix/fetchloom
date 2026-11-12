//! stamps the commit fetchloom is built from into `FETCHLOOM_COMMIT`, which `--version -v` and
//! its json name for a bug report: the one `FETCHLOOM_COMMIT` already names, as a release build
//! sets it, else the one cargo recorded in a crate it packaged from git, else the one checked out
//! where fetchloom's own repository is. a build outside git gets none, and says so. only the
//! commit's hex goes in, never a path or a name from the machine that built it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=FETCHLOOM_COMMIT");
    let root = std::env::var_os("CARGO_MANIFEST_DIR").map(PathBuf::from);
    let commit = std::env::var("FETCHLOOM_COMMIT")
        .ok()
        .filter(|commit| is_commit(commit))
        .or_else(|| root.as_deref().and_then(packaged))
        .or_else(|| root.as_deref().and_then(checked_out))
        .unwrap_or_default();
    // an empty stamp hides any other value the variable had, which fetchloom would print
    println!(
        "cargo:rustc-env=FETCHLOOM_COMMIT={}",
        commit.to_ascii_lowercase()
    );
}

/// whether `text` is a whole git commit id: 40 hex digits, or 64 in a repository on SHA-256.
fn is_commit(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// the commit `.cargo_vcs_info.json` names. cargo writes it into a crate it packages from a git
/// checkout, so a build from crates.io or from such a crate knows its commit too.
fn packaged(root: &Path) -> Option<String> {
    let path = root.join(".cargo_vcs_info.json");
    let record = std::fs::read_to_string(&path).ok()?;
    println!("cargo:rerun-if-changed={}", path.display());
    let (_, after) = record.split_once("\"sha1\"")?;
    let commit = after.split('"').nth(1)?;
    is_commit(commit).then(|| commit.to_owned())
}

/// the commit checked out at `root`, when `root` is the top of a git checkout, as fetchloom's
/// repository is: a crate unpacked inside another repository, such as a home folder kept in git,
/// is not that repository's. cargo runs this again once the checkout moves to another commit.
fn checked_out(root: &Path) -> Option<String> {
    let git = |args: &[&str]| -> Option<Vec<String>> {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()?;
        let text = String::from_utf8(output.stdout).ok()?;
        output
            .status
            .success()
            .then(|| text.lines().map(str::to_owned).collect())
    };
    let [top, commit] =
        <[String; 2]>::try_from(git(&["rev-parse", "--show-toplevel", "HEAD"])?).ok()?;
    if Path::new(&top).canonicalize().ok()? != root.canonicalize().ok()? {
        return None;
    }
    let branch = git(&["rev-parse", "--symbolic-full-name", "HEAD"])
        .and_then(|lines| lines.into_iter().next())
        .filter(|name| name.starts_with("refs/"));
    // this checkout's own files, which another worktree of the repository leaves alone
    let mut args = vec!["rev-parse", "--git-path", "HEAD", "--git-path", "logs/HEAD"];
    if let Some(branch) = &branch {
        args.extend(["--git-path", branch.as_str()]);
    }
    // a file that is not there would make cargo run this on every build
    for path in git(&args).unwrap_or_default() {
        let path = root.join(path);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    is_commit(&commit).then_some(commit)
}
