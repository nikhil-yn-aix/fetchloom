use std::time::Duration;

use super::*;
use crate::cache::Validators;
use crate::cache::partial::{Claim, PartialWriter};

pub(super) fn wait_for_release(ready: &SyncSender<()>, release: &Receiver<()>) {
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
    let claim = Arc::downgrade(partial.lock.as_ref().unwrap());
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

fn with_workers(hasher: Hasher, file: Arc<File>, received: u64, workers: usize) -> HashPipe {
    HashPipe::with_policy(
        hasher,
        file,
        received,
        Policy {
            workers: Arc::new(Workers::new(workers)),
            start: lanes::spawn,
        },
    )
    .unwrap()
}

fn paired(pipe: &mut HashPipe) -> bool {
    pipe.send_gathered().unwrap();
    let (reply, result) = mpsc::sync_channel(1);
    pipe.sender
        .as_ref()
        .unwrap()
        .send(Message::Paired { reply })
        .unwrap();
    result.recv_timeout(Duration::from_secs(5)).unwrap()
}

#[test]
fn streamed_checksums_and_checkpoints_match_independent_vectors() {
    use std::io::Write as _;
    let prefix = b"header";
    let first = vec![b'a'; BATCH];
    let middle = vec![b'c'; 4096];
    let last = vec![b'b'; BATCH + 7];
    let mut body = Vec::new();
    for bytes in [prefix.as_slice(), &first, &middle, &last] {
        body.extend_from_slice(bytes);
    }
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(&body).unwrap();
    let mut hasher = Hasher::with_md5();
    hasher.update(prefix);
    let mut pipe = with_workers(hasher, Arc::new(file), prefix.len() as u64, 2);
    pipe.update(Bytes::from(first)).unwrap();
    let checkpoint = prefix.len() + BATCH;
    assert_eq!(
        pipe.sha256_so_far(checkpoint as u64).unwrap().hex(),
        "9a4847bf45fdb48eed8066100fcec6158d350787e5d9fe1e124e08860c95702a"
    );
    pipe.range(checkpoint as u64, (checkpoint + middle.len()) as u64)
        .unwrap();
    pipe.update(Bytes::from(last)).unwrap();
    let (sha, md5) = pipe.finish(body.len() as u64).unwrap();
    assert_eq!(
        sha.hex(),
        "6302717197051a248ed8ac6b97fb6d13f5759e1075380643c841c88efdc10d32"
    );
    assert_eq!(md5.unwrap().hex(), "d5dfca435c65dbd8654670756f4d822c");
}

#[test]
fn only_full_md5_batches_with_cpu_capacity_get_a_second_lane() {
    for (md5, workers, size, expected) in [
        (false, 2, BATCH, false),
        (true, 1, BATCH, false),
        (true, 2, 31, false),
        (true, 2, BATCH, true),
    ] {
        let hasher = if md5 {
            Hasher::with_md5()
        } else {
            Hasher::default()
        };
        let mut pipe = with_workers(hasher, Arc::new(tempfile::tempfile().unwrap()), 0, workers);
        pipe.update(Bytes::from(vec![b'a'; size])).unwrap();
        assert_eq!(
            paired(&mut pipe),
            expected,
            "md5={md5}, workers={workers}, size={size}"
        );
        pipe.finish(size as u64).unwrap();
    }
}

#[test]
fn a_refused_md5_worker_keeps_the_prefixed_digest_on_one_lane() {
    fn refuse(_work: lanes::Work) -> io::Result<JoinHandle<lanes::Hashed>> {
        Err(io::Error::new(io::ErrorKind::WouldBlock, "no thread"))
    }
    let mut hasher = Hasher::with_md5();
    hasher.update(&vec![b'a'; BATCH]);
    let mut pipe = HashPipe::with_policy(
        hasher,
        Arc::new(tempfile::tempfile().unwrap()),
        BATCH as u64,
        Policy {
            workers: Arc::new(Workers::new(2)),
            start: refuse,
        },
    )
    .unwrap();
    pipe.update(Bytes::from(vec![b'a'; BATCH])).unwrap();
    assert!(!paired(&mut pipe));
    let (sha, md5) = pipe.finish((2 * BATCH) as u64).unwrap();
    assert_eq!(
        sha.hex(),
        "5256ec18f11624025905d057d6befb03d77b243511ac5f77ed5e0221ce6d84b5"
    );
    assert_eq!(md5.unwrap().hex(), "de89461b64701958984c95d1bfb0065a");
}

#[test]
fn another_hash_worker_leaves_no_spare_slot_for_md5() {
    let workers = Arc::new(Workers::new(2));
    let other = workers.enter();
    let mut pipe = HashPipe::with_policy(
        Hasher::with_md5(),
        Arc::new(tempfile::tempfile().unwrap()),
        0,
        Policy {
            workers,
            start: lanes::spawn,
        },
    )
    .unwrap();
    pipe.update(Bytes::from(vec![b'a'; BATCH])).unwrap();
    assert!(!paired(&mut pipe));
    drop(other);
    pipe.finish(BATCH as u64).unwrap();
}

#[test]
fn a_paused_md5_lane_keeps_shared_bytes_in_a_bounded_queue() {
    struct Owned {
        bytes: Vec<u8>,
        _alive: Arc<()>,
    }
    impl AsRef<[u8]> for Owned {
        fn as_ref(&self) -> &[u8] {
            &self.bytes
        }
    }
    let workers = Arc::new(Workers::new(2));
    let _primary = workers.enter();
    let mut hasher = Hasher::with_md5();
    let mut lane = Lane::start(&mut hasher, 0, &workers, lanes::spawn).unwrap();
    let (ready, entered) = mpsc::sync_channel(1);
    let (release, paused) = mpsc::sync_channel(1);
    lane.send(Input::Pause {
        ready,
        release: paused,
    })
    .unwrap();
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let alive = Arc::new(());
    let weak = Arc::downgrade(&alive);
    let bytes = Bytes::from_owner(Owned {
        bytes: vec![b'a'; BATCH],
        _alive: alive,
    });
    hasher.update(&bytes);
    lane.send(Input::Chunks(Arc::new(vec![bytes]))).unwrap();
    assert!(
        weak.upgrade().is_some(),
        "MD5 still owns bytes that SHA has finished"
    );
    assert!(matches!(
        lane.sender
            .as_ref()
            .unwrap()
            .try_send(Input::Chunks(Arc::new(Vec::new()))),
        Err(mpsc::TrySendError::Full(_))
    ));
    release.send(()).unwrap();
    lane.checkpoint(BATCH as u64).unwrap();
    assert!(
        weak.upgrade().is_none(),
        "the barrier follows release of its earlier batches"
    );
    let (state, received) = lane.finish().unwrap();
    assert_eq!(received, BATCH as u64);
    hasher.restore_md5(state);
    let (sha, md5) = hasher.finish();
    assert_eq!(
        sha.hex(),
        "9bc1b2a288b26af7257a36277ae3816a7d4f16e89c1e7e77d0a5c48bad62b360"
    );
    assert_eq!(md5.unwrap().hex(), "7202826a7791073fe2787f0c94603278");
}

#[test]
fn dropping_a_paired_writer_joins_md5_before_releasing_its_claim() {
    let (_dir, cache) = crate::cache::tests::cache();
    let Claim::Owned(partial) = cache.claim("https://example.org/join-md5").unwrap() else {
        panic!("the test owns the download");
    };
    let mut writer = partial
        .writer(0, None, Validators::default(), true, false)
        .unwrap();
    writer.hasher = with_workers(Hasher::with_md5(), Arc::clone(&writer.file), 0, 2);
    writer
        .write_chunk(0, &Bytes::from(vec![b'a'; BATCH]))
        .unwrap();
    assert!(paired(&mut writer.hasher));
    let claim = Arc::downgrade(partial.lock.as_ref().unwrap());
    let (ready, closed) = mpsc::sync_channel(1);
    let (release, paused) = mpsc::sync_channel(1);
    writer
        .hasher
        .sender
        .as_ref()
        .unwrap()
        .send(Message::Md5Closing {
            ready,
            release: paused,
        })
        .unwrap();
    drop(partial);
    let dropping = thread::spawn(move || drop(writer));
    closed.recv_timeout(Duration::from_secs(5)).unwrap();
    let held = claim.upgrade().is_some();
    release.send(()).unwrap();
    dropping.join().unwrap();
    assert!(
        held,
        "the claim stays held until the second checksum lane has joined"
    );
    assert!(claim.upgrade().is_none());
}

#[test]
fn a_secondary_failure_reaches_the_checkpoint() {
    fn fail(work: lanes::Work) -> io::Result<JoinHandle<lanes::Hashed>> {
        drop(work);
        thread::Builder::new().spawn(|| {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "md5 worker failed",
            ))
        })
    }
    let mut pipe = HashPipe::with_policy(
        Hasher::with_md5(),
        Arc::new(tempfile::tempfile().unwrap()),
        0,
        Policy {
            workers: Arc::new(Workers::new(2)),
            start: fail,
        },
    )
    .unwrap();
    pipe.update(Bytes::from(vec![b'a'; BATCH])).unwrap();
    let error = pipe.sha256_so_far(BATCH as u64).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(error.to_string(), "md5 worker failed");
}

#[test]
fn a_new_hash_worker_returns_an_overfull_pair_to_one_lane() {
    let workers = Arc::new(Workers::new(2));
    let mut pipe = HashPipe::with_policy(
        Hasher::with_md5(),
        Arc::new(tempfile::tempfile().unwrap()),
        0,
        Policy {
            workers: Arc::clone(&workers),
            start: lanes::spawn,
        },
    )
    .unwrap();
    pipe.update(Bytes::from(vec![b'a'; BATCH])).unwrap();
    assert!(paired(&mut pipe));
    let _other = workers.enter();
    pipe.update(Bytes::from(vec![b'a'; BATCH])).unwrap();
    assert!(!paired(&mut pipe));
    let (sha, md5) = pipe.finish((2 * BATCH) as u64).unwrap();
    assert_eq!(
        sha.hex(),
        "5256ec18f11624025905d057d6befb03d77b243511ac5f77ed5e0221ce6d84b5"
    );
    assert_eq!(md5.unwrap().hex(), "de89461b64701958984c95d1bfb0065a");
}
