//! `fetchloom settings set` and `unset`: one key of the settings file changed in place. a value is
//! read as a run reads it before anything is written, comments and the other keys stay, and the
//! file is replaced in one step, keeping its permissions.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Component, Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use super::{
    BOOLEANS, CLEARED, File, KEYS, LISTS, OTHERS, Settings, Value, located, parse, plain, split,
    table, text_at,
};
use crate::error::{Error, Kind};
use crate::machine::{Machine, Var};

/// the keys whose text stays text even when it is a whole number.
const WORDS: [&str; 3] = ["cache", "push_to", "resources"];

/// what a run checks of its settings past reading each one, as that `mirrors_only` has mirrors to
/// ask, so a change that would leave every run stopping there is refused.
pub(crate) type Usable<'a> = &'a dyn Fn(&Settings) -> Result<(), Error>;

/// what `set` or `unset` did to the settings file.
#[derive(Debug)]
pub(crate) struct Changed {
    pub(crate) key: String,
    /// the settings file, past a link to it.
    pub(crate) path: PathBuf,
    /// the key's value as the file held it, and as it holds it now; none where it holds none.
    pub(crate) before: Option<toml_edit::Value>,
    pub(crate) after: Option<toml_edit::Value>,
    /// whether the file was written, which it is not when it held the value already.
    pub(crate) written: bool,
    /// whether the change made the file.
    pub(crate) created: bool,
    /// what still stops every run reading the file, which was there before the change.
    pub(crate) broken: Option<Error>,
    /// the mirror or push folders the new value names that are not there.
    pub(crate) missing: Vec<PathBuf>,
}

/// sets `name` to `values` in the settings file `var` locates: for a list, every value, each split
/// at commas as its variable is, else the one value, a relative folder taken from `here`. a value
/// a run would refuse, or one that leaves a file every run stops at, as `usable` and the reader
/// tell, is refused with their message, and nothing is written.
pub(crate) fn set(
    var: Var<'_>,
    name: &str,
    values: &[String],
    here: Option<&Path>,
    usable: Usable<'_>,
) -> Result<Changed, Error> {
    let key = known(name, "set")?;
    let home = crate::machine::home(var);
    let places = Places {
        here,
        home: home.as_deref(),
    };
    let value = given(key, values, places)?;
    let after = written_as(key, &value);
    let Opened {
        path,
        text,
        found,
        held: _held,
    } = open(var, true)?;
    let mut document = document(&path, &text)?;
    check(key, &after, &path)?;
    let before = document.get(key).and_then(Item::as_value).cloned();
    let mut changed = Changed {
        key: key.to_owned(),
        path,
        before: before.clone(),
        after: Some(after.clone()),
        written: false,
        created: !found,
        broken: None,
        missing: missing(key, &value),
    };
    if before.is_some() && before.as_ref().and_then(read_as) == read_as(&after) {
        changed.broken = whole(&changed.path, &text, &text, usable)?;
        return Ok(changed);
    }
    match document.get_mut(key) {
        // in place, so the key keeps the comments above it, and the value the one after it
        Some(Item::Value(held)) => {
            let decor = held.decor().clone();
            *held = after;
            *held.decor_mut() = decor;
        }
        Some(item) => *item = Item::Value(after),
        None => appended(&mut document, key, after),
    }
    let written = document.to_string();
    changed.broken = whole(&changed.path, &text, &written, usable)?;
    write(&changed.path, &written, found)?;
    changed.written = true;
    Ok(changed)
}

/// takes `name` out of the settings file `var` locates: a setting, or a key the file holds that
/// is none, so a mistake that stops every run can be taken out too. comments above it stay. a
/// removal that leaves a file every run stops at, as `usable` and the reader tell, is refused.
pub(crate) fn unset(var: Var<'_>, name: &str, usable: Usable<'_>) -> Result<Changed, Error> {
    let Opened {
        path,
        text,
        found,
        held: _held,
    } = open(var, false)?;
    let mut document = document(&path, &text)?;
    if !document.contains_key(name) {
        known(name, "unset")?;
    }
    // a table, as a `[mystuff]` the file holds, shows as the inline table it would be
    let before = document
        .get(name)
        .and_then(|item| item.clone().into_value().ok());
    let mut changed = Changed {
        key: name.to_owned(),
        path,
        before,
        after: None,
        written: false,
        created: false,
        broken: None,
        missing: Vec::new(),
    };
    if !removed(&mut document, name) {
        changed.broken = whole(&changed.path, &text, &text, usable)?;
        return Ok(changed);
    }
    let written = document.to_string();
    changed.broken = whole(&changed.path, &text, &written, usable)?;
    write(&changed.path, &written, found)?;
    changed.written = true;
    Ok(changed)
}

/// what stops every run reading the settings file at `path` once it holds `after`, where it was
/// there with `before` too, so a change cannot be blamed for it; a usage error where the change
/// brings it, so nothing is written.
fn whole(
    path: &Path,
    before: &str,
    after: &str,
    usable: Usable<'_>,
) -> Result<Option<Error>, Error> {
    let Err(stops) = reads(path, after, usable) else {
        return Ok(None);
    };
    if before == after || reads(path, before, usable).is_err() {
        return Ok(Some(stops));
    }
    Err(stops.detail("nothing was saved, since every run would stop at this"))
}

/// reads the settings as a run would from a settings file at `path` holding `text`, with no
/// variable set, then checks them as `usable` does.
fn reads(path: &Path, text: &str, usable: Usable<'_>) -> Result<(), Error> {
    let file = File {
        path: Some(path.to_owned()),
        values: parse(path, text)?,
        found: true,
        bypassed: None,
    };
    usable(&Settings::read(&|_| None, file, Machine::default())?)
}

/// the variable that gives `key` a value for this run in place of the settings file, when one is
/// set: a nonempty one, or an empty one for a key an empty variable clears.
pub(crate) fn overridden(var: Var<'_>, key: &str) -> Option<&'static str> {
    let (_, variable) = KEYS.iter().find(|(known, _)| *known == key)?;
    var(variable)
        .filter(|value| !value.is_empty() || CLEARED.contains(&key))
        .map(|_| *variable)
}

/// `name` as a key the settings file takes, or a usage error naming the nearest one, or the
/// variable that sets it when only a variable does, which `doing`, `set` or `unset`, then names.
fn known(name: &str, doing: &str) -> Result<&'static str, Error> {
    if let Some((key, _)) = KEYS.iter().find(|(key, _)| *key == name) {
        return Ok(key);
    }
    let variable = format!("FETCHLOOM_{}", name.to_ascii_uppercase());
    if OTHERS.contains(&variable.as_str()) {
        let help = if doing == "unset" {
            format!("unset {variable} where it is set, as in your shell's profile")
        } else {
            format!("set {variable} where fetchloom runs, as in your shell's profile")
        };
        return Err(Error::new(
            Kind::Usage,
            format!("{name} is set by {variable}, not the settings file"),
        )
        .help(help));
    }
    let error = Error::new(Kind::Usage, format!("{name} is not a setting"));
    // a variable's name, as FETCHLOOM_JOBS, or the key in capitals, means the key
    let meant = KEYS
        .iter()
        .find(|(key, variable)| {
            variable.eq_ignore_ascii_case(name) || key.eq_ignore_ascii_case(name)
        })
        .map(|(key, _)| *key)
        .or_else(|| crate::name::closest(name, KEYS.map(|(key, _)| key)));
    Err(match meant {
        Some(meant) => error.help(format!("did you mean `{meant}`?")),
        None => error.help(format!(
            "the settings are {}",
            KEYS.map(|(key, _)| key).join(", ")
        )),
    })
}

/// what the command line gives `key`, as the settings file would hold it.
fn given(key: &str, values: &[String], places: Places<'_>) -> Result<Value, Error> {
    if LISTS.contains(&key) {
        let items = values
            .iter()
            .flat_map(|value| split(value))
            .map(|item| from(key, item, places))
            .collect::<Result<_, _>>()?;
        return Ok(Value::List(items));
    }
    let [value] = values else {
        let error = Error::new(
            Kind::Usage,
            format!("{key} takes one value, and {} were given", values.len()),
        );
        // `settings set jobs 4 host_jobs 2` gives a second key where the one value goes
        let another = values
            .iter()
            .skip(1)
            .any(|value| KEYS.iter().any(|(known, _)| known == value));
        return Err(match values.first().filter(|_| another) {
            Some(first) => error.help(format!(
                "settings set saves one key at a time, so run `{}`, then set the next",
                crate::suggested::command(&format!(
                    "settings set {key} {}",
                    crate::suggested::shell_word(first)
                ))
            )),
            None => error.help("put quotes around a value with spaces in it, as \"my folder\""),
        });
    };
    let value = value.trim();
    if value.is_empty() {
        return Err(
            Error::new(Kind::Usage, format!("{key} needs a value")).help(format!(
                "`fetchloom settings unset {key}` takes it out of the settings file"
            )),
        );
    }
    Ok(Value::Text(from(key, value.to_owned(), places)?))
}

/// the folders `value` names for `key` that are not there: a mirror's or a reuse folder, which
/// runs skip until it is, or the push folder, which the first push makes. a run makes the cache
/// folder itself.
fn missing(key: &str, value: &Value) -> Vec<PathBuf> {
    if !matches!(key, "mirrors" | "push_to" | "reuse") {
        return Vec::new();
    }
    let items = match value {
        Value::Text(text) => std::slice::from_ref(text),
        Value::List(items) => items.as_slice(),
    };
    items
        .iter()
        .filter_map(|item| match crate::mirror::Location::parse(item, None) {
            Ok(crate::mirror::Location::Folder(folder)) if !folder.exists() => Some(folder),
            _ => None,
        })
        .collect()
}

/// the folders a folder given on the command line may lead from.
#[derive(Clone, Copy)]
struct Places<'a> {
    /// the current folder, which a relative folder is taken from.
    here: Option<&'a Path>,
    /// the home folder, which a leading `~` names.
    home: Option<&'a Path>,
}

/// `text` from the command line, a relative folder for `key` taken from the current folder,
/// since the settings file takes one from its own folder instead, and a leading `~` as the home
/// folder, as a shell takes it outside quotes. a web address or a bucket stays as it is, told
/// apart as a mirror is.
fn from(key: &str, text: String, places: Places<'_>) -> Result<String, Error> {
    let names_folder = match key {
        "cache" | "reuse" => true,
        "push_to" | "mirrors" => matches!(
            crate::mirror::Location::parse(&text, None),
            Ok(crate::mirror::Location::Folder(_))
        ),
        _ => false,
    };
    if !names_folder || Path::new(&text).is_absolute() {
        return Ok(text);
    }
    let refused = |reason: String| {
        Error::new(
            Kind::Usage,
            format!("{key} cannot take the folder {text}: {reason}"),
        )
        .help("give the folder from the top, as /mnt/lab/fetchloom")
    };
    let as_text = |folder: PathBuf, from: &Path| {
        folder.into_os_string().into_string().map_err(|_| {
            refused(format!(
                "{} holds a name that is not text, which the settings file cannot hold",
                from.display()
            ))
        })
    };
    if let Some(rest) = text.strip_prefix('~') {
        let rest = match rest.chars().next() {
            None => "",
            Some(first) if std::path::is_separator(first) => &rest[first.len_utf8()..],
            Some(_) => {
                return Err(refused(
                    "fetchloom takes ~ for your own home folder alone, not another's".to_owned(),
                ));
            }
        };
        let Some(home) = places.home else {
            return Err(refused(
                "~ stands for the home folder, and no home folder is set".to_owned(),
            ));
        };
        let folder = if rest.is_empty() {
            home.to_path_buf()
        } else {
            home.join(rest)
        };
        return as_text(folder, home);
    }
    let Some(here) = places.here else {
        return Err(refused(
            "the current folder, which it is relative to, could not be read".to_owned(),
        ));
    };
    // the current folder holds no links, as the system gives it, so a `..` leading the folder
    // given is its parent
    let mut parts = Path::new(&text).components().peekable();
    let mut folder = here.to_path_buf();
    while let Some(part) =
        parts.next_if(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        if part == Component::ParentDir {
            folder.pop();
        }
    }
    folder.extend(parts);
    as_text(folder, here)
}

/// the settings file as a change finds it.
struct Opened {
    /// where it is, past a link to it.
    path: PathBuf,
    /// its text, empty while it is not there.
    text: String,
    found: bool,
    /// the lock that keeps a second change off the file until this one is written, so both stay.
    /// none where the disk cannot lock, or where no folder holds the file yet and `open` was not
    /// asked to make one.
    held: Option<fs::File>,
}

/// the settings file `var` locates, read under its lock. `making` makes its folder when missing,
/// to lock in it, as a set writes the file there.
fn open(var: Var<'_>, making: bool) -> Result<Opened, Error> {
    let (path, _) = located(var);
    let path = path.ok_or_else(|| {
        Error::new(
            Kind::Usage,
            "this machine has no folder for a settings file, as no home folder is set",
        )
        .help("set FETCHLOOM_SETTINGS to the file to keep the settings in")
    })?;
    // a link, as from a folder of dotfiles, stays a link, and the file it names changes
    let path = match fs::symlink_metadata(&path) {
        Ok(meta) if meta.file_type().is_symlink() => fs::canonicalize(&path).map_err(|err| {
            Error::io(
                format_args!("could not follow the link at {}", path.display()),
                &err,
            )
        })?,
        _ => path,
    };
    let unread = |err: &io::Error| {
        Error::io(
            format_args!("could not read the settings file {}", path.display()),
            err,
        )
    };
    // what is no file to change is told as such, before a lock is made beside it
    match plain(&path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(unread(&err)),
        _ => {}
    }
    let held = hold(&path, making)?;
    let (text, found) = match text_at(&path) {
        Ok(text) => (text, true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => (String::new(), false),
        Err(err) => return Err(unread(&err)),
    };
    Ok(Opened {
        path,
        text,
        found,
        held,
    })
}

/// the lock on the settings file at `path`, taken on a file beside it, since a write replaces the
/// settings file itself. `making` makes the folder first; else a missing folder, with no file to
/// change, needs no lock.
fn hold(path: &Path, making: bool) -> Result<Option<fs::File>, Error> {
    let failed = |err: &io::Error| {
        Error::io(
            format_args!("could not lock the settings file {}", path.display()),
            err,
        )
    };
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    if making {
        fs::create_dir_all(folder).map_err(|err| failed(&err))?;
    } else if !folder.is_dir() {
        return Ok(None);
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(folder.join(format!(".{name}.lock")))
        .map_err(|err| failed(&err))?;
    match lock.lock() {
        Ok(()) => Ok(Some(lock)),
        Err(err) if err.kind() == io::ErrorKind::Unsupported => Ok(None),
        Err(err) => Err(failed(&err)),
    }
}

/// the settings file's text as a document to edit, or the error a run gives for text that is not
/// TOML.
fn document(path: &Path, text: &str) -> Result<DocumentMut, Error> {
    table(path, text)?;
    text.parse::<DocumentMut>().map_err(|err| {
        Error::new(
            Kind::Usage,
            format!(
                "the settings file {} is not valid TOML: {}",
                path.display(),
                err.message()
            ),
        )
    })
}

/// refuses `value` for `key` as a run reading it from the settings file at `path` would: the
/// reader takes the line that would be written and reads the setting from it, naming the value as
/// the command line gave it.
fn check(key: &str, value: &toml_edit::Value, path: &Path) -> Result<(), Error> {
    let mut line = DocumentMut::new();
    line.insert(key, Item::Value(value.clone()));
    let file = File {
        path: Some(path.to_owned()),
        values: parse(path, &line.to_string())?,
        found: true,
        bypassed: None,
    };
    Settings::read_asked(&|_| None, file, Machine::default(), Some(key))
        .map(drop)
        .map_err(|error| error.detail("nothing was saved"))
}

/// `value` for `key` as TOML writes it: a list as an array of text, a switch as true or false, a
/// whole number as a number unless the key names a folder or a level, and other text, a switch
/// the reader refuses among it, as text.
fn written_as(key: &str, value: &Value) -> toml_edit::Value {
    match value {
        Value::List(items) => items.iter().collect::<toml_edit::Array>().into(),
        Value::Text(text) if BOOLEANS.contains(&key) => {
            match crate::machine::switched(key, Some(std::ffi::OsStr::new(text))) {
                Ok(Some(on)) => on.into(),
                _ => text.as_str().into(),
            }
        }
        // as a run reads it, so `+5` is saved as 5
        Value::Text(text) if !WORDS.contains(&key) && text.parse::<u64>().is_ok() => text
            .parse::<i64>()
            .map_or_else(|_| text.as_str().into(), Into::into),
        Value::Text(text) => text.as_str().into(),
    }
}

/// a TOML value as a run reads it from the settings file, none for one it refuses.
fn read_as(value: &toml_edit::Value) -> Option<Value> {
    match value {
        toml_edit::Value::String(text) => Some(Value::Text(text.value().clone())),
        toml_edit::Value::Integer(number) => Some(Value::Text(number.value().to_string())),
        toml_edit::Value::Boolean(on) => Some(Value::Text(on.value().to_string())),
        toml_edit::Value::Array(items) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()
            .map(Value::List),
        _ => None,
    }
}

/// puts `key` last in `document`, below the comments that end it, as a file holding only a
/// heading comment has them, so they stay above what is added.
fn appended(document: &mut DocumentMut, key: &str, value: toml_edit::Value) {
    let ending = document.trailing().as_str().unwrap_or_default().to_owned();
    document.insert(key, Item::Value(value));
    if ending.trim().is_empty() {
        return;
    }
    document.set_trailing("");
    if let Some((mut written, _)) = document.get_key_value_mut(key) {
        written.leaf_decor_mut().set_prefix(ending);
    }
}

/// takes `key` out of `document`, moving the comments above it to what follows, so only the key
/// and what trails it on its line go. false when the document has no such key.
fn removed(document: &mut DocumentMut, key: &str) -> bool {
    let keys: Vec<String> = document.iter().map(|(key, _)| key.to_owned()).collect();
    let Some(at) = keys.iter().position(|known| known == key) else {
        return false;
    };
    let Some((gone, _)) = document.remove_entry(key) else {
        return false;
    };
    let above = gone
        .leaf_decor()
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or_default()
        .to_owned();
    if above.trim().is_empty() {
        return true;
    }
    let Some(next) = keys.get(at + 1) else {
        let trailing = document.trailing().as_str().unwrap_or_default();
        let trailing = format!("{above}{trailing}");
        document.set_trailing(trailing);
        return true;
    };
    let decor = match document.get_key_value_mut(next) {
        Some((_, Item::Table(table))) => table.decor_mut(),
        Some((mut key, _)) => {
            let decor = key.leaf_decor_mut();
            let below = prefix(decor);
            decor.set_prefix(format!("{above}{below}"));
            return true;
        }
        None => return true,
    };
    let below = prefix(decor);
    decor.set_prefix(format!("{above}{below}"));
    true
}

fn prefix(decor: &toml_edit::Decor) -> String {
    decor
        .prefix()
        .and_then(toml_edit::RawString::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// replaces the settings file at `path` with `text` in one step, through a new file beside it,
/// with the old file's permissions when `found`, and on unix only its owner's otherwise.
fn write(path: &Path, text: &str, found: bool) -> Result<(), Error> {
    let failed = |err: &io::Error| {
        Error::io(
            format_args!("could not write the settings file {}", path.display()),
            err,
        )
    };
    let folder = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(folder).map_err(|err| failed(&err))?;
    let kept = if found {
        let permissions = fs::metadata(path)
            .map_err(|err| failed(&err))?
            .permissions();
        // the rename would replace a file its mode keeps from writes, which someone set so
        if permissions.readonly() || !may_write(path) {
            return Err(Error::new(
                Kind::LocalIo,
                format!(
                    "the settings file {} is read-only, so it was left as it is",
                    path.display()
                ),
            )
            .help(format!(
                "if it is yours to change, make it writable first, as `chmod u+w {}` does; else set FETCHLOOM_SETTINGS to a settings file of your own",
                crate::suggested::shell_word(&path.to_string_lossy())
            )));
        }
        Some(permissions)
    } else {
        None
    };
    let mut staged = tempfile::Builder::new()
        .prefix(".settings-")
        .suffix(".tmp")
        .tempfile_in(folder)
        .map_err(|err| failed(&err))?;
    staged
        .write_all(text.as_bytes())
        .and_then(|()| staged.as_file().sync_all())
        .map_err(|err| failed(&err))?;
    if let Some(permissions) = kept {
        fs::set_permissions(staged.path(), permissions).map_err(|err| failed(&err))?;
    }
    staged.persist(path).map_err(|err| failed(&err.error))?;
    // the rename is done; a folder that cannot be flushed leaves it to the system to write
    let _ = crate::volume::sync_directory(folder);
    Ok(())
}

/// whether this user may write the file at `path`, as its mode and owner say.
#[cfg(unix)]
fn may_write(path: &Path) -> bool {
    rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_ok()
}

#[cfg(not(unix))]
fn may_write(_: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests;
