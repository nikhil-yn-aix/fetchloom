use std::time::Duration;

use super::*;
use crate::cache::Validators;
use crate::cache::partial::{Claim, PartialWriter};

fn pause_hashing(writer: &mut PartialWriter) -> SyncSender<()> {
    let (sender, receiver) = mpsc::sync_channel(QUEUED);
    let (release, paused) = mpsc::sync_channel(1);
    let file = writer.file.try_clone().unwrap();
    let worker = thread::spawn(move || {
        paused.recv().unwrap();
        hash(&file, Hasher::default(), 0, &receiver)
    });
    writer.hasher = HashPipe {
        sender: Some(sender),
        worker: Some(worker),
    };
    release
}

#[test]
fn writing_can_run_ahead_of_hashing_but_a_checkpoint_waits() {
    let (_dir, cache) = crate::cache::tests::cache();
    let Claim::Owned(partial) = cache.claim("https://example.org/overlap").unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    let release = pause_hashing(&mut writer);
    let chunk = Bytes::from(vec![b'x'; super::super::BUFFER]);
    let expected = crate::digest::sha256(&chunk);
    let (written, write_done) = mpsc::sync_channel(1);
    let (saved, save_done) = mpsc::sync_channel(1);
    let background = thread::spawn(move || {
        let result = writer.write_chunk(0, &chunk);
        written.send(result).unwrap();
        let result = writer.save();
        saved.send(result).unwrap();
        writer
    });
    let written = write_done.recv_timeout(Duration::from_secs(5));
    let before_hashing = save_done.recv_timeout(Duration::from_millis(100));
    let bytes = std::fs::read(&partial.bytes).unwrap();
    release.send(()).unwrap();
    let writer = background.join().unwrap();
    written.unwrap().unwrap();
    assert!(matches!(
        before_hashing,
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    save_done.recv().unwrap().unwrap();
    assert_eq!(crate::digest::sha256(&bytes), expected);
    let record = partial.recorded().unwrap();
    assert_eq!(record.received, u64::try_from(bytes.len()).unwrap());
    assert_eq!(record.sha256, expected);
    assert_eq!(writer.finish().unwrap().digest, expected);
}

#[test]
fn a_hash_worker_failure_preserves_its_io_error() {
    for kind in [io::ErrorKind::StorageFull, io::ErrorKind::PermissionDenied] {
        for checkpoint in [true, false] {
            let (sender, receiver) = mpsc::sync_channel(QUEUED);
            let worker = thread::spawn(move || {
                drop(receiver);
                Err(io::Error::new(kind, "hash worker failed"))
            });
            let mut pipe = HashPipe {
                sender: Some(sender),
                worker: Some(worker),
            };
            let error = if checkpoint {
                pipe.sha256_so_far(0).unwrap_err()
            } else {
                pipe.finish(0).unwrap_err()
            };
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), "hash worker failed");
        }
    }
}

#[test]
fn a_panicked_hash_worker_is_an_io_error() {
    let (sender, receiver) = mpsc::sync_channel(QUEUED);
    let worker = thread::spawn(move || -> io::Result<(Hasher, u64)> {
        drop(receiver);
        panic!("hash worker failed");
    });
    let pipe = HashPipe {
        sender: Some(sender),
        worker: Some(worker),
    };
    assert_eq!(pipe.finish(0).unwrap_err().kind(), io::ErrorKind::Other);
}

#[test]
fn a_slow_hasher_bounds_the_number_of_in_flight_chunks() {
    let (_dir, cache) = crate::cache::tests::cache();
    let Claim::Owned(partial) = cache.claim("https://example.org/bounded").unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    let release = pause_hashing(&mut writer);
    let (progress, received) = mpsc::sync_channel(QUEUED + 1);
    let background = thread::spawn(move || {
        for _ in 0..=QUEUED {
            let result = writer.write_chunk(writer.written(), &Bytes::from_static(b"x"));
            progress.send(result).unwrap();
        }
        writer
    });
    let mut first = Vec::new();
    for _ in 0..QUEUED {
        let result = received.recv_timeout(Duration::from_secs(5));
        let failed = !matches!(result, Ok(Ok(())));
        first.push(result);
        if failed {
            break;
        }
    }
    let excess = received.recv_timeout(Duration::from_millis(100));
    release.send(()).unwrap();
    let writer = background.join().unwrap();
    assert_eq!(first.len(), QUEUED);
    for result in first {
        result.unwrap().unwrap();
    }
    assert!(matches!(excess, Err(mpsc::RecvTimeoutError::Timeout)));
    received.recv().unwrap().unwrap();
    assert_eq!(
        writer.finish().unwrap().digest,
        crate::digest::sha256(&[b'x'; QUEUED + 1])
    );
}

#[test]
fn dropping_a_writer_joins_hashing_before_releasing_its_claim() {
    let (_dir, cache) = crate::cache::tests::cache();
    let url = "https://example.org/join";
    let Claim::Owned(partial) = cache.claim(url).unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    let release = pause_hashing(&mut writer);
    writer.write_chunk(0, &Bytes::from_static(b"abc")).unwrap();
    drop(partial);
    let (done, ended) = mpsc::sync_channel(1);
    let dropping = thread::spawn(move || {
        drop(writer);
        done.send(()).unwrap();
    });
    let before_hashing = ended.recv_timeout(Duration::from_millis(100));
    let held = matches!(cache.claim(url).unwrap(), Claim::Busy);
    release.send(()).unwrap();
    dropping.join().unwrap();
    assert!(matches!(
        before_hashing,
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(held);
    ended.recv().unwrap();
    assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
}
