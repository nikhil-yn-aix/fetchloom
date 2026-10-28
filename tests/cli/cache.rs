//! what an earlier run leaves in the cache, and how the next run copes with it.

use std::fs;

use crate::support::{Result, Sandbox, Server, file, text};

const IRIS: &[u8] =
    b"sepal_length,sepal_width,petal_length,petal_width,species\n5.1,3.5,1.4,0.2,setosa\n";

#[test]
fn a_download_sealed_but_never_published_is_fetched_again() -> Result {
    let sandbox = Sandbox::new()?;
    // the scripted server answers every request whole, as a server that ignores ranges does
    let server = Server::start(
        "/iris.csv",
        vec![file(IRIS, "\"v1\""), file(IRIS, "\"v1\"")],
    )?;
    // a file where each object folder belongs makes publishing fail once the download is sealed
    let objects = sandbox.cache().join("objects");
    fs::create_dir_all(&objects)?;
    let blockers: Vec<_> = (0..=u8::MAX)
        .map(|prefix| objects.join(format!("{prefix:02x}")))
        .collect();
    for blocker in &blockers {
        fs::write(blocker, "")?;
    }
    let first = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(first.status.code(), Some(50), "{}", text(&first.stdout)?);
    let left: Vec<_> = fs::read_dir(sandbox.cache().join("partial"))?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    let named = |extension: &str| {
        left.iter()
            .find(|path| path.extension().is_some_and(|found| found == extension))
    };
    let sealed = named("part").is_some_and(|path| {
        fs::metadata(path).is_ok_and(|metadata| metadata.permissions().readonly())
    });
    assert!(
        sealed && named("json").is_some(),
        "the first run left {left:?}"
    );
    for blocker in &blockers {
        fs::remove_file(blocker)?;
    }

    let second = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(second.status.code(), Some(0), "{}", text(&second.stdout)?);
    assert_eq!(
        fs::read(sandbox.work().join("iris").join("iris.csv"))?,
        IRIS
    );
    Ok(())
}
