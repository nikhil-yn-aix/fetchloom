//! a large download starts as one stream and splits across several only once that stream proves
//! slow.

use std::fs;
use std::process::Stdio;

use serde_json::Value;

use crate::support::{Ranged, Result, Sandbox, text, wait_until};

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

fn json(output: &std::process::Output) -> Result<Value> {
    Ok(serde_json::from_slice(&output.stdout)?)
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
        Ok(!server.parts().is_empty() && saved_ranges(&sandbox))
    });
    let ended = child.try_wait()?;
    child.kill()?;
    child.wait()?;
    checkpoint?;
    assert!(
        ended.is_none() && !server.parts().is_empty(),
        "the first run ended or never split before the kill"
    );

    server.pace(0);
    let before = server.requests().len();
    let output = sandbox.run(&["--json", "get", &server.url])?;
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr)?);
    let work = &json(&output)?["work"];
    assert_eq!(work["resumes"], 1, "{work}");
    let asked = &server.requests()[before];
    let from = asked
        .lines()
        .find_map(|line| line.strip_prefix("range: bytes=")?.strip_suffix('-'))
        .and_then(|start| start.parse::<u64>().ok())
        .ok_or_else(|| format!("the second run did not continue: {asked}"))?;
    assert!(asked.contains("if-range: \"v1\""), "{asked}");
    let down = work["bytes_down"].as_u64().unwrap_or(LARGE);
    assert!(
        from > 0 && down < LARGE - from,
        "the second run continued from {from} and downloaded {down} bytes, so the ranges past it \
         came again"
    );
    assert_eq!(
        fs::read(sandbox.work().join("large").join("large.bin"))?,
        body
    );
    Ok(())
}

/// the on-disk receipt must prove there is work beyond the prefix for the next run to reuse.
fn saved_ranges(sandbox: &Sandbox) -> bool {
    fs::read_dir(sandbox.cache().join("partial"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .any(|record| {
            let received = record["received"].as_u64().unwrap_or_default();
            received > 0
                && record["ranges"].as_array().is_some_and(|ranges| {
                    ranges.iter().any(|range| {
                        let start = range["start"].as_u64().unwrap_or_default();
                        let end = range["end"].as_u64().unwrap_or_default();
                        start > received && end > start
                    })
                })
        })
}
