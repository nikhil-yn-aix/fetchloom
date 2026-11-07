use std::time::Duration;

use super::*;
use crate::cache::Validators;
use crate::cache::partial::{Claim, PartialWriter};

pub(super) fn wait_for_release(ready: SyncSender<()>, release: Receiver<()>) {
    ready.send(()).unwrap();
    release.recv().unwrap();
}

fn pause_hashing(writer: &mut PartialWriter) -> SyncSender<()> {
    let (release, paused) = mpsc::sync_channel(1);
    let (ready, entered) = mpsc::sync_channel(1);
    assert!(
        writer
            .hasher
            .sender
            .as_ref()
            .unwrap()
            .send(Message::Pause {
                ready,
                release: paused
            })
            .is_ok()
    );
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    release
}

#[test]
fn writing_can_run_ahead_of_hashing_and_checkpoints_keep_the_digest() {
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
    writer.write_chunk(0, &chunk).unwrap();
    let bytes = std::fs::read(&partial.bytes).unwrap();
    assert_eq!(crate::digest::sha256(&bytes), expected);
    release.send(()).unwrap();
    writer.save().unwrap();
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
                gathered: Vec::new(),
                gathered_bytes: 0,
                inline: None,
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
        gathered: Vec::new(),
        gathered_bytes: 0,
        inline: None,
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
    let chunk = Bytes::from(vec![b'x'; BATCH]);
    for _ in 0..QUEUED {
        writer.write_chunk(writer.written(), &chunk).unwrap();
    }
    let excess = writer
        .hasher
        .sender
        .as_ref()
        .unwrap()
        .try_send(Message::Chunks(vec![chunk]));
    release.send(()).unwrap();
    assert!(matches!(excess, Err(mpsc::TrySendError::Full(_))));
    assert_eq!(
        writer.finish().unwrap().digest,
        crate::digest::sha256(&vec![b'x'; QUEUED * BATCH])
    );
}

#[test]
fn dropping_a_writer_joins_hashing_before_releasing_its_claim() {
    let (_dir, cache) = crate::cache::tests::cache();
    let Claim::Owned(partial) = cache.claim("https://example.org/join").unwrap() else {
        panic!("nothing else holds the download");
    };
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    let claim = Arc::downgrade(writer._lock.as_ref().unwrap());
    let (sender, inbox) = mpsc::sync_channel(QUEUED);
    let (closed, closing) = mpsc::sync_channel(1);
    let (release, paused) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || {
        while inbox.recv().is_ok() {}
        closed.send(()).unwrap();
        paused.recv().unwrap();
        Ok((Hasher::default(), 0))
    });
    writer.hasher = HashPipe {
        sender: Some(sender),
        worker: Some(worker),
        gathered: Vec::new(),
        gathered_bytes: 0,
        inline: None,
    };
    drop(partial);
    let dropping = thread::spawn(move || drop(writer));
    let closed = closing.recv_timeout(Duration::from_secs(5));
    let held = claim.upgrade().is_some();
    release.send(()).unwrap();
    dropping.join().unwrap();
    closed.unwrap();
    assert!(
        held,
        "the hash worker was still draining when the claim was released"
    );
    assert!(claim.upgrade().is_none());
}

#[test]
fn a_small_download_hashes_on_the_writers_thread_with_the_same_result() {
    let (_dir, cache) = crate::cache::tests::cache();
    let Claim::Owned(partial) = cache.claim("https://example.org/small").unwrap() else {
        panic!("nothing else holds the download");
    };
    let body = b"0123456789abcdef";
    let mut writer = partial
        .writer(
            0,
            Some(body.len() as u64),
            Validators::default(),
            false,
            false,
        )
        .unwrap();
    assert!(
        writer.hasher.inline.is_some(),
        "a known small length hashes inline"
    );
    // the end first, as a split stream may write it, then the start that reaches it
    writer
        .write_chunk(8, &Bytes::from_static(&body[8..]))
        .unwrap();
    writer
        .write_chunk(0, &Bytes::from_static(&body[..8]))
        .unwrap();
    writer.save().unwrap();
    assert_eq!(writer.finish().unwrap().digest, crate::digest::sha256(body));
}

#[test]
fn inline_hashing_shares_the_original_writer_handle() {
    let (_dir, cache) = crate::cache::tests::cache();
    let Claim::Owned(partial) = cache.claim("https://example.org/shared-handle").unwrap() else {
        panic!("the test owns the download");
    };
    let mut writer = partial
        .writer(0, Some(4), Validators::default(), true, false)
        .unwrap();
    assert!(Arc::ptr_eq(
        &writer.file,
        &writer.hasher.inline.as_ref().unwrap().file
    ));
    writer.write_chunk(2, &Bytes::from_static(b"cd")).unwrap();
    writer.write_chunk(0, &Bytes::from_static(b"ab")).unwrap();
    let stored = writer.finish().unwrap();
    assert_eq!(stored.digest, crate::digest::sha256(b"abcd"));
    assert_eq!(
        std::fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcd"
    );
}
