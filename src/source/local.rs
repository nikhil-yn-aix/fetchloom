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

/// what became of an output's folder since it was pinned, as the ways on from it differ. `yes`
/// when the add that pins what it holds must say yes ahead, as a run that cannot ask a person
/// does, and `reach` whether a `sync --force` here can write the pinned files it lacks or holds
/// changed, so none is offered that would put nothing back.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Since<'a> {
    /// it holds other files than the pin.
    Changed { yes: bool, reach: Reach },
    /// nothing is at it.
    Gone { reach: Reach },
    /// nothing is at it, and the folder data.toml named before, `from` as a command names it,
    /// still holds the pin.
    Left { from: &'a str, reach: Reach },
    /// it holds other files than the pin, and this machine placed none of the output there, as a
    /// clone whose data.toml may name a folder of code: nothing offered deletes them.
    Unplaced { yes: bool },
    /// it holds other files than the pin, and git tracks `files` of them, which `--force` leaves as
    /// they are; `yes` as for [`Since::Changed`].
    Tracked { files: &'a [String], yes: bool },
    /// files of it git tracks differ from the pin only in their line ends, `ends` here, in the
    /// folder whose files `.gitattributes` names by `pattern`, and when others differ too, `rest`
    /// says how, as the ways on from them once those line ends are fixed.
    LineEnds {
        pattern: &'a str,
        ends: &'a LineEnds,
        rest: Option<&'a Since<'a>>,
    },
}

/// whether a sync here can write the pinned files an output's folder lacks or holds changed, as a
/// `sync --force` would.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reach {
    /// a cache here holds each, or a mirror set may.
    Reachable,
    /// no cache here holds one of them and no mirror is set, so no sync writes it.
    Lost,
    /// no cache here holds one of them, and the mirrors set lacked one when a sync here asked, so
    /// none writes it until a machine that has them pushes them there.
    Lacked,
}

/// the files git tracks in an output's folder that differ from the pin only in their line ends, as
/// git converts them when it checks a file out or keeps it, each by its path in the project and in
/// path order, by the kind they hold where the pin holds the other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineEnds {
    /// CRLF where the pin has LF, as a checkout writes them where git converts line ends.
    pub(crate) crlf: Vec<String>,
    /// the git commands that write those of `crlf` whose blob in git's index and last commit is
    /// the pinned bytes fresh from that commit, if any: unstaging them first, so git holds no
    /// record of the file it could skip one by.
    pub(crate) restore: Option<[String; 2]>,
    /// those of `crlf` whose blob is not, which no checkout gives back.
    pub(crate) stale: Vec<String>,
    /// LF where the pin has CRLF, as git keeps the CRLF file it was given where it converts them.
    pub(crate) lf: Vec<String>,
}

impl LineEnds {
    /// each file it holds, by its path in the project.
    pub(crate) fn files(&self) -> impl Iterator<Item = &str> {
        self.crlf.iter().chain(&self.lf).map(String::as_str)
    }
}

/// that files git tracks in the folder of the output `name`, whose files `.gitattributes` names by
/// `pattern`, differ from the pin only in their line ends, as `ends` holds them, with the fix: git
/// keeping that folder's bytes as they are, committed, then a fresh checkout of just the files
/// whose blob git keeps is the pinned bytes, and LF line ends for the rest, which no command writes
/// for them, or, where git kept them as LF, a commit of the pinned bytes where they are.
fn line_ends_said(name: &str, pattern: &str, ends: &LineEnds) -> String {
    let named = |files: &[String]| match files {
        [one] => one.clone(),
        _ => format!(
            "{} files git tracks in {name}'s folder, such as {},",
            files.len(),
            files.first().map_or("", String::as_str)
        ),
    };
    let forms = |files: &[String]| {
        if files.len() == 1 {
            ("differs", "its", "it")
        } else {
            ("differ", "their", "them")
        }
    };
    let rule =
        format!("add `{pattern} -text` to the .gitattributes beside data.toml and commit it");
    let mut said = Vec::new();
    if !ends.crlf.is_empty() {
        let (these, (differ, its, them)) = (named(&ends.crlf), forms(&ends.crlf));
        said.push(match &ends.restore {
            Some([unstage, checkout]) if ends.stale.is_empty() => format!(
                "{these} {differ} from the pin only in {its} line ends, made CRLF by git's checkout; {rule}, then `{unstage}` and `{checkout}` write the bytes git keeps, which the pin holds"
            ),
            Some([unstage, checkout]) => format!(
                "{these} differ from the pin only in their line ends, made CRLF by git's checkout; {rule}, then `{unstage}` and `{checkout}` write the bytes git keeps for those it keeps as pinned, and give {} LF line ends, as git's staged or committed copy is not the pinned one",
                named(&ends.stale)
            ),
            None => format!(
                "{these} {differ} from the pin only in {its} line ends, made CRLF by git's checkout, but git's staged or committed copy is not the pinned one, so no checkout brings the pin back; {rule}, then give {them} LF line ends"
            ),
        });
    }
    if !ends.lf.is_empty() {
        let (these, (differ, its, _)) = (named(&ends.lf), forms(&ends.lf));
        let (literal, paths) = git_paths(&ends.lf);
        said.push(format!(
            "{these} {differ} from the pin only in {its} line ends, which git keeps as LF where the pin has CRLF; where the pinned files are, {rule}, then `git {literal}add --renormalize -- {paths}` and commit, so a pull brings them as pinned"
        ));
    }
    said.join("; ")
}

/// `paths`, files in the project, as the words a git command a person runs names them by: each a
/// word of the shell, with `--literal-pathspecs ` to give git ahead of the command where one holds
/// a character git reads as a pattern or a leading `:`, which would name other files too.
pub(crate) fn git_paths(paths: &[String]) -> (&'static str, String) {
    let patterned = paths
        .iter()
        .any(|path| path.contains(['*', '?', '[', '\\']) || path.starts_with(':'));
    let words: Vec<String> = paths
        .iter()
        .map(|path| crate::suggested::shell_word(path).into_owned())
        .collect();
    let literal = if patterned {
        "--literal-pathspecs "
    } else {
        ""
    };
    (literal, words.join(" "))
}

/// why `sync --force` leaves a file git tracks in an output's folder as it is, in every line that
/// tells of one.
pub(crate) const FORCE_KEEPS_GIT: &str =
    "`sync --force` never deletes or overwrites a file git tracks that a plain sync keeps";

/// that `sync --force` leaves `files`, which git tracks in the folder of the output `name`.
pub(crate) fn kept_by_git(name: &str, files: &[String]) -> String {
    match files {
        [one] => format!("{one} stays, as git tracks it, and {FORCE_KEEPS_GIT}"),
        _ => format!(
            "{} files git tracks stay in {name}'s folder, such as {}, as {FORCE_KEEPS_GIT}",
            files.len(),
            files.first().map_or("", String::as_str)
        ),
    }
}

/// the ways on from files git tracks that differ from the pin of the output `name`, whose folder
/// a command names `shown`: pinning what it holds now, saying yes ahead for `yes`, or bringing
/// git's copy to the pin.
pub(crate) fn git_ways(name: &str, shown: &str, yes: bool) -> String {
    format!(
        "`{}` pins what it holds now, or make git's copy match data.lock",
        pinned(name, shown, yes)
    )
}

/// the ways on from the other changes in the folder of the output `name`, `shown` as a command
/// names it, beside files that differ from its pin only in their line ends, once those are fixed,
/// as `since` tells them: such a file shows the folder is the output's, so `at` is not in doubt,
/// and the lines that tell of the files git tracks say they stay.
fn after_line_ends(name: &str, shown: &str, since: Since<'_>) -> String {
    match since {
        Since::Unplaced { yes } => format!("`{}` pins what it holds now", pinned(name, shown, yes)),
        Since::Tracked { yes, .. } => git_ways(name, shown, yes),
        since => ways_on(name, shown, since),
    }
}

/// the ways on for the output `name`, whose folder, `shown` as a command names it, is not as it
/// was pinned: what it holds now pinned again, or the files it pins put back, which only its
/// pipeline makes again once they are lost; a folder that is gone is moved from where it was
/// left, else put back or named where it went.
pub(crate) fn ways_on(name: &str, shown: &str, since: Since<'_>) -> String {
    let force = crate::suggested::command(&format!("sync --force {name}"));
    let pin = |yes: bool| pinned(name, shown, yes);
    match since {
        Since::Changed {
            yes,
            reach: Reach::Reachable,
        } => format!(
            "`{}` pins what it holds now, or `{force}` makes it hold just the files {name} pins",
            pin(yes)
        ),
        Since::Changed { yes, reach } => format!(
            "{}, so {}{}",
            unreached(&[name]),
            reach_ways(&format!("`{}`", pin(yes))),
            way_back(
                reach,
                &format!("`{force}` makes it hold just the files {name} pins")
            )
        ),
        Since::Gone {
            reach: Reach::Reachable,
        } => format!(
            "`{force}` puts back the files {name} pins, or if you moved it, {}",
            set_moved(name, true)
        ),
        Since::Gone { reach } => format!(
            "{}, so {}, or if you moved it, {}{}",
            unreached(&[name]),
            reach_ways(&format!("`{}`", pin(false))),
            set_moved(name, true),
            way_back(reach, &format!("`{force}` puts back the files {name} pins"))
        ),
        Since::Left { from, reach } => {
            let moved = format!(
                "{} moves it where data.toml puts it now",
                crate::suggested::moving(from, shown)
            );
            if reach == Reach::Reachable {
                format!("{moved}, or `{force}` puts back the files {name} pins")
            } else {
                moved
            }
        }
        Since::Unplaced { yes } => format!(
            "no file there is one fetchloom placed on this machine, so check that `at` for {name} in data.toml names its folder; `{}` pins what it holds now",
            pin(yes)
        ),
        Since::Tracked { files, yes } => format!(
            "{}; {}",
            kept_by_git(name, files),
            git_ways(name, shown, yes)
        ),
        Since::LineEnds {
            pattern,
            ends,
            rest,
        } => {
            let said = line_ends_said(name, pattern, ends);
            match rest {
                Some(rest) => format!(
                    "{said}; after that, {}",
                    after_line_ends(name, shown, *rest)
                ),
                None => said,
            }
        }
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

/// [`pinned_yes`] for `yes`, else [`pinned_again`].
fn pinned(name: &str, at: &str, yes: bool) -> String {
    if yes {
        pinned_yes(name, at)
    } else {
        pinned_again(name, at)
    }
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
/// whose pinned files no cache or mirror here holds, in the words every command tells it with.
pub(crate) fn out_of_reach(made: &[(&str, &str)]) -> Error {
    let names: Vec<&str> = made.iter().map(|(name, _)| *name).collect();
    let pin = match made {
        [(name, at)] => format!("`{}`", pinned_again(name, at)),
        _ => format!(
            "`{}` for each",
            crate::suggested::command("add <folder> --as <name>")
        ),
    };
    Error::new(Kind::ReferenceUnresolved, unreached(&names)).help(reach_ways(&pin))
}

/// that no cache or mirror here holds the files the outputs `names` pin, which says nothing of
/// the machine that made them.
fn unreached(names: &[&str]) -> String {
    let pin = if names.len() == 1 { "pins" } else { "pin" };
    format!(
        "the files {} {pin} are in no cache or mirror here",
        crate::units::listed(names, "and")
    )
}

/// the two ways on from pinned files [`unreached`] names, `pin` the add that pins what a pipeline
/// makes: a machine that has them pushes them to a mirror added here, or the pipeline runs here.
fn reach_ways(pin: &str) -> String {
    format!(
        "push them from a machine that has them with `{}` and add that mirror here with `{}`, or run your pipeline here, then {pin}",
        crate::suggested::command("push --to <mirror>"),
        crate::suggested::command("settings set mirrors <mirror>"),
    )
}

/// what `sync --force` does once the files are pushed, `what` saying it, when the mirrors set
/// lacked them as `reach` says: no sync asks them again for a folder it leaves whole, so the way
/// back is named rather than offered.
fn way_back(reach: Reach, what: &str) -> String {
    if reach == Reach::Lacked {
        format!("; once the files are pushed, {what}")
    } else {
        String::new()
    }
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
    fn a_told_git_command_takes_a_path_git_would_read_as_a_pattern_literally() {
        for (path, literal) in [
            ("results/a.csv", false),
            ("my results/a b.csv", false),
            ("results/[run]/a.csv", true),
            ("out/*.csv", true),
            ("out/a?.csv", true),
            ("out/a\\b.csv", true),
            (":out/a.csv", true),
        ] {
            let (flag, _) = git_paths(&[String::from("results/z.csv"), path.to_owned()]);
            assert_eq!(flag == "--literal-pathspecs ", literal, "{path}");
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
