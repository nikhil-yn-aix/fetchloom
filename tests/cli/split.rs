//! a large download starts as one stream and splits across several only once that stream proves
//! slow.

use std::fs;
use std::process::Stdio;

use crate::support::{Ranged, Result, Sandbox, json, partial_files, text, wait_until};

/// the smallest file that may split, 64 MiB.
const LARGE: u64 = 64 * 1024 * 1024;
/// what each connection gets from a slow server: below the 1 MB/s under which one stream splits.
const SLOW: u64 = 768 * 1024;

/// a file of [`LARGE`] bytes whose every 4 bytes count up, so a byte in the wrong place changes
/// its hash.
fn large() -> Vec<u8> {
    (0..16 * 1024 * 1024u32)
        .flat_map(u32::to_le_bytes)
        .collect()
}

#[test]
fn a_slow_large_download_splits_across_streams() -> Result {
    let body = large();
    let server = Ranged::start("/large.bin", body.clone(), SLOW)?;
    let sandbox = Sandbox::new()?;
    let mut child = sandbox
        .command(&["--json", "get", &server.url])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let split = wait_until("concurrent range requests", || {
        if child.try_wait()?.is_some() {
            return Err("the download ended before splitting".into());
        }
        Ok(server.most() >= 2 && !server.parts().is_empty())
    });
    // only the adaptive handover needs a slow stream; verify the remaining bytes at full speed.
    server.pace(0);
    let output = child.wait_with_output()?;
    split?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = &json(&output)?["work"];
    let requests = server.requests();
    assert!(!requests[0].contains("range:"), "{}", requests[0]);
    assert_eq!(
        requests.len() - server.parts().len(),
        1,
        "the first answer goes on as one of the streams"
    );
    assert!(server.most() >= 2, "{} streams at once", server.most());
    for part in &server.parts() {
        assert!(part.contains("if-range: \"v1\""), "{part}");
    }
    assert_eq!(work["requests"], requests.len(), "every stream is counted");
    assert_eq!(work["resumes"], 0);
    assert_eq!(
        fs::read(sandbox.work().join("large").join("large.bin"))?,
        body
    );
    Ok(())
}

#[test]
fn a_fast_large_download_stays_one_stream() -> Result {
    let body = large();
    let server = Ranged::start("/large.bin", body.clone(), 0)?;
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = &json(&output)?["work"];
    assert_eq!(work["requests"], 1);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(work["bytes_down"], LARGE);
    assert_eq!(
        fs::read(sandbox.work().join("large").join("large.bin"))?,
        body
    );
    Ok(())
}

#[test]
fn a_download_killed_while_split_continues_from_what_it_kept() -> Result {
    let body = large();
    let server = Ranged::start("/large.bin", body.clone(), SLOW)?;
    let sandbox = Sandbox::new()?;
    let mut child = sandbox
        .command(&["--json", "get", &server.url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let checkpoint = wait_until("a checkpoint with ranges past the prefix", || {
        if child.try_wait()?.is_some() {
            return Err("the first download ended before its split checkpoint".into());
        }
        Ok(!server.parts().is_empty() && kept(&sandbox).is_some())
    });
    let ended = child.try_wait()?;
    child.kill()?;
    child.wait()?;
    checkpoint?;
    assert!(
        ended.is_none() && !server.parts().is_empty(),
        "the first run ended or never split before the kill"
    );
    let (prefix, ranges) = kept(&sandbox).ok_or("the killed run's record lost its ranges")?;

    server.pace(0);
    let before = server.requests().len();
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = &json(&output)?["work"];
    assert_eq!(work["resumes"], 1, "{work}");
    let requests = &server.requests()[before..];
    assert_eq!(
        asked(&requests[0]),
        Some((prefix, None)),
        "the second run did not continue from the prefix kept: {}",
        requests[0]
    );
    assert!(requests[0].contains("if-range: \"v1\""), "{}", requests[0]);
    // bytes_down cannot show this: the first answer runs on into the first range kept until it
    // is dropped, and the chunk read then can be larger than that range
    let parts: Vec<_> = requests[1..]
        .iter()
        .map(|head| asked(head).unwrap_or((0, None)))
        .collect();
    for (start, end) in ranges {
        assert!(
            parts
                .iter()
                .all(|&(from, to)| from >= end || to.is_some_and(|to| to <= start)),
            "bytes {start} to {end} were kept, and asked for again: {parts:?}"
        );
        assert!(
            end == LARGE || parts.iter().any(|&(from, _)| from == end),
            "the second run did not skip the bytes {start} to {end} it kept: {parts:?}"
        );
    }
    assert_eq!(
        fs::read(sandbox.work().join("large").join("large.bin"))?,
        body
    );
    Ok(())
}

/// the prefix and the ranges past it that the download's record on disk keeps, ranges that touch
/// joined. none unless it keeps both.
fn kept(sandbox: &Sandbox) -> Option<(u64, Vec<(u64, u64)>)> {
    let record = partial_files(&sandbox.cache())
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| fs::read(path).ok())
        .find_map(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())?;
    let prefix = record["received"].as_u64().filter(|&prefix| prefix > 0)?;
    let mut ranges: Vec<(u64, u64)> = record["ranges"]
        .as_array()?
        .iter()
        .filter_map(|range| Some((range["start"].as_u64()?, range["end"].as_u64()?)))
        .filter(|&(start, end)| start > prefix && end > start)
        .collect();
    ranges.sort_unstable();
    let mut joined: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        match joined.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => joined.push((start, end)),
        }
    }
    (!joined.is_empty()).then_some((prefix, joined))
}

/// the bytes a request's `Range` asks for: where they start, and where they end unless they run
/// to the end of the file.
fn asked(head: &str) -> Option<(u64, Option<u64>)> {
    let range = head
        .lines()
        .find_map(|line| line.strip_prefix("range: bytes="))?;
    let (start, end) = range.trim().split_once('-')?;
    let end = match end {
        "" => None,
        end => Some(end.parse::<u64>().ok()?.checked_add(1)?),
    };
    Some((start.parse().ok()?, end))
}

#[test]
fn a_date_that_does_not_distinguish_versions_is_not_resumed() -> Result {
    check_a_replacement_is_whole(
        "Date: Wed, 21 Oct 2015 07:28:00 GMT\r\nLast-Modified: Wed, 21 Oct 2015 07:28:00 GMT\r\n"
            .to_owned(),
    )
}

/// a cut download whose tag cannot prove its version starts over whole through the binary.
/// `net::headers::tests::malformed_entity_tags_cannot_prove_a_version` holds every malformed tag;
/// an unquoted one and a weak one stand for them here.
#[test]
fn malformed_tags_cannot_splice_versions() -> Result {
    for tag in ["v1", "w/\"v1\""] {
        check_a_replacement_is_whole(format!("ETag: {tag}\r\n"))?;
    }
    Ok(())
}

fn check_a_replacement_is_whole(validators: String) -> Result {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let url = format!("http://{}/payload.bin", listener.local_addr()?);
    let serving = std::thread::spawn(move || -> std::io::Result<Vec<String>> {
        let mut asked = Vec::new();
        for attempt in 0..2 {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(10)))?;
            let mut head = Vec::new();
            let mut byte = [0; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte)? == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
            let (status, range, length, body) = if attempt == 0 {
                ("200 OK", "", 8, "aaaa")
            } else if head.contains("range:") {
                (
                    "206 Partial Content",
                    "Content-Range: bytes 4-7/8\r\n",
                    4,
                    "BBBB",
                )
            } else {
                ("200 OK", "", 8, "BBBBBBBB")
            };
            let response = format!(
                "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Length: {length}\r\n{range}{validators}\r\n{body}"
            );
            asked.push(head);
            stream.write_all(response.as_bytes())?;
        }
        Ok(asked)
    });
    let sandbox = Sandbox::new()?;
    let output = sandbox.run(&["--json", "get", &url, "--no-extract", "--dir", "out"])?;
    let asked = serving
        .join()
        .map_err(|_| "the replacement server failed")??;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    assert_eq!(
        fs::read(sandbox.work().join("out/payload.bin"))?,
        b"BBBBBBBB"
    );
    assert!(!asked[1].contains("range:"), "{}", asked[1]);
    assert_eq!(json(&output)?["work"]["resumes"], 0);
    Ok(())
}
