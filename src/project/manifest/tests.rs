use std::fmt::Write as _;

use super::*;

thread_local! {
    static READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn reading() {
    READS.set(READS.get() + 1);
}

/// how many times this thread has read a data.toml's text whole, to edit or read it.
pub(crate) fn reads() -> usize {
    READS.get()
}

fn parse(text: &str) -> Manifest {
    Manifest::parse(text).unwrap()
}

fn fails(text: &str) -> Error {
    Manifest::parse(text).unwrap_err()
}

#[test]
fn a_full_manifest_reads_with_defaults() {
    let manifest = parse(
        "\u{feff}[project]\ndata_dir = \"inputs/\"\n\n[datasets.eeg]\nref = \"osf:abc12\"\nselect = [\"**/*.edf\", \"participants.tsv\"]\nexclude = \"**/derivatives/**\"\n\n[datasets.weights]\nref = \" https://example.org/w.bin \"\nextract = false\ncopy = true\npath = \"models/w\"\n",
    );
    assert_eq!(manifest.data_dir, "inputs");
    let eeg = &manifest.datasets["eeg"];
    let source = &eeg.source;
    assert_eq!(source.reference, "osf:abc12");
    assert_eq!(
        source.select.iter().map(Glob::as_str).collect::<Vec<_>>(),
        ["**/*.edf", "participants.tsv"]
    );
    assert_eq!(source.exclude.len(), 1);
    assert!(source.extract && !eeg.copy && eeg.path.is_none());
    assert_eq!(
        eeg.lines["select"],
        "data.toml:6   select = [\"**/*.edf\", \"participants.tsv\"]"
    );
    let weights = &manifest.datasets["weights"];
    assert_eq!(weights.source.reference, "https://example.org/w.bin");
    assert!(!weights.source.extract && weights.copy);
    assert_eq!(weights.path.as_deref(), Some("models/w"));
    assert_eq!(parse("").data_dir, "data");
    assert!(parse("").datasets.is_empty());
    let inline = parse("datasets.iris = { ref = \"zenodo:3608990\" }\n");
    assert_eq!(inline.datasets.len(), 1, "inline tables work too");
    assert!(
        !inline.datasets["iris"].lines.is_empty(),
        "an entry from data.toml always cites its lines"
    );
}

const PETS: &str = "[datasets.pets]\nref = \"https://example.org/pets.zip\"\n";

/// a members table matches paths inside the archive, top folder and all, however it is written.
#[test]
fn members_tables_choose_files_inside_each_archive() {
    let manifest = parse(&format!(
        "{PETS}\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/dog_010.png\", \"pets-v1/**/*.tmp\"]\n\n[datasets.pets.members.\"raw/b.tar.gz\"]\nselect = \"**/*.csv\"\nexclude = \"old/**\"\n"
    ));
    let members = &manifest.datasets["pets"].source.members;
    assert_eq!(
        members.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        ["pets.zip", "raw/b.tar.gz"]
    );
    let zip = members.get("pets.zip").unwrap();
    assert!(zip.keeps("pets-v1/images/cat_001.png"));
    assert!(!zip.keeps("pets-v1/images/dog_010.png"));
    assert!(!zip.keeps("pets-v1/a/b.tmp"));
    assert!(
        zip.keeps("images/dog_010.png"),
        "the top folder is part of it"
    );
    let tar = members.get("raw/b.tar.gz").unwrap();
    assert!(tar.keeps("x/a.csv") && !tar.keeps("x/a.txt") && !tar.keeps("old/a.csv"));
    assert_eq!(
        zip.lines["exclude"],
        "data.toml:5   exclude = [\"pets-v1/images/dog_010.png\", \"pets-v1/**/*.tmp\"]"
    );
    assert_eq!(
        zip.lines["members"],
        "data.toml:4   [datasets.pets.members.\"pets.zip\"]"
    );
    for written in [
        format!("{PETS}members = {{ \"pets.zip\" = {{ exclude = [\"a/x.png\"] }} }}\n"),
        format!("{PETS}members.\"pets.zip\".exclude = \"a/x.png\"\n"),
    ] {
        let manifest = parse(&written);
        let members = &manifest.datasets["pets"].source.members;
        let zip = members.get("pets.zip").unwrap();
        assert!(!zip.keeps("a/x.png") && zip.keeps("a/y.png"), "{written}");
    }
    assert!(
        parse(PETS).datasets["pets"]
            .source
            .members
            .iter()
            .next()
            .is_none()
    );
}

/// editing member filters needs no network, and `sync --locked` still passes.
#[test]
fn member_filters_stay_out_of_the_spec() {
    let plain = parse(PETS);
    let filtered = parse(&format!(
        "{PETS}[datasets.pets.members.\"pets.zip\"]\nexclude = [\"a/x.png\"]\n"
    ));
    let spec = |manifest: &Manifest| crate::project::lock::spec(&manifest.datasets["pets"].source);
    assert_eq!(spec(&plain), spec(&filtered));
}

#[test]
fn members_tables_refuse_what_cannot_apply() {
    for (text, message, line) in [
        (
            format!(
                "{PETS}extract = false\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"a/x.png\"]\n"
            ),
            "members in datasets.pets chooses files inside archives, but extract = false keeps them packed",
            "data.toml:4   [datasets.pets.members.\"pets.zip\"]",
        ),
        (
            format!("{PETS}members = [\"pets.zip\"]\n"),
            "members in datasets.pets must be a table for each archive",
            "data.toml:3   members = [\"pets.zip\"]",
        ),
        (
            format!("{PETS}members.\"pets.zip\" = \"a/x.png\"\n"),
            "members.\"pets.zip\" in datasets.pets must be a table with select or exclude",
            "data.toml:3   members.\"pets.zip\" = \"a/x.png\"",
        ),
        (
            format!("{PETS}[datasets.pets.members.\"pets.zip\"]\nexclude = 7\n"),
            "exclude in data.toml must be a pattern or a list of patterns",
            "data.toml:4   exclude = 7",
        ),
        (
            format!("{PETS}[datasets.pets.members.\"pets.zip\"]\nexclude = [\"a\\\\x.png\"]\n"),
            "the exclude pattern \"a\\x.png\" has a \\. patterns use / between folders",
            "data.toml:4   exclude = [\"a\\\\x.png\"]",
        ),
    ] {
        let error = fails(&text);
        assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
        assert_eq!(error.message, message, "{text}");
        assert_eq!(error.details, [line], "{text}");
    }
}

#[test]
fn mistakes_point_at_their_line_and_suggest_a_fix() {
    let typo = fails("[datasets.eeg]\nref = \"osf:abc12\"\nselct = [\"*.edf\"]\n");
    assert_eq!(typo.kind, Kind::ProjectInvalid);
    assert_eq!(typo.details, ["data.toml:3   selct = [\"*.edf\"]"]);
    assert_eq!(typo.help.as_deref(), Some("did you mean `select`?"));
    let no_ref = fails("[datasets.eeg]\nselect = \"*\"\n");
    assert!(no_ref.message.contains("has no ref"), "{}", no_ref.message);
    let empty = fails("[datasets.iris]\n");
    assert_eq!(empty.message, "datasets.iris in data.toml has no ref");
    assert_eq!(
        empty.help.as_deref(),
        Some("add a line such as ref = \"zenodo:3608990\"")
    );
    let name = fails("[datasets.EEG]\nref = \"osf:abc12\"\n");
    assert!(
        name.help
            .as_deref()
            .is_some_and(|help| help.contains("\"eeg\""))
    );
    for bad in [
        "[datasets.x]\nref = 3\n",
        "[datasets.x]\nref = \"a\"\ncopy = \"yes\"\n",
        "[datasets.x]\nref = \"a\"\nselect = [\"[abc\"]\n",
        "[datasets.x]\nref = \"a\"\npath = \"../outside\"\n",
        "[project]\ndata_dir = \"/abs\"\n",
        "[datasets.x\nref = \"a\"\n",
        "[datasets.x]\nref = \"\"\n",
    ] {
        assert_eq!(fails(bad).kind, Kind::ProjectInvalid, "{bad}");
    }
}

const METADATA_COMPONENTS: &[&str] = &[
    ".git",
    ".GIT",
    ".hg",
    ".HG",
    ".svn",
    ".SVN",
    ".fetchloom",
    ".FETCHLOOM",
    ".git.",
    ".git ",
    ".git. ",
    ".hg.",
    ".hg ",
    ".svn.",
    ".svn ",
    ".fetchloom.",
    ".fetchloom ",
    "git~1",
    "GIT~1",
    "hg~1",
    "svn~1",
    "fetchl~1",
    ".git:stream",
    ".git::$INDEX_ALLOCATION",
    ".hg:stream",
    ".svn:stream",
    ".fetchloom:stream",
    ".GiT. :stream",
    "git~1:stream",
];

fn refuses_metadata(key: &str, table: &str) {
    let mut taken = vec![".github", "data/git", "data/.hgdata"];
    // a data_dir named as one of git's own files is refused on its own
    if key != "data_dir" {
        taken.push(".gitignore");
    }
    for path in taken {
        let text = format!("{table}\n{key} = \"{path}\"\n");
        assert!(
            Manifest::parse(&text).is_ok(),
            "refused data folder: {path}"
        );
    }
    for component in METADATA_COMPONENTS {
        // .fetchloom is fetchloom's own folder, not a version control system's
        let own = component.to_ascii_lowercase().starts_with(".fetchloom")
            || component.starts_with("fetchl~1");
        let (said, outside) = if own {
            (
                "because it names .fetchloom, fetchloom's own folder",
                ".fetchloom",
            )
        } else {
            (
                "because it names repository metadata",
                "the repository's metadata",
            )
        };
        for path in [(*component).to_owned(), format!("nested/{component}/files")] {
            let text = format!("{table}\n{key} = \"{path}\"\n");
            let result = Manifest::parse(&text);
            assert!(result.is_err(), "accepted metadata {key}: {path:?}");
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(error.message.contains(path.trim()), "{}", error.message);
            assert!(error.message.ends_with(said), "{}", error.message);
            assert_eq!(
                error.help.as_deref(),
                Some(format!("choose a {key} outside {outside}").as_str())
            );
        }
    }
}

#[test]
fn metadata_data_dirs_are_refused() {
    refuses_metadata("data_dir", "[project]");
}

/// a data.toml is shared with windows clones, so a `data_dir` or a `path` some disk cannot store is
/// refused as it is read, on every system, naming the setting and its line, before any download.
#[test]
fn a_folder_windows_cannot_store_is_refused_as_data_toml_is_read() {
    for (table, key, value, reason) in [
        (
            "[project]",
            "data_dir",
            "NUL",
            "\"NUL\" is a name windows reserves for devices",
        ),
        (
            "[project]",
            "data_dir",
            "data.",
            "\"data.\" ends with a dot or a space, which windows drops",
        ),
        // a space is kept as written, so the folder data.toml names is the one made
        (
            "[project]",
            "data_dir",
            "Data ",
            "\"Data \" ends with a dot or a space, which windows drops",
        ),
        (
            "[project]",
            "data_dir",
            " ",
            "\" \" ends with a dot or a space, which windows drops",
        ),
        (
            "[project]",
            "data_dir",
            "data:stream",
            "\"data:stream\" has ':', which windows does not allow in names",
        ),
        (
            "[project]",
            "data_dir",
            "inputs/con",
            "\"con\" is a name windows reserves for devices",
        ),
        (
            "[datasets.iris]\nref = \"https://example.org/iris.csv\"",
            "path",
            "iris:stream",
            "\"iris:stream\" has ':', which windows does not allow in names",
        ),
    ] {
        let error = fails(&format!("{table}\n{key} = '{value}'\n"));
        assert_eq!(error.kind, Kind::ProjectInvalid, "{value}");
        assert_eq!(
            error.message,
            format!("{key} = \"{value}\" in data.toml is refused because {reason}")
        );
        assert_eq!(error.details.len(), 1, "{value}: {:?}", error.details);
        assert!(
            error.details[0].ends_with(&format!("{key} = '{value}'")),
            "{:?}",
            error.details
        );
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains("every system can store")),
            "{:?}",
            error.help
        );
    }
}

/// a `data_dir` named as a file git reads is refused as `.git` is, in any case, since git would
/// read its rules from a folder; `./data` is the folder data.
#[test]
fn a_data_dir_is_no_file_git_reads_and_may_start_at_dot() {
    for (written, file) in [
        (".gitignore", ".gitignore"),
        (".GitAttributes", ".GitAttributes"),
        (".gitmodules", ".gitmodules"),
        ("inputs/.gitignore", ".gitignore"),
    ] {
        let error = fails(&format!("[project]\ndata_dir = '{written}'\n"));
        assert_eq!(
            error.message,
            format!(
                "data_dir = \"{written}\" in data.toml is refused because {file} is the name of a file git reads"
            )
        );
        assert_eq!(
            error.help.as_deref(),
            Some("use a folder of its own inside the project, such as data")
        );
    }
    for (written, read) in [("./data", "data"), ("./inputs/raw/", "inputs/raw")] {
        let manifest = Manifest::parse(&format!("[project]\ndata_dir = '{written}'\n")).unwrap();
        assert_eq!(manifest.data_dir, read, "{written}");
    }
}

#[test]
fn metadata_dataset_paths_are_refused() {
    refuses_metadata("path", "[datasets.x]\nref = \"https://example.org/x\"");
}

/// data.toml comes with a cloned repository, so `~` or a variable in it is never expanded,
/// and a folder that only makes sense expanded is refused, not made under its literal name.
#[test]
fn folders_are_taken_as_written_so_shell_shorthand_is_refused() {
    for (key, table, inside) in [
        ("data_dir", "[project]", "the project"),
        (
            "path",
            "[datasets.x]\nref = \"https://example.org/x\"",
            "the data folder",
        ),
    ] {
        for folder in ["data~old", "price$", "a$1", "notes/~drafts"] {
            let text = format!("{table}\n{key} = \"{folder}\"\n");
            assert!(Manifest::parse(&text).is_ok(), "{key} = {folder}");
        }
        for folder in [
            "~/scratch/data",
            "~",
            "~ada/data",
            "$SCRATCH/data",
            "${SCRATCH}/data",
            "$1",
            "inputs/$USER",
            "inputs/${RUN}/raw",
            "runs/$_last",
        ] {
            let error = fails(&format!("{table}\n{key} = \"{folder}\"\n"));
            assert_eq!(error.kind, Kind::ProjectInvalid, "{key} = {folder}");
            assert_eq!(
                error.message,
                format!(
                    "{key} = \"{folder}\" in data.toml is refused because fetchloom does not expand ~ or $NAME, and the folder is always inside {inside}"
                )
            );
            assert_eq!(
                error.details,
                [format!(
                    "data.toml:{}   {key} = \"{folder}\"",
                    table.lines().count() + 1
                )]
            );
            assert_eq!(
                error.help.as_deref(),
                Some(
                    "to keep data on a scratch disk, set FETCHLOOM_CACHE to a folder there, or move the whole project there. see https://nikhil-yn-aix.github.io/fetchloom/user/guides/clusters/#keep-the-project-and-the-cache-on-one-drive"
                )
            );
        }
    }
}

#[test]
fn a_link_to_one_file_selects_that_file_unless_select_is_given() {
    let manifest = Manifest::parse(
        "[datasets.one]\nref = \"https://huggingface.co/datasets/acme/data/blob/main/a[1].csv\"\n\n[datasets.chosen]\nref = \"https://huggingface.co/datasets/acme/data/blob/main/a.csv\"\nselect = \"*.json\"\n",
    )
    .unwrap();
    let select = |name: &str| {
        manifest.datasets[name]
            .source
            .select
            .iter()
            .map(|glob| glob.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(select("one"), ["a[[]1].csv"]);
    assert_eq!(select("chosen"), ["*.json"]);
}

#[test]
fn a_link_to_one_folder_selects_that_folder_unless_select_is_given() {
    let manifest = Manifest::parse(
        "[datasets.folder]\nref = \"https://huggingface.co/datasets/acme/data/tree/main/sample/10BT\"\n\n[datasets.chosen]\nref = \"https://huggingface.co/datasets/acme/data/tree/main/sample\"\nselect = \"*.json\"\n\n[datasets.whole]\nref = \"https://huggingface.co/datasets/acme/data/tree/main\"\n",
    )
    .unwrap();
    let select = |name: &str| {
        manifest.datasets[name]
            .source
            .select
            .iter()
            .map(|glob| glob.as_str().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(select("folder"), ["sample/10BT/**"]);
    assert_eq!(select("chosen"), ["*.json"]);
    assert_eq!(select("whole"), Vec::<String>::new());
}

#[test]
fn a_ref_with_a_line_break_is_refused_without_showing_it() {
    let error = Manifest::parse(
        "[datasets.data]\nref = \"https://example.org/data?token=abc\\nselect=*\"\n",
    )
    .unwrap_err();
    assert!(error.message.contains("control character"), "{error:?}");
    assert_eq!(error.details, ["data.toml:2"]);
    assert!(!format!("{error:?}").contains("abc"));
}

#[test]
fn reference_help_masks_query_secrets() {
    let error = with_dataset(
        "datasets = {}\n",
        "data",
        "https://example.org/data?token=abc",
        &[],
        &[],
        true,
    )
    .unwrap_err();
    let help = error.help.unwrap();
    assert!(
        help.contains("token=***"),
        "manifest help exposed its query: {help}"
    );
    assert!(!help.contains("abc"), "{help}");
}

#[test]
fn adding_keeps_comments_and_order() {
    let text =
        "# my data\n[project]\ndata_dir = \"data\" # here\n\n[datasets.b]\nref = \"zenodo:2\"\n";
    let added = with_dataset(text, "a", "zenodo:1", &["*.csv".to_owned()], &[], true).unwrap();
    assert_eq!(
        added,
        "# my data\n[project]\ndata_dir = \"data\" # here\n\n[datasets.a]\nref = \"zenodo:1\"\nselect = [\"*.csv\"]\n\n[datasets.b]\nref = \"zenodo:2\"\n"
    );
    assert_eq!(parse(&added).datasets.len(), 2);
    let fresh = with_dataset("", "iris", "zenodo:3608990", &[], &[], true).unwrap();
    assert_eq!(fresh, "[datasets.iris]\nref = \"zenodo:3608990\"\n");
}

/// the order datasets are added in changes nothing, comments, notes and blank lines included;
/// those that go before the same dataset land in name order.
#[test]
fn datasets_added_together_land_as_each_would_alone() {
    let texts = [
        "",
        "[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n",
        "# inputs\n\n# the core set\n[datasets.core]\nref = \"zenodo:core\"\n",
        "[datasets.core]\nref = \"zenodo:core\"\n\n[project]\ndata_dir = \"in\"\n",
        "# my data\n[project]\ndata_dir = \"data\" # here\n\n[datasets.core]\nref = \"zenodo:2\"\n",
        "[datasets.mid]\nref = \"zenodo:mid\"\n\n[datasets.alpha]\nref = \"zenodo:alpha\"\n# about zulu\n[datasets.zulu]\nref = \"zenodo:zulu\"\n",
        "[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.core.members.\"a.zip\"]\nselect = [\"*.csv\"]\n\n# kilo\n\n[datasets.kilo]\nref = \"zenodo:kilo\"\n",
        "datasets.dot.ref = \"zenodo:dot\"\n",
        "datasets = { x = { ref = \"zenodo:9\" } }\n",
    ];
    let select = ["*.csv".to_owned()];
    let names = [
        "beta", "aaa", "echo", "zzz", "core-2", "mid-a", "alpha-0", "kilo-z", "dot-1",
    ];
    let references: Vec<String> = names.iter().map(|name| format!("zenodo:{name}")).collect();
    let added: Vec<NewDataset<'_>> = names
        .iter()
        .zip(&references)
        .enumerate()
        .map(|(index, (name, reference))| NewDataset {
            name,
            reference,
            select: if index % 3 == 0 { &select } else { &[] },
            exclude: &[],
            extract: index % 4 != 1,
            web_page: false,
        })
        .collect();
    let mut sorted: Vec<&NewDataset<'_>> = added.iter().collect();
    sorted.sort_by_key(|added| added.name);
    for text in texts {
        for order in [
            added.iter().collect::<Vec<_>>(),
            added.iter().rev().collect(),
            sorted.clone(),
        ] {
            let alone = order.iter().try_fold(text.to_owned(), |text, added| {
                with_dataset(
                    &text,
                    added.name,
                    added.reference,
                    added.select,
                    added.exclude,
                    added.extract,
                )
            });
            let order: Vec<NewDataset<'_>> = order.into_iter().copied().collect();
            assert_eq!(
                with_datasets(text, &order).map_err(|err| (err.message, err.help)),
                alone.map_err(|err| (err.message, err.help)),
                "{text:?}"
            );
        }
    }
}

#[test]
fn add_inserts_in_sorted_order() {
    let add = |text: &str, name: &str| {
        with_dataset(text, name, &format!("zenodo:{name}"), &[], &[], true).unwrap()
    };
    let three = "[datasets.alpha]\nref = \"zenodo:alpha\"\n\n[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n";
    assert_eq!(
        add(three, "beta"),
        "[datasets.alpha]\nref = \"zenodo:alpha\"\n\n[datasets.beta]\nref = \"zenodo:beta\"\n\n[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n"
    );
    assert_eq!(
        add(three, "echo"),
        format!("{three}\n[datasets.echo]\nref = \"zenodo:echo\"\n"),
        "the last name still goes last"
    );
    assert_eq!(
        add("[datasets.core]\nref = \"zenodo:core\"\n", "alpha"),
        "[datasets.alpha]\nref = \"zenodo:alpha\"\n\n[datasets.core]\nref = \"zenodo:core\"\n"
    );
    // a note at the top stays there, and a comment on a dataset stays on it
    assert_eq!(
        add(
            "# inputs\n\n# the core set\n[datasets.core]\nref = \"zenodo:core\"\n",
            "alpha"
        ),
        "# inputs\n\n[datasets.alpha]\nref = \"zenodo:alpha\"\n\n# the core set\n[datasets.core]\nref = \"zenodo:core\"\n"
    );
    let after_project = "[datasets.core]\nref = \"zenodo:core\"\n\n[project]\ndata_dir = \"in\"\n";
    assert_eq!(
        add(after_project, "delta"),
        "[datasets.core]\nref = \"zenodo:core\"\n\n[datasets.delta]\nref = \"zenodo:delta\"\n\n[project]\ndata_dir = \"in\"\n"
    );
}

/// a dataset taken from the other side lands in place of this side's, or in name order.
#[test]
fn a_dataset_from_the_other_side_keeps_its_comments_and_its_place() {
    let ours =
        "# inputs\n\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.core]\nref = \"zenodo:3\"\n";
    let theirs = "[datasets.beta]\nref = \"zenodo:2\" # new\n\n# upgraded\n[datasets.core]\nref = \"zenodo:4\"\n";
    let with_beta = with_dataset_from(ours, theirs, "beta").unwrap();
    assert_eq!(
        with_beta,
        "# inputs\n\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.beta]\nref = \"zenodo:2\" # new\n\n[datasets.core]\nref = \"zenodo:3\"\n"
    );
    assert_eq!(
        with_dataset_from(&with_beta, theirs, "core").unwrap(),
        "# inputs\n\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.beta]\nref = \"zenodo:2\" # new\n\n# upgraded\n[datasets.core]\nref = \"zenodo:4\"\n"
    );
    assert_eq!(
        with_dataset_from("", "datasets = { x = { ref = \"zenodo:9\" } }\n", "x").unwrap(),
        "[datasets.x]\nref = \"zenodo:9\"\n"
    );
    assert!(with_dataset_from(ours, theirs, "missing").is_err());
}

/// with `datasets = { … }` the help is to merge by hand.
#[test]
fn a_dataset_from_the_other_side_is_refused_where_datasets_are_inline() {
    let ours = "datasets = { alpha = { ref = \"zenodo:1\" } }\n";
    let theirs = "[datasets.beta]\nref = \"zenodo:2\"\n";
    let error = match with_dataset_from(ours, theirs, "beta") {
        Ok(text) => panic!("merged into inline datasets as {text:?}"),
        Err(error) => error,
    };
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "data.toml writes its datasets inline, so fetchloom cannot merge them"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("merge data.toml by hand, then run `fetchloom lock`")
    );
}

#[test]
fn each_dataset_reads_alone_and_the_rest_without_them() {
    let text = "# inputs\n\n[project]\ndata_dir = \"in\"\n\n# about alpha\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.beta]\nref = \"zenodo:2\"\n";
    let tables = tables(text).unwrap();
    assert_eq!(
        tables.into_iter().collect::<Vec<_>>(),
        [
            (
                "alpha".to_owned(),
                "# about alpha\n[datasets.alpha]\nref = \"zenodo:1\"\n".to_owned()
            ),
            (
                "beta".to_owned(),
                "[datasets.beta]\nref = \"zenodo:2\"\n".to_owned()
            ),
        ]
    );
    assert_eq!(
        outside(text).unwrap(),
        "# inputs\n[project]\ndata_dir = \"in\""
    );
    assert_eq!(
        outside(&text.replace("\n\n[datasets.beta]", "\n[datasets.beta]")).unwrap(),
        outside(text).unwrap(),
        "blank lines do not count"
    );
}

/// each pattern goes under the dataset's own header, or one of its own for the archive.
#[test]
fn an_exclusion_is_added_once_keeping_comments() {
    let text = "# pets for the study\n[datasets.pets]\nref = \"https://example.org/pets.zip\" # v1\n\n[datasets.zoo]\nref = \"zenodo:1\"\nexclude = \"*.tmp\" # scratch\n";
    let dog = "pets-v1/images/dog_010.png".to_owned();
    let blurry = "pets-v1/images/blurry_011.png".to_owned();
    let once = with_exclusions(text, "pets", Some("pets.zip"), std::slice::from_ref(&dog)).unwrap();
    assert_eq!(
        once,
        "# pets for the study\n[datasets.pets]\nref = \"https://example.org/pets.zip\" # v1\n\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\"pets-v1/images/dog_010.png\"]\n\n[datasets.zoo]\nref = \"zenodo:1\"\nexclude = \"*.tmp\" # scratch\n"
    );
    let twice = with_exclusions(&once, "pets", Some("pets.zip"), &[blurry.clone(), dog]).unwrap();
    assert_eq!(
        twice,
        "# pets for the study\n[datasets.pets]\nref = \"https://example.org/pets.zip\" # v1\n\n[datasets.pets.members.\"pets.zip\"]\nexclude = [\n  \"pets-v1/images/dog_010.png\",\n  \"pets-v1/images/blurry_011.png\",\n]\n\n[datasets.zoo]\nref = \"zenodo:1\"\nexclude = \"*.tmp\" # scratch\n"
    );
    let read = parse(&twice);
    let members = &read.datasets["pets"].source.members;
    let zip = members.get("pets.zip").unwrap();
    assert!(!zip.keeps(&blurry) && zip.keeps("pets-v1/images/cat.png"));
    let listed = with_exclusions(text, "zoo", None, &["b[[]1].csv".to_owned()]).unwrap();
    assert!(
        listed.ends_with(
            "[datasets.zoo]\nref = \"zenodo:1\"\nexclude = [\n  \"*.tmp\",\n  \"b[[]1].csv\",\n] # scratch\n"
        ),
        "{listed}"
    );
    assert_eq!(without_members(&twice, "pets", "pets.zip").unwrap(), text);
    for inline in [
        "datasets.pets = { ref = \"x\" }\n",
        "[datasets.pets]\nref = \"x\"\nmembers = { \"pets.zip\" = { exclude = \"a\" } }\n",
    ] {
        let error = with_exclusions(
            inline,
            "pets",
            Some("pets.zip"),
            std::slice::from_ref(&blurry),
        )
        .unwrap_err();
        assert_eq!(error.kind, Kind::ProjectInvalid, "{inline}");
        assert!(error.message.contains("inline"), "{}", error.message);
    }
}

#[test]
fn removing_keeps_everything_else() {
    let text = "# my thesis data\n\n# the classic one\n[datasets.a]\nref = \"zenodo:1\" # first\n\n[datasets.b]\nref = \"zenodo:2\"\n";
    assert_eq!(
        without_dataset(text, "a").unwrap(),
        "# my thesis data\n\n[datasets.b]\nref = \"zenodo:2\"\n"
    );
    assert_eq!(
        without_dataset(text, "b").unwrap(),
        "# my thesis data\n\n# the classic one\n[datasets.a]\nref = \"zenodo:1\" # first\n"
    );
    assert_eq!(
        without_dataset("[datasets.a]\nref = \"x\"\n", "a").unwrap(),
        ""
    );
    assert_eq!(
        without_dataset("# only\n\n[datasets.a]\nref = \"x\"\n", "a").unwrap(),
        "# only\n"
    );
}

/// taking several datasets out at once reads data.toml once and leaves what taking them out one by
/// one leaves, notes above a removed table included.
#[test]
fn removing_many_reads_once_as_removing_each_would() {
    let text = "# my thesis data\n\n# the classic one\n[datasets.a]\nref = \"zenodo:1\" # first\n\n# kept for b\n\n[datasets.b]\nref = \"zenodo:2\"\n\n[datasets.c]\nref = \"zenodo:3\"\n\n# the last\n[datasets.d]\nref = \"zenodo:4\"\n";
    for names in [
        vec!["a", "b"],
        vec!["b", "a", "d"],
        vec!["d", "c"],
        vec!["a", "b", "c", "d"],
    ] {
        let names: Vec<String> = names.into_iter().map(str::to_owned).collect();
        let one_by_one = names
            .iter()
            .try_fold(text.to_owned(), |text, name| without_dataset(&text, name))
            .unwrap();
        let before = reads();
        let at_once = without_datasets(text, &names, |_| Ok(())).unwrap();
        assert_eq!(reads() - before, 1, "{names:?}");
        assert_eq!(at_once, one_by_one, "{names:?}");
    }
}

/// an error between names, as a stop gives, ends the edit at that name with no text.
#[test]
fn a_stop_between_names_ends_the_edit_there() {
    let text =
        "[datasets.a]\nref = \"x\"\n\n[datasets.b]\nref = \"y\"\n\n[datasets.c]\nref = \"z\"\n";
    let names = ["a", "b", "c"].map(str::to_owned);
    let mut asked = Vec::new();
    let error = without_datasets(text, &names, |at| {
        asked.push(at);
        if at == 1 {
            Err(crate::stop::Stop::stopped())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.kind, Kind::Interrupted);
    assert_eq!(asked, [0, 1]);
}

/// what data.toml states: the data folder, and each dataset's values without their lines.
type Stated = (String, BTreeMap<String, Values>);
type Values = (Fetched, bool, Option<String>);
/// a source's ref, select, exclude, extract and member filters.
type Fetched = (String, Vec<String>, Vec<String>, bool, Vec<Filtered>);
/// an archive's path, and the select and exclude patterns of its members table.
type Filtered = (String, Vec<String>, Vec<String>);

fn patterns(globs: &[Glob]) -> Vec<String> {
    globs.iter().map(|glob| glob.as_str().to_owned()).collect()
}

fn stated(manifest: &Manifest) -> Stated {
    let datasets = manifest.datasets.iter().map(|(dataset, entry)| {
        let source = &entry.source;
        let fetched = (
            source.reference.clone(),
            patterns(&source.select),
            patterns(&source.exclude),
            source.extract,
            source
                .members
                .iter()
                .map(|(archive, filter)| {
                    (
                        archive.clone(),
                        patterns(filter.select.globs()),
                        patterns(filter.exclude.globs()),
                    )
                })
                .collect(),
        );
        (dataset.clone(), (fetched, entry.copy, entry.path.clone()))
    });
    (manifest.data_dir.clone(), datasets.collect())
}

/// any text: what reads is valid, and removing or adding a dataset changes only it. each is
/// added again under another name, so the writer is given every string the reader takes.
#[test]
fn fuzz_manifest() {
    bolero::check!().for_each(|bytes| {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return;
        };
        let Ok(manifest) = Manifest::parse(text) else {
            return;
        };
        assert_eq!(name::escapes(&manifest.data_dir), None);
        assert_eq!(name::metadata(&manifest.data_dir), None);
        let before = stated(&manifest);
        for (dataset, entry) in &manifest.datasets {
            assert!(name::is_dataset_name(dataset), "{dataset:?}");
            let source = &entry.source;
            assert!(!source.reference.is_empty(), "{dataset:?}");
            assert_eq!(source.reference.trim(), source.reference);
            if let Some(path) = entry
                .path
                .as_deref()
                .filter(|path| *path != DATA_FOLDER_ITSELF)
            {
                assert_eq!(name::escapes(path), None, "{path:?}");
                assert_eq!(name::metadata(path), None, "{path:?}");
            }

            let removed = without_dataset(text, dataset).unwrap();
            let mut expected = before.clone();
            expected.1.remove(dataset);
            let after = Manifest::parse(&removed).map(|manifest| stated(&manifest));
            assert_eq!(after.ok(), Some(expected), "{removed:?}");

            let copy = format!("{dataset}-2");
            if !name::is_dataset_name(&copy) || manifest.datasets.contains_key(&copy) {
                continue;
            }
            let added = match with_dataset(
                text,
                &copy,
                &source.reference,
                &patterns(&source.select),
                &patterns(&source.exclude),
                source.extract,
            ) {
                Ok(added) => added,
                Err(err) => {
                    assert!(err.message.contains("inline"), "{}", err.message);
                    continue;
                }
            };
            let mut expected = before.clone();
            let (fetched, _, _) = before.1[dataset].clone();
            // a dataset is added without member filters, which only a person writes
            let (reference, select, exclude, extract, _) = fetched;
            let fetched = (reference, select, exclude, extract, Vec::new());
            expected.1.insert(copy, (fetched, false, None));
            let after = Manifest::parse(&added).map(|manifest| stated(&manifest));
            assert_eq!(after.ok(), Some(expected), "{added:?}");
        }
    });
}

#[test]
fn unknown_keys_are_rejected_at_each_table_level() {
    for (text, citation, help) in [
        (
            "datsets = {}\n",
            "data.toml:1   datsets = {}",
            "did you mean `datasets`?",
        ),
        (
            "[project]\ndata_dr = 'inputs'\n",
            "data.toml:2   data_dr = 'inputs'",
            "did you mean `data_dir`?",
        ),
        (
            "[datasets.x]\nref = 'zenodo:1'\nunknown_setting = true\n",
            "data.toml:3   unknown_setting = true",
            "the keys here are ref, select, exclude, extract, members, web_page, copy, path",
        ),
        (
            "[datasets.x]\nref = 'zenodo:1'\n[datasets.x.members.'x.zip']\nexculde = ['a/b.csv']\n",
            "data.toml:4   exculde = ['a/b.csv']",
            "did you mean `exclude`?",
        ),
        (
            "[project]\nrequired_version = '>=0.1'\n",
            "data.toml:2   required_version = '>=0.1'",
            "required_version goes at the top of data.toml, above every [table]",
        ),
    ] {
        let error = fails(text);
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(error.details, [citation]);
        assert_eq!(error.help.as_deref(), Some(help));
    }
}

/// `required_version` is read before every other key: a range this fetchloom is in reads as if
/// the key were not there, and one it is not in stops the read with both versions and how to
/// update, even where data.toml holds a key only a newer fetchloom knows. a value that is no
/// range is refused at its line, as any other value.
#[test]
fn a_project_needing_another_fetchloom_says_so_before_anything_else() {
    let running = required::RUNNING;
    let met = parse(&format!(
        "required_version = \">={running}\"\n\n[datasets.iris]\nref = \"zenodo:1\"\n"
    ));
    assert_eq!(met.datasets.keys().collect::<Vec<_>>(), ["iris"]);
    let install = "https://nikhil-yn-aix.github.io/fetchloom/user/start/install/";
    for (range, help) in [
        (
            ">=999.0",
            format!(
                "update fetchloom to a version that matches >=999.0, as {install} shows, then run the same command again"
            ),
        ),
        (
            "<0.0.1",
            format!(
                "install a fetchloom that matches <0.0.1, as {install} shows, then run the same command again"
            ),
        ),
    ] {
        let error = fails(&format!(
            "# needs a newer fetchloom\nrequired_version = \"{range}\"\nreport = true\n\n[datasets.iris]\nref = \"zenodo:1\"\ncitation = \"a key a newer fetchloom reads\"\n"
        ));
        assert_eq!(error.kind, Kind::ProjectInvalid, "{range}");
        assert_eq!(
            error.message,
            format!("this project needs fetchloom {range}, and this one is {running}")
        );
        assert_eq!(
            error.details,
            [format!("data.toml:2   required_version = \"{range}\"")]
        );
        assert_eq!(error.help.as_deref(), Some(help.as_str()));
    }
    for (text, message) in [
        (
            "required_version = \"0.2.0\"\n",
            "required_version = \"0.2.0\" in data.toml is refused because 0.2.0 has no comparison before it, such as >=0.2.0 for 0.2.0 or newer",
        ),
        (
            "required_version = \">=0.2 or so\"\n",
            "required_version = \">=0.2 or so\" in data.toml is refused because 0.2 or so is no version of numbers and dots, such as 0.1.0",
        ),
        (
            "required_version = 2\n",
            "required_version in data.toml must be a string in quotes",
        ),
    ] {
        let error = fails(text);
        assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
        assert_eq!(error.message, message, "{text}");
        assert_eq!(
            error.details,
            [format!("data.toml:1   {}", text.trim_end())],
            "{text}"
        );
    }
}

#[test]
fn wrong_table_and_pattern_types_point_to_the_bad_value() {
    for (text, message, line) in [
        ("project = 7\n", "must be a table", "data.toml:1"),
        ("datasets = []\n", "must be a table", "data.toml:1"),
        (
            "[datasets]\nx = false\n",
            "must be a table with a ref",
            "data.toml:2",
        ),
        (
            "[datasets.x]\nref = 'zenodo:1'\nselect = 1\n",
            "must be a pattern or a list",
            "data.toml:3",
        ),
        (
            "[datasets.x]\nref = 'zenodo:1'\nexclude = ['*.tmp', 7]\n",
            "every pattern in exclude must be a string",
            "data.toml:3",
        ),
        (
            "[datasets.x]\nref = 'zenodo:1'\nextract = 'yes'\n",
            "must be true or false",
            "data.toml:3",
        ),
        (
            "[project]\ndata_dir = false\n",
            "must be a string",
            "data.toml:2",
        ),
    ] {
        let error = fails(text);
        assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
        assert!(error.message.contains(message), "{}", error.message);
        assert_eq!(error.details.len(), 1);
        assert!(error.details[0].starts_with(line), "{:?}", error.details);
    }
}

#[test]
fn duplicate_datasets_and_duplicate_refs_are_not_silently_overwritten() {
    for text in [
        "[datasets.x]\nref = 'zenodo:1'\n[datasets.x]\nref = 'zenodo:2'\n",
        "[datasets.x]\nref = 'zenodo:1'\nref = 'zenodo:2'\n",
        "datasets = { x = { ref = 'zenodo:1' }, x = { ref = 'zenodo:2' } }\n",
    ] {
        let error = fails(text);
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert!(error.message.contains("not valid TOML"));
        assert_eq!(error.details.len(), 1);
        assert!(error.help.unwrap().contains("fix that line"));
    }
}

#[test]
fn path_dot_means_the_data_folder_and_empty_is_refused() {
    let table = "[datasets.x]\nref = \"https://example.org/x\"\n";
    for (written, read) in [(".", "."), ("./", "."), ("./.", "."), ("./raw", "raw")] {
        let manifest = parse(&format!("{table}path = \"{written}\"\n"));
        assert_eq!(
            manifest.datasets["x"].path.as_deref(),
            Some(read),
            "{written:?}"
        );
    }
    // spaces are kept as written, and windows drops one at the end of a name
    assert_eq!(
        fails(&format!("{table}path = \" . \"\n")).message,
        "path = \" . \" in data.toml is refused because \" . \" ends with a dot or a space, which windows drops"
    );
    let empty = fails(&format!("{table}path = \"\"\n"));
    assert_eq!(
        empty.message,
        "path = \"\" in data.toml is refused because it is empty"
    );
    assert_eq!(
        empty.help.as_deref(),
        Some("use \".\" for the data folder itself")
    );
    for (written, reason) in [
        ("..", "it climbs out of its folder"),
        ("raw/../x", "it climbs out of its folder"),
        ("raw/./x", "it has . as a folder name"),
    ] {
        let error = fails(&format!("{table}path = \"{written}\"\n"));
        assert_eq!(
            error.message,
            format!("path = \"{written}\" in data.toml is refused because {reason}")
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                "use a folder inside the data folder, with / between folders, or \".\" for the data folder itself"
            )
        );
    }
}

#[test]
fn data_directories_must_stay_inside_the_project() {
    for directory in [
        "",
        ".",
        "./",
        "../outside",
        "data/../outside",
        "/absolute",
        "C:/outside",
        r"data\files",
        "data//files",
    ] {
        let text = format!("[project]\ndata_dir = '{directory}'\n");
        let error = fails(&text);
        assert_eq!(error.kind, Kind::ProjectInvalid, "{directory}");
        assert!(error.message.contains("data_dir"));
        assert_eq!(
            error.details,
            [format!("data.toml:2   data_dir = '{directory}'")]
        );
        assert!(error.help.unwrap().contains("inside the project"));
    }
    assert_eq!(
        fails("[project]\ndata_dir = '.'\n").message,
        "data_dir = \".\" in data.toml is refused because it would mix data with the project's own files"
    );
    // the project's own files beside data.toml, in any case a disk may fold, but deeper down
    for own in ["data.toml", "data.lock", "data.lock.d", "Data.Lock.D/x"] {
        let first = own.split('/').next().unwrap();
        assert_eq!(
            fails(&format!("[project]\ndata_dir = '{own}'\n")).message,
            format!(
                "data_dir = \"{own}\" in data.toml is refused because {first} is the name of one of the project's own files"
            )
        );
    }
    assert_eq!(
        parse("[project]\ndata_dir = 'inputs/data.lock.d'\n").data_dir,
        "inputs/data.lock.d"
    );
    assert_eq!(parse("[project]\n").data_dir, "data");
    assert_eq!(
        parse("[project]\ndata_dir = 'inputs/raw///'\n").data_dir,
        "inputs/raw"
    );
}

#[test]
fn an_unusable_dataset_name_still_explains_how_to_rename_it() {
    let error = fails("[datasets.'!!!']\nref = 'zenodo:1'\n");
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert!(error.message.contains("not a valid dataset name"));
    assert!(error.help.unwrap().contains("lowercase letters"));
}

#[test]
fn a_windows_device_name_is_refused_with_a_usable_one() {
    for device in ["con", "aux", "nul", "com1", "lpt1"] {
        let error = fails(&format!("[datasets.{device}]\nref = 'zenodo:1'\n"));
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            format!("\"{device}\" is not a valid dataset name")
        );
        assert_eq!(
            error.details,
            [
                format!("data.toml:1   [datasets.{device}]"),
                "windows keeps it for a device, so it cannot be a folder there".to_owned()
            ]
        );
        assert_eq!(
            error.help.as_deref(),
            Some(
                format!(
                    "rename it to \"{device}-data\" in data.toml, then run the same command again"
                )
                .as_str()
            )
        );
    }
}

/// a rename names the new table only when data.toml shows which one it is.
#[test]
fn members_left_by_a_rename_say_to_rename_them_too() {
    let members = "\n[datasets.glob.members.\"glob-000000.tar\"]\nexclude = ['a1.wav']\n";
    for (sounds, help) in [
        (
            "ref = 'https://example.org/v/glob-000000.tar?download=1'",
            "if glob is now sounds, rename its members tables too, as [datasets.sounds.members.\"glob-000000.tar\"]",
        ),
        (
            "ref = 'zenodo:1'",
            "if glob was renamed, rename its members tables with it, as [datasets.<new name>.members.\"glob-000000.tar\"], or remove them",
        ),
    ] {
        let error = fails(&format!("[datasets.sounds]\n{sounds}\n{members}"));
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(
            error.message,
            "datasets.glob in data.toml has members tables but no ref"
        );
        assert_eq!(
            error.details,
            ["data.toml:4   [datasets.glob.members.\"glob-000000.tar\"]"]
        );
        assert_eq!(error.help.as_deref(), Some(help));
    }
}

#[test]
fn removing_and_readding_preserves_other_datasets_and_all_requested_options() {
    let text = "# research inputs\n[project]\ndata_dir = 'inputs'\n\n[datasets.keep]\nref = 'zenodo:2' # leave this\ncopy = true\npath = 'raw/keep'\n\n[datasets.replace]\nref = 'zenodo:1'\n";
    let removed = without_dataset(text, "replace").unwrap();
    let mut expected = stated(&parse(text));
    expected.1.remove("replace");
    assert_eq!(stated(&parse(&removed)), expected);
    assert!(removed.contains("ref = 'zenodo:2' # leave this"));
    let added = with_dataset(
        &removed,
        "replace",
        "zenodo:3",
        &["**/*.csv".to_owned()],
        &["**/old/**".to_owned()],
        false,
    )
    .unwrap();
    let manifest = parse(&added);
    let replacement = &manifest.datasets["replace"].source;
    assert_eq!(manifest.data_dir, "inputs");
    assert_eq!(manifest.datasets.len(), 2);
    assert_eq!(replacement.reference, "zenodo:3");
    assert_eq!(patterns(&replacement.select), ["**/*.csv"]);
    assert_eq!(patterns(&replacement.exclude), ["**/old/**"]);
    assert!(!replacement.extract);
    assert_eq!(
        stated(&parse(&without_dataset(&added, "replace").unwrap())),
        expected
    );
    assert_eq!(without_dataset(&added, "missing").unwrap(), added);
    assert_eq!(
        without_dataset("# no datasets\n[project]\n", "missing").unwrap(),
        "# no datasets\n[project]\n"
    );
}

#[test]
fn removing_an_inline_entry_and_editing_bom_text_round_trip() {
    let text = "\u{feff}datasets = { a = { ref = 'zenodo:1' }, b = { ref = 'zenodo:2' } }\n";
    let removed = without_dataset(text, "a").unwrap();
    let manifest = parse(&removed);
    assert_eq!(manifest.datasets.len(), 1);
    assert_eq!(manifest.datasets["b"].source.reference, "zenodo:2");
    let removed = without_dataset(&removed, "b").unwrap();
    assert_eq!(parse(&removed).datasets.len(), 0);
    let added = with_dataset("\u{feff}# notes\n", "a", "zenodo:1", &[], &[], true).unwrap();
    assert!(!added.starts_with('\u{feff}'));
    assert!(added.contains("# notes"));
    assert_eq!(parse(&added).datasets["a"].source.reference, "zenodo:1");
}

/// a seeded walk through data.toml layouts, to compare two ways of editing each.
struct Layouts(u64);

impl Layouts {
    fn below(&mut self, count: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % u64::try_from(count).unwrap()).unwrap()
    }

    fn one_in(&mut self, count: usize) -> bool {
        self.below(count) == 0
    }

    /// a data.toml holding some of `names` in an order of its own, with notes, comments
    /// touching headers, member tables and `[project]` first or last; with `odd`, now and then
    /// a layout less often written: `[project]` among the datasets, a `[datasets]` header, a
    /// member table far from its dataset, a dataset written inline or with dotted keys.
    fn text<'n>(&mut self, names: &[&'n str], odd: bool) -> (String, Vec<&'n str>) {
        let mut chosen: Vec<&str> = names.iter().copied().filter(|_| !self.one_in(3)).collect();
        for at in (1..chosen.len()).rev() {
            if self.one_in(3) {
                let other = self.below(at + 1);
                chosen.swap(at, other);
            }
        }
        let mut text = "\n".repeat(self.below(3));
        if self.one_in(3) {
            text.push_str("# a note at the top\n\n");
        }
        let project = self.below(if odd { 4 } else { 3 });
        if project == 1 {
            text.push_str("[project]\ndata_dir = \"data\"\n\n");
            if self.one_in(2) {
                text.push_str("# the datasets\n\n");
            }
        }
        if odd && self.one_in(8) {
            text.push_str("[datasets]\n\n");
        }
        for (at, name) in chosen.iter().enumerate() {
            if self.one_in(4) {
                text.push_str("# a note that stands apart\n\n");
            }
            if self.one_in(2) {
                writeln!(text, "# {name}, from its own record").unwrap();
            }
            let record = self.below(9);
            if odd && self.one_in(12) {
                writeln!(text, "datasets.{name}.ref = \"zenodo:{record}\"").unwrap();
            } else {
                writeln!(text, "[datasets.{name}]\nref = \"zenodo:{record}\"").unwrap();
            }
            if self.one_in(if odd { 3 } else { 8 }) {
                writeln!(
                    text,
                    "\n[datasets.{name}.members.\"{name}.zip\"]\nexclude = [\"*.tmp\"]"
                )
                .unwrap();
            }
            if !self.one_in(4) {
                text.push('\n');
            }
            if project == 3 && at == 0 {
                text.push_str("[project]\ndata_dir = \"data\"\n\n");
            }
        }
        if odd
            && self.one_in(8)
            && let Some(first) = chosen.first()
        {
            writeln!(
                text,
                "[datasets.{first}.members.\"late.zip\"]\nselect = [\"*\"]"
            )
            .unwrap();
        }
        if project == 2 {
            text.push_str("[project]\ndata_dir = \"data\"\n");
        }
        if self.one_in(3) {
            text.push_str("\n# the end\n");
        }
        (text, chosen)
    }
}

/// the layouts vary, with copies in place, before another dataset or last, from either of two
/// sources, and removals; a layout edits at once cannot follow falls back to one at a time.
#[test]
fn edits_at_once_write_what_edits_one_at_a_time_write() {
    let names = ["a", "b", "c", "d", "e", "f"];
    let mut layouts = Layouts(0x9e37_79b9_7f4a_7c15);
    let (mut compared, mut cases) = (0, 0);
    for case in 0..3000 {
        let odd = case % 4 == 3;
        let (text, _) = layouts.text(&names, odd);
        let written = [layouts.text(&names, false), layouts.text(&names, odd)];
        let sources = [written[0].0.as_str(), written[1].0.as_str()];
        let mut edits: Vec<(&str, Edit)> = names
            .iter()
            .filter_map(|name| match layouts.below(4) {
                0 => None,
                1 => Some((*name, Edit::Out)),
                source => written[source - 2]
                    .1
                    .contains(name)
                    .then_some((*name, Edit::From(source - 2))),
            })
            .collect();
        if layouts.one_in(4) {
            edits.reverse();
        }
        cases += 1;
        let Some(at_once) = edited_at_once(&text, &sources, &edits) else {
            continue;
        };
        compared += 1;
        let one_at_a_time = one_at_a_time(&text, &sources, &edits).unwrap_or_else(|err| {
            panic!("{err:?}\ntext:\n{text}\nsources:\n{sources:#?}\nedits: {edits:?}")
        });
        assert_eq!(
            at_once, one_at_a_time,
            "text:\n{text}\nsources:\n{sources:#?}\nedits: {edits:?}"
        );
    }
    assert!(compared * 2 > cases, "{compared} of {cases} compared");
}

/// a data.toml for exclude to change: excludes as one pattern or a list, members tables for a
/// glob or one archive, dotted or inline, and now and then a dataset written inline.
fn narrowable(layouts: &mut Layouts) -> (String, Vec<&'static str>) {
    let mut text = String::new();
    let mut written = Vec::new();
    if layouts.one_in(2) {
        text.push_str("[project]\ndata_dir = \"data\"\n\n");
    }
    for name in ["a", "b", "c", "d"] {
        if layouts.one_in(5) {
            continue;
        }
        written.push(name);
        if layouts.one_in(3) {
            writeln!(text, "# {name}, its comment").unwrap();
        }
        writeln!(text, "[datasets.{name}]\nref = \"zenodo:1\"").unwrap();
        match layouts.below(4) {
            0 => text.push_str("exclude = \"*.tmp\"  # scratch\n"),
            1 => text.push_str("exclude = [\n  \"*.tmp\",  # scratch\n  \"*.bak\",\n]\n"),
            _ => {}
        }
        match layouts.below(10) {
            0 => text.push_str("members.\"dot.zip\".exclude = [\"x/*\"]\n"),
            1 => text.push_str("members = { \"in.zip\" = { exclude = [\"x/*\"] } }\n"),
            _ => {}
        }
        text.push('\n');
        if layouts.one_in(2) {
            writeln!(
                text,
                "[datasets.{name}.members.\"*.zip\"]\nselect = [\"a/*\", \"b/*\"]\nexclude = [\"*.log\"]\n"
            )
            .unwrap();
        }
        if layouts.one_in(3) {
            writeln!(
                text,
                "[datasets.{name}.members.\"own.zip\"]\nexclude = [\"junk/*\"]\n"
            )
            .unwrap();
        }
    }
    if layouts.one_in(6) {
        text.push_str("[datasets]\ne = { ref = \"zenodo:2\" }\n");
    }
    (text, written)
}

/// at once or one at a time, changes fail alike where one meets a part written inline.
#[test]
fn exclude_changes_at_once_write_what_changes_one_at_a_time_write() {
    let missing = ["e", "z"];
    let archives = [
        "*.zip", "own.zip", "new.zip", "*.zip", "own.zip", "new.zip", "dot.zip", "in.zip",
    ];
    let patterns = ["a/one.txt", "*.log", "x/*", "b/two.txt"];
    let unmatched = [
        vec![],
        vec!["b/*".to_owned()],
        vec!["a/*".to_owned(), "b/*".to_owned()],
    ];
    let mut layouts = Layouts(0x2545_f491_4f6c_dd1d);
    let (mut compared, mut failed) = (0, 0);
    for _ in 0..5000 {
        let (text, written) = narrowable(&mut layouts);
        let changes: Vec<Narrowing> = (0..=layouts.below(4))
            .map(|_| {
                let name = if written.is_empty() || layouts.one_in(10) {
                    missing[layouts.below(missing.len())]
                } else {
                    written[layouts.below(written.len())]
                };
                let archive = archives[layouts.below(archives.len())];
                let patterns: Vec<String> = (0..=layouts.below(2))
                    .map(|_| patterns[layouts.below(patterns.len())].to_owned())
                    .collect();
                match layouts.below(8) {
                    0 => Narrowing::MembersCopied {
                        name,
                        from: archives[layouts.below(2)],
                        archive,
                        unmatched: &unmatched[layouts.below(unmatched.len())],
                    },
                    1 => Narrowing::WithoutMembers {
                        name,
                        archive: archive.to_owned(),
                    },
                    2 | 3 => Narrowing::Exclusions {
                        name,
                        archive: None,
                        patterns,
                    },
                    _ => Narrowing::Exclusions {
                        name,
                        archive: Some(archive),
                        patterns,
                    },
                }
            })
            .collect();
        let at_once = narrowed(&text, &changes);
        if editable(&text).is_ok_and(|document| plain(&document)) {
            compared += 1;
            failed += usize::from(at_once.is_err());
        }
        assert_eq!(
            at_once.map_err(|err| err.message),
            narrowed_one_at_a_time(&text, &changes).map_err(|err| err.message),
            "text:\n{text}\nchanges: {changes:#?}"
        );
    }
    assert!(
        compared > 1000 && (100..compared / 2).contains(&failed),
        "{failed} of {compared} failed"
    );
}

/// what [`a_plain_data_toml_reads_as_the_parser_reads_it`] joins: names, lines, near misses.
const PLAIN_NAMES: [&str; 4] = ["iris", "mnist-2_b", "a", "iris"];
const PLAIN_LINES: [&str; 6] = [
    "ref = \"https://example.org/iris.csv\"",
    "ref = \"zenodo:3608990\"",
    "ref = \" arxiv:2401.00001 \"",
    "ref = \"https://example.org/a b?token=x#y\"",
    "# a note, with \"quotes\" and = signs",
    "  \t",
];
/// `required_version` lines above the tables: ranges this fetchloom is in, then ones it is not
/// in, or that are no range, or that the parser reads another way.
const PLAIN_REQUIRED: [&str; 8] = [
    "required_version = \">=0.0.1\"",
    "required_version = \" >=0.0.1, <999 \"",
    "required_version = \">=999\"",
    "required_version = \"0.1\"",
    "required_version = '>=0.0.1'",
    "required_version = \">=0.0.1\" # a note",
    "required_version=\">=0.0.1\"",
    "required_version = \">=0.0.1\\u002C <999\"",
];

const NEAR_MISSES: [&str; 28] = [
    "[datasets.Iris]",
    "[datasets.\"iris\"]",
    "[datasets.a.b]",
    "[ datasets.iris ]",
    "[datasets]",
    "[[datasets.iris]]",
    "[project]",
    "data_dir = \"raw\"",
    "ref = \"\"",
    "ref = \"   \"",
    "ref = \"a\tb\"",
    "ref = \"a\\tb\"",
    "ref = \"a\\\"b\"",
    "ref = 'zenodo:1'",
    "ref = \"zenodo:1\" # a note",
    "ref=\"zenodo:1\"",
    "ref = \"zenodo:1\"\"",
    "select = [\"*.csv\"]",
    "copy = true",
    "ref = \"a\u{7f}b\"",
    "# a \u{7f} note",
    "\u{85}",
    "ref = \"zenodo:1\u{85}\"",
    "datasets.iris.ref = \"zenodo:1\"",
    "\"ref\" = \"zenodo:1\"",
    "ref = \"\"\"zenodo:1\"\"\"",
    "",
    "\r",
];

/// [`read_plain`] reads a data.toml only as [`Manifest::parse`] reads it: the data folder and each
/// dataset's name and ref. tables `add` writes, some lines swapped for near misses, check it; what
/// it declines goes to the parser.
#[test]
fn a_plain_data_toml_reads_as_the_parser_reads_it() {
    let mut rng = fastrand::Rng::with_seed(4);
    let mut plain_texts = 0;
    let mut required_texts = 0;
    let mut required_plain = 0;
    for round in 0..3_000 {
        let mut lines: Vec<String> = Vec::new();
        for _ in 0..rng.usize(0..4) {
            lines.push(format!(
                "[datasets.{}]",
                PLAIN_NAMES[rng.usize(..PLAIN_NAMES.len())]
            ));
            for _ in 0..rng.usize(0..4) {
                lines.push(PLAIN_LINES[rng.usize(..PLAIN_LINES.len())].to_owned());
            }
        }
        if !lines.is_empty() && rng.bool() {
            let at = rng.usize(..lines.len());
            lines[at] = NEAR_MISSES[rng.usize(..NEAR_MISSES.len())].to_owned();
        }
        if round % 3 == 0 {
            lines.insert(
                0,
                PLAIN_REQUIRED[rng.usize(..PLAIN_REQUIRED.len())].to_owned(),
            );
            required_texts += 1;
        }
        let mut text = lines.join(if round % 2 == 0 { "\n" } else { "\r\n" });
        if rng.bool() {
            text.push('\n');
        }
        if round % 7 == 0 {
            text.insert(0, '\u{feff}');
        }
        let Some(plain) = read_plain(&text) else {
            continue;
        };
        plain_texts += 1;
        if text.contains("required_version") {
            required_plain += 1;
        }
        let whole = Manifest::parse(&text)
            .unwrap_or_else(|error| panic!("{text:?} read plain, but {}", error.message));
        assert_eq!(plain.data_dir, whole.data_dir, "{text:?}");
        let names = |manifest: &Manifest| manifest.datasets.keys().cloned().collect::<Vec<_>>();
        assert_eq!(names(&plain), names(&whole), "{text:?}");
        for (name, entry) in &whole.datasets {
            let read = &plain.datasets[name];
            assert_eq!(read.source.reference, entry.source.reference, "{text:?}");
        }
    }
    // a run that read nothing plain would prove nothing
    assert!(plain_texts > 500, "only {plain_texts} texts read plain");
    assert!(
        required_texts > 900,
        "only {required_texts} texts had a range"
    );
    assert!(
        required_plain > 50,
        "only {required_plain} texts with a range read plain"
    );
    let added = with_datasets(
        "# my project\n",
        &[
            NewDataset {
                name: "iris",
                reference: "https://example.org/iris.csv",
                select: &[],
                exclude: &[],
                extract: true,
                web_page: false,
            },
            NewDataset {
                name: "paper",
                reference: "arxiv:2401.00001",
                select: &[],
                exclude: &[],
                extract: true,
                web_page: false,
            },
        ],
    )
    .unwrap();
    let plain = read_plain(&added).unwrap_or_else(|| panic!("{added:?} is not read plain"));
    assert_eq!(plain.datasets.keys().collect::<Vec<_>>(), ["iris", "paper"]);
}
