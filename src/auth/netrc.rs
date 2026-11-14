//! `.netrc`, the file curl, wget and python's requests read a host's login from. a login saved
//! here works in all of them.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::machine::Var;

/// one `machine` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub(crate) host: String,
    pub(crate) login: String,
    pub(crate) password: String,
    /// where the entry starts and ends in the file, so it can be replaced in place.
    span: (usize, usize),
}

/// the file the other tools read: `NETRC`, else `.netrc` in the home folder, and on windows
/// `_netrc` there when `.netrc` does not exist, as curl looks.
pub(crate) fn path(var: Var<'_>) -> Option<PathBuf> {
    if let Some(named) = var("NETRC").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(named));
    }
    let home = crate::machine::home(var)?;
    let dotted = home.join(".netrc");
    if cfg!(windows) && !dotted.exists() {
        let underscored = home.join("_netrc");
        if underscored.exists() {
            return Some(underscored);
        }
    }
    Some(dotted)
}

/// whether the logs already said other users can open the `.netrc`, which they say once a run.
static OPEN_TOLD: AtomicBool = AtomicBool::new(false);

/// every `machine` entry with a login and a password. `default` is skipped on purpose: a login
/// for every host would go to hosts nobody named.
pub(crate) fn read(var: Var<'_>) -> Vec<Entry> {
    let Some(path) = path(var) else {
        return Vec::new();
    };
    if too_open(&path) && !OPEN_TOLD.swap(true, Ordering::Relaxed) {
        tracing::warn!(path = %path.display(), "other users can open this .netrc");
    }
    fs::read_to_string(path)
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// whether users other than its owner may open the file, which python's netrc module refuses.
/// only unix modes say so; elsewhere the file takes its folder's permissions.
pub(crate) fn too_open(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o077 != 0)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

pub(crate) fn parse(text: &str) -> Vec<Entry> {
    let words = words(text);
    let mut entries = Vec::new();
    let mut at = 0;
    while at < words.len() {
        if words[at].0 != "machine" {
            at += 1;
            continue;
        }
        let start = words[at].1;
        let Some((host, _, host_end)) = words.get(at + 1) else {
            break;
        };
        let mut login = None;
        let mut password = None;
        let mut end = *host_end;
        let mut next = at + 2;
        while next < words.len() && !matches!(words[next].0.as_str(), "machine" | "default") {
            match (words[next].0.as_str(), words.get(next + 1)) {
                ("login", Some((value, _, stop))) => {
                    login = Some(value.clone());
                    end = *stop;
                    next += 2;
                }
                ("password", Some((value, _, stop))) => {
                    password = Some(value.clone());
                    end = *stop;
                    next += 2;
                }
                ("account", Some((_, _, stop))) => {
                    end = *stop;
                    next += 2;
                }
                _ => {
                    end = words[next].2;
                    next += 1;
                }
            }
        }
        if let (Some(login), Some(password)) = (login, password)
            && !host.contains(['/', '\\', '@', '?', '#', '%'])
            && super::dataverse_host(host).is_some()
        {
            entries.push(Entry {
                host: host.to_ascii_lowercase(),
                login,
                password,
                span: (start, end),
            });
        }
        at = next;
    }
    entries
}

/// the file's words with where each starts and ends. quotes group a value with spaces, as curl
/// reads them, and a `macdef` body runs to the next blank line.
fn words(text: &str) -> Vec<(String, usize, usize)> {
    let bytes = text.as_bytes();
    let mut words = Vec::new();
    let mut at = 0;
    // the word after machine, login, password or account is its value, even one spelled macdef
    let mut value_next = false;
    while at < bytes.len() {
        if bytes[at].is_ascii_whitespace() {
            at += 1;
            continue;
        }
        if bytes[at] == b'#' {
            while at < bytes.len() && bytes[at] != b'\n' {
                at += 1;
            }
            continue;
        }
        let start = at;
        let quoted = bytes[at] == b'"';
        let word = if quoted {
            let mut word = Vec::new();
            at += 1;
            while at < bytes.len() && bytes[at] != b'"' {
                if bytes[at] == b'\\' && at + 1 < bytes.len() {
                    at += 1;
                }
                word.push(bytes[at]);
                at += 1;
            }
            at = (at + 1).min(bytes.len());
            String::from_utf8_lossy(&word).into_owned()
        } else {
            while at < bytes.len() && !bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            text[start..at].to_owned()
        };
        if word == "macdef" && !quoted && !value_next {
            at = macro_end(text, at);
            continue;
        }
        value_next = !value_next
            && !quoted
            && matches!(word.as_str(), "machine" | "login" | "password" | "account");
        words.push((word, start, at));
    }
    words
}

/// where a `macdef` body that starts at `at` ends: at the next line that is empty, a windows line
/// ending included, or at the end of the file.
fn macro_end(text: &str, at: usize) -> usize {
    let mut line = text[at..]
        .find('\n')
        .map_or(text.len(), |found| at + found + 1);
    while line < text.len() {
        let end = text[line..]
            .find('\n')
            .map_or(text.len(), |found| line + found);
        if text[line..end].trim_end_matches('\r').is_empty() {
            return end;
        }
        line = end + 1;
    }
    text.len()
}

/// writes `login` and `password` for `host`, replacing an entry the file already has for it and
/// keeping everything else as it was. the file is readable by its owner only.
pub(crate) fn save(path: &Path, host: &str, login: &str, password: &str) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err),
    };
    let line = format!(
        "machine {host} login {} password {}",
        quoted(login),
        quoted(password)
    );
    let updated = match parse(&text).into_iter().find(|entry| entry.host == host) {
        Some(entry) => format!("{}{line}{}", &text[..entry.span.0], &text[entry.span.1..]),
        None if text.is_empty() || text.ends_with('\n') => format!("{text}{line}\n"),
        None => format!("{text}\n{line}\n"),
    };
    super::store::write_private(path, updated.as_bytes())
}

/// removes the entry for `host`. returns whether there was one.
pub(crate) fn remove(path: &Path, host: &str) -> io::Result<bool> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(false);
    };
    let Some(entry) = parse(&text).into_iter().find(|entry| entry.host == host) else {
        return Ok(false);
    };
    let rest = format!(
        "{}{}",
        &text[..entry.span.0],
        text[entry.span.1..].trim_start_matches([' ', '\t', '\r', '\n'])
    );
    super::store::write_private(path, rest.as_bytes())?;
    Ok(true)
}

fn quoted(value: &str) -> String {
    // an unquoted word starting with # reads back as a comment, which would shift the entry
    if value.is_empty()
        || value.starts_with('#')
        || value.contains(|c: char| c.is_whitespace() || c == '"' || c == '\\')
    {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_read_the_way_curl_reads_them() {
        let text = "# lab logins\nmachine urs.earthdata.nasa.gov login ada password \"s3cret pass\"\n\
                    machine physionet.org\n  login grace\n  password pw\n  account x\n\
                    macdef init\ncd /\n\nmachine half.example login only\n\
                    default login anyone password anything\n";
        let pairs: Vec<_> = parse(text)
            .into_iter()
            .map(|e| (e.host, e.login, e.password))
            .collect();
        let expected = [
            ("urs.earthdata.nasa.gov", "ada", "s3cret pass"),
            ("physionet.org", "grace", "pw"),
        ]
        .map(|(h, l, p)| (h.to_owned(), l.to_owned(), p.to_owned()));
        assert_eq!(pairs, expected);
        // a windows _netrc ends its lines with \r\n, its blank lines too
        let windows = "machine a.example login x password y\r\nmacdef init\r\ncd /\r\n\r\n\
                       machine b.example login u password p\r\n";
        let hosts: Vec<_> = parse(windows).into_iter().map(|e| e.host).collect();
        assert_eq!(hosts, ["a.example", "b.example"]);
        // a value spelled like the keyword is still a value
        let named =
            "machine a.example login u password macdef\nmachine b.example login v password w\n";
        let pairs: Vec<_> = parse(named)
            .into_iter()
            .map(|e| (e.host, e.password))
            .collect();
        assert_eq!(
            pairs,
            [
                ("a.example".to_owned(), "macdef".to_owned()),
                ("b.example".to_owned(), "w".to_owned())
            ]
        );
    }

    #[test]
    fn a_saved_password_starting_with_a_hash_reads_back_for_its_own_host() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join(".netrc");
        fs::write(&path, "machine b.example login bob password pw\n").unwrap();
        save(&path, "a.example", "ada", "#Secr3t").unwrap();
        save(&path, "c.example", "cy", "pw2").unwrap();
        let entries: Vec<_> = parse(&fs::read_to_string(&path).unwrap())
            .into_iter()
            .map(|e| (e.host, e.login, e.password))
            .collect();
        assert_eq!(
            entries,
            [
                ("b.example".into(), "bob".into(), "pw".into()),
                ("a.example".into(), "ada".into(), "#Secr3t".into()),
                ("c.example".into(), "cy".into(), "pw2".into()),
            ]
        );
    }

    #[test]
    fn saving_replaces_one_entry_and_keeps_the_rest() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join(".netrc");
        fs::write(
            &path,
            "# mine\nmachine a.example login x password y\nmachine b.example login p password q\n",
        )
        .unwrap();
        save(&path, "a.example", "ada", "new pass").unwrap();
        save(&path, "c.example", "c", "d").unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "# mine\nmachine a.example login ada password \"new pass\"\n\
             machine b.example login p password q\nmachine c.example login c password d\n"
        );
        assert!(remove(&path, "b.example").unwrap());
        assert!(!remove(&path, "b.example").unwrap());
        let hosts: Vec<_> = parse(&fs::read_to_string(&path).unwrap())
            .into_iter()
            .map(|e| e.host)
            .collect();
        assert_eq!(hosts, ["a.example", "c.example"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }
}
