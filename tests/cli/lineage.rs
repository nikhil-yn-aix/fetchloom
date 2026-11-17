//! lineage: `add --from` records the datasets one was made from, with each one's files as they
//! were, and status, `lock --check` and sync say when those files changed since. it is a record:
//! nothing runs a pipeline or checks that the files came from those inputs.

use std::fs;

use crate::support::{Result, Sandbox, json, text};

/// writes data.toml, two datasets whose `from` lists name each other.
fn circular(sandbox: &Sandbox) -> Result {
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.raw]\nref = \"https://example.org/raw.csv\"\nfrom = [\"clean\"]\n\n[datasets.clean]\nref = \"https://example.org/clean.csv\"\nfrom = [\"raw\"]\n",
    )?;
    Ok(())
}

#[test]
fn a_from_that_goes_round_in_a_circle_stops_every_command() -> Result {
    let sandbox = Sandbox::new()?;
    circular(&sandbox)?;
    let output = sandbox.run(&["--json", "status"])?;
    assert_eq!(output.status.code(), Some(50), "{}", text(&output.stdout)?);
    let error = &json(&output)?["errors"][0];
    assert_eq!(error["kind"], "project.invalid");
    assert_eq!(
        error["message"],
        "data.toml says clean was made from raw and raw from clean"
    );
    assert_eq!(
        error["details"],
        serde_json::json!([
            "data.toml:7   from = [\"raw\"]",
            "data.toml:3   from = [\"clean\"]"
        ])
    );
    Ok(())
}
