//! the code a recipe names, hashed the same on every system: a text file with the line endings
//! git for windows checks out made plain, since `core.autocrlf` turns LF into CRLF there, any
//! other file as it is, and a folder as the tree of its files.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use icu_normalizer::ComposingNormalizerBorrowed;

use crate::cache::{Member, tree_hash};
use crate::digest::{Hasher, Sha256};
use crate::error::{Error, Kind};
use crate::project::lock::derived::Code;

/// how much of a file git reads to tell text from binary: text holds no NUL in it.
const SNIFF: usize = 8000;
/// what a folder of code leaves out: version control, caches and the files file browsers add.
const SKIPPED: [&str; 7] = [
    ".git",
    ".hg",
    ".svn",
    "__pycache__",
    ".ipynb_checkpoints",
    ".DS_Store",
    "Thumbs.db",
];

/// the hash of the code at `path`, a file or a folder below `root` with `/` between folders, for
/// the recipe `recipe`. none when nothing is there. a link anywhere on the way, or a special file,
/// is refused, since code is the project's own files.
///
/// # Errors
///
/// `project.invalid` for a link or a special file, `local.io` when it cannot be read.
pub(crate) fn hash(root: &Path, recipe: &str, path: &str) -> Result<Option<Code>, Error> {
    let Some((at, is_folder)) = locate(root, recipe, path)? else {
        return Ok(None);
    };
    if !is_folder {
        let (sha256, _) = file(&at).map_err(|err| unreadable(recipe, path, &err))?;
        return Ok(Some(Code {
            path: path.to_owned(),
            sha256,
            folder: false,
        }));
    }
    let mut members = Vec::new();
    walk(&at, "", recipe, path, &mut |inside, found| {
        let shown = format!("{path}/{inside}");
        let (sha256, size) = file(found).map_err(|err| unreadable(recipe, &shown, &err))?;
        members.push(Member {
            path: ComposingNormalizerBorrowed::new_nfc()
                .normalize(inside)
                .into_owned(),
            size,
            sha256,
        });
        Ok(())
    })?;
    Ok(Some(Code {
        path: path.to_owned(),
        sha256: tree_hash(&members),
        folder: true,
    }))
}

/// the files whose bytes [`hash`] reads for the code at `path`, by their paths from `root` with
/// `/` between folders, as the disk names them: the file itself, or each file of the folder its
/// hash takes in. empty when nothing is there. no file is read.
///
/// # Errors
///
/// as [`hash`] refuses the code, or cannot read a folder of it.
pub(crate) fn files(root: &Path, recipe: &str, path: &str) -> Result<Vec<String>, Error> {
    let Some((at, is_folder)) = locate(root, recipe, path)? else {
        return Ok(Vec::new());
    };
    if !is_folder {
        return Ok(vec![path.to_owned()]);
    }
    let mut files = Vec::new();
    walk(&at, "", recipe, path, &mut |inside, _| {
        files.push(format!("{path}/{inside}"));
        Ok(())
    })?;
    Ok(files)
}

/// where the code at `path` below `root` is, and whether it is a folder. none when nothing is
/// there; a link anywhere on the way, or a special file, is refused.
fn locate(root: &Path, recipe: &str, path: &str) -> Result<Option<(PathBuf, bool)>, Error> {
    let mut at = root.to_path_buf();
    let parts: Vec<&str> = path.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        at.push(part);
        let shown = parts[..=index].join("/");
        let metadata = match fs::symlink_metadata(&at) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(unreadable(recipe, &shown, &err)),
        };
        let kind = metadata.file_type();
        if kind.is_symlink() {
            return Err(refused(recipe, path, &format!("{shown} is a link")));
        }
        let last = index + 1 == parts.len();
        if last && kind.is_file() {
            return Ok(Some((at, false)));
        }
        if !kind.is_dir() {
            let what = if kind.is_file() {
                format!("{shown} is a file, not a folder")
            } else {
                format!("{shown} is a special file")
            };
            return Err(refused(recipe, path, &what));
        }
    }
    Ok(Some((at, true)))
}

/// calls `found` with each file below `at` that a folder of code holds, its path inside the
/// folder after `prefix` as the disk names it and where it is. what [`SKIPPED`] names stays out.
fn walk(
    at: &Path,
    prefix: &str,
    recipe: &str,
    code: &str,
    found: &mut dyn FnMut(&str, &Path) -> Result<(), Error>,
) -> Result<(), Error> {
    let entries = fs::read_dir(at).map_err(|err| unreadable(recipe, code, &err))?;
    for entry in entries {
        let entry = entry.map_err(|err| unreadable(recipe, code, &err))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            let shown = crate::name::joined(prefix, &name.to_string_lossy());
            return Err(refused(
                recipe,
                code,
                &format!("{code}/{shown} has a name that is not UTF-8"),
            ));
        };
        if SKIPPED.contains(&name) {
            continue;
        }
        let inside = crate::name::joined(prefix, name);
        let shown = format!("{code}/{inside}");
        let path = entry.path();
        let kind = fs::symlink_metadata(&path)
            .map_err(|err| unreadable(recipe, &shown, &err))?
            .file_type();
        if kind.is_symlink() {
            return Err(refused(recipe, code, &format!("{shown} is a link")));
        } else if kind.is_dir() {
            walk(&path, &inside, recipe, code, found)?;
        } else if kind.is_file() {
            found(&inside, &path)?;
        } else {
            return Err(refused(recipe, code, &format!("{shown} is a special file")));
        }
    }
    Ok(())
}

/// the hash and size of a file's bytes, with each CRLF made LF when the file is text: no NUL in
/// its first [`SNIFF`] bytes, as git decides.
fn file(path: &Path) -> io::Result<(Sha256, u64)> {
    let mut file = crate::cache::read_unfollowed(path)?;
    let mut first = vec![0; SNIFF];
    let mut filled = 0;
    while filled < SNIFF {
        let read = file.read(&mut first[filled..])?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    first.truncate(filled);
    let mut plain = Plain {
        text: !first.contains(&0),
        carriage: false,
        hasher: Hasher::default(),
        size: 0,
    };
    plain.feed(&first);
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(plain.finish());
        }
        plain.feed(&buffer[..read]);
    }
}

/// a hash of bytes as they stream past, a text file's CRLF made LF on the way.
struct Plain {
    text: bool,
    /// whether the last byte fed was a CR, held back until the next byte says whether an LF
    /// follows it.
    carriage: bool,
    hasher: Hasher,
    size: u64,
}

impl Plain {
    fn feed(&mut self, bytes: &[u8]) {
        if !self.text {
            self.take(bytes);
            return;
        }
        let mut start = 0;
        for (at, byte) in bytes.iter().enumerate() {
            if std::mem::take(&mut self.carriage) && *byte != b'\n' {
                self.take(b"\r");
            }
            if *byte == b'\r' {
                self.take(&bytes[start..at]);
                start = at + 1;
                self.carriage = true;
            }
        }
        self.take(&bytes[start..]);
    }

    fn take(&mut self, bytes: &[u8]) {
        self.hasher.update(bytes);
        self.size += bytes.len() as u64;
    }

    fn finish(mut self) -> (Sha256, u64) {
        if self.carriage {
            self.take(b"\r");
        }
        (self.hasher.finish().0, self.size)
    }
}

fn refused(recipe: &str, code: &str, what: &str) -> Error {
    Error::new(
        Kind::ProjectInvalid,
        format!("the code {code} of datasets.{recipe} is refused: {what}"),
    )
    .help(
        "code names the project's own plain files and folders; put a copy there instead of a link",
    )
}

fn unreadable(recipe: &str, code: &str, err: &io::Error) -> Error {
    Error::io(
        format!("could not read {code}, code of datasets.{recipe}"),
        err,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::digest;

    /// writes `files`, each a path below `root` with `/` between folders and its bytes.
    fn write(root: &Path, files: &[(&str, &[u8])]) {
        for (path, bytes) in files {
            let at = root.join(path);
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(at, bytes).unwrap();
        }
    }

    fn hashed(root: &Path, path: &str) -> Code {
        hash(root, "clean", path).unwrap().unwrap()
    }

    #[test]
    fn crlf_and_lf_code_give_one_key() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            &[
                ("lf.py", b"a\nb\n"),
                ("crlf.py", b"a\r\nb\r\n"),
                ("lone.py", b"a\rb\n"),
                ("double.py", b"a\r\r\nb"),
            ],
        );
        let lf = hashed(root.path(), "lf.py");
        assert_eq!(lf.sha256, digest::sha256(b"a\nb\n"));
        assert!(!lf.folder);
        assert_eq!(hashed(root.path(), "crlf.py").sha256, lf.sha256);
        assert_eq!(
            hashed(root.path(), "lone.py").sha256,
            digest::sha256(b"a\rb\n"),
            "a CR alone stays"
        );
        assert_eq!(
            hashed(root.path(), "double.py").sha256,
            digest::sha256(b"a\r\nb")
        );
        // a CRLF split between the bytes git reads to tell text and the rest
        let mut long = vec![b'x'; SNIFF - 1];
        long.extend(b"\r\nend\r\n");
        write(root.path(), &[("long.py", &long)]);
        let mut plain = vec![b'x'; SNIFF - 1];
        plain.extend(b"\nend\n");
        assert_eq!(
            file(&root.path().join("long.py")).unwrap(),
            (digest::sha256(&plain), plain.len() as u64)
        );
    }

    #[test]
    fn binary_code_is_hashed_as_is() {
        let root = tempfile::tempdir().unwrap();
        let binary = b"\x00\x01\r\n\x02";
        let mut late = vec![b'x'; SNIFF];
        late.extend(b"\0\r\n");
        write(root.path(), &[("model.bin", binary), ("late.txt", &late)]);
        assert_eq!(
            hashed(root.path(), "model.bin").sha256,
            digest::sha256(binary)
        );
        let mut plain = vec![b'x'; SNIFF];
        plain.extend(b"\0\n");
        assert_eq!(
            hashed(root.path(), "late.txt").sha256,
            digest::sha256(&plain),
            "a NUL past what git reads keeps a file text"
        );
    }

    #[test]
    fn code_folders_skip_git_and_pycache() {
        let root = tempfile::tempdir().unwrap();
        write(
            root.path(),
            &[
                ("lib/a.py", b"a\n"),
                ("lib/sub/b.py", b"b\n"),
                ("other/a.py", b"a\r\n"),
                ("other/sub/b.py", b"b\n"),
            ],
        );
        let lib = hashed(root.path(), "lib");
        assert!(lib.folder);
        let listed = || {
            let mut listed = files(root.path(), "clean", "lib").unwrap();
            listed.sort();
            listed
        };
        assert_eq!(listed(), ["lib/a.py", "lib/sub/b.py"]);
        let members = [
            Member {
                path: "a.py".to_owned(),
                size: 2,
                sha256: digest::sha256(b"a\n"),
            },
            Member {
                path: "sub/b.py".to_owned(),
                size: 2,
                sha256: digest::sha256(b"b\n"),
            },
        ];
        assert_eq!(lib.sha256, tree_hash(&members));
        assert_eq!(
            hashed(root.path(), "other").sha256,
            lib.sha256,
            "a CRLF checkout of the folder gives its tree"
        );
        write(
            root.path(),
            &[
                ("lib/.git/config", b"x"),
                ("lib/sub/__pycache__/b.cpython-312.pyc", b"x"),
                ("lib/.ipynb_checkpoints/a-checkpoint.py", b"x"),
                ("lib/.DS_Store", b"x"),
                ("lib/Thumbs.db", b"x"),
            ],
        );
        assert_eq!(hashed(root.path(), "lib").sha256, lib.sha256);
        assert_eq!(
            listed(),
            ["lib/a.py", "lib/sub/b.py"],
            "the files the hash reads"
        );
        write(root.path(), &[("lib/sub/c.py", b"c\n")]);
        assert_ne!(hashed(root.path(), "lib").sha256, lib.sha256);
        assert_eq!(hash(root.path(), "clean", "missing.py").unwrap(), None);
        assert_eq!(
            files(root.path(), "clean", "missing.py").unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            files(root.path(), "clean", "lib/a.py").unwrap(),
            ["lib/a.py"]
        );
        assert_eq!(
            hash(root.path(), "clean", "lib/missing/x.py").unwrap(),
            None
        );
        let file_as_folder = hash(root.path(), "clean", "lib/a.py/x.py").unwrap_err();
        assert_eq!(file_as_folder.kind, Kind::ProjectInvalid);
        assert_eq!(
            file_as_folder.message,
            "the code lib/a.py/x.py of datasets.clean is refused: lib/a.py is a file, not a folder"
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_in_code_are_refused() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), &[("lib/a.py", b"a\n"), ("real.py", b"r\n")]);
        std::os::unix::fs::symlink("real.py", root.path().join("link.py")).unwrap();
        std::os::unix::fs::symlink("lib", root.path().join("linked")).unwrap();
        std::os::unix::fs::symlink("../real.py", root.path().join("lib/b.py")).unwrap();
        for (path, said) in [
            ("link.py", "link.py is a link"),
            ("linked/a.py", "linked is a link"),
            ("lib", "lib/b.py is a link"),
        ] {
            let error = hash(root.path(), "clean", path).unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid, "{path}");
            assert_eq!(
                error.message,
                format!("the code {path} of datasets.clean is refused: {said}")
            );
        }
    }
}
