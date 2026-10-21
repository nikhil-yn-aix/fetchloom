//! A download in a real process killed halfway, then continued by another: only the missing
//! bytes are asked for, and the file comes out whole.

use std::error::Error;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use fl_transfer::client::Config;
use fl_transfer::credentials::Chain;
use fl_transfer::download::Download;
use fl_transfer::url::SafeUrl;

use crate::support::disk::{Disk, read_record};
use crate::support::memory::{Waits, client_with, quick};
use crate::support::{Body, Recorded, Reply, Server};

type TestResult = Result<(), Box<dyn Error>>;

const HELPER: &str = "killed::child";
const URL: &str = "FL_TRANSFER_URL";
const DIR: &str = "FL_TRANSFER_DIR";
const MIB: usize = 1 << 20;
const LENGTH: usize = 192 * MIB;
const STALL_AT: usize = 140 * MIB;
const WAIT: Duration = Duration::from_secs(120);

fn pattern(length: usize) -> Vec<u8> {
    let mut state = 0x9E37_79B9_7F4A_7C15_u64;
    let mut bytes = vec![0_u8; length];
    for chunk in bytes.as_chunks_mut::<8>().0 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *chunk = state.to_le_bytes();
    }
    bytes
}

fn job(url: &str) -> Result<Download, Box<dyn Error>> {
    Ok(Download {
        url: SafeUrl::parse(url)?,
        length: Some(LENGTH as u64),
        stated: Vec::new(),
        stable: true,
    })
}

fn patient() -> Config {
    Config {
        idle: Duration::from_secs(600),
        ..quick()
    }
}

#[test]
fn child() -> TestResult {
    let (Ok(url), Ok(dir)) = (std::env::var(URL), std::env::var(DIR)) else {
        return Ok(());
    };
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let client = client_with(patient(), Chain::new(Vec::new()), &Waits::default())?;
        client
            .download(&job(&url)?, Disk::open(Path::new(&dir))?)
            .await?;
        Ok::<_, Box<dyn Error>>(())
    })
}

fn from(request: &Recorded) -> Option<usize> {
    request
        .header("range")?
        .strip_prefix("bytes=")?
        .strip_suffix('-')?
        .parse()
        .ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_killed_download_continues_with_only_the_missing_bytes() -> TestResult {
    let bytes = Arc::new(pattern(LENGTH));
    let source = Arc::clone(&bytes);
    let server = Server::plain(move |request| match from(request) {
        Some(start) => Reply {
            status: 206,
            ..Reply::ok(source[start..].to_vec())
        }
        .header(
            "Content-Range",
            &format!("bytes {start}-{}/{LENGTH}", LENGTH - 1),
        ),
        None => Reply {
            body: Body::Stall {
                bytes: source.to_vec(),
                after: STALL_AT,
            },
            ..Reply::ok(Vec::new())
        },
    })
    .await?;
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let url = server.url("/big.bin");
    let mut child = Command::new(std::env::current_exe()?)
        .args([HELPER, "--exact", "--nocapture"])
        .env(URL, &url)
        .env(DIR, temp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let record = temp.path().join("record.json");
    let deadline = Instant::now() + WAIT;
    let kept = loop {
        if let Some(saved) = read_record(&record).filter(|saved| saved.received >= 128 << 20) {
            break saved.received;
        }
        if Instant::now() > deadline {
            child.kill()?;
            return Err("the child never saved its progress".into());
        }
        sleep(Duration::from_millis(10));
    };
    child.kill()?;
    child.wait()?;
    let client = client_with(patient(), Chain::new(Vec::new()), &Waits::default())?;
    let done = client
        .download(&job(&url)?, Disk::open(temp.path())?)
        .await?;
    let counted = client.counters().read();
    assert_eq!(counted.requests, 1);
    assert_eq!(counted.resumes, 1);
    assert_eq!(counted.bytes_down, LENGTH as u64 - kept);
    let last = server.requests().pop().ok_or("no request")?;
    assert_eq!(from(&last), usize::try_from(kept).ok());
    assert!(done.resumed);
    assert_eq!(done.hashes.blake3, *blake3::hash(&bytes).as_bytes());
    Ok(())
}
