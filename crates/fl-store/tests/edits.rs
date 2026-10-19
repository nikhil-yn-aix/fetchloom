//! An edit made through a hardlink is caught by fingerprints and by verify.

use std::error::Error;
use std::fs;

use fl_model::document::Work;
use fl_model::path::DataPath;
use fl_store::data_dir::DataDir;
use fl_store::fingerprint::{hash_file, scan};
use fl_store::link::{Ladder, Linker, Mode};
use fl_store::tier::{Sharing, Tier};

type TestResult = Result<(), Box<dyn Error>>;

#[test]
fn an_edited_hardlinked_file_is_caught_by_fingerprints_and_by_verify() -> TestResult {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let source = temp.path().join("source");
    fs::write(&source, b"original bytes")?;
    let digest = tier.ingest_file(&source)?.digest;
    let kept = tier.ingest_file(&{
        let other = temp.path().join("other");
        fs::write(&other, b"untouched")?;
        other
    })?;
    let data = DataDir::new(temp.path().join("data"));
    let place = data.destination(&DataPath::new("ds")?, &DataPath::new("f.bin")?)?;
    let linked = Linker::new(&tier, Ladder::only(Mode::Hardlink)).link(&digest, &place)?;
    assert_eq!(linked.mode, Mode::Hardlink);
    let recorded = scan(&temp.path().join("data").join("ds"))?;

    let mut permissions = fs::metadata(place.path())?.permissions();
    #[expect(
        clippy::permissions_set_readonly_false,
        reason = "the test edits a read only file on purpose, as a careless user would"
    )]
    permissions.set_readonly(false);
    fs::set_permissions(place.path(), permissions)?;
    fs::write(place.path(), b"edited by hand, longer")?;

    let current = scan(&temp.path().join("data").join("ds"))?;
    assert_eq!(recorded.len(), 1);
    assert_ne!(current, recorded);
    assert_ne!(hash_file(place.path())?, digest);

    let verified = tier.verify(|_| true)?;
    assert_eq!(verified.checked, 2);
    assert_eq!(verified.damaged, [digest]);
    assert!(tier.locate(&digest).is_none());
    assert!(tier.locate(&kept.digest).is_some());
    let mut work = Work::default();
    verified.add_to(&mut work);
    assert_eq!(work.files_rehashed, 2);
    Ok(())
}
