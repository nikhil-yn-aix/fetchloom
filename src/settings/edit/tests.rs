use std::ffi::OsString;

use super::*;

/// a settings file at `name` in a fresh folder, holding `text` unless it is none, and the
/// environment that names it.
fn settings(
    name: &str,
    text: Option<&str>,
) -> (
    tempfile::TempDir,
    PathBuf,
    impl Fn(&str) -> Option<OsString>,
) {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join(name);
    if let Some(text) = text {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
    }
    let named = path.clone().into_os_string();
    let var = move |name: &str| (name == "FETCHLOOM_SETTINGS").then(|| named.clone());
    (folder, path, var)
}

/// a check past reading that every settings file passes.
const FINE: Usable<'static> = &|_| Ok(());

fn values(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

/// the values a run reads from the settings file at `path`.
fn read(path: &Path) -> Vec<(String, Value)> {
    parse(path, &fs::read_to_string(path).unwrap()).unwrap()
}

/// the key's comment above it, a comment after its value and the other keys stay, a new key goes
/// last, and the file reads back as written.
#[test]
fn a_set_keeps_comments_and_the_other_keys() {
    let text = "# the lab's settings\n\n# downloads at once\njobs = 8 # the default\nmirrors = [\"/mnt/lab\"]\n";
    let (_folder, path, var) = settings("settings.toml", Some(text));
    let changed = set(&var, "jobs", &values(&["16"]), None, FINE).unwrap();
    assert_eq!(
        (changed.written, changed.created),
        (true, false),
        "{changed:?}"
    );
    assert_eq!(
        changed.before.as_ref().and_then(read_as),
        Some(Value::Text("8".to_owned()))
    );
    set(&var, "mirrors_only", &values(&["true"]), None, FINE).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# the lab's settings\n\n# downloads at once\njobs = 16 # the default\nmirrors = [\"/mnt/lab\"]\nmirrors_only = true\n"
    );
    assert_eq!(
        read(&path),
        [
            ("jobs".to_owned(), Value::Text("16".to_owned())),
            (
                "mirrors".to_owned(),
                Value::List(vec!["/mnt/lab".to_owned()])
            ),
            ("mirrors_only".to_owned(), Value::Text("true".to_owned())),
        ]
    );
}

/// a run would refuse each value, so `set` refuses it with the reader's message, naming the value
/// as given rather than as the file's, which it never was, and writes nothing.
#[test]
fn a_value_runs_refuse_is_refused_with_the_readers_message() {
    let text = "jobs = 8\n";
    let (_folder, path, var) = settings("settings.toml", Some(text));
    for (key, value, message) in [
        (
            "jobs",
            "1000",
            "jobs is \"1000\", which is not a number from 1 to 256".to_owned(),
        ),
        (
            "memory",
            "lots",
            "memory is \"lots\", which is not a size such as 512M or 4G".to_owned(),
        ),
        (
            "mirrors_only",
            "yes",
            "mirrors_only is \"yes\", which is not true or false".to_owned(),
        ),
        (
            "mirrors",
            "ftp://lab.example/data",
            "mirrors holds \"ftp://lab.example/data\", which is no mirror: a mirror is a folder, an http or https address or an S3 bucket, not ftp".to_owned(),
        ),
        (
            "mirrors",
            "s3:lab-data",
            "mirrors holds \"s3:lab-data\", which is no mirror: it names no S3 bucket".to_owned(),
        ),
        (
            "push_to",
            "https://lab.example/data",
            "push_to is \"https://lab.example/data\", which is a web address, and push writes to folders and S3 buckets".to_owned(),
        ),
    ] {
        let error = set(&var, key, &values(&[value]), None, FINE).unwrap_err();
        assert_eq!(error.kind, Kind::Usage, "{key}");
        assert_eq!(error.message, message, "{key}");
        assert_eq!(error.details, ["nothing was saved"], "{key}");
        let help = error.help.as_deref().unwrap_or_default();
        assert!(
            help.contains(&format!("run `fetchloom settings unset {key}`")),
            "{help}"
        );
        assert!(!help.contains("settings file"), "{help}");
        assert_eq!(fs::read_to_string(&path).unwrap(), text, "{key}");
    }
}

/// a list takes every value, each split at commas as its variable is, and they replace the list;
/// a relative folder is taken from the current folder, and a web address or a bucket stays.
#[test]
fn a_list_takes_several_values_and_replaces_the_list() {
    let (folder, path, var) = settings("settings.toml", Some("mirrors = [\"/old\"]\n"));
    let here = folder.path().join("project");
    let lab = folder.path().join("lab").display().to_string();
    let changed = set(
        &var,
        "mirrors",
        &values(&[
            &lab,
            "https://data.lab.example/fl, ./usb",
            "../shared/./mirror",
            "s3://lab-data/fetchloom",
        ]),
        Some(&here),
        FINE,
    )
    .unwrap();
    assert!(changed.written);
    assert_eq!(
        read(&path),
        [(
            "mirrors".to_owned(),
            Value::List(vec![
                lab,
                "https://data.lab.example/fl".to_owned(),
                here.join("usb").display().to_string(),
                folder
                    .path()
                    .join("shared")
                    .join("mirror")
                    .display()
                    .to_string(),
                "s3://lab-data/fetchloom".to_owned(),
            ])
        )]
    );
    let error = set(&var, "jobs", &values(&["8", "16"]), Some(&here), FINE).unwrap_err();
    assert_eq!(error.message, "jobs takes one value, and 2 were given");
    let pairs = values(&["4", "host_jobs", "2"]);
    let error = set(&var, "jobs", &pairs, Some(&here), FINE).unwrap_err();
    assert_eq!(error.message, "jobs takes one value, and 3 were given");
    assert_eq!(
        error.help.as_deref(),
        Some(
            "settings set saves one key at a time, so run `fetchloom settings set jobs 4`, then set the next"
        )
    );
}

/// a leading ~ is the home folder, as a shell takes it outside quotes, never a folder named ~
/// here, and ~ with a name after it is refused; a mirror or push folder that is not there is
/// named, so the change can say so.
#[test]
fn a_tilde_is_the_home_folder_and_a_missing_folder_is_named() {
    let (folder, path, _) = settings("settings.toml", None);
    let home = folder.path().join("home");
    fs::create_dir_all(home.join("lab")).unwrap();
    let (named, at) = (path.clone().into_os_string(), home.clone().into_os_string());
    let var = move |name: &str| match name {
        "FETCHLOOM_SETTINGS" => Some(named.clone()),
        "HOME" | "USERPROFILE" => Some(at.clone()),
        _ => None,
    };
    let here = folder.path().join("work");
    let changed = set(
        &var,
        "mirrors",
        &values(&["~/lab", "~/gone"]),
        Some(&here),
        FINE,
    )
    .unwrap();
    assert_eq!(changed.missing, [home.join("gone")]);
    set(&var, "cache", &values(&["~"]), Some(&here), FINE).unwrap();
    let changed = set(&var, "push_to", &values(&["~/out"]), Some(&here), FINE).unwrap();
    assert_eq!(changed.missing, [home.join("out")]);
    let shown = |path: PathBuf| Value::Text(path.display().to_string());
    assert_eq!(
        read(&path),
        [
            ("cache".to_owned(), shown(home.clone())),
            (
                "mirrors".to_owned(),
                Value::List(vec![
                    home.join("lab").display().to_string(),
                    home.join("gone").display().to_string()
                ])
            ),
            ("push_to".to_owned(), shown(home.join("out"))),
        ]
    );
    let error = set(&var, "push_to", &values(&["~ada/lab"]), Some(&here), FINE).unwrap_err();
    assert_eq!(
        error.message,
        "push_to cannot take the folder ~ada/lab: fetchloom takes ~ for your own home folder alone, not another's"
    );
    let changed = set(&var, "cache", &values(&["./gone"]), Some(&here), FINE).unwrap();
    assert!(changed.missing.is_empty(), "a run makes its cache");
}

/// a whole number is written as a number, a switch as true or false, and a folder or a level as
/// text even when it is all digits.
#[test]
fn each_value_is_written_as_toml_reads_it() {
    let (folder, path, var) = settings("settings.toml", None);
    for (key, value) in [
        ("ask_above", "1000000B"),
        ("memory", "512M"),
        ("mirrors_only", "1"),
        ("resources", "lean"),
        ("push_to", "2024"),
        ("jobs", "+5"),
    ] {
        set(&var, key, &values(&[value]), Some(folder.path()), FINE).unwrap();
    }
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.ends_with("\njobs = 5\n"), "{text}");
    assert!(
        text.starts_with(
            "ask_above = \"1000000B\"\nmemory = \"512M\"\nmirrors_only = true\nresources = \"lean\"\npush_to = "
        ),
        "{text}"
    );
    let pushed = read(&path).into_iter().find(|(key, _)| key == "push_to");
    assert_eq!(
        pushed,
        Some((
            "push_to".to_owned(),
            Value::Text(folder.path().join("2024").display().to_string())
        ))
    );
}

/// setting a key to what the file holds writes nothing; taking out a key it does not hold either.
#[test]
fn a_change_to_what_is_there_writes_nothing() {
    let (_folder, path, var) = settings("settings.toml", Some("jobs = \"16\"\n"));
    let changed = set(&var, "jobs", &values(&["16"]), None, FINE).unwrap();
    assert!(!changed.written && !changed.created, "{changed:?}");
    let changed = unset(&var, "mirrors", FINE).unwrap();
    assert!(!changed.written, "{changed:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "jobs = \"16\"\n");
}

/// the key and the rest of its line go, its comments move to what follows, and a key the file
/// holds that is no setting can be taken out, as one that stops every run.
#[test]
fn unset_takes_the_key_out_and_keeps_the_comments() {
    let text =
        "# the lab's settings\njobs = 16 # fast\nmirrors = [\"/mnt/lab\"]\n# a typo\njbos = 4\n";
    let (_folder, path, var) = settings("settings.toml", Some(text));
    let changed = unset(&var, "jobs", FINE).unwrap();
    assert!(changed.written && changed.after.is_none(), "{changed:?}");
    assert_eq!(
        changed.before.as_ref().and_then(read_as),
        Some(Value::Text("16".to_owned()))
    );
    unset(&var, "jbos", FINE).unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "# the lab's settings\nmirrors = [\"/mnt/lab\"]\n# a typo\n"
    );
    // a table the file holds is told as what it held, not as a key that was never set
    fs::write(&path, "jobs = 2\n\n[mystuff]\nx = 1\n").unwrap();
    let table = unset(&var, "mystuff", FINE).unwrap();
    assert!(table.written, "{table:?}");
    let held = table
        .before
        .map(|value| value.decorated("", "").to_string());
    assert_eq!(held.as_deref(), Some("{ x = 1 }"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "jobs = 2\n");
}

/// an unknown key is refused with the nearest setting, or the variable that sets it when only a
/// variable does, before the file is read.
#[test]
fn an_unknown_key_names_the_nearest_setting() {
    let (_folder, path, var) = settings("settings.toml", None);
    for (key, message, help) in [
        ("jbos", "jbos is not a setting", "did you mean `jobs`?"),
        ("JOBS", "JOBS is not a setting", "did you mean `jobs`?"),
        (
            "FETCHLOOM_PUSH_TO",
            "FETCHLOOM_PUSH_TO is not a setting",
            "did you mean `push_to`?",
        ),
    ] {
        for error in [
            set(&var, key, &values(&["1"]), None, FINE).unwrap_err(),
            unset(&var, key, FINE).unwrap_err(),
        ] {
            assert_eq!(error.kind, Kind::Usage, "{key}");
            assert_eq!(error.message, message, "{key}");
            assert_eq!(error.help.as_deref(), Some(help), "{key}");
        }
    }
    // a key only a variable sets is set, or unset, where that variable is
    let message = "rate is set by FETCHLOOM_RATE, not the settings file";
    for (error, help) in [
        (
            set(&var, "rate", &values(&["1M"]), None, FINE).unwrap_err(),
            "set FETCHLOOM_RATE where fetchloom runs, as in your shell's profile",
        ),
        (
            unset(&var, "rate", FINE).unwrap_err(),
            "unset FETCHLOOM_RATE where it is set, as in your shell's profile",
        ),
    ] {
        assert_eq!(error.message, message);
        assert_eq!(error.help.as_deref(), Some(help));
    }
    assert!(!path.exists());
}

/// text that is not TOML is refused as a run refuses it, so nothing half-read is written back.
#[test]
fn a_file_that_is_not_toml_is_left_alone() {
    let text = "jobs = 8\nmirrors = [\n";
    let (_folder, path, var) = settings("settings.toml", Some(text));
    let error = set(&var, "jobs", &values(&["16"]), None, FINE).unwrap_err();
    assert!(
        error.message.starts_with(&format!(
            "the settings file {} is not valid TOML at line 2",
            path.display()
        )),
        "{}",
        error.message
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

/// a missing file is made, with its folder, readable only by its owner; one that is there keeps
/// its permissions, and a link to it stays a link.
#[cfg(unix)]
#[test]
fn the_file_is_made_when_missing_and_keeps_its_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let (_folder, path, var) = settings("new/fetchloom/settings.toml", None);
    let changed = set(&var, "jobs", &values(&["16"]), None, FINE).unwrap();
    assert!(changed.created && changed.written, "{changed:?}");
    assert_eq!(fs::read_to_string(&path).unwrap(), "jobs = 16\n");
    assert_eq!(mode(&path), 0o600);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    set(&var, "jobs", &values(&["8"]), None, FINE).unwrap();
    assert_eq!(mode(&path), 0o644);

    let (folder, target, _) = settings("dotfiles/settings.toml", Some("jobs = 8\n"));
    let link = folder.path().join("settings.toml");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let named = link.clone().into_os_string();
    let var = move |name: &str| (name == "FETCHLOOM_SETTINGS").then(|| named.clone());
    let changed = set(&var, "jobs", &values(&["4"]), None, FINE).unwrap();
    assert_eq!(changed.path, fs::canonicalize(&target).unwrap());
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read_to_string(&target).unwrap(), "jobs = 4\n");
}

/// a read-only settings file, as a lab marks the file it shares, is refused and left as it was,
/// though the folder would let a new file take its place.
#[test]
fn a_read_only_file_is_refused_and_left_as_it_was() {
    let text = "# the lab's, do not edit\njobs = 4\n";
    let (_folder, path, var) = settings("settings.toml", Some(text));
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    let error = set(&var, "jobs", &values(&["12"]), None, FINE).unwrap_err();
    assert_eq!(
        error.message,
        format!(
            "the settings file {} is read-only, so it was left as it is",
            path.display()
        )
    );
    assert!(
        error.help.as_deref().unwrap().contains("chmod u+w"),
        "{error:?}"
    );
    assert!(unset(&var, "jobs", FINE).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
    assert!(fs::metadata(&path).unwrap().permissions().readonly());
}

/// changes made at once each read the file the one before wrote, so every key stays, and a file
/// holding only a heading comment keeps it at the top.
#[test]
fn changes_at_once_keep_every_key() {
    let (_folder, path, var) = settings("settings.toml", Some("# race test\n"));
    let changes = [
        ("jobs", "3"),
        ("host_jobs", "2"),
        ("retries", "5"),
        ("timeout", "60"),
        ("ask_above", "20G"),
        ("cache_budget", "50G"),
        ("archive_size", "2G"),
        ("memory", "512M"),
        ("most_files", "200000"),
        ("archive_ratio", "100"),
    ];
    let start = std::sync::Barrier::new(changes.len());
    std::thread::scope(|scope| {
        for (key, value) in changes {
            let (start, var) = (&start, &var);
            scope.spawn(move || {
                start.wait();
                set(var, key, &values(&[value]), None, FINE).unwrap();
            });
        }
    });
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# race test\n"), "{text}");
    let kept: Vec<String> = read(&path).into_iter().map(|(key, _)| key).collect();
    for (key, _) in changes {
        assert!(
            kept.iter().any(|kept| kept == key),
            "{key} was lost: {text}"
        );
    }
}

/// a variable overrides a key while it is set, and an empty one only where it clears the key.
#[test]
fn a_set_variable_overrides_its_key() {
    let given = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(set, _)| *set == name)
                .map(|(_, value)| OsString::from(value))
        }
    };
    assert_eq!(
        overridden(&given(&[("FETCHLOOM_JOBS", "4")]), "jobs"),
        Some("FETCHLOOM_JOBS")
    );
    assert_eq!(overridden(&given(&[("FETCHLOOM_JOBS", "")]), "jobs"), None);
    assert_eq!(
        overridden(&given(&[("FETCHLOOM_MIRRORS", "")]), "mirrors"),
        Some("FETCHLOOM_MIRRORS")
    );
    assert_eq!(overridden(&given(&[]), "jobs"), None);
}
