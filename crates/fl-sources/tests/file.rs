//! Local files and directories.

use std::fs;

use fl_model::error::ErrorKind;
use fl_model::reference::Reference;
use fl_sources::{Files, Source};

use crate::support::Failure;

#[tokio::test]
async fn a_directory_lists_every_file_below_it_in_name_order() -> Result<(), Failure> {
    let temp = tempfile::tempdir()?;
    let raw = temp.path().join("raw");
    fs::create_dir_all(raw.join("b"))?;
    fs::write(raw.join("b").join("two.csv"), b"22")?;
    fs::write(raw.join("a.csv"), b"1")?;
    let files = Files::new(temp.path().to_owned());
    let listing = files.resolve(&"./raw".parse()?).await?;
    let found: Vec<(&str, Option<u64>)> = listing
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.size))
        .collect();
    assert_eq!(found, [("a.csv", Some(1)), ("b/two.csv", Some(2))]);
    let url = &listing.entries[1].urls[0];
    assert_eq!(url.to_file_path(), Some(raw.join("b").join("two.csv")));
    Ok(())
}

#[tokio::test]
async fn a_file_url_is_one_file_and_a_missing_path_is_unresolved() -> Result<(), Failure> {
    let temp = tempfile::tempdir()?;
    let file = temp.path().join("one.csv");
    fs::write(&file, b"x")?;
    let files = Files::new(temp.path().to_owned());
    let url = url::Url::from_file_path(&file).map_err(|()| "no file URL")?;
    let listing = files.resolve(&url.as_str().parse()?).await?;
    assert_eq!(listing.entries.len(), 1);
    assert_eq!(listing.entries[0].path.as_str(), "one.csv");
    let error = files
        .resolve(&"./missing".parse()?)
        .await
        .err()
        .ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::ReferenceUnresolved);
    Ok(())
}

#[test]
fn files_serve_the_file_scheme() {
    assert_eq!(Files::new(".".into()).schemes(), ["file"]);
}

#[tokio::test]
async fn a_path_that_cannot_be_read_is_a_local_error_not_a_missing_one() -> Result<(), Failure> {
    let temp = tempfile::tempdir()?;
    fs::write(temp.path().join("one.csv"), b"x")?;
    let files = Files::new(temp.path().to_owned());
    let unreadable = if cfg!(windows) {
        "./a*b"
    } else {
        "./one.csv/inside"
    };
    let reference: Reference = unreadable.parse()?;
    let error = files.resolve(&reference).await.err().ok_or("resolved")?;
    assert_eq!(error.kind(), ErrorKind::LocalIo);
    Ok(())
}
