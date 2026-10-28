//! errors whose help names the fix that works for how the command was given.

use std::fs;

use crate::support::{Result, Sandbox, Server, text};

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
