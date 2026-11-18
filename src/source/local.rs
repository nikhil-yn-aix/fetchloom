//! text that names something on this machine: a path such as `./x.csv` or `C:\data`, a `file:`
//! link, or a bare word naming a file here. fetchloom never searches for these or sends them
//! anywhere; `fetchloom add <path>` imports them under the ref [`IMPORTED`].

use std::path::Path;

use crate::error::{Error, Kind};

/// the ref of a dataset imported from files on a machine, in data.toml and data.lock alike. it
/// names no path, which would mean nothing on another machine and say where the files were.
pub(crate) const IMPORTED: &str = "local";

/// whether `reference`, as data.toml or data.lock writes it, is that of an import.
pub(crate) fn is_imported(reference: &str) -> bool {
    reference == IMPORTED
}

/// the refusal to resolve an import: no source lists its files.
pub(crate) fn unlisted() -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        "\"local\" names files imported from a folder, which no source can list again",
    )
    .help(format!(
        "run `{}` where the files are",
        crate::suggested::command("add <folder> --as <name>")
    ))
}

/// the words of the add that pins the output `name` from `folder`, its folder as a command run
/// here names it, written as a path, as `./data/processed`, so add never takes the folder for
/// words to search with, even once it is gone.
pub(crate) fn output_pin(name: &str, folder: &str) -> String {
    let path = if is_path(folder) {
        folder.to_owned()
    } else {
        format!("./{folder}")
    };
    format!("add {} --as {name}", crate::suggested::shell_word(&path))
}

/// what became of an output's folder since it was pinned, as the ways on from it differ.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Since<'a> {
    /// it holds other files than the pin; `yes` when the add that pins them must say yes ahead,
    /// as a run that cannot ask a person does.
    Changed { yes: bool },
    /// nothing is at it.
    Gone,
    /// nothing is at it, and the folder data.toml named before, as a command names it, still
    /// holds the pin.
    Left(&'a str),
    /// it holds other files than the pin, and this machine placed none of the output there, as a
    /// clone whose data.toml may name a folder of code: nothing offered deletes them.
    Unplaced,
}

/// the ways on for the output `name`, whose folder, `shown` as a command names it, is not as it
/// was pinned: what it holds now pinned again, or the files it pins put back; a folder that is
/// gone is moved from where it was left, else put back or named where it went.
pub(crate) fn ways_on(name: &str, shown: &str, since: Since<'_>) -> String {
    let force = crate::suggested::command(&format!("sync --force {name}"));
    match since {
        Since::Changed { yes } => format!(
            "`{}` pins what it holds now, or `{force}` makes it hold just the files {name} pins",
            if yes {
                pinned_yes(name, shown)
            } else {
                pinned_again(name, shown)
            }
        ),
        Since::Gone => format!(
            "`{force}` puts back the files {name} pins, or if you moved it, {}",
            set_moved(name, true)
        ),
        Since::Left(from) => format!(
            "`{}` moves it where data.toml puts it now, or `{force}` puts back the files {name} pins",
            crate::suggested::moving(from, shown)
        ),
        Since::Unplaced => format!(
            "no file there is one fetchloom placed on this machine, so check that `at` for {name} in data.toml names its folder; `{}` pins what it holds now",
            pinned_again(name, shown)
        ),
    }
}

/// the step that names the folder of `name` a person moved on purpose: `at` for an output, whose
/// folder data.toml names whole, else `path`, inside the data folder.
pub(crate) fn set_moved(name: &str, output: bool) -> String {
    let key = if output { "at" } else { "path" };
    format!("set {key} = \"<new folder>\" for {name} in data.toml")
}

/// the command that pins what the folder of the output `name` holds now, `at` as a command names
/// it, keeping its table in data.toml as written.
fn pinned_again(name: &str, at: &str) -> String {
    crate::suggested::command(&output_pin(name, at))
}

/// [`pinned_again`] saying yes ahead, for a run that cannot ask a person.
pub(crate) fn pinned_yes(name: &str, at: &str) -> String {
    crate::suggested::command(&format!("{} --yes", output_pin(name, at)))
}

/// the refusal of a changed data.toml entry of `name`: an import, which only importing the files
/// again can pin other ones of, or an output, whose folder `at`, as a command names it, is pinned
/// again by the new entry.
pub(crate) fn changed(name: &str, at: Option<&str>) -> Error {
    if let Some(at) = at {
        return Error::new(
            Kind::ReferenceUnresolved,
            format!(
                "{name} is an output, so a change to its entry in data.toml takes a new pin of its folder"
            ),
        )
        .help(format!(
            "run `{}` to pin what it holds by the new entry, or put datasets.{name} back as it was",
            pinned_again(name, at)
        ));
    }
    Error::new(
        Kind::ReferenceUnresolved,
        format!("{name} was imported from files, so a change to its entry in data.toml cannot pin other files"),
    )
    .help(format!(
        "put datasets.{name} back as it was, or run `{}`, then `{}` where the files are",
        crate::suggested::command(&format!("remove {name}")),
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the refusal of `name` that data.lock holds no pin of: an import, which nothing tells the files
/// of, not even a rename, since every import's table reads the same, or an output, whose folder
/// `at`, as a command names it, holds them.
pub(crate) fn unpinned(name: &str, at: Option<&str>) -> Error {
    if let Some(at) = at {
        return Error::new(
            Kind::ReferenceUnresolved,
            format!("{name} is an output, and data.lock holds no pin of it"),
        )
        .help(format!(
            "run `{}` to pin what it holds",
            pinned_again(name, at)
        ));
    }
    Error::new(
        Kind::ReferenceUnresolved,
        format!("{name} was imported from files, and data.lock holds no pin of them"),
    )
    .help(format!(
        "give it back its old name if you renamed it in data.toml, restore data.lock from git, or import the files again with `{}` where they are",
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the refusal to upgrade `name`: an import, or an output, whose folder `at`, as a command names
/// it, is pinned again only by the add that read it.
pub(crate) fn no_newer(name: &str, at: Option<&str>) -> Error {
    if let Some(at) = at {
        return Error::new(
            Kind::Usage,
            format!("{name} is an output, so no source has a newer version of it"),
        )
        .help(format!(
            "run `{}` to pin what it holds now",
            pinned_again(name, at)
        ));
    }
    Error::new(
        Kind::Usage,
        format!("{name} was imported from files, and no source has a newer version of them"),
    )
    .help(format!(
        "to pin other files, run `{}`, then `{}` where they are",
        crate::suggested::command(&format!("remove {name}")),
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the error for files of `name`, an import, that this machine's cache lacks, as a clone's does,
/// and the mirrors too when `mirrored`: no link fetches them. `folder` is where they go.
pub(crate) fn imported_elsewhere(name: &str, folder: &str, mirrored: bool) -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        format!(
            "{name} was imported from files, and {}",
            lacking(mirrored, "them")
        ),
    )
    .help(format!(
        "a folder mirror that holds them brings them back, as `{}`; else copy them to {folder} by hand, then run `{}`, or run `{}` where they are",
        crate::suggested::command(&format!("sync {name} --mirror <folder>")),
        crate::suggested::command("sync"),
        crate::suggested::command(&format!("add <folder> --as {name}"))
    ))
}

/// the error for the outputs of `made`, each with its folder as a command run here names it,
/// whose files this machine's cache lacks, and the mirrors too when `mirrored`, as a clone's: git
/// carries data.lock, never the bytes, which only the machine that made them has.
pub(crate) fn made_elsewhere(made: &[(&str, &str)], mirrored: bool) -> Error {
    let names: Vec<&str> = made.iter().map(|(name, _)| *name).collect();
    let (was, files, pin) = match made {
        [(name, at)] => ("was", "its files", format!("`{}`", pinned_again(name, at))),
        _ => (
            "were",
            "their files",
            format!(
                "`{}` for each",
                crate::suggested::command("add <folder> --as <name>")
            ),
        ),
    };
    let message = format!(
        "{} {was} made on another machine, and {}",
        crate::units::listed(&names, "and"),
        lacking(mirrored, files)
    );
    Error::new(Kind::ReferenceUnresolved, message).help(format!(
        "push them from the machine that made them with `{}` and add that mirror here with `{}`, or run your pipeline here, then {pin}",
        crate::suggested::command("push --to <mirror>"),
        crate::suggested::command("settings set mirrors <mirror>"),
    ))
}

/// where files no link fetches are not, `files` naming them: this machine's cache, and every
/// mirror too when `mirrored`, as a run that asked them knows.
fn lacking(mirrored: bool, files: &str) -> String {
    if mirrored {
        format!("neither this machine's cache nor a mirror holds {files}")
    } else {
        format!("this machine's cache does not hold {files}")
    }
}

/// [`imported_elsewhere`] for several imports, `names`, whose files this machine lacks, in one
/// error, so a teammate supplies them all in one round rather than learn of one a run.
pub(crate) fn several_imported_elsewhere(names: &[&str], mirrored: bool) -> Error {
    Error::new(
        Kind::ReferenceUnresolved,
        format!(
            "{} were imported from files, and {}",
            crate::units::listed(names, "and"),
            lacking(mirrored, "them")
        ),
    )
    .help(format!(
        "copy each one's files to its folder by hand, then run `{}`, or run `{}` where they are",
        crate::suggested::command("sync"),
        crate::suggested::command("add <folder> --as <name>")
    ))
}

/// whether `text` is written as a path on this machine, from the root, `.`, `..`, `~`, a drive
/// or `file:`. it reads the text alone, never the disk, so data.toml means the same everywhere.
pub(crate) fn is_path(text: &str) -> bool {
    let text = text.trim_start();
    let relative = [".", ".."].iter().any(|dots| {
        text.strip_prefix(dots)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '\\']))
    });
    // no reference prefix has one letter, so a letter and a colon can only be a drive
    let drive = matches!(text.as_bytes(), [letter, b':', ..] if letter.is_ascii_alphabetic());
    text.starts_with(['/', '\\', '~'])
        || relative
        || drive
        || text
            .get(..5)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file:"))
}

/// whether `word`, though [`is_path`] does not read it as a path, is still written as one: it ends
/// with `/`, holds two `/` or a `\`, or its last name ends with a file type, as `visits.csv` does.
/// a link, an id and a DOI are none of these, and such a word is never sent off as search words.
pub(crate) fn reads_as_path(word: &str) -> bool {
    let word = word.trim();
    let last = word.rsplit(['/', '\\']).next().unwrap_or_default();
    !word.contains(':')
        && super::doi::Doi::bare(word).is_none()
        && (word.ends_with('/')
            || word.matches('/').count() > 1
            || word.contains('\\')
            || crate::name::has_file_type(last))
}

/// what `word` names in `folder`, `file` or `folder`, when it names something there, so a local
/// file's name is never sent off as search words.
pub(crate) fn here(word: &str, folder: &Path) -> Option<&'static str> {
    let path = folder.join(word);
    std::fs::symlink_metadata(&path).ok()?;
    Some(if path.is_dir() { "folder" } else { "file" })
}

/// the refusal for `text`, a path on this machine given where a reference belongs.
pub(crate) fn refused(text: &str) -> Error {
    Error::new(
        Kind::ReferenceInvalid,
        format!(
            "\"{}\" is a path on this machine, and fetchloom downloads data from links, DOIs and ids",
            crate::shown::mask_query(text.trim())
        ),
    )
    .help("give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_on_this_machine_are_never_search_words() {
        for (text, path) in [
            ("/home/ada/flowers.csv", true),
            ("/", true),
            ("./flowers.csv", true),
            ("../flowers", true),
            (".", true),
            ("..", true),
            ("~", true),
            ("~/Downloads/flowers.csv", true),
            ("~ada/flowers.csv", true),
            (".\\flowers.csv", true),
            ("..\\flowers", true),
            ("\\\\lab\\share\\flowers.csv", true),
            ("C:\\data\\flowers.csv", true),
            ("c:/data", true),
            ("D:", true),
            ("d:flowers.csv", true),
            ("file:///etc/hostname", true),
            ("FILE:/etc/hostname", true),
            ("file://lab/share/flowers.csv", true),
            ("  ./flowers.csv", true),
            ("iris", false),
            ("fashion mnist", false),
            ("uciml/iris", false),
            (".hidden", false),
            ("...", false),
            ("zenodo:3608990", false),
            ("hf:datasets/scikit-learn/iris", false),
            ("https://example.org/flowers.csv", false),
            ("s3://bucket/flowers/", false),
            ("10.5281/zenodo.3608990", false),
            ("doi:10.5281/zenodo.3608990", false),
            ("", false),
        ] {
            assert_eq!(is_path(text), path, "{text:?}");
            assert!(
                !(path && crate::source::search::is_search(text)),
                "{text:?} would be searched for"
            );
        }
    }

    #[test]
    fn a_path_is_refused_as_a_reference_with_what_to_give_instead() {
        for text in [
            "./flowers.csv",
            "~/Downloads/flowers.csv",
            "C:\\data\\flowers.csv",
            "file:///etc/hostname",
        ] {
            let error = crate::source::Reference::parse(text).unwrap_err();
            assert_eq!(error.kind, crate::error::Kind::ReferenceInvalid, "{text}");
            assert_eq!(
                error.message,
                format!(
                    "\"{text}\" is a path on this machine, and fetchloom downloads data from links, DOIs and ids"
                )
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "give the link, DOI or id where the data is published, such as https://zenodo.org/records/3608990"
                )
            );
        }
    }
}
