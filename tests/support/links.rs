use std::io;
use std::path::Path;

/// makes `link` a link to the folder `target`: a symbolic link, or on windows a junction, which
/// needs no privilege. an error where the disk makes neither, as fat cannot.
pub(crate) fn link_folder(target: &Path, link: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()?;
        if output.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ))
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)
}
