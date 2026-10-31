//! resuming a download, what an earlier run left behind, and one process per download.

use super::super::tests::{cache, fat, hardlinks, seal, store};
use super::*;
use std::io::{Seek, SeekFrom};

fn owned(cache: &Cache, url: &str) -> Partial {
    match cache.claim(url).unwrap() {
        Claim::Owned(partial) => partial,
        Claim::Busy => panic!("nothing else holds {url}"),
    }
}

#[test]
fn a_saved_partial_resumes_and_hashes_the_whole_file() {
    let (_dir, cache) = cache();
    let url = "https://example.org/big";
    let validators = Validators {
        etag: Some("\"v1\"".to_owned()),
        last_modified: None,
    };
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, Some(6), validators.clone(), true, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    // bytes written after the last save are lost on a crash, so the record must win
    writer.write_all(b"XYZ").unwrap();
    drop(writer);
    let (received, seen) = partial.resumable().unwrap();
    assert_eq!(received, 3);
    assert_eq!(seen, validators);

    let mut resumed = partial
        .writer(received, Some(6), seen, true, false)
        .unwrap();
    assert_eq!(resumed.written(), 3);
    resumed.write_all(b"def").unwrap();
    let stored = resumed.finish().unwrap();
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcdef"
    );
    assert_eq!(stored.size, 6);
    assert_eq!(
        stored.md5.map(|md5| md5.hex()).as_deref(),
        Some("e80b5017098950fc58aad83c8c14978e"),
        "md5 covers the resumed prefix too"
    );
    assert!(
        partial.resumable().is_none(),
        "a finished download leaves nothing to resume"
    );
}

#[test]
fn bytes_that_differ_from_their_record_are_never_continued() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/big");
    let mut writer = partial
        .writer(0, Some(6), Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    // a power cut can keep the record and lose the bytes it counts
    fs::write(&partial.bytes, b"\0\0\0").unwrap();
    let (received, validators) = partial.resumable().unwrap();
    let Err(error) = partial.writer(received, Some(6), validators, false, false) else {
        panic!("zeros were continued as if they were abc");
    };
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
}

#[test]
fn a_sealed_download_is_dropped_and_its_object_stays_read_only() {
    let (dir, cache) = cache();
    // FAT32 and exFAT cannot hardlink, so there a sealed download is never a second name of its
    // object, and a_sealed_download_is_dropped_on_fat covers them
    if !hardlinks(dir.path()) {
        return;
    }
    let url = "https://example.org/sealed";
    let object = store(&cache, b"abc");
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, Some(3), Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    // a run that died while publishing leaves its record and, sealed, a second name of the
    // object it was publishing
    fs::remove_file(&partial.bytes).unwrap();
    fs::hard_link(cache.object_path(&object.digest), &partial.bytes).unwrap();
    drop(partial);

    let partial = owned(&cache, url);
    assert!(partial.resumable().is_none());
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"xyz").unwrap();
    assert_eq!(
        fs::read(cache.object_path(&writer.finish().unwrap().digest)).unwrap(),
        b"xyz"
    );
    assert_eq!(fs::read(cache.object_path(&object.digest)).unwrap(), b"abc");
    assert!(
        fs::metadata(cache.object_path(&object.digest))
            .unwrap()
            .permissions()
            .readonly()
    );
}

#[test]
fn a_sealed_download_is_dropped_on_fat() {
    let (_dir, cache) = fat();
    let url = "https://example.org/sealed";
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, Some(3), Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    writer.save().unwrap();
    drop(writer);
    // a run that died while publishing leaves its record and the bytes it sealed
    seal(&partial.bytes);
    drop(partial);

    let partial = owned(&cache, url);
    assert!(partial.resumable().is_none());
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"xyz").unwrap();
    assert_eq!(
        fs::read(cache.object_path(&writer.finish().unwrap().digest)).unwrap(),
        b"xyz"
    );
}

#[test]
fn a_download_belongs_to_one_process_at_a_time() {
    let (_dir, cache) = cache();
    let url = "https://example.org/shared";
    let first = owned(&cache, url);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Busy));
    assert!(matches!(
        cache.claim("https://example.org/other").unwrap(),
        Claim::Owned(_)
    ));
    drop(first);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
}

#[test]
fn a_writer_keeps_its_download_claimed_until_it_stops() {
    let (_dir, cache) = cache();
    let url = "https://example.org/held";
    let partial = owned(&cache, url);
    let writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    // a stopped run drops its claim while a writer thread may still be writing
    drop(partial);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Busy));
    drop(writer);
    assert!(matches!(cache.claim(url).unwrap(), Claim::Owned(_)));
}

#[test]
fn a_published_object_can_be_read_while_its_download_is_still_claimed() {
    let (_dir, cache) = cache();
    let url = "https://example.org/a";
    let partial = owned(&cache, url);
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.write_all(b"abc").unwrap();
    let stored = writer.finish().unwrap();
    assert_eq!(fs::read(cache.object_path(&stored.digest)).unwrap(), b"abc");
    drop(partial);
    assert_eq!(store(&cache, b"abc").digest, stored.digest);
}

/// a writer for a 12 byte file, for streams that each write their own part of it.
fn split_writer(partial: &Partial) -> PartialWriter {
    partial
        .writer(0, Some(12), Validators::default(), true, true)
        .unwrap()
}

#[test]
fn bytes_written_out_of_order_hash_as_the_whole_file() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/split");
    let mut writer = split_writer(&partial);
    writer.write_at(8, b"ijkl").unwrap();
    writer.write_at(4, b"efgh").unwrap();
    assert_eq!(
        writer.written(),
        0,
        "nothing is hashed before the first byte"
    );
    writer.write_at(0, b"abcd").unwrap();
    assert_eq!(writer.written(), 12);
    assert_eq!(
        writer.read_back(),
        8,
        "the prefix reads back what other streams wrote"
    );
    let stored = writer.finish().unwrap();
    assert_eq!(stored.digest, crate::digest::sha256(b"abcdefghijkl"));
    assert_eq!(
        stored.md5.map(|md5| md5.hex()).as_deref(),
        Some("9fc9d606912030dca86582ed62595cf7")
    );
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcdefghijkl"
    );
}

#[test]
fn a_single_stream_reads_nothing_back() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/one");
    let mut writer = split_writer(&partial);
    writer.write_all(b"abcdef").unwrap();
    writer.write_all(b"ghijkl").unwrap();
    assert_eq!(writer.read_back(), 0);
    assert_eq!(
        writer.finish().unwrap().digest,
        crate::digest::sha256(b"abcdefghijkl")
    );
}

#[test]
fn a_stream_the_prefix_reaches_is_hashed_as_it_writes() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/caught");
    let mut writer = split_writer(&partial);
    writer.write_at(4, b"ef").unwrap();
    writer.write_at(0, b"abcd").unwrap();
    assert_eq!(writer.read_back(), 2);
    writer.write_at(6, b"ghijkl").unwrap();
    assert_eq!(writer.read_back(), 2, "bytes at the prefix need no read");
    assert_eq!(
        writer.finish().unwrap().digest,
        crate::digest::sha256(b"abcdefghijkl")
    );
}

/// writes `abc` at the start and `ghi` in the middle of a 12 byte file, saves, and stops.
fn two_parts(partial: &Partial) {
    let mut writer = split_writer(partial);
    writer.write_at(0, b"abc").unwrap();
    writer.write_at(6, b"ghi").unwrap();
    writer.save().unwrap();
}

#[test]
fn a_resume_keeps_the_ranges_other_streams_finished() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/parts");
    two_parts(&partial);
    let (received, seen) = partial.resumable().unwrap();
    assert_eq!(received, 3, "the prefix is what a plain resume continues");
    let mut resumed = partial
        .writer(received, Some(12), seen, true, true)
        .unwrap();
    assert_eq!(resumed.gaps(12), [(3, 6), (9, 12)]);
    assert_eq!(resumed.kept(), 3);
    resumed.write_at(9, b"jkl").unwrap();
    resumed.write_at(3, b"def").unwrap();
    let stored = resumed.finish().unwrap();
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcdefghijkl"
    );
}

#[test]
fn a_kept_range_whose_bytes_changed_is_fetched_again() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/changed");
    two_parts(&partial);
    // a power cut can keep the record and lose the bytes it counts
    let mut file = OpenOptions::new().write(true).open(&partial.bytes).unwrap();
    file.seek(SeekFrom::Start(6)).unwrap();
    file.write_all(b"\0\0\0").unwrap();
    drop(file);
    let (received, seen) = partial.resumable().unwrap();
    let resumed = partial
        .writer(received, Some(12), seen, true, true)
        .unwrap();
    assert_eq!(resumed.gaps(12), [(3, 12)]);
}

#[test]
fn a_resume_that_does_not_split_forgets_kept_ranges() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/single");
    two_parts(&partial);
    let (received, seen) = partial.resumable().unwrap();
    let mut resumed = partial
        .writer(received, Some(12), seen, true, false)
        .unwrap();
    assert_eq!(resumed.gaps(12), [(3, 12)]);
    assert_eq!(fs::metadata(&partial.bytes).unwrap().len(), 3);
    resumed.write_all(b"defghijkl").unwrap();
    let stored = resumed.finish().unwrap();
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        b"abcdefghijkl"
    );
}

#[test]
fn a_download_with_gaps_is_never_published() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/gaps");
    let mut writer = split_writer(&partial);
    writer.write_at(0, b"abc").unwrap();
    writer.write_at(6, b"ghi").unwrap();
    let error = writer.finish().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(cache.objects().is_empty());
}

#[test]
fn pipelined_checkpoints_hash_exactly_the_received_prefix() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/checkpoints");
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    let mut expected = Vec::new();
    for index in 0u8..48 {
        let chunk = Bytes::from(vec![index; 96 * 1024 + usize::from(index)]);
        writer.write_chunk(writer.written(), &chunk).unwrap();
        expected.extend_from_slice(&chunk);
        if index % 7 == 0 {
            writer.save().unwrap();
            let record = partial.recorded().unwrap();
            assert_eq!(record.received, u64::try_from(expected.len()).unwrap());
            let received = usize::try_from(record.received).unwrap();
            let on_disk = fs::read(&partial.bytes).unwrap();
            assert_eq!(record.sha256, digest::sha256(&on_disk[..received]));
            assert_eq!(record.sha256, digest::sha256(&expected));
            assert!(record.ranges.is_empty());
        }
    }
    assert_eq!(writer.finish().unwrap().digest, digest::sha256(&expected));
}

#[test]
fn pipelined_chunks_resume_with_sha256_and_md5() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/pipelined-resume");
    let mut writer = partial
        .writer(0, None, Validators::default(), true, false)
        .unwrap();
    let first = Bytes::from(vec![b'a'; 128 * 1024]);
    let last = Bytes::from(vec![b'b'; 128 * 1024]);
    let mut expected = Vec::new();
    for _ in 0..40 {
        writer.write_chunk(writer.written(), &first).unwrap();
        expected.extend_from_slice(&first);
    }
    writer.save().unwrap();
    writer.write_chunk(writer.written(), &last).unwrap();
    writer.flush().unwrap();
    drop(writer);

    let (received, validators) = partial.resumable().unwrap();
    assert_eq!(received, u64::try_from(expected.len()).unwrap());
    let mut writer = partial
        .writer(received, None, validators, true, false)
        .unwrap();
    for _ in 0..40 {
        writer.write_chunk(writer.written(), &last).unwrap();
        expected.extend_from_slice(&last);
    }
    let mut hashes = Hasher::with_md5();
    hashes.update(&expected);
    let stored = writer.finish().unwrap();
    assert_eq!((stored.digest, stored.md5), hashes.finish());
    assert_eq!(
        fs::read(cache.object_path(&stored.digest)).unwrap(),
        expected
    );
}

#[test]
fn a_split_checkpoint_waits_for_read_back_hashing() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/pipelined-split");
    let mut writer = split_writer(&partial);
    writer.write_chunk(8, &Bytes::from_static(b"ijkl")).unwrap();
    writer.write_chunk(4, &Bytes::from_static(b"efgh")).unwrap();
    writer.save().unwrap();
    let record = partial.recorded().unwrap();
    assert_eq!(record.received, 0);
    assert_eq!(record.sha256, digest::sha256(b""));
    assert_eq!(record.ranges.len(), 2);
    writer.write_chunk(0, &Bytes::from_static(b"abcd")).unwrap();
    writer.save().unwrap();
    let record = partial.recorded().unwrap();
    assert_eq!(record.received, 12);
    assert_eq!(record.sha256, digest::sha256(b"abcdefghijkl"));
    assert!(record.ranges.is_empty());
    assert_eq!(writer.read_back(), 8);
    assert_eq!(writer.finish().unwrap().digest, record.sha256);
}

#[test]
fn pipelined_write_errors_preserve_the_last_checkpoint() {
    for size in [3, BUFFER] {
        let (_dir, cache) = cache();
        let partial = owned(&cache, "https://example.org/write-error");
        let mut writer = partial
            .writer(0, None, Validators::default(), false, false)
            .unwrap();
        writer.write_all(b"abc").unwrap();
        writer.save().unwrap();
        writer.file = File::open(&partial.bytes).unwrap();
        let refused = write_all_at(&writer.file, 3, b"x").unwrap_err();
        let chunk = Bytes::from(vec![b'x'; size]);
        let error = writer
            .write_chunk(3, &chunk)
            .and_then(|()| writer.save())
            .unwrap_err();
        assert_eq!(error.kind(), refused.kind());
        assert_eq!(error.raw_os_error(), refused.raw_os_error());
        let record = partial.recorded().unwrap();
        assert_eq!(record.received, 3);
        assert_eq!(record.sha256, digest::sha256(b"abc"));
        assert!(cache.objects().is_empty());
    }
}

#[test]
fn read_back_hashing_errors_prevent_checkpoints_and_publication() {
    for checkpoint in [true, false] {
        let (_dir, cache) = cache();
        let partial = owned(&cache, "https://example.org/hash-error");
        let mut writer = split_writer(&partial);
        writer.write_chunk(0, &Bytes::from_static(b"abc")).unwrap();
        writer.save().unwrap();
        writer.write_chunk(6, &Bytes::from_static(b"ghi")).unwrap();
        writer.file.set_len(6).unwrap();
        let result = writer.write_chunk(3, &Bytes::from_static(b"def"));
        let error = result
            .and_then(|()| {
                if checkpoint {
                    writer.save()
                } else {
                    writer.finish().map(|_| ())
                }
            })
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        let record = partial.recorded().unwrap();
        assert_eq!(record.received, 3);
        assert_eq!(record.sha256, digest::sha256(b"abc"));
        assert!(cache.objects().is_empty());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn pipelined_writes_preserve_full_disk_errors() {
    let (_dir, cache) = cache();
    let partial = owned(&cache, "https://example.org/full-disk");
    let mut writer = partial
        .writer(0, None, Validators::default(), false, false)
        .unwrap();
    writer.file = OpenOptions::new().write(true).open("/dev/full").unwrap();
    let error = writer
        .write_chunk(0, &Bytes::from(vec![0; BUFFER]))
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    assert_eq!(writer.written(), 0);
    assert!(partial.recorded().is_none());
    assert!(cache.objects().is_empty());
    assert_eq!(
        crate::error::advice(&error),
        Some("free some space on that disk, then run the command again")
    );
}
