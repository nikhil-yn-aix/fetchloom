//! errors whose help names the fix that works for how the command was given.

use std::fs;

use crate::support::{Result, Sandbox, Server, text};

/// windows holds a file another program opened without sharing it, as Excel does, so reading it
/// fails, and replacing it fails while that program keeps it open even for reading.
#[cfg(windows)]
#[test]
fn a_file_another_program_holds_says_to_close_it() -> Result {
    use std::os::windows::fs::OpenOptionsExt;
    const SHARE_READ: u32 = 1;
    let sandbox = Sandbox::new()?;
    let manifest = sandbox.work().join("data.toml");
    fs::write(
        &manifest,
        "[datasets.iris]\nref = \"https://example.org/iris.csv\"\n",
    )?;
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&manifest)?;
    let output = sandbox.run(&["status"])?;
    drop(held);
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("(os error 32)"), "{stderr}");
    assert!(
        stderr.ends_with("help: close the program using the file, then run the command again\n"),
        "{stderr}"
    );
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(SHARE_READ)
        .open(&manifest)?;
    let output = sandbox.run(&["remove", "iris"])?;
    drop(held);
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("(os error 5)"), "{stderr}");
    assert!(
        stderr.ends_with("help: close any program that has it open and check its permissions, then run the command again\n"),
        "{stderr}"
    );
    Ok(())
}

#[test]
fn patterns_are_loosened_where_they_were_given() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/iris.csv", Vec::new())?;
    for command in ["add", "get"] {
        let output = sandbox.run(&[command, &server.url, "--exclude", "*"])?;
        assert_eq!(output.status.code(), Some(50), "{command}");
        let stderr = text(&output.stderr)?;
        assert!(
            stderr.ends_with("help: loosen the patterns given to --select and --exclude\n"),
            "{command}: {stderr}"
        );
    }
    let manifest = format!(
        "[datasets.iris]\nref = \"{}\"\nexclude = \"*\"\n",
        server.url
    );
    fs::write(sandbox.work().join("data.toml"), manifest)?;
    let output = sandbox.run(&["sync"])?;
    assert_eq!(output.status.code(), Some(50));
    let stderr = text(&output.stderr)?;
    assert!(stderr.contains("data.toml:3   exclude = \"*\""), "{stderr}");
    assert!(
        stderr.ends_with("help: loosen the patterns in data.toml\n"),
        "{stderr}"
    );
    Ok(())
}
