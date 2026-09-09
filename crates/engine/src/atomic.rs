//! Writing a file whose name another user may reach, without following what they left at it.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static WRITES: AtomicU64 = AtomicU64::new(0);

const ATTEMPTS: u32 = 16;

pub enum Site {
    Scratch(PathBuf),
    Final,
}

fn scratch_beside(path: &Path) -> PathBuf {
    let mut beside = path.as_os_str().to_owned();
    beside.push(format!(
        ".{}.{}.writing",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(beside)
}

fn create_scratch(path: &Path) -> Result<(File, PathBuf), (Site, std::io::Error)> {
    let mut last = None;
    for _ in 0..ATTEMPTS {
        let beside = scratch_beside(path);
        match File::create_new(&beside) {
            Ok(file) => return Ok((file, beside)),
            Err(reason) if reason.kind() == std::io::ErrorKind::AlreadyExists => {
                last = Some((Site::Scratch(beside), reason));
            }
            Err(reason) => return Err((Site::Scratch(beside), reason)),
        }
    }
    Err(last.unwrap_or_else(|| {
        (
            Site::Scratch(path.to_path_buf()),
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "every scratch name beside it was already taken",
            ),
        )
    }))
}

/// # Errors
/// The site and the reason, so the caller renders its own kind.
pub fn replace(path: &Path, bytes: &[u8]) -> Result<(), (Site, std::io::Error)> {
    let (mut file, beside) = create_scratch(path)?;
    if let Err(reason) = file.write_all(bytes) {
        drop(file);
        let _ = std::fs::remove_file(&beside);
        return Err((Site::Scratch(beside), reason));
    }
    drop(file);
    std::fs::rename(&beside, path).map_err(|reason| {
        let _ = std::fs::remove_file(&beside);
        (Site::Final, reason)
    })
}

/// # Errors
/// The reason the name could not be created, and `AlreadyExists` when it holds something other than a plain file.
pub fn touch(path: &Path) -> std::io::Result<()> {
    match File::create_new(path) {
        Ok(_) => Ok(()),
        Err(reason) if reason.kind() == std::io::ErrorKind::AlreadyExists => {
            let found = std::fs::symlink_metadata(path)?;
            if found.is_file() {
                Ok(())
            } else {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!(
                        "{} is not a plain file, so it was left rather than written through",
                        path.display()
                    ),
                ))
            }
        }
        Err(reason) => Err(reason),
    }
}
