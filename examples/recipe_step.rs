//! the step program fetchloom's tests put in recipes, with the standard library alone. its first
//! argument names what it does, the arguments of that mode follow, then the folder of each
//! dataset it reads, and its last argument is the folder its result goes in. every run adds a
//! line with its mode to `runs` in the folder it runs in, so a test can count its runs.
//!
//! | mode | does |
//! |---|---|
//! | `copy` | copies the files of each folder it reads into the result folder |
//! | `write <name> <text>` | writes `text` into the file `name` of the result folder |
//! | `random` | writes bytes that differ on every run |
//! | `fail <code>` | prints a few lines, writes a file and exits with `code` |
//! | `nothing` | writes nothing |
//! | `hold <marker> <release>` | makes the file `marker`, waits for `release`, then copies |
//! | `link` | writes a file and a link to it |
//! | `case` | writes `Data.csv` and `data.csv` |
//! | `say <bytes>` | prints `bytes` bytes of lines, then copies |
//! | `call <program>` | runs `program sync` and exits with its exit code |
//! | `write-code <file>` | adds a line to `file`, as a step that edits its own code, then copies |
//! | `leave-child <lock> <marker>` | starts a process that locks `lock`, waits until it made `marker`, then waits to be ended |
//! | `lock <lock> <marker>` | locks the file `lock`, makes `marker` and waits to be ended, as `leave-child` starts it |

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// how long `hold` and `call` wait before they give up, so a broken test ends.
const PATIENCE: Duration = Duration::from_secs(60);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            let _ = writeln!(io::stderr(), "recipe-step: {err}");
            ExitCode::from(101)
        }
    }
}

fn run(args: &[String]) -> io::Result<ExitCode> {
    let (mode, rest) = args
        .split_first()
        .ok_or_else(|| invalid("name a mode first"))?;
    let mut runs = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("runs")?;
    writeln!(runs, "{mode}")?;
    match (mode.as_str(), rest) {
        ("copy", folders) => copy(folders)?,
        ("write", [name, text, folders @ ..]) => fs::write(out(folders)?.join(name), text)?,
        ("random", folders) => {
            let seed = std::hash::BuildHasher::hash_one(
                &std::collections::hash_map::RandomState::new(),
                Instant::now(),
            );
            fs::write(out(folders)?.join("random.bin"), seed.to_le_bytes())?;
        }
        ("fail", [code, folders @ ..]) => {
            let code: u8 = code.parse().map_err(|_| invalid("fail takes a code"))?;
            for line in 1..=30 {
                writeln!(io::stdout(), "working on part {line}")?;
            }
            writeln!(io::stderr(), "something went wrong in part 30")?;
            fs::write(out(folders)?.join("half.csv"), "a,b\n")?;
            return Ok(ExitCode::from(code));
        }
        ("nothing", folders) => {
            out(folders)?;
        }
        ("hold", [marker, release, folders @ ..]) => {
            fs::write(marker, "started")?;
            let started = Instant::now();
            while !Path::new(release).exists() {
                if started.elapsed() > PATIENCE {
                    return Err(invalid("nothing released the hold"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            copy(folders)?;
        }
        ("link", folders) => link(out(folders)?)?,
        ("case", folders) => {
            let out = out(folders)?;
            fs::write(out.join("Data.csv"), "upper")?;
            fs::write(out.join("data.csv"), "lower")?;
        }
        ("say", [bytes, folders @ ..]) => {
            let bytes: usize = bytes.parse().map_err(|_| invalid("say takes a size"))?;
            let line = "a line of output from the step, to fill its log\n";
            let mut stdout = io::BufWriter::new(io::stdout().lock());
            for _ in 0..bytes.div_ceil(line.len()) {
                stdout.write_all(line.as_bytes())?;
            }
            stdout.flush()?;
            copy(folders)?;
        }
        ("write-code", [code, folders @ ..]) => {
            let mut code = fs::OpenOptions::new().append(true).open(code)?;
            writeln!(code, "# edited by the step")?;
            copy(folders)?;
        }
        ("call", [program, folders @ ..]) => {
            out(folders)?;
            return call(program);
        }
        ("leave-child", [lock, marker, folders @ ..]) => {
            out(folders)?;
            let _child = Command::new(std::env::current_exe()?)
                .args(["lock", lock, marker])
                .spawn()?;
            wait_for(Path::new(marker))?;
            idle()?;
        }
        ("lock", [lock, marker]) => {
            let file = fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(lock)?;
            file.lock()?;
            fs::write(marker, "locked")?;
            idle()?;
        }
        _ => return Err(invalid(&format!("cannot {mode} with {rest:?}"))),
    }
    Ok(ExitCode::SUCCESS)
}

/// waits until `path` exists, or gives up after [`PATIENCE`].
fn wait_for(path: &Path) -> io::Result<()> {
    let started = Instant::now();
    while !path.exists() {
        if started.elapsed() > PATIENCE {
            return Err(invalid(&format!("{} never appeared", path.display())));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// waits to be ended, and fails after [`PATIENCE`] when nothing ended it.
fn idle() -> io::Result<()> {
    std::thread::sleep(PATIENCE);
    Err(invalid("nothing ended the step"))
}

/// the result folder: the last of `folders`, which must be there.
fn out(folders: &[String]) -> io::Result<&Path> {
    let out = Path::new(
        folders
            .last()
            .ok_or_else(|| invalid("give the result folder last"))?,
    );
    if !out.is_dir() {
        return Err(invalid(&format!("{} is no folder", out.display())));
    }
    Ok(out)
}

/// copies the files of every folder but the last into the last, keeping their paths.
fn copy(folders: &[String]) -> io::Result<()> {
    let out = out(folders)?;
    for input in &folders[..folders.len() - 1] {
        copy_tree(Path::new(input), out)?;
    }
    Ok(())
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            fs::create_dir_all(&target)?;
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::write(&target, fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn link(out: &Path) -> io::Result<()> {
    fs::write(out.join("real.csv"), "a,b\n")?;
    std::os::unix::fs::symlink("real.csv", out.join("latest"))
}

#[cfg(windows)]
fn link(out: &Path) -> io::Result<()> {
    fs::write(out.join("real.csv"), "a,b\n")?;
    std::os::windows::fs::symlink_file("real.csv", out.join("latest"))
}

/// runs `program sync` where the step runs, and passes on how it ended.
fn call(program: &str) -> io::Result<ExitCode> {
    let mut child = Command::new(program).arg("sync").spawn()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let code = status.code().and_then(|code| u8::try_from(code).ok());
            return Ok(ExitCode::from(code.unwrap_or(102)));
        }
        if started.elapsed() > PATIENCE {
            child.kill()?;
            return Err(invalid(&format!("{program} sync never ended")));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_owned())
}
