//! where datasets land in the data folder: straight in it with `path = "."`, or several in one
//! folder, each file with one dataset that owns it.

use std::fs;

use crate::support::{Result, Sandbox, Server, file, json, text};

const TRAIN: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const TEST: &[u8] = b"sepal_length,species\n6.3,virginica\n";

/// a server for each `(path, body)`, every one reachable from data.toml.
fn serve(sandbox: &Sandbox, files: &[(&str, &[u8])]) -> Result<Vec<Server>> {
    let mut servers = Vec::new();
    for (path, body) in files {
        let server = Server::start(path, vec![file(body, "\"v1\"")])?;
        sandbox.allow(&server.url);
        servers.push(server);
    }
    Ok(servers)
}

/// scripts that read data/train.csv keep working: a dataset with `path = "."` puts its files
/// straight in the data folder, beside another that does the same.
#[test]
fn flat_layout_keeps_data_train_csv() -> Result {
    let sandbox = Sandbox::new()?;
    let servers = serve(&sandbox, &[("/train.csv", TRAIN), ("/test.csv", TEST)])?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.train]\nref = \"{}\"\npath = \".\"\n\n[datasets.test]\nref = \"{}\"\npath = \".\"\n",
            servers[0].url, servers[1].url
        ),
    )?;

    let synced = sandbox.run(&["sync"])?;

    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let data = sandbox.work().join("data");
    assert_eq!(fs::read(data.join("train.csv"))?, TRAIN);
    assert_eq!(fs::read(data.join("test.csv"))?, TEST);
    let path = sandbox.run(&["path", "train/train.csv"])?;
    assert_eq!(
        text(&path.stdout)?.trim_end(),
        data.join("train.csv").display().to_string()
    );
    Ok(())
}

/// datasets share a folder when no two files clash. each placed file belongs to one dataset, so
/// removing one leaves the other's files and a person's own file in that folder.
#[test]
fn shared_folder_removal_keeps_the_other_dataset_and_extras() -> Result {
    let sandbox = Sandbox::new()?;
    let servers = serve(&sandbox, &[("/train.csv", TRAIN), ("/test.csv", TEST)])?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.train]\nref = \"{}\"\npath = \"splits\"\n\n[datasets.test]\nref = \"{}\"\npath = \"splits\"\n",
            servers[0].url, servers[1].url
        ),
    )?;
    let synced = sandbox.run(&["sync"])?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let splits = sandbox.work().join("data").join("splits");
    fs::write(splits.join("notes.txt"), b"my notes\n")?;

    let removed = sandbox.run(&["remove", "train"])?;

    assert_eq!(removed.status.code(), Some(0), "{}", text(&removed.stderr)?);
    assert!(!splits.join("train.csv").exists());
    assert_eq!(fs::read(splits.join("test.csv"))?, TEST);
    assert_eq!(fs::read(splits.join("notes.txt"))?, b"my notes\n");
    let status = json(&sandbox.run(&["--json", "status"])?)?;
    assert_eq!(
        status["result"]["missing"],
        serde_json::json!([]),
        "{status}"
    );
    assert_eq!(
        status["result"]["extra"],
        serde_json::json!(["data/splits/notes.txt"]),
        "{status}"
    );
    Ok(())
}
