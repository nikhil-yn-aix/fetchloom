//! the private copy of each dataset a step reads, made from the cache, never the data folder, as
//! read-only reflinks or copies, never hardlinks, so a step writing one changes only its own copy.

use std::io;
use std::path::Path;
use std::sync::Arc;

use crate::cache::Cache;
use crate::digest::Sha256;
use crate::place::{Order, Placer};
use crate::stop::Stop;
use crate::threads::Threads;

/// a file of a dataset a step reads: its path in the dataset's folder, its bytes and size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct File {
    pub(crate) path: String,
    pub(crate) sha256: Sha256,
    pub(crate) size: u64,
}

/// makes `folder` hold `files`, each read-only from its object in `cache`. it fails on an object
/// the cache lacks, a disk that refuses the copy, or a stop.
pub(crate) fn make(
    cache: &Cache,
    folder: &Path,
    files: &[File],
    threads: Threads,
    stop: &Arc<Stop>,
) -> io::Result<()> {
    let _span = tracing::debug_span!("inputs", files = files.len()).entered();
    std::fs::create_dir_all(folder)?;
    let mut placer = Placer::without_hardlinks(folder)
        .with_threads(threads)
        .with_stop(Arc::clone(stop));
    let order = |index: usize| {
        let file = &files[index];
        Order {
            object: cache.object_path(&file.sha256),
            destination: folder.join(&file.path),
            writable: false,
            size: file.size,
        }
    };
    for placed in placer.place_all(files.len(), order, |_| ()) {
        placed?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{cache, store};

    /// a step that makes its input writable and changes it leaves the cache as it was.
    #[test]
    fn an_input_is_a_read_only_copy_never_the_object() {
        let (dir, cache) = cache();
        let stored = store(&cache, b"sepal_length\n5.1\n");
        let folder = dir.path().join("in").join("iris");
        let files = [File {
            path: "data/iris.csv".to_owned(),
            sha256: stored.digest,
            size: stored.size,
        }];
        make(&cache, &folder, &files, Threads::default(), &Arc::default()).unwrap();
        let input = folder.join("data").join("iris.csv");
        assert!(std::fs::metadata(&input).unwrap().permissions().readonly());
        assert!(!same_file::is_same_file(&input, cache.object_path(&stored.digest)).unwrap());
        crate::cache::tests::rewrite(&input, b"changed by a step");
        assert_eq!(
            std::fs::read(cache.object_path(&stored.digest)).unwrap(),
            b"sepal_length\n5.1\n"
        );
    }
}
