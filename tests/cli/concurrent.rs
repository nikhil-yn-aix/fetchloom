//! project-lock handoff and independent projects sharing one cache.

use std::fs;

use crate::support::{Part, Result, Running, Sandbox, Server, file, reply, text, wait_until};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";
const PATH: &str = "/files/iris.csv";

#[test]
fn path_waits_for_a_running_sync_and_answers() -> Result {
    let sandbox = Sandbox::new()?;
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    let (release, held) = std::sync::mpsc::channel();
    slow.push(Part::Wait(held));
    slow.push(Part::Bytes(IRIS.to_vec()));
    let server = Server::start(PATH, vec![file(IRIS, "\"v1\""), slow])?;
    let added = sandbox.run(&["add", &server.url])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let clone = sandbox.folder("clone")?;
    for name in ["data.toml", "data.lock"] {
        fs::copy(sandbox.work().join(name), clone.join(name))?;
    }
    let cold = sandbox.folder("cold")?;
    let syncing = Running::start(
        sandbox
            .command_in(&clone, &["sync"])
            .env("FETCHLOOM_CACHE", &cold),
    )?;
    wait_until("the sync download request", || {
        Ok(server.requests().len() >= 2)
    })?;
    let path = Running::start(
        sandbox
            .command_in(&clone, &["path", "iris"])
            .env("FETCHLOOM_CACHE", &cold),
    )?;
    let waiting = path.wait_for("Waiting");
    drop(release);
    waiting?;
    let path = path.finish()?;
    let synced = syncing.finish()?;
    assert_eq!(synced.status.code(), Some(0), "{}", text(&synced.stderr)?);
    let story = text(&path.stderr)?;
    assert_eq!(path.status.code(), Some(0), "{story}");
    assert_eq!(
        text(&path.stdout)?.trim(),
        clone.join("data").join("iris").display().to_string()
    );
    assert!(
        story.contains("Waiting") && story.lines().all(|line| line.contains("Waiting")),
        "only the wait is told: {story}"
    );
    Ok(())
}

struct Syncing {
    child: Option<std::process::Child>,
    readers: Vec<std::thread::JoinHandle<std::io::Result<Vec<u8>>>>,
    closed: std::sync::mpsc::Receiver<()>,
}

impl Syncing {
    fn start(command: &mut std::process::Command) -> Result<Self> {
        use std::io::Read as _;
        use std::process::Stdio;

        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let pipes: [Box<dyn std::io::Read + Send>; 2] = [
            Box::new(child.stdout.take().ok_or("missing child stdout")?),
            Box::new(child.stderr.take().ok_or("missing child stderr")?),
        ];
        let (done, closed) = std::sync::mpsc::channel();
        let readers = pipes
            .into_iter()
            .map(|mut pipe| {
                let done = done.clone();
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let result = pipe.read_to_end(&mut bytes).map(|_| bytes);
                    let _ = done.send(());
                    result
                })
            })
            .collect();
        Ok(Self {
            child: Some(child),
            readers,
            closed,
        })
    }

    fn finish(mut self) -> Result<std::process::Output> {
        for _ in 0..2 {
            self.closed
                .recv_timeout(std::time::Duration::from_secs(30))
                .map_err(|error| format!("shared-cache sync did not finish: {error}"))?;
        }
        let status = self.child.as_mut().ok_or("missing sync child")?.wait()?;
        self.child = None;
        let mut output = self
            .readers
            .drain(..)
            .map(|reader| {
                reader
                    .join()
                    .map_err(|_| std::io::Error::other("output reader panicked"))?
            })
            .collect::<std::io::Result<Vec<_>>>()?;
        let stderr = output.pop().ok_or("missing stderr")?;
        let stdout = output.pop().ok_or("missing stdout")?;
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }
}

impl Drop for Syncing {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    use std::fmt::Write as _;
    let mut text = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

fn shared_cache_syncs(processes: usize, overlapping: bool, warm: bool) -> Result {
    for switches in [
        &[][..],
        &[
            "CACHE_LEAN_PARTIAL",
            "CACHE_LOCAL_FSYNC",
            "CACHE_RENAME_NOREPLACE",
        ][..],
    ] {
        check_shared_cache_syncs(processes, overlapping, warm, switches)?;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "one fixture owns the gated servers, children, and byte checks"
)]
fn check_shared_cache_syncs(
    processes: usize,
    overlapping: bool,
    warm: bool,
    switches: &[&str],
) -> Result {
    use std::fmt::Write as _;

    let sandbox = Sandbox::new()?;
    let original = Server::start("/iris.csv", vec![file(IRIS, "\"v1\"")])?;
    let added = sandbox.run(&["add", &original.url])?;
    assert!(added.status.success(), "{}", text(&added.stderr)?);
    let locked = fs::read_to_string(sandbox.work().join("data.lock"))?;
    let (_, template) = locked.split_once("[[dataset]]").ok_or("missing dataset")?;
    let parsed: toml::Value = toml::from_str(&locked)?;
    let original_digest = parsed["dataset"][0]["files"][0]["sha256"]
        .as_str()
        .ok_or("missing digest")?;
    let cache = sandbox.folder("shared-cache")?;
    let count = processes * if overlapping { 3 } else { 2 };
    let bodies: Vec<_> = (0..count)
        .map(|index| format!("object-{index:04}\n").into_bytes())
        .collect();
    let (claimed, claims) = std::sync::mpsc::channel();
    let mut releases = Vec::new();
    let mut servers = Vec::new();
    for (index, body) in bodies.iter().enumerate() {
        let mut response = Vec::new();
        if !warm && index < processes * 2 && index % 2 == 0 {
            let claimed = claimed.clone();
            let (release, held) = std::sync::mpsc::channel();
            response.push(Part::Action(Box::new(move || {
                let _ = claimed.send(index);
            })));
            response.push(Part::Wait(held));
            releases.push(release);
        }
        response.extend(file(body, "\"v1\""));
        servers.push(Server::start(&format!("/part{index}.csv"), vec![response])?);
        if warm {
            let digest = sha256(body);
            let object = cache.join("objects").join(&digest[..2]).join(&digest[2..]);
            fs::create_dir_all(object.parent().ok_or("object needs a parent")?)?;
            fs::write(&object, body)?;
            let mut permissions = fs::metadata(&object)?.permissions();
            permissions.set_readonly(true);
            fs::set_permissions(object, permissions)?;
        }
    }
    drop(claimed);
    let mut projects = Vec::new();
    let mut syncing = Vec::new();
    for index in 0..processes {
        let project = sandbox.folder(&format!("project{index}"))?;
        let order: Vec<_> = if overlapping {
            let next = (index + 1) % processes;
            vec![
                index * 2,
                index * 2 + 1,
                next * 2,
                next * 2 + 1,
                processes * 2 + index,
            ]
        } else {
            (0..processes * 2)
                .map(|slot| (index * 2 + slot) % (processes * 2))
                .collect()
        };
        let mut manifest = String::new();
        let mut lock = "version = 1\n".to_owned();
        for (slot, &body) in order.iter().enumerate() {
            let name = format!("d{slot}");
            writeln!(manifest, "[datasets.{name}]\nref = {:?}", original.url)?;
            let entry = template
                .replace("name = \"iris\"", &format!("name = {name:?}"))
                .replace(
                    &format!("size = {}", IRIS.len()),
                    &format!("size = {}", bodies[body].len()),
                )
                .replace(original_digest, &sha256(&bodies[body]))
                .replace(
                    &format!("url = {:?}", original.url),
                    &format!("url = {:?}", servers[body].url),
                );
            write!(lock, "\n[[dataset]]{entry}")?;
        }
        fs::write(project.join("data.toml"), manifest)?;
        fs::write(project.join("data.lock"), lock)?;
        let mut command = sandbox.command_in(&project, &["--json", "sync"]);
        command
            .env("FETCHLOOM_JOBS", "1")
            .env("FETCHLOOM_THREADS", "1")
            .env("FETCHLOOM_CACHE", &cache);
        for switch in switches {
            command.env(format!("FETCHLOOM_EXP_{switch}"), "1");
        }
        syncing.push(Syncing::start(&mut command)?);
        projects.push((project, order));
    }
    if !warm {
        // every run owns a disjoint URL before any run can finish its shared set.
        let mut owned = std::collections::BTreeSet::new();
        for _ in 0..processes {
            owned.insert(claims.recv_timeout(std::time::Duration::from_secs(30))?);
        }
        assert_eq!(owned, (0..processes).map(|index| index * 2).collect());
    }
    drop(releases);
    for run in syncing {
        let output = run.finish()?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}\n{}",
            text(&output.stdout)?,
            text(&output.stderr)?
        );
    }
    for (project, order) in projects {
        for (slot, body) in order.into_iter().enumerate() {
            assert_eq!(
                fs::read(project.join(format!("data/d{slot}/iris.csv")))?,
                bodies[body]
            );
        }
    }
    for body in &bodies {
        let digest = sha256(body);
        let object = cache.join("objects").join(&digest[..2]).join(&digest[2..]);
        assert_eq!(fs::read(&object)?, *body);
        assert!(fs::metadata(object)?.permissions().readonly());
    }
    assert!(
        servers
            .iter()
            .all(|server| server.requests().len() == usize::from(!warm))
    );
    Ok(())
}

#[test]
fn cold_two_processes_share_the_same_urls() -> Result {
    shared_cache_syncs(2, false, false)
}
#[test]
fn cold_eight_processes_share_the_same_urls() -> Result {
    shared_cache_syncs(8, false, false)
}
#[test]
fn cold_two_processes_share_overlapping_urls() -> Result {
    shared_cache_syncs(2, true, false)
}
#[test]
fn cold_eight_processes_share_overlapping_urls() -> Result {
    shared_cache_syncs(8, true, false)
}
#[test]
fn warm_two_processes_share_the_same_urls() -> Result {
    shared_cache_syncs(2, false, true)
}
#[test]
fn warm_eight_processes_share_the_same_urls() -> Result {
    shared_cache_syncs(8, false, true)
}
#[test]
fn warm_two_processes_share_overlapping_urls() -> Result {
    shared_cache_syncs(2, true, true)
}
#[test]
fn warm_eight_processes_share_overlapping_urls() -> Result {
    shared_cache_syncs(8, true, true)
}
