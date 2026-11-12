use std::fs;

use super::*;
use crate::digest;
use crate::project::lock::{LockedFile, lists};
use crate::source::Access;

/// data.toml holding one dataset for each `(name, ref)`, in that order.
fn toml(datasets: &[(&str, &str)]) -> String {
    datasets
        .iter()
        .map(|(name, reference)| format!("[datasets.{name}]\nref = \"{reference}\"\n"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// the pin of `name`, made from `reference`, of one file holding `bytes`.
fn pin(name: &str, reference: &str, bytes: &[u8]) -> Locked {
    let entry = &Manifest::parse(&toml(&[(name, reference)]))
        .unwrap()
        .datasets[name];
    Locked {
        name: name.to_owned(),
        reference: reference.to_owned(),
        spec: spec(entry.source().unwrap()),
        extract: true,
        resolved: reference.to_owned(),
        title: None,
        license: None,
        doi: None,
        access: Access::Unknown,
        timestamp: "2026-11-10T00:00:00Z".to_owned(),
        files: vec![LockedFile {
            path: format!("{name}.csv"),
            size: bytes.len() as u64,
            sha256: digest::sha256(bytes),
            md5: None,
            url: format!("https://example.org/{name}.csv"),
            unpacks_to: None,
        }],
    }
}

/// data.lock pinning `pins`.
fn lock(mut pins: Vec<Locked>) -> String {
    pins.sort_by(|a, b| a.name.cmp(&b.name));
    Lock {
        datasets: pins,
        ..Lock::default()
    }
    .render()
}

fn sides(ours: String, theirs: String, base: Option<String>) -> Sides {
    Sides {
        ours,
        theirs,
        shows_base: base.is_some(),
        base,
        ..Sides::default()
    }
}

/// merges the sides with `choices` in a project folder holding no lists.
fn merged(manifest: &Sides, locks: &Sides, choices: &[(&str, Side)]) -> Result<Merged, Error> {
    let root = tempfile::tempdir().unwrap();
    let choices = choices
        .iter()
        .map(|(name, side)| ((*name).to_owned(), *side))
        .collect();
    merge(manifest, Some(locks), &choices, root.path(), &|_| {})
}

fn names(lock: &Lock) -> Vec<&str> {
    lock.datasets
        .iter()
        .map(|locked| locked.name.as_str())
        .collect()
}

/// two teammates each added a dataset beside `core`.
fn two_additions(base: bool) -> (Sides, Sides) {
    let core = pin("core", "zenodo:3", b"core");
    let manifest = sides(
        toml(&[("alpha", "zenodo:1"), ("core", "zenodo:3")]),
        toml(&[("beta", "zenodo:2"), ("core", "zenodo:3")]),
        base.then(|| toml(&[("core", "zenodo:3")])),
    );
    let locks = sides(
        lock(vec![pin("alpha", "zenodo:1", b"alpha"), core.clone()]),
        lock(vec![pin("beta", "zenodo:2", b"beta"), core.clone()]),
        base.then(|| lock(vec![core])),
    );
    (manifest, locks)
}

/// the merge checks the data.toml it wrote before keeping it: one that lost or changed a dataset
/// the merge decided on, as the sides agreed it (`check_merged`) or as a person chose it
/// (`check_chosen`), is refused, with help to merge by hand.
#[test]
fn a_merged_data_toml_that_lost_or_changed_a_decided_dataset_is_refused() {
    let ours = toml(&[("alpha", "zenodo:1")]);
    let theirs = toml(&[("alpha", "zenodo:1"), ("beta", "zenodo:2")]);
    let mut both = sides(ours.clone(), theirs.clone(), None);
    both.whole = Some([ours, theirs]);
    let manifests = read_manifests(&both).unwrap();
    let takes = BTreeMap::from([("beta".to_owned(), Take::Side(Side::Theirs))]);
    let choices = Choices::from([("beta".to_owned(), Side::Theirs)]);
    let decided = Decided {
        chosen: BTreeSet::from(["beta".to_owned()]),
        ..Decided::default()
    };
    for (wrote, refused) in [
        (toml(&[("alpha", "zenodo:1"), ("beta", "zenodo:2")]), false),
        (toml(&[("alpha", "zenodo:1")]), true),
        (toml(&[("alpha", "zenodo:1"), ("beta", "zenodo:9")]), true),
    ] {
        let merged = Manifest::parse(&wrote).unwrap();
        for (check, result) in [
            ("check_merged", check_merged(&manifests, &merged, &takes)),
            (
                "check_chosen",
                check_chosen(&manifests, &merged, &choices, &decided),
            ),
        ] {
            match (refused, result) {
                (false, Ok(())) => {}
                (true, Err(error)) => {
                    assert_eq!(error.kind, Kind::ProjectInvalid);
                    assert_eq!(
                        error.message,
                        "fetchloom could not write the merged data.toml in its layout"
                    );
                    assert_eq!(
                        error.help.as_deref(),
                        Some("merge data.toml by hand, then run `fetchloom lock`")
                    );
                }
                (_, result) => panic!("{check} on {wrote:?}: {result:?}"),
            }
        }
    }
}

#[test]
fn independent_additions_are_kept() {
    let (manifest, locks) = two_additions(true);
    let merged = merged(&manifest, &locks, &[]).unwrap();
    assert_eq!(
        merged.manifest,
        toml(&[
            ("alpha", "zenodo:1"),
            ("beta", "zenodo:2"),
            ("core", "zenodo:3")
        ])
    );
    let lock = merged.lock.unwrap();
    assert_eq!(names(&lock), ["alpha", "beta", "core"]);
    assert_eq!(lock.datasets[1], pin("beta", "zenodo:2", b"beta"));
    assert_eq!(
        merged.summary,
        Summary {
            from_ours: vec!["alpha".to_owned()],
            from_theirs: vec!["beta".to_owned()],
            same: vec!["core".to_owned()],
            ..Summary::default()
        }
    );
    assert_eq!(merged.warnings, Vec::<String>::new());
}

#[test]
fn a_removal_survives_with_a_base() {
    let zeta = pin("zeta", "zenodo:9", b"zeta");
    let core = pin("core", "zenodo:3", b"core");
    let manifest = sides(
        toml(&[("zeta", "zenodo:9")]),
        toml(&[
            ("beta", "zenodo:2"),
            ("core", "zenodo:3"),
            ("zeta", "zenodo:9"),
        ]),
        Some(toml(&[("core", "zenodo:3"), ("zeta", "zenodo:9")])),
    );
    let locks = sides(
        lock(vec![zeta.clone()]),
        lock(vec![
            pin("beta", "zenodo:2", b"beta"),
            core.clone(),
            zeta.clone(),
        ]),
        Some(lock(vec![core, zeta])),
    );
    let merged = merged(&manifest, &locks, &[]).unwrap();
    assert_eq!(
        merged.manifest,
        toml(&[("beta", "zenodo:2"), ("zeta", "zenodo:9")])
    );
    assert_eq!(names(&merged.lock.unwrap()), ["beta", "zeta"]);
    assert_eq!(merged.summary.removed, ["core"]);
    assert_eq!(merged.summary.from_theirs, ["beta"]);
}

/// a base git's conflict lines showed but fetchloom could not read leaves the conflict style
/// alone: the style that shows a base is in use already, so the help names only git.
#[test]
fn a_base_shown_but_unread_names_no_conflict_style() {
    let (mut manifest, mut locks) = two_additions(false);
    manifest.shows_base = true;
    locks.shows_base = true;
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(
        error.details[2],
        "no merge base fetchloom can read was found: git on PATH, before `git add` marks the files resolved, lets fetchloom see what each side changed"
    );
}

/// a merge that needs a choice for more datasets than a thousand counts them with their
/// thousands apart.
#[test]
fn a_thousand_choices_are_counted_with_their_thousands_apart() {
    let conflicts = (0..1_200)
        .map(|index| (format!("set{index}"), Why::BothChanged))
        .collect();
    let error = undecided(&conflicts, &Summary::default(), false);
    assert_eq!(error.message, "the merge needs a choice for 1,200 datasets");
}

#[test]
fn one_sided_datasets_without_a_base_need_a_choice() {
    let (manifest, locks) = two_additions(false);
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(error.kind, Kind::Unconfirmed);
    assert_eq!(error.message, "the merge needs a choice for 2 datasets");
    assert_eq!(
        error.details[..2],
        [
            "alpha: only ours has it, and fetchloom cannot tell whether ours added it or theirs removed it",
            "beta: only theirs has it, and fetchloom cannot tell whether theirs added it or ours removed it",
        ]
    );
    assert!(error.details[2].starts_with("no merge base was found"));
    let help = error.help.as_deref().unwrap();
    assert!(help.contains("`fetchloom lock --ours <name>`"), "{help}");
    let result = error.result.unwrap();
    assert_eq!(
        result["merge"]["conflicts"],
        serde_json::json!(["alpha", "beta"])
    );
    assert_eq!(
        result["merge"]["same"],
        serde_json::json!(["core"]),
        "what needs no choice is told too"
    );

    let kept = merged(
        &manifest,
        &locks,
        &[("alpha", Side::Ours), ("beta", Side::Theirs)],
    )
    .unwrap();
    assert_eq!(names(&kept.lock.unwrap()), ["alpha", "beta", "core"]);
    let dropped = merged(
        &manifest,
        &locks,
        &[("alpha", Side::Theirs), ("beta", Side::Theirs)],
    )
    .unwrap();
    assert_eq!(
        dropped.manifest,
        toml(&[("beta", "zenodo:2"), ("core", "zenodo:3")])
    );
    assert_eq!(dropped.summary.removed, ["alpha"]);

    let both = [("alpha", Side::Ours), ("beta", Side::Ours)];
    for (extra, message) in [
        ("core", "core needs no choice in this merge"),
        ("nope", "nope is on neither side of the merge"),
    ] {
        let mut choices = both.to_vec();
        choices.push((extra, Side::Theirs));
        let error = merged(&manifest, &locks, &choices).unwrap_err();
        assert_eq!(error.kind, Kind::Usage);
        assert_eq!(error.message, message);
        assert_eq!(
            error.help.as_deref(),
            Some(format!("leave out --theirs {extra}").as_str())
        );
    }
}

/// both teammates upgraded one dataset: data.toml did not change, data.lock pins other bytes on
/// each side.
#[test]
fn two_changes_to_one_dataset_need_a_choice() {
    let text = toml(&[("version", "https://example.org/v.csv")]);
    let manifest = sides(text.clone(), text, None);
    let pinned = |bytes: &[u8]| pin("version", "https://example.org/v.csv", bytes);
    let locks = sides(
        lock(vec![pinned(b"2,left")]),
        lock(vec![pinned(b"3,right")]),
        Some(lock(vec![pinned(b"1,base")])),
    );
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(error.kind, Kind::Unconfirmed);
    assert_eq!(error.message, "the merge needs a choice for version");
    assert_eq!(error.details, ["version: each side changed it its own way"]);
    let theirs = merged(&manifest, &locks, &[("version", Side::Theirs)]).unwrap();
    assert_eq!(theirs.lock.unwrap().datasets, [pinned(b"3,right")]);
    assert_eq!(theirs.summary.from_theirs, ["version"]);

    // one side upgraded, and pinning it again at another time changes nothing else
    let mut later = pinned(b"1,base");
    later.timestamp = "2026-11-11T00:00:00Z".to_owned();
    let locks = sides(
        lock(vec![later]),
        lock(vec![pinned(b"3,right")]),
        Some(lock(vec![pinned(b"1,base")])),
    );
    let upgraded = merged(&manifest, &locks, &[]).unwrap();
    assert_eq!(upgraded.lock.unwrap().datasets, [pinned(b"3,right")]);
}

/// `extract` and `access` sit beside the spec: a pin one side made again with archives kept
/// packed, or with what its source now says about access, is that side's change.
#[test]
fn a_pin_one_side_made_again_for_extract_or_access_wins() {
    let packed_toml = "[datasets.x]\nref = \"zenodo:1\"\nextract = false\n".to_owned();
    let manifest = sides(
        packed_toml.clone(),
        toml(&[("x", "zenodo:1")]),
        Some(toml(&[("x", "zenodo:1")])),
    );
    let mut unpacked = pin("x", "zenodo:1", b"x");
    unpacked.files[0].unpacks_to = Some(digest::sha256(b"tree"));
    let packed = Locked {
        extract: false,
        files: vec![LockedFile {
            unpacks_to: None,
            ..unpacked.files[0].clone()
        }],
        ..unpacked.clone()
    };
    let public = Locked {
        access: Access::Public,
        ..unpacked.clone()
    };
    let locks = sides(
        lock(vec![packed.clone()]),
        lock(vec![public.clone()]),
        Some(lock(vec![unpacked.clone()])),
    );
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(error.details, ["x: each side changed it its own way"]);
    let ours = merged(&manifest, &locks, &[("x", Side::Ours)]).unwrap();
    assert_eq!(ours.manifest, packed_toml);
    assert_eq!(ours.lock.unwrap().datasets, [packed]);
    let only_access = sides(
        lock(vec![unpacked.clone()]),
        lock(vec![public.clone()]),
        Some(lock(vec![unpacked])),
    );
    let text = toml(&[("x", "zenodo:1")]);
    let same_toml = sides(text.clone(), text, None);
    let theirs = merged(&same_toml, &only_access, &[]).unwrap();
    assert_eq!(theirs.lock.unwrap().datasets, [public]);
    assert_eq!(theirs.summary.from_theirs, ["x"]);
}

/// a dataset both sides changed in data.toml takes the chosen side's entry, and that side's pin
/// when it answers the entry; otherwise it is left for `fetchloom lock` to pin.
#[test]
fn the_lock_record_follows_the_chosen_entry_by_spec() {
    let manifest = sides(
        toml(&[("x", "zenodo:2")]),
        toml(&[("x", "zenodo:3")]),
        Some(toml(&[("x", "zenodo:1")])),
    );
    let relocked = sides(
        lock(vec![pin("x", "zenodo:2", b"two")]),
        lock(vec![pin("x", "zenodo:3", b"three")]),
        Some(lock(vec![pin("x", "zenodo:1", b"one")])),
    );
    let error = merged(&manifest, &relocked, &[]).unwrap_err();
    assert_eq!(error.details, ["x: each side changed it its own way"]);
    let theirs = merged(&manifest, &relocked, &[("x", Side::Theirs)]).unwrap();
    assert_eq!(theirs.manifest, toml(&[("x", "zenodo:3")]));
    assert_eq!(
        theirs.lock.unwrap().datasets,
        [pin("x", "zenodo:3", b"three")]
    );
    assert_eq!(theirs.summary.unpinned, Vec::<String>::new());

    let unlocked = sides(
        lock(vec![pin("x", "zenodo:2", b"two")]),
        lock(vec![pin("x", "zenodo:1", b"one")]),
        Some(lock(vec![pin("x", "zenodo:1", b"one")])),
    );
    let stale = merged(&manifest, &unlocked, &[("x", Side::Theirs)]).unwrap();
    assert_eq!(stale.lock.unwrap().datasets, Vec::<Locked>::new());
    assert_eq!(stale.summary.unpinned, ["x"]);
}

/// a big dataset's files live in parts named by their content, so after a merge the parts of
/// both sides can be there, or only those of the side git kept. only the kept side's are read.
#[test]
fn listed_datasets_merge_from_the_parts_of_the_side_kept() {
    let root = tempfile::tempdir().unwrap();
    let mut photos = pin("photos", "zenodo:7", b"one");
    photos.files.push(LockedFile {
        path: "photos2.csv".to_owned(),
        ..photos.files[0].clone()
    });
    let mut upgraded = photos.clone();
    upgraded.files[1].size = 2;
    let listed = |datasets: Vec<Locked>| Lock {
        datasets,
        ..Lock::default()
    };
    let ours = listed(vec![photos.clone()]);
    let theirs = listed(vec![upgraded.clone(), pin("beta", "zenodo:2", b"beta")]);
    fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
    for (part, text) in theirs.lists_with(1) {
        fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
    }
    let manifest = sides(
        toml(&[("photos", "zenodo:7")]),
        toml(&[("beta", "zenodo:2"), ("photos", "zenodo:7")]),
        Some(toml(&[("photos", "zenodo:7")])),
    );
    let locks = sides(
        ours.render_with(1),
        theirs.render_with(1),
        Some(ours.render_with(1)),
    );
    let choices = Choices::new();
    let merged = merge(&manifest, Some(&locks), &choices, root.path(), &|_| {}).unwrap();
    let lock = merged.lock.unwrap();
    assert_eq!(names(&lock), ["beta", "photos"]);
    assert_eq!(lock.datasets[1].files, upgraded.files);
    assert_eq!(merged.summary.from_theirs, ["beta", "photos"]);
}

#[test]
fn recipe_results_merge_by_name() {
    let root = tempfile::tempdir().unwrap();
    let files = vec![crate::cache::Member {
        path: "out.csv".to_owned(),
        size: 1,
        sha256: digest::sha256(b"x"),
    }];
    let clean = Derived {
        name: "clean".to_owned(),
        from: vec![super::super::derived::Input {
            name: "core".to_owned(),
            content: digest::sha256(b"core"),
        }],
        run: ["python3", "clean.py", "{core}", "{out}"]
            .map(str::to_owned)
            .to_vec(),
        code: Vec::new(),
        env: BTreeMap::new(),
        key: digest::sha256(b"key"),
        timestamp: "2026-11-10T00:00:00Z".to_owned(),
        tree: crate::cache::tree_hash(&files),
        files,
    };
    let core = pin("core", "zenodo:3", b"core");
    let with_result = Lock {
        datasets: vec![core.clone()],
        derived: vec![clean.clone()],
    };
    fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
    for (part, text) in with_result.lists() {
        fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
    }
    let recipe = "\n[datasets.clean]\nfrom = [\"core\"]\nrun = [\"python3\", \"clean.py\", \"{core}\", \"{out}\"]\n";
    let text = format!("{}{recipe}", toml(&[("core", "zenodo:3")]));
    let manifest = sides(text.clone(), text.clone(), None);
    let added = sides(
        with_result.render(),
        lock(vec![core.clone()]),
        Some(lock(vec![core.clone()])),
    );
    let choices = Choices::new();
    let merged = merge(&manifest, Some(&added), &choices, root.path(), &|_| {}).unwrap();
    assert_eq!(merged.lock.unwrap().derived, std::slice::from_ref(&clean));
    assert_eq!(merged.summary.from_ours, ["clean"]);
    // the one result data.toml's recipe has is its pin, whatever the base was
    let unknown = sides(added.ours.clone(), added.theirs.clone(), None);
    let merged = merge(&manifest, Some(&unknown), &choices, root.path(), &|_| {}).unwrap();
    assert_eq!(merged.lock.unwrap().derived, std::slice::from_ref(&clean));
    let rebuilt = Lock {
        datasets: vec![core.clone()],
        derived: vec![Derived {
            key: digest::sha256(b"another key"),
            ..clean.clone()
        }],
    };
    let both = sides(with_result.render(), rebuilt.render(), None);
    let error = merge(&manifest, Some(&both), &choices, root.path(), &|_| {}).unwrap_err();
    assert_eq!(error.message, "the merge needs a choice for clean");
    // a result goes with its recipe, as a fetched dataset's pin goes with its entry, unless the
    // other side built it anew: then removing the recipe waits for a choice
    let dropped = sides(text.clone(), toml(&[("core", "zenodo:3")]), Some(text));
    let error = merge(&dropped, Some(&added), &choices, root.path(), &|_| {}).unwrap_err();
    assert_eq!(
        error.details,
        ["clean: one side changed it and the other removed it"]
    );
    let removal = Choices::from([("clean".to_owned(), Side::Theirs)]);
    let merged = merge(&dropped, Some(&added), &removal, root.path(), &|_| {}).unwrap();
    assert!(!merged.manifest.contains("clean"), "{}", merged.manifest);
    assert_eq!(merged.lock.unwrap().derived, []);
    let built_before = sides(
        with_result.render(),
        lock(vec![core.clone()]),
        Some(with_result.render()),
    );
    let merged = merge(
        &dropped,
        Some(&built_before),
        &choices,
        root.path(),
        &|_| {},
    )
    .unwrap();
    assert_eq!(merged.lock.unwrap().derived, []);
    assert_eq!(merged.summary.removed, ["clean"]);
}

/// each side's recipes read, yet the merge of them does not: a person merges data.toml, told
/// why.
#[test]
fn a_merge_whose_recipes_would_not_read_says_why() {
    let recipe = |name: &str, from: &str| {
        format!(
            "\n[datasets.{name}]\nfrom = [\"{from}\"]\nrun = [\"t\", \"{{{from}}}\", \"{{out}}\"]\n"
        )
    };
    let raw = toml(&[("raw", "zenodo:1")]);
    let base = format!("{raw}{}{}", recipe("a", "raw"), recipe("b", "raw"));
    let ours = format!("{raw}{}{}", recipe("a", "b"), recipe("b", "raw"));
    let theirs = format!("{raw}{}{}", recipe("a", "raw"), recipe("b", "a"));
    let error = merged(
        &sides(ours, theirs, Some(base)),
        &sides(lock(Vec::new()), lock(Vec::new()), None),
        &[],
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "the merged data.toml would not read: datasets a and b are made from each other: a \u{2192} b \u{2192} a"
    );
    assert_eq!(
        error.help.as_deref(),
        Some("merge data.toml by hand, then run `fetchloom lock`")
    );
}

/// a recipe is pinned only by a build, so a merge leaving one unbuilt does not name it as a
/// dataset the next sync or lock pins.
#[test]
fn an_unbuilt_recipe_is_not_left_for_sync_to_pin() {
    let text = format!(
        "{}\n[datasets.clean]\nfrom = [\"core\"]\nrun = [\"t\", \"{{core}}\", \"{{out}}\"]\n",
        toml(&[("core", "zenodo:3"), ("new", "zenodo:4")])
    );
    let core = lock(vec![pin("core", "zenodo:3", b"core")]);
    let result = merged(
        &sides(text.clone(), text, None),
        &sides(core.clone(), core, None),
        &[],
    )
    .unwrap();
    assert_eq!(result.summary.unpinned, ["new"]);
}

#[test]
fn text_outside_the_datasets_must_agree_or_be_changed_on_one_side() {
    let datasets = toml(&[("a", "zenodo:1")]);
    let with = |note: &str| format!("# {note}\n\n{datasets}");
    let locks = sides(lock(Vec::new()), lock(Vec::new()), None);
    let both = sides(with("ours"), with("theirs"), Some(with("base")));
    let error = merged(&both, &locks, &[]).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert!(
        error
            .message
            .starts_with("data.toml differs outside its datasets")
    );
    assert_eq!(
        error.help.as_deref(),
        Some(
            "in data.toml, keep one side's lines of that part and delete the other side's and git's conflict lines by hand, then run `fetchloom lock`"
        ),
        "only a person can merge that part, so no fix"
    );
    let one = sides(with("base"), with("theirs"), Some(with("base")));
    assert_eq!(merged(&one, &locks, &[]).unwrap().manifest, with("theirs"));

    let broken = sides("[datasets.a\n".to_owned(), datasets.clone(), None);
    let error = merged(&broken, &locks, &[]).unwrap_err();
    assert_eq!(
        error.message,
        "data.toml on our side of the merge cannot be read"
    );
}

/// the same dataset written two ways on each side, with no base to tell which changed, is
/// written as our side has it.
#[test]
fn a_dataset_alike_but_written_apart_keeps_our_text_and_says_so() {
    let ours = "[datasets.a]\nref = \"zenodo:1\" # ours\n".to_owned();
    let theirs = "[datasets.a]\nref = \"zenodo:1\"\n".to_owned();
    let locks = sides(lock(Vec::new()), lock(Vec::new()), None);
    let merged = merged(&sides(ours.clone(), theirs, None), &locks, &[]).unwrap();
    assert_eq!(merged.manifest, ours);
    assert_eq!(merged.summary.same, ["a"]);
    assert_eq!(
        merged.warnings,
        [
            "kept datasets.a as our side writes it in data.toml; their side differs only in comments or spacing"
        ]
    );
}

/// one side removed beta, entry and pin, while the other pinned other files for it and left its
/// entry as it was, so git merged data.toml by itself, without beta. the new pin is not dropped
/// unasked. a choice of the side that pinned it takes back that side's entry whole, note and
/// folder too, which needs each side whole from git; a choice of the other side removes it. a pin
/// the other side left as it was goes with the removal.
#[test]
fn a_removal_waits_for_a_choice_when_the_other_side_pinned_anew() {
    let core = toml(&[("core", "zenodo:3")]);
    let kept_toml = format!(
        "# what the labels are\n[datasets.beta]\nref = \"zenodo:2\"\npath = \"tags\"\n\n{core}"
    );
    let manifest = sides(core.clone(), core.clone(), None);
    let core_pin = pin("core", "zenodo:3", b"core");
    let beta = |bytes: &[u8]| pin("beta", "zenodo:2", bytes);
    let locks = sides(
        lock(vec![core_pin.clone()]),
        lock(vec![beta(b"new"), core_pin.clone()]),
        Some(lock(vec![beta(b"old"), core_pin.clone()])),
    );
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(error.kind, Kind::Unconfirmed);
    assert_eq!(
        error.details,
        ["beta: one side changed it and the other removed it"]
    );
    let result = error.result.unwrap();
    assert_eq!(result["merge"]["conflicts"], serde_json::json!(["beta"]));
    assert_eq!(result["merge"]["same"], serde_json::json!(["core"]));
    assert_eq!(result["merge"]["removed"], serde_json::json!([]));

    let removed = merged(&manifest, &locks, &[("beta", Side::Ours)]).unwrap();
    assert_eq!(removed.manifest, core);
    assert_eq!(names(&removed.lock.unwrap()), ["core"]);
    assert_eq!(removed.summary.removed, ["beta"]);

    let without_git = merged(&manifest, &locks, &[("beta", Side::Theirs)]).unwrap_err();
    assert_eq!(without_git.kind, Kind::ProjectInvalid);
    assert_eq!(
        without_git.help.as_deref(),
        Some(
            "copy [datasets.beta] from their side's data.toml into data.toml by hand, then run `fetchloom lock`"
        )
    );
    let whole = Sides {
        whole: Some([core.clone(), kept_toml.clone()]),
        ..manifest.clone()
    };
    let kept = merged(&whole, &locks, &[("beta", Side::Theirs)]).unwrap();
    assert_eq!(
        manifest::tables(&kept.manifest).unwrap(),
        manifest::tables(&kept_toml).unwrap()
    );
    assert_eq!(
        kept.lock.unwrap().datasets,
        [beta(b"new"), core_pin.clone()]
    );
    assert_eq!(kept.summary.from_theirs, ["beta"]);

    let unknown = sides(locks.ours.clone(), locks.theirs.clone(), None);
    assert_eq!(
        merged(&manifest, &unknown, &[]).unwrap_err().details[0],
        "beta: only theirs has it, and fetchloom cannot tell whether theirs added it or ours removed it"
    );
    let untouched = sides(
        lock(vec![core_pin.clone()]),
        lock(vec![beta(b"old"), core_pin.clone()]),
        Some(lock(vec![beta(b"old"), core_pin])),
    );
    let gone = merged(&manifest, &untouched, &[]).unwrap();
    assert_eq!(names(&gone.lock.unwrap()), ["core"]);
    assert_eq!(gone.summary.removed, ["beta"]);
}

/// git merged their new list's name into our side of data.lock's conflict lines, beside our
/// list's hash, and took our list's part away. read from those lines, our pin of photos is no pin
/// either side made: the merge stops and says how to put ours back by hand, with no command to
/// run as it is. with each side whole from git, our pin is the one we made.
#[test]
fn a_pin_mixed_from_both_sides_is_never_kept() {
    let root = tempfile::tempdir().unwrap();
    let mut photos = pin("photos", "zenodo:7", b"one");
    photos.files.push(LockedFile {
        path: "photos2.csv".to_owned(),
        ..photos.files[0].clone()
    });
    let mut upgraded = photos.clone();
    upgraded.files[1].size = 2;
    let listed = |datasets: Vec<Locked>| Lock {
        datasets,
        ..Lock::default()
    };
    let (ours, theirs) = (listed(vec![photos.clone()]), listed(vec![upgraded]));
    fs::create_dir(root.path().join(lists::FOLDER)).unwrap();
    let write = |lock: &Lock| {
        for (part, text) in lock.lists_with(1) {
            fs::write(root.path().join(part), lists::compress(&text).unwrap()).unwrap();
        }
    };
    write(&theirs);
    let (ours_text, theirs_text) = (ours.render_with(1), theirs.render_with(1));
    let files_in = |text: &str| {
        text.lines()
            .find(|line| line.starts_with("files_in"))
            .unwrap()
            .to_owned()
    };
    let mixed = ours_text.replace(&files_in(&ours_text), &files_in(&theirs_text));
    let text = toml(&[("photos", "zenodo:7")]);
    let manifest = sides(text.clone(), text, None);
    let locks = sides(mixed, theirs_text.clone(), None);
    let choices = Choices::from([("photos".to_owned(), Side::Ours)]);
    let error = merge(&manifest, Some(&locks), &choices, root.path(), &|_| {}).unwrap_err();
    assert_eq!(error.kind, Kind::ProjectInvalid);
    assert_eq!(
        error.message,
        "photos on our side of data.lock cannot be read whole"
    );
    assert!(
        error
            .help
            .as_deref()
            .is_some_and(|help| help.ends_with("by hand, then run `fetchloom lock`")),
        "{error:?}"
    );

    write(&ours);
    let whole = Sides {
        whole: Some([ours_text, theirs_text]),
        ..locks
    };
    let kept = merge(&manifest, Some(&whole), &choices, root.path(), &|_| {}).unwrap();
    assert_eq!(kept.lock.unwrap().datasets, [photos]);
}

/// data.toml merged alone, with `choices`, as when neither side has a data.lock.
fn text_merged(manifest: &Sides, choices: &[(&str, Side)]) -> String {
    let root = tempfile::tempdir().unwrap();
    let choices = choices
        .iter()
        .map(|(name, side)| ((*name).to_owned(), *side))
        .collect();
    merge(manifest, None, &choices, root.path(), &|_| {})
        .unwrap()
        .manifest
}

const PROJECT: &str = "[project]\ndata_dir = \"data\"\n\n# datasets the lab tracks\n";

/// their side, which took a note out with the dataset under it, keeps its layout, and our
/// addition goes in before the dataset it comes before, with the comment touching it.
#[test]
fn a_side_that_changed_a_note_gives_the_layout_our_additions_go_into() {
    let ours = format!(
        "{PROJECT}\n# the alpha set, from zenodo\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.core]\nref = \"zenodo:3\"  # pinned for the paper\n\n# a note that stands apart\n\n# gone goes with this comment\n[datasets.gone]\nref = \"zenodo:9\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let theirs = format!(
        "{PROJECT}\n# beta, added on their side\n[datasets.beta]\nref = \"zenodo:2\"\n\n[datasets.core]\n# core moved to its new record\nref = \"zenodo:4\"\n\n[datasets.omega]\nref = \"zenodo:15\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n\n# the last one, after them all\n[datasets.zz]\nref = \"zenodo:27\"\n"
    );
    let base = format!(
        "{PROJECT}\n[datasets.core]\nref = \"zenodo:3\"  # pinned for the paper\n\n# a note that stands apart\n\n# gone goes with this comment\n[datasets.gone]\nref = \"zenodo:9\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let merged = text_merged(&sides(ours, theirs, Some(base)), &[]);
    assert_eq!(
        merged,
        "[project]\ndata_dir = \"data\"\n\n# datasets the lab tracks\n\n# the alpha set, from zenodo\n[datasets.alpha]\nref = \"zenodo:1\"\n\n# beta, added on their side\n[datasets.beta]\nref = \"zenodo:2\"\n\n[datasets.core]\n# core moved to its new record\nref = \"zenodo:4\"\n\n[datasets.omega]\nref = \"zenodo:15\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n\n# the last one, after them all\n[datasets.zz]\nref = \"zenodo:27\"\n"
    );
}

/// with the text outside the datasets alike on both sides, the merge keeps our layout: their
/// additions go in before the dataset each comes before, or last, a change of theirs replaces
/// ours where it stands, and their removal takes our table out with the comment touching it,
/// leaving the note that stood apart above what follows.
#[test]
fn a_merge_of_many_edits_keeps_our_layout_and_their_comments() {
    let ours = format!(
        "{PROJECT}\n# the alpha set, from zenodo\n[datasets.alpha]\nref = \"zenodo:1\"\n\n[datasets.core]\nref = \"zenodo:3\"  # pinned for the paper\n\n# a note that stands apart\n\n# gone goes with this comment\n[datasets.gone]\nref = \"zenodo:9\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let theirs = format!(
        "{PROJECT}\n# beta, added on their side\n[datasets.beta]\nref = \"zenodo:2\"\n\n[datasets.core]\n# core moved to its new record\nref = \"zenodo:4\"\n\n# a note that stands apart\n\n[datasets.omega]\nref = \"zenodo:15\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n\n# the last one, after them all\n[datasets.zz]\nref = \"zenodo:27\"\n"
    );
    let base = format!(
        "{PROJECT}\n[datasets.core]\nref = \"zenodo:3\"  # pinned for the paper\n\n# a note that stands apart\n\n# gone goes with this comment\n[datasets.gone]\nref = \"zenodo:9\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let merged = text_merged(&sides(ours, theirs, Some(base)), &[]);
    assert_eq!(
        merged,
        "[project]\ndata_dir = \"data\"\n\n# datasets the lab tracks\n\n# the alpha set, from zenodo\n[datasets.alpha]\nref = \"zenodo:1\"\n\n# beta, added on their side\n[datasets.beta]\nref = \"zenodo:2\"\n\n[datasets.core]\n# core moved to its new record\nref = \"zenodo:4\"\n\n# a note that stands apart\n\n[datasets.omega]\nref = \"zenodo:15\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n\n# the last one, after them all\n[datasets.zz]\nref = \"zenodo:27\"\n"
    );
}

/// a choice of a side keeps that side's entry whole, as its branch writes it, or its removal,
/// each in its place, whichever side each choice names.
#[test]
fn choices_keep_each_chosen_entry_whole_in_place() {
    let ours = format!(
        "{PROJECT}\n[datasets.core]\nref = \"zenodo:5\"  # ours moved it\n\n# the kept one\n[datasets.kept]\nref = \"zenodo:7\"\n\n# gone, changed on our side\n[datasets.gone]\nref = \"zenodo:10\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let theirs = format!(
        "{PROJECT}\n# theirs moved core\n[datasets.core]\nref = \"zenodo:4\"\n\n# the kept one, theirs\n[datasets.kept]\nref = \"zenodo:8\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let base = format!(
        "{PROJECT}\n[datasets.core]\nref = \"zenodo:3\"\n\n[datasets.kept]\nref = \"zenodo:6\"\n\n[datasets.gone]\nref = \"zenodo:9\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
    let mut manifest = sides(ours.clone(), theirs.clone(), Some(base));
    manifest.whole = Some([ours, theirs]);
    let merged = text_merged(
        &manifest,
        &[
            ("core", Side::Theirs),
            ("kept", Side::Ours),
            ("gone", Side::Theirs),
        ],
    );
    assert_eq!(
        merged,
        "[project]\ndata_dir = \"data\"\n\n# datasets the lab tracks\n\n# theirs moved core\n[datasets.core]\nref = \"zenodo:4\"\n\n# the kept one\n[datasets.kept]\nref = \"zenodo:7\"\n\n[datasets.zeta]\nref = \"zenodo:26\"\n"
    );
}

/// a merge reads data.toml as often for many datasets as for few, rather than once more for each
/// dataset it copies in, changes in place or takes out.
#[test]
fn a_merge_reads_data_toml_as_often_for_many_datasets_as_for_few() {
    let datasets = |prefix: &str, count: usize, record: &dyn Fn(usize) -> Option<usize>| {
        (0..count)
            .filter_map(|at| {
                let record = record(at)?;
                Some(format!(
                    "[datasets.{prefix}{at:03}]\nref = \"zenodo:{record}\"\n\n"
                ))
            })
            .collect::<String>()
    };
    let reads = |count: usize| {
        let base = datasets("c", count, &Some);
        let ours = format!("{PROJECT}\n{}{base}", datasets("a", count, &Some));
        let theirs = format!(
            "{PROJECT}\n{}{}{}",
            datasets("b", count, &|at| Some(at + 100)),
            datasets("c", count, &|at| match at % 3 {
                0 => Some(at + 1000),
                1 => None,
                _ => Some(at),
            }),
            datasets("z", count, &|at| Some(at + 200)),
        );
        let before = manifest::tests::reads();
        let merged = text_merged(
            &sides(ours, theirs, Some(format!("{PROJECT}\n{base}"))),
            &[],
        );
        let reads = manifest::tests::reads() - before;
        let manifest = Manifest::parse(&merged).unwrap();
        let removed = (count + 1) / 3;
        assert_eq!(manifest.datasets.len(), 4 * count - removed);
        assert_eq!(
            manifest.datasets["c000"].source().unwrap().reference,
            "zenodo:1000"
        );
        reads
    };
    assert_eq!(
        reads(200),
        reads(2),
        "data.toml is read again for each dataset"
    );
}
