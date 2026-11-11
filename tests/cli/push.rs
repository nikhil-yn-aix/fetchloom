//! `fetchloom push`: pinned files copied from this machine's cache into a folder mirror, laid out
//! as mirrors are read.

use std::fs;

use serde_json::Value;

use crate::caps::shown;
use crate::support::{Result, Sandbox, json};

/// the `push_to` setting names the folder push writes to: a relative one is taken from the
/// settings file's folder, as mirrors are, and `fetchloom settings` shows it with where it came
/// from.
#[test]
fn push_to_is_shown_with_where_it_came_from() -> Result {
    let sandbox = Sandbox::new()?;
    let settings = sandbox.work().join("settings.toml");
    fs::write(&settings, "push_to = \"lab, shared\"\n")?;
    let document = json(
        &sandbox
            .command(&["--json", "settings"])
            .env("FETCHLOOM_SETTINGS", &settings)
            .output()?,
    )?;
    let push_to = shown(&document, "push_to");
    assert_eq!(
        push_to["value"],
        sandbox.work().join("lab, shared").display().to_string()
    );
    assert_eq!(push_to["origin"], "file");
    assert_eq!(push_to["variable"], "FETCHLOOM_PUSH_TO");
    let document = json(&sandbox.run(&["--json", "settings"])?)?;
    assert_eq!(shown(&document, "push_to")["value"], Value::Null);
    assert_eq!(shown(&document, "push_to")["origin"], "default");
    Ok(())
}
