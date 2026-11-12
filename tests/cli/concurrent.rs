//! project-lock handoff and independent projects sharing one cache.

use std::fs;

#[cfg(unix)]
use crate::support::wait_until;
use crate::support::{Part, Result, Running, Sandbox, Server, file, reply, sha256_hex, text};

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
    sandbox.allow(&server.url);
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
    server.wait_for_requests(2)?;
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

/// eight syncs of different projects run at once against one cache, as cluster jobs do. each
/// project needs files the others need too: every file (`overlapping` false) or its own and its
/// neighbour's (`overlapping` true). cold, each URL is downloaded once; warm, the cache already
/// holds every file and nothing is downloaded.
#[expect(
    clippy::too_many_lines,
    reason = "one fixture owns the gated servers, children, and byte checks"
)]
fn shared_cache_syncs(overlapping: bool, warm: bool) -> Result {
    use std::fmt::Write as _;

    let processes = 8;
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
        let mut responses = vec![response];
        responses.extend((1..processes).map(|_| file(body, "\"v1\"")));
        let server = Server::start(&format!("/part{index}.csv"), responses)?;
        sandbox.allow(&server.url);
        servers.push(server);
        if warm {
            let digest = sha256_hex(body);
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
                .replace(original_digest, &sha256_hex(&bodies[body]))
                .replace(
                    &format!("url = {:?}", original.url),
                    &format!("url = {:?}", servers[body].url),
                );
            write!(lock, "\n[[dataset]]{entry}")?;
        }
        fs::write(project.join("data.toml"), manifest)?;
        fs::write(project.join("data.lock"), lock)?;
        let mut command = sandbox.command_in(&project, &["--json", "-vv", "sync"]);
        command
            .env("FETCHLOOM_JOBS", "1")
            .env("FETCHLOOM_THREADS", "1")
            .env("FETCHLOOM_CACHE", &cache);
        syncing.push(Running::start(&mut command)?);
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
    let mut children = Vec::new();
    for run in syncing {
        let pid = run.id()?;
        children.push((pid, run.finish()?));
    }
    let requests: Vec<_> = servers
        .iter()
        .map(|server| (&server.url, server.requests()))
        .collect();
    if children.iter().any(|(_, output)| !output.status.success())
        || requests
            .iter()
            .any(|(_, requests)| requests.len() != usize::from(!warm))
    {
        let mut story = format!("warm={warm}, overlapping={overlapping}\nrequests={requests:#?}\n");
        for (index, (pid, output)) in children.iter().enumerate() {
            writeln!(
                story,
                "project {index} pid {pid} exit {:?}\n{}\n{}",
                output.status.code(),
                text(&output.stdout)?,
                text(&output.stderr)?
            )?;
        }
        return Err(story.into());
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
        let digest = sha256_hex(body);
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
fn processes_needing_the_same_files_download_each_once() -> Result {
    shared_cache_syncs(false, false)
}
#[test]
fn processes_sharing_some_files_download_each_once() -> Result {
    shared_cache_syncs(true, false)
}
#[test]
fn processes_place_from_a_full_cache_without_downloading() -> Result {
    shared_cache_syncs(false, true)
}

/// a push stopped with ctrl-c waits for its copies to see the stop and remove the unfinished ones
/// from the mirror's tmp/, where the run used to end first and leave them there for good, while a
/// file another push is writing there stays. the push holds each copy once its part is in tmp/,
/// as a debug build lets a test ask, so the stop lands mid-copy even on a disk that clones files.
#[cfg(unix)]
#[test]
fn a_stopped_push_leaves_no_unfinished_copy_in_the_mirror() -> Result {
    let sandbox = Sandbox::new()?;
    let inputs = sandbox.folder("inputs")?;
    for at in 0..3_u8 {
        fs::write(inputs.join(format!("f{at}.bin")), vec![at; 1 << 10])?;
    }
    let inputs = inputs.display().to_string();
    let added = sandbox.run(&["add", &inputs, "--as", "inputs", "--yes"])?;
    assert_eq!(added.status.code(), Some(0), "{}", text(&added.stderr)?);
    let mirror = sandbox.folder("mirror")?;
    let tmp = mirror.join("tmp");
    fs::create_dir_all(&tmp)?;
    let other = tmp.join(".0123456789abcdef.part");
    fs::write(&other, b"another push is writing this")?;
    let unfinished = || -> Result<usize> {
        let mut count = 0;
        for entry in fs::read_dir(&tmp)? {
            let path = entry?.path();
            if path != other && path.extension().is_some_and(|end| end == "part") {
                count += 1;
            }
        }
        Ok(count)
    };
    let mut command = sandbox.command(&["push", "--to", &mirror.display().to_string(), "--yes"]);
    // a file that never appears, so only the stop lets the copies go on
    command.env("FETCHLOOM_TEST_HOLD_PUSH", sandbox.work().join("never"));
    let mut pushing = Running::start(&mut command)?;
    let mut ended = false;
    wait_until("a copy held in the mirror's tmp/", || {
        ended = pushing.has_ended()?;
        Ok(ended || unfinished()? > 0)
    })?;
    assert!(!ended, "the push ended before it was stopped");
    pushing.interrupt()?;
    let output = pushing.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert_eq!(unfinished()?, 0, "{stderr}");
    assert!(other.exists(), "another push's file stays");
    Ok(())
}

/// two first adds in one empty folder: the second waits while the first holds the project, then
/// adds its dataset to the project the first started, so only the first says it started one.
#[test]
fn of_two_first_adds_only_the_one_that_starts_the_project_says_so() -> Result {
    let sandbox = Sandbox::new()?;
    let body = vec![b'a'; 1 << 20];
    let mut held = reply(
        "200 OK",
        &[
            ("Content-Length", &body.len().to_string()),
            ("ETag", "\"a1\""),
        ],
        &body[..1 << 16],
    );
    let (release, wait) = std::sync::mpsc::channel();
    held.push(Part::Wait(wait));
    held.push(Part::Bytes(body[1 << 16..].to_vec()));
    let first = Server::start("/files/a.csv", vec![held])?;
    let second = Server::start(PATH, vec![file(IRIS, "\"v1\"")])?;
    let adding = Running::start(&mut sandbox.command(&["add", &first.url]))?;
    // the first add holds the project once it asks for its file
    first.wait_for_requests(1)?;
    let waiting = Running::start(&mut sandbox.command(&["add", &second.url]))?;
    let told = waiting.wait_for("Waiting");
    drop(release);
    told?;
    let (first, second) = (adding.finish()?, waiting.finish()?);
    let stories = [text(&first.stderr)?, text(&second.stderr)?];
    assert_eq!(first.status.code(), Some(0), "{}", stories[0]);
    assert_eq!(second.status.code(), Some(0), "{}", stories[1]);
    let started: Vec<bool> = stories
        .iter()
        .map(|story| story.contains("Started a project"))
        .collect();
    assert_eq!(started, [true, false], "{stories:?}");
    let manifest = fs::read_to_string(sandbox.work().join("data.toml"))?;
    assert!(
        manifest.contains("[datasets.a]") && manifest.contains("[datasets.iris]"),
        "{manifest}"
    );
    Ok(())
}

/// j6-m4: a run that waits on no other names its finished downloads within a second, so a second
/// run that wants them does not wait for the first run's longest download. here the first run's
/// large file stays unfinished until the second run is done.
#[cfg(debug_assertions)]
#[test]
fn a_run_names_its_finished_downloads_while_its_largest_still_downloads() -> Result {
    use crate::buckets::{md5, s3_listing, s3_page};
    use crate::support::{Routes, wait_until};

    let sandbox = Sandbox::new()?;
    let keys: Vec<String> = (0..70).map(|index| format!("s/f{index:02}.bin")).collect();
    let bodies: Vec<Vec<u8>> = keys.iter().map(|key| key.repeat(40).into_bytes()).collect();
    let listed: Vec<(&str, &[u8])> = keys
        .iter()
        .zip(&bodies)
        .map(|(key, body)| (key.as_str(), body.as_slice()))
        .collect();
    let mut routes = vec![(s3_listing("/lab/", "s/", None), s3_page(&listed, None))];
    for (key, body) in keys.iter().zip(&bodies) {
        routes.push((
            format!("/lab/{key}"),
            file(body, &format!("\"{}\"", md5(body))),
        ));
    }
    let small = Routes::start(routes)?;
    let big = vec![b'b'; 64 * 1024];
    let mut held = reply(
        "200 OK",
        &[
            ("Content-Length", &big.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    let (release, wait) = std::sync::mpsc::channel();
    held.push(Part::Wait(wait));
    held.push(Part::Bytes(big.clone()));
    let large = Server::start("/big.bin", vec![file(&big, "\"v1\""), held])?;
    for url in [&small.url, &large.url] {
        sandbox.allow(url);
    }
    let first = sandbox.folder("first")?;
    let second = sandbox.folder("second")?;
    let shards = format!("[datasets.small]\nref = \"s3:{}/lab/s/\"\n", small.url);
    fs::write(
        first.join("data.toml"),
        format!("[datasets.big]\nref = \"{}\"\n\n{shards}", large.url),
    )?;
    fs::write(second.join("data.toml"), &shards)?;
    for project in [&first, &second] {
        let locked = sandbox.command_in(project, &["lock"]).output()?;
        assert_eq!(locked.status.code(), Some(0), "{}", text(&locked.stderr)?);
    }
    let asked = small.requests().len();
    let shared = sandbox.folder("shared")?;
    let syncing = Running::start(
        sandbox
            .command_in(&first, &["sync"])
            .env("FETCHLOOM_CACHE", &shared),
    )?;
    large.wait_for_requests(2)?;
    wait_until("the first run to have every small file", || {
        Ok(small.requests().len() == asked + keys.len())
    })?;
    let waiting = Running::start(
        sandbox
            .command_in(&second, &["sync"])
            .env("FETCHLOOM_CACHE", &shared),
    )?;
    let synced = waiting.wait_for("Synced");
    drop(release);
    let second_run = waiting.finish()?;
    let first_run = syncing.finish()?;
    synced?;
    assert_eq!(
        second_run.status.code(),
        Some(0),
        "{}",
        text(&second_run.stderr)?
    );
    assert_eq!(
        first_run.status.code(),
        Some(0),
        "{}",
        text(&first_run.stderr)?
    );
    assert_eq!(
        small.requests().len(),
        asked + keys.len(),
        "the second run downloaded nothing"
    );
    for (key, body) in keys.iter().zip(&bodies) {
        let name = key.trim_start_matches("s/");
        assert_eq!(fs::read(second.join("data/small").join(name))?, *body);
    }
    Ok(())
}
