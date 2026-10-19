//! Linking on the real filesystem: every mode forced, the fallbacks, replacement and the counters.

use std::error::Error;
use std::fs;
use std::io::Write;
use std::path::Path;

use fl_model::digest::Digest;
use fl_model::document::Work;
use fl_model::path::DataPath;
use fl_store::data_dir::{DataDir, Destination};
use fl_store::link::{Ladder, Linked, Linker, Mode};
use fl_store::tier::{Sharing, Tier};
use tempfile::TempDir;

type TestResult = Result<(), Box<dyn Error>>;

fn temp() -> Result<TempDir, Box<dyn Error>> {
    Ok(tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?)
}

fn object(tier: &Tier, bytes: &[u8]) -> Result<Digest, Box<dyn Error>> {
    let mut staged = tier.stage(Some(bytes.len() as u64))?;
    staged.write_all(bytes)?;
    Ok(tier.ingest(staged)?.digest)
}

fn destination(data: &DataDir, file: &str) -> Result<Destination, Box<dyn Error>> {
    Ok(data.destination(&DataPath::new("ds")?, &DataPath::new(file)?)?)
}

fn read_only(path: &Path) -> Result<bool, Box<dyn Error>> {
    Ok(fs::metadata(path)?.permissions().readonly())
}

#[test]
fn each_mode_forced_alone_places_the_bytes_and_reports_itself() -> TestResult {
    let temp = temp()?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let digest = object(&tier, b"forced")?;
    let data = DataDir::new(temp.path().join("data"));
    for mode in [Mode::Reflink, Mode::Hardlink, Mode::Symlink, Mode::Copy] {
        let place = destination(&data, &format!("{mode:?}"))?;
        let linked = Linker::new(&tier, Ladder::only(mode)).link(&digest, &place);
        if mode == Mode::Reflink && !cfg!(target_os = "macos") {
            if cfg!(windows) {
                assert!(linked.is_err(), "windows does not try reflink");
            }
            if linked.is_err() {
                continue;
            }
        }
        let linked = linked?;
        let copied = if mode == Mode::Copy { 6 } else { 0 };
        assert_eq!(
            linked,
            Linked {
                mode,
                bytes_copied: copied
            }
        );
        assert_eq!(fs::read(place.path())?, b"forced");
        assert!(read_only(place.path())?, "{mode:?}");
    }
    Ok(())
}

#[test]
fn the_auto_ladder_falls_to_the_first_mode_this_filesystem_allows() -> TestResult {
    let temp = temp()?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let digest = object(&tier, b"auto")?;
    let data = DataDir::new(temp.path().join("data"));
    let auto = Linker::new(&tier, Ladder::AUTO);
    for file in ["a", "b"] {
        let linked = auto.link(&digest, &destination(&data, file)?)?;
        let expected: &[Mode] = if cfg!(target_os = "macos") {
            &[Mode::Reflink]
        } else if cfg!(windows) {
            &[Mode::Hardlink]
        } else {
            &[Mode::Reflink, Mode::Hardlink]
        };
        assert!(expected.contains(&linked.mode), "{linked:?}");
        assert_eq!(linked.bytes_copied, 0);
    }
    Ok(())
}

#[test]
fn the_copy_ladder_places_a_writable_copy() -> TestResult {
    let temp = temp()?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let digest = object(&tier, b"writable")?;
    let data = DataDir::new(temp.path().join("data"));
    let place = destination(&data, "w")?;
    let linked = Linker::new(&tier, Ladder::COPY).link(&digest, &place)?;
    assert_eq!(linked.mode, Mode::Copy);
    assert!(!read_only(place.path())?);
    fs::write(place.path(), b"edited")?;
    let object = tier.locate(&digest).ok_or("object gone")?;
    assert_eq!(fs::read(object.path)?, b"writable");
    Ok(())
}

#[test]
fn replacing_a_hardlinked_file_keeps_the_old_object_read_only() -> TestResult {
    let temp = temp()?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let old = object(&tier, b"old")?;
    let new = object(&tier, b"new")?;
    let data = DataDir::new(temp.path().join("data"));
    let place = destination(&data, "x")?;
    let linker = Linker::new(&tier, Ladder::only(Mode::Hardlink));
    linker.link(&old, &place)?;
    let parent = place.path().parent().ok_or("no parent")?;
    fs::write(parent.join(".x.fl-tmp"), b"left by a crash")?;
    linker.link(&new, &place)?;
    assert_eq!(fs::read(place.path())?, b"new");
    let old_object = tier.locate(&old).ok_or("old object gone")?.path;
    assert_eq!(fs::read(&old_object)?, b"old");
    assert!(read_only(&old_object)?);
    let names: Vec<_> = fs::read_dir(parent)?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names, ["x"]);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
#[test]
fn an_object_at_its_link_limit_falls_to_the_next_mode_for_that_file() -> TestResult {
    let temp = temp()?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let full = object(&tier, b"full")?;
    let spare = object(&tier, b"spare")?;
    let full_path = tier.locate(&full).ok_or("object gone")?.path;
    let links = temp.path().join("links");
    fs::create_dir(&links)?;
    let mut count = 0_u32;
    loop {
        match fs::hard_link(&full_path, links.join(count.to_string())) {
            Ok(()) => count += 1,
            Err(error) if error.kind() == std::io::ErrorKind::TooManyLinks => break,
            Err(error) => return Err(error.into()),
        }
    }
    let data = DataDir::new(temp.path().join("data"));
    let linker = Linker::new(&tier, Ladder::AUTO);
    let at_limit = linker.link(&full, &destination(&data, "full")?)?;
    assert_eq!(at_limit.mode, Mode::Symlink, "after {count} links");
    assert_eq!(fs::read(destination(&data, "full")?.path())?, b"full");
    let next = linker.link(&spare, &destination(&data, "spare")?)?;
    assert!(
        matches!(next.mode, Mode::Reflink | Mode::Hardlink),
        "{next:?}"
    );
    Ok(())
}

#[test]
fn a_second_link_of_ten_thousand_objects_copies_nothing() -> TestResult {
    let temp = temp()?;
    let tier = Tier::open(&temp.path().join("tier"), Sharing::Private)?;
    let digests = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let tier = &tier;
                scope.spawn(move || {
                    (worker * 1250..(worker + 1) * 1250)
                        .map(|index| object(tier, format!("object {index}").as_bytes()))
                        .map(|digest| digest.map_err(|error| error.to_string()))
                        .collect::<Result<Vec<_>, _>>()
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().map_err(|_| "a worker panicked".to_owned())?)
            .collect::<Result<Vec<_>, _>>()
    })?
    .concat();
    let mut works = Vec::new();
    for project in ["first", "second"] {
        let data = DataDir::new(temp.path().join(project).join("data"));
        let pairs = digests
            .iter()
            .enumerate()
            .map(|(index, digest)| {
                let file = format!("{}/{index}", index % 100);
                Ok((*digest, destination(&data, &file)?))
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let mut work = Work::default();
        for linked in Linker::new(&tier, Ladder::AUTO).link_all(&pairs) {
            linked?.add_to(&mut work);
        }
        works.push(work);
    }
    let second = &works[1];
    assert_eq!(second.files_linked, 10_000);
    assert_eq!(second.bytes_copied, 0);
    assert_eq!(second.links.hardlink + second.links.reflink, 10_000);
    Ok(())
}
