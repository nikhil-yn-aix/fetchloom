//! what a run says while it waits when stderr is no terminal, as in a log or a pipe.

use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use crate::support::{Part, Result, Running, Sandbox, Server, reply, text};

const IRIS: &[u8] = b"sepal_length,species\n5.1,setosa\n";

/// a reply that sends its head at once and its body once the sender it returns is dropped, so
/// a download lasts until the test lets it end.
fn held_iris() -> (Vec<Part>, Sender<()>) {
    let mut held = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    let (release, waiting) = mpsc::channel();
    held.push(Part::Wait(waiting));
    held.push(Part::Bytes(IRIS.to_vec()));
    (held, release)
}

/// a reply that holds back its body for 3 s, past the moment a batch says what it is doing.
fn slow_iris() -> Vec<Part> {
    let mut slow = reply(
        "200 OK",
        &[
            ("Content-Length", &IRIS.len().to_string()),
            ("ETag", "\"v1\""),
        ],
        b"",
    );
    slow.push(Part::Pause(Duration::from_millis(3000)));
    slow.push(Part::Bytes(IRIS.to_vec()));
    slow
}

/// the sandbox turns off the lines a batch tells as time passes, so a run prints the same
/// lines on a busy machine as on an idle one, and a snapshot of it holds.
#[test]
fn tests_see_no_line_that_time_decides() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![slow_iris()])?;
    let output = sandbox.run(&["add", &server.url])?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(!stderr.contains("Downloading iris.csv"), "{stderr}");
    Ok(())
}

#[test]
fn a_long_download_says_what_it_is_doing_once() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start("/files/iris.csv", vec![slow_iris()])?;
    let output = sandbox
        .command(&["add", &server.url])
        .env("FETCHLOOM_TEST_TICKER", "on")
        .output()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    // a loaded machine can take past 10 s, which adds lines of how far it got; they come after
    let started: Vec<&str> = stderr
        .lines()
        .filter(|line| line.starts_with("Downloading iris.csv"))
        .collect();
    assert_eq!(started, ["Downloading iris.csv · 32 B"], "{stderr}");
    assert!(
        stderr.find("Downloading") < stderr.find("Downloaded"),
        "{stderr}"
    );
    Ok(())
}

/// `--json` alone keeps stderr empty, and `--progress` beside it keeps the plain lines a long
/// download tells there, while stdout holds the one JSON document either way. the run without
/// the flag starts its download first, so it has run longer when the other tells its first line.
#[test]
fn progress_tells_its_lines_beside_json() -> Result {
    let (quiet_reply, release_quiet) = held_iris();
    let (told_reply, release_told) = held_iris();
    let quiet_server = Server::start("/files/iris.csv", vec![quiet_reply])?;
    let told_server = Server::start("/files/iris.csv", vec![told_reply])?;
    let (quiet_sandbox, told_sandbox) = (Sandbox::new()?, Sandbox::new()?);
    let quiet = Running::start(
        quiet_sandbox
            .command(&["--json", "get", &quiet_server.url])
            .env("FETCHLOOM_TEST_TICKER", "on"),
    )?;
    quiet_server.wait_for_requests(1)?;
    let told = Running::start(
        told_sandbox
            .command(&["--json", "--progress", "get", &told_server.url])
            .env("FETCHLOOM_TEST_TICKER", "on"),
    )?;
    let telling = told.wait_for("Downloading iris.csv");
    drop((release_quiet, release_told));
    telling?;
    let (quiet, told) = (quiet.finish()?, told.finish()?);
    for output in [&quiet, &told] {
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout)?);
        let document: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(document["status"], "ok", "{document}");
    }
    assert_eq!(text(&quiet.stderr)?, "");
    let stderr = text(&told.stderr)?;
    assert_eq!(
        stderr.lines().next(),
        Some("Downloading iris.csv · 32 B"),
        "{stderr}"
    );
    // a loaded machine can take past 10 s, which adds lines of how far it got
    assert!(
        stderr.lines().all(|line| {
            let line = line.trim_start();
            line.starts_with("Downloading ") || line.starts_with("Waiting for the disk")
        }),
        "{stderr}"
    );
    Ok(())
}

/// the lines a terminal `columns` wide shows once it has drawn `text`, as from `script`: each
/// `\n` starts a line, and a character past the last column moves to the next line first.
#[cfg(target_os = "linux")]
fn shown_on(text: &str, columns: usize) -> Vec<String> {
    let mut lines: Vec<Vec<char>> = vec![Vec::new()];
    let (mut row, mut column, mut wrapping) = (0_usize, 0_usize, false);
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '\n' => (row, column, wrapping) = (row + 1, 0, false),
            '\r' => (column, wrapping) = (0, false),
            '\u{1b}' if characters.peek() == Some(&'[') => {
                characters.next();
                let mut number = String::new();
                let mut command = ' ';
                for next in characters.by_ref() {
                    if next.is_ascii_alphabetic() {
                        command = next;
                        break;
                    }
                    number.push(next);
                }
                let count = number.trim_start_matches('?').parse().unwrap_or(1).max(1);
                wrapping = false;
                match command {
                    'A' => row = row.saturating_sub(count),
                    'B' => row += count,
                    'C' => column = (column + count).min(columns - 1),
                    'D' => column = column.saturating_sub(count),
                    'K' if number == "2" => lines.get_mut(row).map_or((), Vec::clear),
                    'K' => lines.get_mut(row).map_or((), |line| line.truncate(column)),
                    _ => {}
                }
            }
            '\u{1b}' => {
                // an operating system command, such as a link, ends at a bell or `ESC \`
                while let Some(next) = characters.next() {
                    if next == '\u{7}' || (next == '\u{1b}' && characters.next() == Some('\\')) {
                        break;
                    }
                }
            }
            control if control.is_control() => {}
            printed => {
                if wrapping {
                    (row, column, wrapping) = (row + 1, 0, false);
                }
                while lines.len() <= row {
                    lines.push(Vec::new());
                }
                let line = &mut lines[row];
                while line.len() <= column {
                    line.push(' ');
                }
                line[column] = printed;
                if column + 1 == columns {
                    wrapping = true;
                } else {
                    column += 1;
                }
            }
        }
        while lines.len() <= row {
            lines.push(Vec::new());
        }
    }
    lines
        .into_iter()
        .map(|line| line.into_iter().collect::<String>().trim_end().to_owned())
        .collect()
}

/// a stop leaves no frame of a download's bar behind on a terminal that echoes the ctrl-c, as
/// most do: a bar drawn to the last column once left the cursor waiting to wrap, the echoed ^C
/// wrapped onto a line of its own, and clearing the bar cleared that line instead.
#[cfg(target_os = "linux")]
#[test]
fn a_stop_leaves_no_bar_behind() -> Result {
    let body: Vec<u8> = (0..1u32 << 20).map(|n| (n % 251) as u8).collect();
    for columns in [100, 40] {
        let sandbox = Sandbox::new()?;
        let server = Server::held_file("/files/big.bin", body.clone())?;
        let get = sandbox.command(&["get", &server.url]);
        let size = format!("rows 30 cols {columns}");
        let Some((code, screen)) =
            crate::support::typed(&sandbox.work(), &get, &size, "Downloading", "\u{3}")?
        else {
            return Ok(());
        };
        assert_eq!(code, Some(130), "{screen:?}");
        let shown = shown_on(&screen, columns);
        assert!(
            shown.iter().any(|line| line.contains("Stopping")),
            "{columns}: {shown:#?}"
        );
        assert!(
            !shown.iter().any(|line| line.contains("Downloading")),
            "{columns}: {shown:#?}\n{screen:?}"
        );
    }
    Ok(())
}

/// an add of `IRIS` from `server` in a fresh folder whose download holds in its flush, as on a
/// slow disk, until the file `release` appears, once every byte is in.
#[cfg(unix)]
fn add_held_in_flush(
    sandbox: &Sandbox,
    server: &Server,
    release: &std::path::Path,
) -> Result<Running> {
    let mut add = sandbox.command(&["add", &server.url, "--as", "iris"]);
    add.env("FETCHLOOM_TEST_HOLD_FLUSH", release);
    let running = Running::start(&mut add)?;
    let mut held = release.as_os_str().to_owned();
    held.push(".held");
    crate::support::wait_until("the flush to hold", || {
        Ok(std::path::Path::new(&held).exists())
    })?;
    Ok(running)
}

/// a ctrl-c once every byte of a download is in, while the disk keeps the file, lets the add
/// finish rather than stop and download it all again next time, and says so first.
#[cfg(unix)]
#[test]
fn a_ctrl_c_while_the_disk_keeps_a_finished_download_lets_the_add_finish() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![crate::support::file(IRIS, "\"v1\"")],
    )?;
    let release = sandbox.folder("hold")?.join("release");
    let running = add_held_in_flush(&sandbox, &server, &release)?;
    running.interrupt()?;
    running.wait_for("Finishing")?;
    std::fs::write(&release, b"")?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(
        stderr.contains("  Finishing (the download is complete; ctrl-c again to quit at once)\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("      Added iris, its files are in data/iris"),
        "{stderr}"
    );
    assert!(!stderr.contains("Stop"), "{stderr}");
    assert_eq!(
        std::fs::read(sandbox.work().join("data/iris/iris.csv"))?,
        IRIS
    );
    Ok(())
}

/// a second ctrl-c quits at once, while a thread still waits for the disk to keep a download,
/// says so in one line, and a first add quit this way leaves its folder as it was.
#[cfg(unix)]
#[test]
fn a_second_ctrl_c_quits_at_once_and_leaves_a_fresh_folder_as_it_was() -> Result {
    let sandbox = Sandbox::new()?;
    let server = Server::start(
        "/files/iris.csv",
        vec![crate::support::file(IRIS, "\"v1\"")],
    )?;
    let release = sandbox.folder("hold")?.join("release");
    let running = add_held_in_flush(&sandbox, &server, &release)?;
    // only keeps a run that waits for the flush from holding the test for good
    let deadline = release.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(30));
        let _ = std::fs::write(deadline, b"");
    });
    running.interrupt()?;
    running.wait_for("Finishing")?;
    running.interrupt()?;
    let output = running.finish()?;
    let stderr = text(&output.stderr)?;
    assert!(!release.exists(), "the run waited for the flush: {stderr}");
    assert_eq!(output.status.code(), Some(130), "{stderr}");
    assert!(stderr.ends_with("       Quit at once\n"), "{stderr}");
    let left: Vec<_> = std::fs::read_dir(sandbox.work())?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<std::io::Result<_>>()?;
    assert!(left.is_empty(), "{left:?}");
    Ok(())
}

/// a download a server rate limits says on its bar which try comes next and when, as it waits
/// for the limit to reset, in one line that changes in place rather than a line per wait, and
/// never that no data came, since the server answered each try at once.
#[cfg(target_os = "linux")]
#[test]
fn a_rate_limited_download_counts_its_tries_on_its_bar() -> Result {
    let sandbox = Sandbox::new()?;
    let limited = || {
        reply(
            "429 Too Many Requests",
            &[("Retry-After", "1"), ("Content-Length", "0")],
            b"",
        )
    };
    let server = Server::start(
        "/files/iris.csv",
        vec![limited(), limited(), crate::support::file(IRIS, "\"v1\"")],
    )?;
    let get = sandbox.command(&["get", &server.url]);
    let Some((code, screen)) =
        crate::support::typed(&sandbox.work(), &get, "rows 30 cols 120", "", "")?
    else {
        return Ok(());
    };
    assert_eq!(code, Some(0), "{screen:?}");
    assert!(
        screen.contains("waiting for 127.0.0.1's rate limit · try 3 of 15 · next in 1 s"),
        "{screen:?}"
    );
    assert!(!screen.contains("no data for"), "{screen:?}");
    assert_eq!(
        screen.matches("which asked for a pause").count(),
        1,
        "{screen:?}"
    );
    Ok(())
}
