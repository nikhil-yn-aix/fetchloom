use std::cell::RefCell;
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
        spec: spec(&entry.source),
        at: None,
        extract: true,
        decompress: false,
        web_page: false,
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
    merge(manifest, Some(locks), &choices, root.path(), &|_| {}, None)
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

/// a merged data.toml is checked against datasets the sides agreed on (`check_merged`) and those a
/// person chose (`check_chosen`); the help is to merge by hand.
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

/// conflict lines that show a base mean that style is in use already, so the help names only git.
#[test]
fn a_base_shown_but_unread_names_no_conflict_style() {
    let (mut manifest, mut locks) = two_additions(false);
    manifest.shows_base = true;
    locks.shows_base = true;
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(
        error.details.last().unwrap(),
        "no merge base fetchloom can read was found: git on PATH, before `git add` marks the files resolved, lets fetchloom see what each side changed"
    );
}

#[test]
fn a_thousand_choices_are_counted_with_their_thousands_apart() {
    let conflicts = (0..1_200)
        .map(|index| (format!("set{index}"), Why::BothChanged))
        .collect();
    let error = undecided(&conflicts, &Summary::default(), false, Path::new("."));
    assert_eq!(error.message, "the merge needs a choice for 1,200 datasets");
}

#[test]
fn one_sided_datasets_without_a_base_need_a_choice() {
    let (manifest, locks) = two_additions(false);
    let error = merged(&manifest, &locks, &[]).unwrap_err();
    assert_eq!(error.kind, Kind::Unconfirmed);
    assert_eq!(error.message, "the merge needs a choice for 2 datasets");
    assert_eq!(
        error.details[..6],
        [
            "alpha: only ours has it, and fetchloom cannot tell whether ours added it or theirs removed it",
            "alpha on ours: ref = \"zenodo:1\"",
            "alpha on theirs: not in data.toml",
            "beta: only theirs has it, and fetchloom cannot tell whether theirs added it or ours removed it",
            "beta on ours: not in data.toml",
            "beta on theirs: ref = \"zenodo:2\"",
        ]
    );
    assert!(error.details[6].starts_with("no merge base was found"));
    let help = error.help.as_deref().unwrap();
    assert!(help.contains("`fetchloom lock --ours <name>`"), "{help}");
    let result = error.result.unwrap();
    assert_eq!(
        result["merge"]["conflicts"],
        serde_json::json!(["alpha", "beta"])
    );
    assert_eq!(
        result["merge"]["sides"],
        serde_json::json!({
            "alpha": { "ours": ["ref = \"zenodo:1\""], "theirs": ["not in data.toml"] },
            "beta": { "ours": ["not in data.toml"], "theirs": ["ref = \"zenodo:2\""] },
        })
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
    let root = tempfile::tempdir().unwrap();
    let told = RefCell::new(Vec::new());
    let error = merge(
        &manifest,
        Some(&locks),
        &Choices::new(),
        root.path(),
        &|line| told.borrow_mut().push(line.to_owned()),
        None,
    )
    .unwrap_err();
    assert_eq!(error.kind, Kind::Unconfirmed);
    assert_eq!(error.message, "the merge needs a choice for version");
    let pins = |bytes: &[u8]| {
        format!(
            "1 file \u{b7} {} B; version.csv \u{b7} {} B \u{b7} sha256 {}",
            bytes.len(),
            bytes.len(),
            digest::sha256(bytes).short()
        )
    };
    assert_eq!(
        error.details,
        [
            "version: each side changed it its own way".to_owned(),
            format!("version on ours: {}", pins(b"2,left")),
            format!("version on theirs: {}", pins(b"3,right")),
        ]
    );
    assert_eq!(
        told.into_inner(),
        Vec::<String>::new(),
        "-v adds nothing the refusal shows"
    );
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

/// a dataset both sides changed in data.toml shows the keys each side holds apart, each on one
/// line however data.toml spreads it, and `-v` adds what each side pins.
#[test]
fn a_needed_choice_shows_the_keys_each_side_holds_apart() {
    let base = "[datasets.x]\nref = \"zenodo:1\"\n".to_owned();
    let ours =
        format!("{base}select = [\n  \"a.csv\", # the first\n  \"b.csv\",\n]\npath = \"x\"\n");
    let theirs = format!("{base}select = [\"a.csv\"]\nexclude = [\"*.tmp\"]\n");
    let pinned = lock(vec![pin("x", "zenodo:1", b"x")]);
    let locks = sides(pinned.clone(), pinned.clone(), Some(pinned));
    let root = tempfile::tempdir().unwrap();
    let told = RefCell::new(Vec::new());
    let error = merge(
        &sides(ours, theirs, Some(base)),
        Some(&locks),
        &Choices::new(),
        root.path(),
        &|line| told.borrow_mut().push(line.to_owned()),
        None,
    )
    .unwrap_err();
    assert_eq!(
        error.details,
        [
            "x: each side changed it its own way",
            "x on ours: no exclude; path = \"x\"; select = [\"a.csv\", \"b.csv\"]",
            "x on theirs: exclude = [\"*.tmp\"]; no path; select = [\"a.csv\"]",
        ]
    );
    assert_eq!(
        error.result.unwrap()["merge"]["sides"]["x"]["theirs"],
        serde_json::json!(["exclude = [\"*.tmp\"]", "no path", "select = [\"a.csv\"]"])
    );
    assert_eq!(
        told.into_inner(),
        ["x, ours: 1 file \u{b7} 1 B", "x, theirs: 1 file \u{b7} 1 B"]
    );
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
        decompress: false,
        web_page: false,
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
    assert_eq!(
        error.details,
        [
            "x: each side changed it its own way",
            "x on ours: extract = false",
            "x on theirs: no extract",
        ]
    );
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
    assert_eq!(
        error.details,
        [
            "x: each side changed it its own way",
            "x on ours: ref = \"zenodo:2\"",
            "x on theirs: ref = \"zenodo:3\"",
        ]
    );
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
    let merged = merge(
        &manifest,
        Some(&locks),
        &choices,
        root.path(),
        &|_| {},
        None,
    )
    .unwrap();
    let lock = merged.lock.unwrap();
    assert_eq!(names(&lock), ["beta", "photos"]);
    assert_eq!(lock.datasets[1].files, upgraded.files);
    assert_eq!(merged.summary.from_theirs, ["beta", "photos"]);
    // a git merge deletes a part the other side replaced: without git the error names the command
    // that brings it back, and with git it comes from their side's commit
    let parts = theirs.lists_with(1);
    let (part, text) = parts
        .iter()
        .find(|(part, _)| part.contains("photos"))
        .unwrap();
    fs::remove_file(root.path().join(part)).unwrap();
    let gone = merge(
        &manifest,
        Some(&locks),
        &choices,
        root.path(),
        &|_| {},
        None,
    )
    .unwrap_err();
    assert_eq!(
        gone.help.as_deref(),
        Some(format!("bring it back from their side's commit with `git checkout MERGE_HEAD -- {part}`, or `git checkout REBASE_HEAD -- {part}` in a rebase, then run the same command again").as_str())
    );
    let bytes = lists::compress(text).unwrap();
    let committed =
        |side: Side, asked: &str| (side == Side::Theirs && asked == part).then(|| bytes.clone());
    let merged = merge(
        &manifest,
        Some(&locks),
        &choices,
        root.path(),
        &|_| {},
        Some(&committed),
    )
    .unwrap();
    assert_eq!(merged.lock.unwrap().datasets[1].files, upgraded.files);
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

/// no base tells which side changed the dataset.
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

/// git merged data.toml without beta, which one side removed while the other pinned other files
/// for it. choosing the pinning side brings back its entry whole, note and folder too; the other
/// side's choice removes it, and an unchanged pin goes with the removal.
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
        [
            "beta: one side changed it and the other removed it".to_owned(),
            "beta on ours: not in data.lock".to_owned(),
            format!(
                "beta on theirs: 1 file \u{b7} 3 B; beta.csv \u{b7} 3 B \u{b7} sha256 {}",
                digest::sha256(b"new").short()
            ),
        ]
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
        whole: Some([core, kept_toml.clone()]),
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

    let unknown = sides(locks.ours.clone(), locks.theirs, None);
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

/// what a refusal shows of the other side holds only what fetchloom writes: list parts by the
/// names it gives them, and no `ref` query, which may carry a secret.
#[test]
fn a_needed_choice_shows_no_unchecked_part_or_secret() {
    let mut photos = pin("photos", "zenodo:7", b"one");
    photos.files.push(LockedFile {
        path: "photos2.csv".to_owned(),
        ..photos.files[0].clone()
    });
    let listed = Lock {
        datasets: vec![photos],
        ..Lock::default()
    };
    let [(part, _)] = &listed.lists_with(1)[..] else {
        panic!("photos has one list part");
    };
    let theirs = listed
        .render_with(1)
        .replace(part.as_str(), "data.lock.d/../../planted.tsv.zst");
    let text = toml(&[("photos", "zenodo:7")]);
    let ours = lock(vec![pin("photos", "zenodo:7", b"two")]);
    let error = merged(
        &sides(text.clone(), text, None),
        &sides(ours, theirs, None),
        &[],
    )
    .unwrap_err();
    assert!(
        error.details.contains(
            &"photos on theirs: 2 files \u{b7} 6 B, listed in files fetchloom does not write"
                .to_owned()
        ),
        "{:?}",
        error.details
    );

    let entry = |token: &str| {
        format!("[datasets.raw]\nref = \"https://example.org/raw.csv?token={token}\"\n")
    };
    let manifest = sides(entry("ours1"), entry("theirs1"), Some(entry("base1")));
    let none = lock(Vec::new());
    let error = merged(
        &manifest,
        &sides(none.clone(), none.clone(), Some(none)),
        &[],
    )
    .unwrap_err();
    let said = error.details.join("\n");
    for secret in ["ours1", "theirs1", "base1"] {
        assert!(!said.contains(secret), "{said}");
    }
    let line = "raw on theirs: ref = \"https://example.org/raw.csv?token=***\"";
    assert!(error.details.iter().any(|detail| detail == line), "{said}");
}

/// the texts of all three locks are held to the room together before any is parsed, so three
/// that fit only one at a time are refused before parsing fills the container.
#[test]
fn three_lock_texts_must_fit_together_before_any_is_parsed() {
    let garbage = "x".repeat(1024);
    let three = sides(garbage.clone(), garbage.clone(), Some(garbage));
    let error = read_locks(&three, 4096).err().unwrap();
    assert_eq!(error.kind, Kind::LocalIo, "{error:?}");
    assert_eq!(
        error.message,
        "reading this project's data.lock's sides, which are 3.0 KiB together, takes about 6.0 KiB of memory, more than the 4.0 KiB this container leaves fetchloom"
    );
    assert_eq!(
        read_locks(&three, 6144).err().unwrap().kind,
        Kind::ProjectInvalid
    );
    assert_eq!(read_locks(&three, 6143).err().unwrap().kind, Kind::LocalIo);
}

/// three locks are held at once in a merge, so their files together must fit the memory a
/// container leaves, as one lock's must before its lists are read.
#[test]
fn three_locks_must_fit_together() {
    let one = lock(vec![pin("a", "zenodo:1", b"a")]);
    let three = sides(one.clone(), one.clone(), Some(one));
    assert!(read_locks(&three, u64::MAX).is_ok());
    let room = super::super::BASE + 2 * super::super::PER_FILE;
    let error = read_locks(&three, room).err().unwrap();
    assert_eq!(error.kind, Kind::LocalIo);
    assert!(
        error
            .message
            .starts_with("reading this project's 3 files takes about"),
        "{}",
        error.message
    );
}

/// the other side of a merge wrote data.lock, names of list parts too: a part that climbs out of
/// data.lock.d is refused before anything is read or written for it, whatever git gives.
#[test]
fn a_list_part_out_of_data_lock_d_is_never_written() {
    let root = tempfile::tempdir().unwrap();
    let mut photos = pin("photos", "zenodo:7", b"one");
    photos.files.push(LockedFile {
        path: "photos2.csv".to_owned(),
        ..photos.files[0].clone()
    });
    let listed = Lock {
        datasets: vec![photos],
        ..Lock::default()
    };
    let [(part, _)] = &listed.lists_with(1)[..] else {
        panic!("photos has one list part");
    };
    let escaped = format!("escaped-{}.tsv.zst", std::process::id());
    let outside = std::env::temp_dir().join(&escaped);
    let theirs = listed
        .render_with(1)
        .replace(part.as_str(), &format!("data.lock.d/../../{escaped}"));
    let text = toml(&[("photos", "zenodo:7")]);
    let manifest = sides(text.clone(), text, None);
    let locks = sides(lock(Vec::new()), theirs, Some(lock(Vec::new())));
    let committed = |_: Side, _: &str| Some(b"their bytes".to_vec());
    let merged = merge(
        &manifest,
        Some(&locks),
        &Choices::new(),
        root.path(),
        &|_| {},
        Some(&committed),
    );
    let written = outside.exists();
    let _ = fs::remove_file(&outside);
    assert!(!written, "a part was written at {}", outside.display());
    assert_eq!(merged.unwrap_err().kind, Kind::ProjectInvalid);
}

/// git merged their new list's name into our side's conflict lines beside our list's hash, and
/// took our part away: read from those lines, the merge stops with help to restore ours by hand.
/// read whole from git, our pin is the one we made.
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
    let error = merge(
        &manifest,
        Some(&locks),
        &choices,
        root.path(),
        &|_| {},
        None,
    )
    .unwrap_err();
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
    let kept = merge(
        &manifest,
        Some(&whole),
        &choices,
        root.path(),
        &|_| {},
        None,
    )
    .unwrap();
    assert_eq!(kept.lock.unwrap().datasets, [photos]);
}

/// data.toml merged alone, with `choices`, as when neither side has a data.lock.
fn text_merged(manifest: &Sides, choices: &[(&str, Side)]) -> String {
    let root = tempfile::tempdir().unwrap();
    let choices = choices
        .iter()
        .map(|(name, side)| ((*name).to_owned(), *side))
        .collect();
    merge(manifest, None, &choices, root.path(), &|_| {}, None)
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

/// with the text outside the datasets alike, our layout stays: their additions go in before the
/// dataset each precedes, or last; their change replaces ours in place; their removal takes our
/// table out with the comment touching it, leaving a note that stood apart.
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

/// each chosen entry stays as its side's branch writes it, or removed, as each choice names.
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

/// no dataset a merge copies in, changes in place or takes out costs another read.
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
        assert_eq!(manifest.datasets["c000"].source.reference, "zenodo:1000");
        reads
    };
    assert_eq!(
        reads(200),
        reads(2),
        "data.toml is read again for each dataset"
    );
}

/// a pin by its source's digests merges as any pin: the side that downloaded it wins over the side
/// that left it as the base had it, and one only a side added stays as it is.
#[test]
fn a_pin_not_downloaded_yet_merges_as_any_pin() {
    let stated = |name: &str, reference: &str| {
        let whole = pin(name, reference, name.as_bytes());
        super::super::Stated {
            files: whole
                .files
                .iter()
                .map(|file| super::super::StatedFile {
                    path: file.path.clone(),
                    size: file.size,
                    sha256: None,
                    md5: crate::digest::Md5::from_hex("0cc175b9c0f1b6a831c399e269772661"),
                    url: file.url.clone(),
                })
                .collect(),
            written: super::super::Words::of(&whole),
            pin: Locked {
                files: Vec::new(),
                ..whole
            },
        }
    };
    let text = |datasets: Vec<Locked>, stated: Vec<super::super::Stated>| {
        Lock {
            datasets,
            stated,
            ..Lock::default()
        }
        .render()
    };
    let core = stated("core", "zenodo:3");
    let manifest = sides(
        toml(&[("core", "zenodo:3")]),
        toml(&[("beta", "zenodo:2"), ("core", "zenodo:3")]),
        Some(toml(&[("core", "zenodo:3")])),
    );
    let locks = sides(
        text(vec![pin("core", "zenodo:3", b"core")], Vec::new()),
        text(Vec::new(), vec![stated("beta", "zenodo:2"), core.clone()]),
        Some(text(Vec::new(), vec![core.clone()])),
    );
    let lock = merged(&manifest, &locks, &[]).unwrap().lock.unwrap();
    assert_eq!(
        names(&lock),
        ["core"],
        "our download wins over the base's pin"
    );
    assert_eq!(lock.stated, [stated("beta", "zenodo:2")]);
    let unchanged = sides(
        text(Vec::new(), vec![core.clone()]),
        text(Vec::new(), vec![core.clone()]),
        None,
    );
    let manifest = sides(
        toml(&[("core", "zenodo:3")]),
        toml(&[("core", "zenodo:3")]),
        None,
    );
    let kept = merged(&manifest, &unchanged, &[]).unwrap().lock.unwrap();
    assert_eq!(kept.stated, [core]);
}
