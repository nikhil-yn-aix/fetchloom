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
    let source = eeg.source().unwrap();
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
    assert_eq!(
        weights.source().unwrap().reference,
        "https://example.org/w.bin"
    );
    assert!(!weights.source().unwrap().extract && weights.copy);
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
    let members = &manifest.datasets["pets"].source().unwrap().members;
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
        let members = &manifest.datasets["pets"].source().unwrap().members;
        let zip = members.get("pets.zip").unwrap();
        assert!(!zip.keeps("a/x.png") && zip.keeps("a/y.png"), "{written}");
    }
    assert!(
        parse(PETS).datasets["pets"]
            .source()
            .unwrap()
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
    let spec = |manifest: &Manifest| {
        crate::project::lock::spec(manifest.datasets["pets"].source().unwrap())
    };
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
    let made = fails(&format!(
        "{PETS}[datasets.b]\nfrom = [\"pets\"]\nrun = [\"t\", \"{{pets}}\", \"{{out}}\"]\n[datasets.b.members.\"x.zip\"]\nexclude = \"a\"\n"
    ));
    assert_eq!(
        made.message,
        "members is for a dataset fetched from a source, and datasets.b is made by a recipe"
    );
}

#[test]
fn mistakes_point_at_their_line_and_suggest_a_fix() {
    let typo = fails("[datasets.eeg]\nref = \"osf:abc12\"\nselct = [\"*.edf\"]\n");
    assert_eq!(typo.kind, Kind::ProjectInvalid);
    assert_eq!(typo.details, ["data.toml:3   selct = [\"*.edf\"]"]);
    assert_eq!(typo.help.as_deref(), Some("did you mean `select`?"));
    let no_ref = fails("[datasets.eeg]\nselect = \"*\"\n");
    assert!(no_ref.message.contains("has no ref"), "{}", no_ref.message);
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
    for path in [".github", ".gitignore", "data/git", "data/.hgdata"] {
        let text = format!("{table}\n{key} = \"{path}\"\n");
        assert!(
            Manifest::parse(&text).is_ok(),
            "refused data folder: {path}"
        );
    }
    for component in METADATA_COMPONENTS {
        for path in [(*component).to_owned(), format!("nested/{component}/files")] {
            let text = format!("{table}\n{key} = \"{path}\"\n");
            let result = Manifest::parse(&text);
            assert!(result.is_err(), "accepted metadata {key}: {path:?}");
            let error = result.unwrap_err();
            assert_eq!(error.kind, Kind::ProjectInvalid);
            assert!(error.message.contains(path.trim()), "{}", error.message);
            assert!(error.message.contains("metadata"), "{}", error.message);
            assert!(error.help.is_some());
        }
    }
}

#[test]
fn metadata_data_dirs_are_refused() {
    refuses_metadata("data_dir", "[project]");
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
            .source()
            .unwrap()
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
            .source()
            .unwrap()
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
    let members = &read.datasets["pets"].source().unwrap().members;
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

/// what data.toml states: the data folder, and each dataset's values without their lines.
type Stated = (String, BTreeMap<String, Values>);
type Values = (Option<Fetched>, Option<Made>, bool, Option<String>);
/// a source's ref, select, exclude, extract and member filters.
type Fetched = (String, Vec<String>, Vec<String>, bool, Vec<Filtered>);
/// an archive's path, and the select and exclude patterns of its members table.
type Filtered = (String, Vec<String>, Vec<String>);
/// a recipe's from, run, code and env.
type Made = (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    BTreeMap<String, String>,
);

fn patterns(globs: &[Glob]) -> Vec<String> {
    globs.iter().map(|glob| glob.as_str().to_owned()).collect()
}

fn stated(manifest: &Manifest) -> Stated {
    let datasets = manifest.datasets.iter().map(|(dataset, entry)| {
        let fetched = entry.source().map(|source| {
            (
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
            )
        });
        let made = entry.recipe().map(|recipe| {
            (
                recipe.from.clone(),
                recipe
                    .run
                    .iter()
                    .map(|word| word.text().to_owned())
                    .collect(),
                recipe.code.clone(),
                recipe.env.clone(),
            )
        });
        (
            dataset.clone(),
            (fetched, made, entry.copy, entry.path.clone()),
        )
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
            if let Some(source) = entry.source() {
                assert!(!source.reference.is_empty(), "{dataset:?}");
                assert_eq!(source.reference.trim(), source.reference);
            }
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
            // a recipe cannot read a dataset data.toml no longer has
            let read = manifest.datasets.values().any(|entry| {
                entry
                    .recipe()
                    .is_some_and(|recipe| recipe.from.contains(dataset))
            });
            if read {
                assert!(after.is_err(), "{removed:?}");
            } else {
                assert_eq!(after.ok(), Some(expected), "{removed:?}");
            }

            let copy = format!("{dataset}-2");
            let Some(source) = entry.source() else {
                continue;
            };
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
            let (fetched, _, _, _) = before.1[dataset].clone();
            // a dataset is added without member filters, which only a person writes
            let fetched = fetched.map(|(reference, select, exclude, extract, _)| {
                (reference, select, exclude, extract, Vec::new())
            });
            expected.1.insert(copy, (fetched, None, false, None));
            let after = Manifest::parse(&added).map(|manifest| stated(&manifest));
            assert_eq!(after.ok(), Some(expected), "{added:?}");
        }
    });
}

const RAW: &str = "[datasets.raw]\nref = \"zenodo:1\"\n\n";
const CLEAN: &str =
    "[datasets.clean]\nfrom = [\"raw\"]\nrun = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]\n";

/// a dataset is fetched from a source or made by a recipe, never both.
#[test]
fn a_recipe_needs_from_and_run_and_no_ref() {
    let manifest = parse(&format!(
        "{RAW}{CLEAN}code = [\"clean.py\", \"lib/\"]\nenv = {{ SEED = \"1\" }}\ncopy = true\n"
    ));
    let entry = &manifest.datasets["clean"];
    let recipe = entry.recipe().unwrap();
    assert_eq!(recipe.from, ["raw"]);
    let words: Vec<&str> = recipe.run.iter().map(Template::text).collect();
    assert_eq!(words, ["python3", "clean.py", "{raw}", "{out}"]);
    assert_eq!(recipe.code, ["clean.py", "lib"]);
    assert_eq!(
        recipe.env,
        BTreeMap::from([("SEED".to_owned(), "1".to_owned())])
    );
    assert!(entry.copy && entry.source().is_none());
    assert!(manifest.datasets["raw"].recipe().is_none());
    let run = "run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]\n";
    for (table, message, help) in [
        (
            "from = [\"raw\"]\n".to_owned(),
            "datasets.clean has from but no run, so nothing makes it",
            "add run, the program and arguments that make it, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
        ),
        (
            run.to_owned(),
            "datasets.clean has run but no from, so it reads no dataset",
            "add from with the datasets run reads, as from = [\"raw\"]",
        ),
        (
            format!("ref = \"zenodo:2\"\nfrom = [\"raw\"]\n{run}"),
            "datasets.clean has both ref and from, but a dataset is fetched from a source or made by a recipe, not both",
            "keep ref to fetch it, or from and run to make it from other datasets",
        ),
        (
            "ref = \"zenodo:2\"\nenv = { SEED = \"1\" }\n".to_owned(),
            "datasets.clean has both ref and env, but a dataset is fetched from a source or made by a recipe, not both",
            "keep ref to fetch it, or from and run to make it from other datasets",
        ),
        (
            format!("from = [\"raw\"]\n{run}select = \"*.csv\"\n"),
            "select is for a dataset fetched from a source, and datasets.clean is made by a recipe",
            "remove select, and choose files inside the step instead",
        ),
        (
            format!("from = [\"raw\"]\n{run}extract = false\n"),
            "extract is for a dataset fetched from a source, and datasets.clean is made by a recipe",
            "remove extract, and choose files inside the step instead",
        ),
        (
            format!("from = []\n{run}"),
            "datasets.clean is made from no dataset",
            "name the datasets run reads in from, as from = [\"raw\"]",
        ),
        (
            format!("from = \"raw\"\n{run}"),
            "from in data.toml must be a list of dataset names",
            "write it as from = [\"raw\"]",
        ),
        (
            format!("from = [\"raw\", \"raw\"]\n{run}"),
            "datasets.clean is made from raw twice",
            "name each dataset once in from",
        ),
        (
            "from = [\"raw\"]\nrun = \"python3 clean.py {raw} {out}\"\n".to_owned(),
            "run in data.toml must be a list of words, since no shell reads it",
            "write the program and each argument as a word of their own, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
        ),
        (
            "from = [\"raw\"]\nrun = []\n".to_owned(),
            "run in datasets.clean is empty, so there is no program to start",
            "write the program and each argument as a word of their own, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
        ),
        (
            format!("from = [\"raw\"]\n{run}code = \"clean.py\"\n"),
            "code in data.toml must be a list of files and folders in the project",
            "write it as code = [\"clean.py\"]",
        ),
        (
            format!("from = [\"raw\"]\n{run}env = {{ SEED = 1 }}\n"),
            "every value in env must be a string in quotes",
            "write it as SEED = \"1\"",
        ),
    ] {
        let error = fails(&format!("{RAW}[datasets.clean]\n{table}"));
        assert_eq!(error.kind, Kind::ProjectInvalid, "{table}");
        assert_eq!(error.message, message, "{table}");
        assert_eq!(error.help.as_deref(), Some(help), "{table}");
        assert_eq!(error.details.len(), 1, "{table}: {:?}", error.details);
        assert!(error.details[0].starts_with("data.toml:"), "{table}");
    }
}

/// `place = false` keeps a dataset out of the data folder; only true or false is read.
#[test]
fn place_reads_on_both_kinds() {
    let manifest = parse(&format!(
        "{RAW}place = false\n\n{CLEAN}place = false\n\n[datasets.kept]\nref = \"zenodo:2\"\n"
    ));
    assert!(!manifest.datasets["raw"].place);
    assert!(!manifest.datasets["clean"].place);
    assert!(
        manifest.datasets["kept"].place,
        "placed unless data.toml says not"
    );
    assert_eq!(
        manifest.datasets["raw"].lines["place"],
        "data.toml:4   place = false"
    );
    let error = fails(&format!("{RAW}place = \"no\"\n"));
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(error.message, "place in data.toml must be true or false");
    assert_eq!(error.details, ["data.toml:4   place = \"no\""]);
}

/// `{name}` is the folder of a dataset a step reads and `{out}` the result's; a word naming
/// anything else, or a step missing an input or its result, is refused before anything runs.
#[test]
fn templates_name_inputs_and_out() {
    let recipe = |run: &str| format!("{RAW}[datasets.clean]\nfrom = [\"raw\"]\nrun = {run}\n");
    let manifest = parse(&recipe(
        "[\"tool\", \"--in={raw}/x\", \"{{literal}}\", \"{out}\"]",
    ));
    assert_eq!(manifest.datasets["clean"].recipe().unwrap().run.len(), 4);
    for (run, message, help) in [
        (
            "[\"python3\", \"clean.py\", \"{raw}\"]",
            "run in datasets.clean never names {out}, so the step has nowhere to write",
            "put {out} where the step takes the folder to write the result in",
        ),
        (
            "[\"python3\", \"clean.py\", \"{out}\"]",
            "run in datasets.clean never names {raw}, so the step cannot find raw",
            "put {raw} where the step takes the folder holding raw's files",
        ),
        (
            "[\"python3\", \"{rwa}\", \"{raw}\", \"{out}\"]",
            "run in datasets.clean names {rwa}, which is not a dataset it is made from",
            "did you mean {raw}? a name in braces is one of from, or out",
        ),
        (
            "[\"python3\", \"{Raw}\", \"{raw}\", \"{out}\"]",
            "run in datasets.clean names {Raw}, which is not a dataset it is made from",
            "did you mean {raw}? a name in braces is one of from, or out",
        ),
        (
            "[\"python3\", \"{raw\", \"{out}\"]",
            "a word of run in datasets.clean has a { with no } after it",
            "write {{ and }} for a brace of its own, and {name} for the folder of a dataset it reads",
        ),
        (
            "[\"\", \"{raw}\", \"{out}\"]",
            "the program in run of datasets.clean is empty",
            "make the first word the program to start, as run = [\"python3\", \"clean.py\", \"{raw}\", \"{out}\"]",
        ),
        (
            "[\"python3\\n\", \"{raw}\", \"{out}\"]",
            "a word of run in datasets.clean holds a control character",
            "write {{ and }} for a brace of its own, and {name} for the folder of a dataset it reads",
        ),
    ] {
        let error = fails(&recipe(run));
        assert_eq!(error.kind, Kind::ProjectInvalid, "{run}");
        assert_eq!(error.message, message, "{run}");
        assert_eq!(error.help.as_deref(), Some(help), "{run}");
        assert_eq!(error.details.len(), 1, "{run}");
        assert!(error.details[0].starts_with("data.toml:6"), "{run}");
    }
    let out = fails(
        "[datasets.out]\nref = \"zenodo:1\"\n\n[datasets.clean]\nfrom = [\"out\"]\nrun = [\"tool\", \"{out}\"]\n",
    );
    assert_eq!(
        out.message,
        "datasets.clean is made from out, the name run keeps for the result's folder"
    );
    assert_eq!(
        out.help.as_deref(),
        Some("rename the dataset out in data.toml")
    );
}

/// no order of steps could make recipes made from each other; a cycle names its path.
#[test]
fn from_names_exist_and_form_no_cycle() {
    let recipe = |name: &str, from: &[&str]| {
        let names: Vec<String> = from.iter().map(|input| format!("\"{input}\"")).collect();
        let folders: Vec<String> = from
            .iter()
            .map(|input| format!("\"{{{input}}}\""))
            .collect();
        format!(
            "\n[datasets.{name}]\nfrom = [{}]\nrun = [\"tool\", {}, \"{{out}}\"]\n",
            names.join(", "),
            folders.join(", ")
        )
    };
    let chain = format!("{RAW}{}{}", recipe("b", &["a"]), recipe("a", &["raw"]));
    assert_eq!(parse(&chain).datasets.len(), 3);
    let typo = fails(&format!("{RAW}{}", recipe("clean", &["rwa"])));
    assert_eq!(typo.kind, Kind::ProjectInvalid);
    assert_eq!(
        typo.message,
        "datasets.clean is made from rwa, which data.toml does not have"
    );
    assert_eq!(typo.help.as_deref(), Some("did you mean raw?"));
    assert_eq!(typo.details, ["data.toml:6   from = [\"rwa\"]"]);
    for (text, message) in [
        (
            format!("{RAW}{}{}", recipe("a", &["b"]), recipe("b", &["a"])),
            "datasets a and b are made from each other: a \u{2192} b \u{2192} a",
        ),
        (
            format!(
                "{RAW}{}{}{}",
                recipe("c", &["raw", "a"]),
                recipe("b", &["c"]),
                recipe("a", &["b"])
            ),
            "datasets a, b and c are made from each other: a \u{2192} b \u{2192} c \u{2192} a",
        ),
        (
            format!("{RAW}{}", recipe("a", &["a"])),
            "datasets.a is made from itself",
        ),
    ] {
        let error = fails(&text);
        assert_eq!(error.kind, Kind::ProjectInvalid, "{text}");
        assert_eq!(error.message, message, "{text}");
        assert_eq!(
            error.help.as_deref(),
            Some(
                "a recipe can read only datasets made before it; take one of them out of the other's from"
            ),
        );
        assert_eq!(error.details.len(), 1, "{text}");
    }
}

/// code's bytes enter the key, so it stays out of what fetchloom writes, the data folder,
/// data.lock and data.lock.d, and out of repository metadata.
#[test]
fn code_stays_in_the_project_and_out_of_the_data_folder() {
    let recipe = |data_dir: &str, code: &str| {
        format!("[project]\ndata_dir = \"{data_dir}\"\n\n{RAW}{CLEAN}code = [{code}]\n")
    };
    let manifest = parse(&recipe(
        "data",
        "\"clean.py\", \"lib/\", \"data.toml\", \"database\"",
    ));
    assert_eq!(
        manifest.datasets["clean"].recipe().unwrap().code,
        ["clean.py", "lib", "data.toml", "database"]
    );
    for (data_dir, code, message, help) in [
        (
            "data",
            "\"../shared/clean.py\"",
            "code \"../shared/clean.py\" in datasets.clean is refused because it climbs out of its folder",
            "name a file or folder inside the project, with / between folders",
        ),
        (
            "data",
            "\"/etc/passwd\"",
            "code \"/etc/passwd\" in datasets.clean is refused because it starts at the root of the disk",
            "name a file or folder inside the project, with / between folders",
        ),
        (
            "data",
            "\"lib\\\\clean.py\"",
            "code \"lib\\clean.py\" in datasets.clean is refused because it has a \\, which windows reads as a folder separator",
            "name a file or folder inside the project, with / between folders",
        ),
        (
            "data",
            "\".\"",
            "code \".\" in datasets.clean is refused because it has . as a folder name",
            "name a file or folder inside the project, with / between folders",
        ),
        (
            "data",
            "\".git/hooks\"",
            "code \".git/hooks\" in datasets.clean is refused because it names repository metadata",
            "name the project's own files, such as scripts and their settings",
        ),
        (
            "data",
            "\"Data.Lock\"",
            "code \"Data.Lock\" in datasets.clean is refused because fetchloom writes it",
            "name the project's own files, such as scripts and their settings",
        ),
        (
            "data",
            "\"data.lock.d/clean.tsv.zst\"",
            "code \"data.lock.d/clean.tsv.zst\" in datasets.clean is refused because fetchloom writes it",
            "name the project's own files, such as scripts and their settings",
        ),
        (
            "data",
            "\"data/raw/x.csv\"",
            "code \"data/raw/x.csv\" in datasets.clean is refused because it is inside the data folder, which fetchloom fills",
            "name the project's own files, such as scripts and their settings",
        ),
        (
            "work/data",
            "\"WORK\"",
            "code \"WORK\" in datasets.clean is refused because it holds the data folder, which fetchloom fills",
            "name the project's own files, such as scripts and their settings",
        ),
        (
            "data",
            "\"clean.py\", \"clean.py/\"",
            "datasets.clean names the code \"clean.py\" twice",
            "name each file or folder once in code",
        ),
    ] {
        let error = fails(&recipe(data_dir, code));
        assert_eq!(error.kind, Kind::ProjectInvalid, "{code}");
        assert_eq!(error.message, message, "{code}");
        assert_eq!(error.help.as_deref(), Some(help), "{code}");
        assert_eq!(error.details.len(), 1, "{code}");
        assert!(error.details[0].starts_with("data.toml:10"), "{code}");
    }
}

/// data.toml and data.lock are shared, and the step inherits the environment of whoever
/// builds, so env sets plain settings: never fetchloom's own variables or a secret.
#[test]
fn env_refuses_fetchloom_and_secret_names() {
    let recipe = |env: &str| format!("{RAW}{CLEAN}env = {{ {env} }}\n");
    let manifest = parse(&recipe(
        "SEED = \"1\", OMP_NUM_THREADS = \"4\", http_proxy = \"\", TOKENIZERS_PARALLELISM = \"false\", MAX_TOKENS = \"512\"",
    ));
    assert_eq!(manifest.datasets["clean"].recipe().unwrap().env.len(), 5);
    for (env, message, help) in [
        (
            "FETCHLOOM_CACHE = \"/tmp/c\"",
            "env in datasets.clean sets FETCHLOOM_CACHE, one of fetchloom's own variables",
            "set fetchloom's variables in your shell or its settings file, never in data.toml",
        ),
        (
            "fetchloom_jobs = \"1\"",
            "env in datasets.clean sets fetchloom_jobs, one of fetchloom's own variables",
            "set fetchloom's variables in your shell or its settings file, never in data.toml",
        ),
        (
            "HF_TOKEN = \"hf_abc\"",
            "env in datasets.clean sets HF_TOKEN, which looks like a secret",
            "data.toml and data.lock are shared, and the step inherits your environment, so set HF_TOKEN there",
        ),
        (
            "aws_secret_access_key = \"x\"",
            "env in datasets.clean sets aws_secret_access_key, which looks like a secret",
            "data.toml and data.lock are shared, and the step inherits your environment, so set aws_secret_access_key there",
        ),
        (
            "DB_PASSWD = \"x\"",
            "env in datasets.clean sets DB_PASSWD, which looks like a secret",
            "data.toml and data.lock are shared, and the step inherits your environment, so set DB_PASSWD there",
        ),
        (
            "OPENAI_API_KEY = \"x\"",
            "env in datasets.clean sets OPENAI_API_KEY, which looks like a secret",
            "data.toml and data.lock are shared, and the step inherits your environment, so set OPENAI_API_KEY there",
        ),
        (
            "GITHUBTOKEN = \"x\"",
            "env in datasets.clean sets GITHUBTOKEN, which looks like a secret",
            "data.toml and data.lock are shared, and the step inherits your environment, so set GITHUBTOKEN there",
        ),
        (
            "\"MY VAR\" = \"1\"",
            "env in datasets.clean sets \"MY VAR\", which is not a variable name every system takes",
            "name variables with letters, digits and _, not starting with a digit",
        ),
        (
            "\"1SEED\" = \"1\"",
            "env in datasets.clean sets \"1SEED\", which is not a variable name every system takes",
            "name variables with letters, digits and _, not starting with a digit",
        ),
        (
            "SEED = \"a\\u0000b\"",
            "env in datasets.clean gives SEED a value with a NUL character, which no program can be given",
            "remove the NUL character from the value",
        ),
    ] {
        let error = fails(&recipe(env));
        assert_eq!(error.kind, Kind::ProjectInvalid, "{env}");
        assert_eq!(error.message, message, "{env}");
        assert_eq!(error.help.as_deref(), Some(help), "{env}");
        assert_eq!(error.details.len(), 1, "{env}");
        assert!(error.details[0].starts_with("data.toml:7"), "{env}");
        assert!(!error.details[0].contains("hf_abc"), "{env}");
    }
}

/// a CRLF checkout's variable reads with LF, so a recipe's key is the same on every system.
#[test]
fn a_multi_line_variable_reads_alike_in_a_crlf_checkout() {
    let text = format!("{RAW}{CLEAN}env = {{ HEADER = \"\"\"id\nname\"\"\", TAB = \"a\\tb\" }}\n");
    for written in [text.clone(), text.replace('\n', "\r\n")] {
        let manifest = parse(&written);
        let env = &manifest.datasets["clean"].recipe().unwrap().env;
        assert_eq!(env["HEADER"], "id\nname", "{written:?}");
        assert_eq!(env["TAB"], "a\tb");
    }
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
            "the keys here are ref, select, exclude, extract, members, web_page, copy, path, place, from, run, code, env",
        ),
        (
            "[datasets.x]\nref = 'zenodo:1'\n[datasets.x.members.'x.zip']\nexculde = ['a/b.csv']\n",
            "data.toml:4   exculde = ['a/b.csv']",
            "did you mean `exclude`?",
        ),
    ] {
        let error = fails(text);
        assert_eq!(error.kind, Kind::ProjectInvalid);
        assert_eq!(error.details, [citation]);
        assert_eq!(error.help.as_deref(), Some(help));
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
    for written in [".", "./", " . "] {
        let manifest = parse(&format!("{table}path = \"{written}\"\n"));
        assert_eq!(
            manifest.datasets["x"].path.as_deref(),
            Some("."),
            "{written:?}"
        );
    }
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
        ("./raw", "it has . as a folder name"),
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
        " ",
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
    assert_eq!(parse("[project]\n").data_dir, "data");
    assert_eq!(
        parse("[project]\ndata_dir = '  inputs/raw///  '\n").data_dir,
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
    let replacement = manifest.datasets["replace"].source().unwrap();
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
    assert_eq!(
        manifest.datasets["b"].source().unwrap().reference,
        "zenodo:2"
    );
    let removed = without_dataset(&removed, "b").unwrap();
    assert_eq!(parse(&removed).datasets.len(), 0);
    let added = with_dataset("\u{feff}# notes\n", "a", "zenodo:1", &[], &[], true).unwrap();
    assert!(!added.starts_with('\u{feff}'));
    assert!(added.contains("# notes"));
    assert_eq!(
        parse(&added).datasets["a"].source().unwrap().reference,
        "zenodo:1"
    );
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

/// a data.toml for drop to change: excludes as one pattern or a list, members tables for a
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
fn drop_changes_at_once_write_what_changes_one_at_a_time_write() {
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
const PLAIN_LINES: [&str; 8] = [
    "ref = \"https://example.org/iris.csv\"",
    "ref = \"zenodo:3608990\"",
    "ref = \" arxiv:2401.00001 \"",
    "ref = \"https://example.org/a b?token=x#y\"",
    "place = false",
    "place = true",
    "# a note, with \"quotes\" and = signs",
    "  \t",
];
const NEAR_MISSES: [&str; 33] = [
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
    "place = False",
    "place = \"false\"",
    "place = 0",
    "select = [\"*.csv\"]",
    "from = [\"iris\"]",
    "run = [\"python3\", \"clean.py\", \"{iris}\", \"{out}\"]",
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

/// [`read_plain`] reads a data.toml only as [`Manifest::parse`] reads it: the data folder, each
/// dataset's name, ref and place, and no recipe. tables `add` writes, some lines swapped for near
/// misses, check it; what it declines goes to the parser.
#[test]
fn a_plain_data_toml_reads_as_the_parser_reads_it() {
    let mut rng = fastrand::Rng::with_seed(4);
    let mut plain_texts = 0;
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
        let whole = Manifest::parse(&text)
            .unwrap_or_else(|error| panic!("{text:?} read plain, but {}", error.message));
        assert_eq!(plain.data_dir, whole.data_dir, "{text:?}");
        let names = |manifest: &Manifest| manifest.datasets.keys().cloned().collect::<Vec<_>>();
        assert_eq!(names(&plain), names(&whole), "{text:?}");
        for (name, entry) in &whole.datasets {
            let read = &plain.datasets[name];
            assert!(entry.recipe().is_none(), "{text:?}");
            assert_eq!(
                (&read.source().unwrap().reference, read.place),
                (&entry.source().unwrap().reference, entry.place),
                "{text:?}"
            );
        }
    }
    // a run that read nothing plain would prove nothing
    assert!(plain_texts > 500, "only {plain_texts} texts read plain");
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
