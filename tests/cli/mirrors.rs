//! mirrors: folders and web servers that hold pinned files by their SHA-256, asked before the
//! sources data.lock names.

use crate::caps::shown;
use crate::support::{Result, Sandbox, json};

#[test]
fn the_settings_list_each_mirror_and_refuse_what_is_no_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let settings = sandbox.work().join("settings.toml");
    std::fs::write(
        &settings,
        "mirrors = [\"lab, shared\", \"https://data.lab.example/fetchloom\"]\nmirrors_only = true\n",
    )?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_SETTINGS", &settings)
            .output()?,
    )?;
    let mirrors = shown(&document, "mirrors");
    // a relative folder is taken from the settings file's folder, comma and all
    let folder = sandbox.work().join("lab, shared").display().to_string();
    assert_eq!(
        mirrors["value"],
        serde_json::json!([folder, "https://data.lab.example/fetchloom"])
    );
    assert_eq!(mirrors["origin"], "file");
    assert_eq!(mirrors["variable"], "FETCHLOOM_MIRRORS");
    assert_eq!(shown(&document, "mirrors_only")["value"], true);
    let output = sandbox
        .command(&["--json", "settings"])
        .env("FETCHLOOM_MIRRORS", "ftp://lab.example/fetchloom")
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let document = json(&output)?;
    assert_eq!(document["errors"][0]["kind"], "usage");
    assert_eq!(
        document["errors"][0]["message"],
        "FETCHLOOM_MIRRORS holds \"ftp://lab.example/fetchloom\", which is no mirror: a mirror is a folder or an http or https address, not ftp"
    );
    Ok(())
}
