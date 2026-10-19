//! Real operating system processes against one tier: racing ingests of the same bytes, and a
//! writer killed in the middle of a write.

use std::error::Error;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime};

use fl_model::digest::Digest;
use fl_store::fingerprint::hash_file;
use fl_store::gc::Collect;
use fl_store::tier::{Sharing, Tier};

type TestResult = Result<(), Box<dyn Error>>;

const HELPER: &str = "child";
const ROLE: &str = "FL_STORE_CHILD";
const TIER: &str = "FL_STORE_TIER";
const GO: &str = "FL_STORE_GO";
const SOURCE: &str = "FL_STORE_SOURCE";
const READY: &str = "FL_STORE_READY";
const SOURCE_BYTES: usize = 8 << 20;
const CHUNK: usize = 1 << 20;
const WRITTEN_BYTES: usize = 8 << 20;
const CHILDREN: usize = 4;
const INGESTS_PER_CHILD: usize = 3;
const WAIT: Duration = Duration::from_secs(120);
const HOUR: Duration = Duration::from_secs(3600);

fn variable(name: &str) -> Result<String, Box<dyn Error>> {
    Ok(std::env::var(name)?)
}

fn wait_for(path: &Path) -> TestResult {
    let deadline = Instant::now() + WAIT;
    while !path.exists() {
        if Instant::now() > deadline {
            return Err(format!("timed out waiting for {}", path.display()).into());
        }
        sleep(Duration::from_millis(1));
    }
    Ok(())
}

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

#[test]
fn child() -> TestResult {
    let Ok(role) = std::env::var(ROLE) else {
        return Ok(());
    };
    let tier = Tier::open(Path::new(&variable(TIER)?), Sharing::Private)?;
    wait_for(Path::new(&variable(GO)?))?;
    match role.as_str() {
        "ingest" => {
            let source = PathBuf::from(variable(SOURCE)?);
            for _ in 0..INGESTS_PER_CHILD {
                let ingested = tier.ingest_file(&source)?;
                writeln!(std::io::stdout(), "new={}", u8::from(ingested.new))?;
            }
            Ok(())
        }
        "writer" => {
            let mut staged = tier.stage(None)?;
            let chunk = pattern(CHUNK);
            for written in 0..WRITTEN_BYTES / CHUNK {
                staged.write_all(&chunk)?;
                if written == 0 {
                    File::create(variable(READY)?)?;
                }
            }
            loop {
                sleep(Duration::from_secs(1));
            }
        }
        other => Err(format!("unknown role {other}").into()),
    }
}

fn spawn(
    role: &str,
    tier: &Path,
    go: &Path,
    extra: &[(&str, &Path)],
) -> Result<Child, Box<dyn Error>> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([HELPER, "--exact", "--nocapture"])
        .env(ROLE, role)
        .env(TIER, tier)
        .env(GO, go)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (name, value) in extra {
        command.env(name, value);
    }
    Ok(command.spawn()?)
}

fn objects(tier: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let mut found = Vec::new();
    for fan in fs::read_dir(tier.join("v1").join("objects"))? {
        for object in fs::read_dir(fan?.path())? {
            found.push(object?.path());
        }
    }
    Ok(found)
}

fn entries(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    fs::read_dir(dir)?.map(|entry| Ok(entry?.path())).collect()
}

#[test]
fn racing_processes_ingest_the_same_bytes_once() -> TestResult {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let tier_dir = temp.path().join("tier");
    let tier = Tier::open(&tier_dir, Sharing::Private)?;
    let source = temp.path().join("source.bin");
    let bytes = pattern(SOURCE_BYTES);
    fs::write(&source, &bytes)?;
    let expected = Digest::Blake3(*blake3::hash(&bytes).as_bytes());
    let go = temp.path().join("go");
    let children = (0..CHILDREN)
        .map(|_| spawn("ingest", &tier_dir, &go, &[(SOURCE, &source)]))
        .collect::<Result<Vec<_>, _>>()?;
    File::create(&go)?;
    let mut new = 0;
    for child in children {
        let output = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success() {
            return Err(format!("a child failed: {stdout}").into());
        }
        let lines = stdout
            .lines()
            .filter_map(|line| line.strip_prefix("new="))
            .collect::<Vec<_>>();
        if lines.len() != INGESTS_PER_CHILD {
            return Err(format!("a child reported {} ingests: {stdout}", lines.len()).into());
        }
        new += lines.iter().filter(|line| **line == "1").count();
    }
    assert_eq!(new, 1, "exactly one ingest created the object");
    let found = objects(&tier_dir)?;
    assert_eq!(found.len(), 1);
    assert_eq!(hash_file(&found[0])?, expected);
    assert_eq!(
        tier.locate(&expected).map(|located| located.path),
        Some(found[0].clone())
    );
    assert!(fs::metadata(&found[0])?.permissions().readonly());
    assert!(entries(&tier_dir.join("v1").join("tmp"))?.is_empty());
    Ok(())
}

#[test]
fn a_killed_writer_leaves_no_object_and_gc_clears_its_tmp() -> TestResult {
    let temp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let tier_dir = temp.path().join("tier");
    let tier = Tier::open(&tier_dir, Sharing::Private)?;
    let go = temp.path().join("go");
    let ready = temp.path().join("ready");
    let mut child = spawn("writer", &tier_dir, &go, &[(READY, &ready)])?;
    File::create(&go)?;
    wait_for(&ready)?;
    child.kill()?;
    child.wait()?;
    assert!(objects(&tier_dir)?.is_empty());
    let staging = tier_dir.join("v1").join("tmp");
    let left = entries(&staging)?;
    assert_eq!(left.len(), 1);
    let within_grace = tier.collect(&Collect {
        now: SystemTime::now(),
        grace: HOUR,
        dry_run: false,
    })?;
    assert!(within_grace.removed.is_empty());
    assert_eq!(entries(&staging)?, left);
    let after = tier.collect(&Collect {
        now: SystemTime::now() + HOUR * 2,
        grace: HOUR,
        dry_run: false,
    })?;
    assert_eq!(after.removed, left);
    assert!(entries(&staging)?.is_empty());
    Ok(())
}
