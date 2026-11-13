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

/// scripts that read data/train.csv keep working, and two datasets with `path = "."` share data/.
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

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    let data = sandbox.work().join("data");
    assert_eq!(fs::read(data.join("train.csv"))?, TRAIN);
    assert_eq!(fs::read(data.join("test.csv"))?, TEST);
    assert_eq!(
        stderr.matches("Placed").count(),
        1,
        "one line for the folder both share: {stderr}"
    );
    assert!(stderr.contains("Placed data · 2 files"), "{stderr}");
    let path = sandbox.run(&["path", "train/train.csv"])?;
    assert_eq!(
        text(&path.stdout)?.trim_end(),
        data.join("train.csv").display().to_string()
    );
    Ok(())
}

/// datasets share a folder when no two files clash, and each placed file belongs to one of them.
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

/// an S3 listing of one object at `key`, with the MD5 and size of `body` as S3 states them.
#[cfg(debug_assertions)]
fn listing(key: &str, body: &[u8]) -> Vec<crate::support::Part> {
    use std::fmt::Write as _;

    use md5::Digest as _;
    let md5 = md5::Md5::digest(body)
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        });
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Name>lab</Name><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>{}</Key><LastModified>2020-08-21T20:24:12.000Z</LastModified><ETag>&quot;{md5}&quot;</ETag><Size>{}</Size></Contents></ListBucketResult>",
        key.replace('/', "%2F"),
        body.len()
    );
    crate::support::reply(
        "200 OK",
        &[("Content-Type", "application/xml")],
        xml.as_bytes(),
    )
}

/// a listing names its files before any download, so the clash is found before either is fetched.
#[cfg(debug_assertions)]
#[test]
fn a_clash_the_listings_show_stops_sync_before_any_download() -> Result {
    let sandbox = Sandbox::new()?;
    let base = "/lab.s3.amazonaws.com/";
    let listed = |prefix: &str| format!("{base}?list-type=2&prefix={prefix}%2F&encoding-type=url");
    let routes = crate::support::Routes::start(vec![
        (listed("train"), listing("train/labels.csv", TRAIN)),
        (listed("test"), listing("test/labels.csv", TEST)),
        (format!("{base}train/labels.csv"), file(TRAIN, "\"v1\"")),
        (format!("{base}test/labels.csv"), file(TEST, "\"v1\"")),
    ])?;
    fs::write(
        sandbox.work().join("data.toml"),
        "[datasets.train]\nref = \"s3://lab/train/\"\npath = \"splits\"\n\n[datasets.test]\nref = \"s3://lab/test/\"\npath = \"splits\"\n",
    )?;

    let output = sandbox
        .command(&["sync"])
        .env("FETCHLOOM_TEST_BUCKETS", &routes.url)
        .output()?;

    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(50), "{stderr}");
    assert!(
        stderr.ends_with("\nerror: datasets test and train would both place data/splits/labels.csv\n  help: give datasets.test or datasets.train another path in data.toml\n"),
        "{stderr}"
    );
    assert!(!stderr.contains("Downloaded"), "{stderr}");
    let mut asked = routes.targets();
    asked.sort();
    assert_eq!(
        asked,
        [listed("test"), listed("train")],
        "a file was fetched"
    );
    assert!(!sandbox.work().join("data.lock").exists());
    assert!(!sandbox.work().join("data").exists());
    Ok(())
}

/// `path` names a folder, so `path = "train.csv"` puts the file at data/train.csv/train.csv; that
/// works, and the help says how to put it at data/train.csv.
#[test]
fn a_filename_shaped_path_warns() -> Result {
    let sandbox = Sandbox::new()?;
    let servers = serve(&sandbox, &[("/train.csv", TRAIN)])?;
    fs::write(
        sandbox.work().join("data.toml"),
        format!(
            "[datasets.train]\nref = \"{}\"\npath = \"train.csv\"\n",
            servers[0].url
        ),
    )?;

    let synced = sandbox.run(&["sync"])?;

    let stderr = text(&synced.stderr)?;
    assert_eq!(synced.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("warning: path = \"train.csv\" makes a folder named train.csv, so the file is data/train.csv/train.csv\n  help: path names a folder; to put the file at data/train.csv, set path = \".\" on datasets.train in data.toml\n"),
        "{stderr}"
    );
    let placed = sandbox.work().join("data").join("train.csv");
    assert_eq!(fs::read(placed.join("train.csv"))?, TRAIN);
    Ok(())
}
