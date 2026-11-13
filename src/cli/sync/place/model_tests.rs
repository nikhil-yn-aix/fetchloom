//! fixed observations compare placement and removal decisions with a small ownership model.

use std::fs::{File, FileTimes};
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::digest;
use crate::plan::Holder;
use crate::threads::Threads;

const PATH: &str = "data/set/file.bin";
const REFERENCE: &str = "https://example.org/file.bin";

fn payload(version: u8) -> [u8; 16] {
    [b'a' + version; 16]
}

fn stamp(path: &Path, nanos: u64) {
    File::open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_nanos(nanos)))
        .unwrap();
}

fn write_file(path: &Path, version: u8, writable: bool, modified: u64) {
    if path.exists() {
        fs::remove_file(path).unwrap();
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, payload(version)).unwrap();
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if writable { 0o644 } else { 0o444 }),
    )
    .unwrap();
    stamp(path, modified);
}

fn locked(version: u8) -> LockedFile {
    LockedFile {
        path: "file.bin".to_owned(),
        size: 16,
        sha256: digest::sha256(&payload(version)),
        md5: None,
        url: REFERENCE.to_owned(),
        unpacks_to: None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Keep,
    Vouch,
    Adopt,
    Place,
    Replace,
    Changed,
    Shared,
    Foreign,
}

fn choice(action: &Action) -> Choice {
    match action {
        Action::Keep => Choice::Keep,
        Action::Vouch(_) => Choice::Vouch,
        Action::Adopt(_) => Choice::Adopt,
        Action::Place => Choice::Place,
        Action::Replace | Action::Overwrite(_) => Choice::Replace,
        Action::Leave(Reason::Changed) => Choice::Changed,
        Action::Leave(Reason::Shared) => Choice::Shared,
        Action::Leave(Reason::Foreign) => Choice::Foreign,
        Action::Leave(Reason::Unneeded) => panic!("a wanted target was called unneeded"),
        Action::Leave(Reason::Blocked)
        | Action::Blocked(_)
        | Action::Unblock(_)
        | Action::Freed(_) => {
            panic!("the model puts nothing in a file's way")
        }
        Action::Unselected => panic!("the model leaves no dataset out"),
    }
}

#[derive(Clone, Copy, Debug)]
enum Ownership {
    Untracked,
    Clean,
    Edited,
    CheckedIntact,
    CheckedDamaged,
}

#[derive(Clone, Copy, Debug)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the model enumerates independent observed flags"
)]
struct Observation {
    exists: bool,
    ownership: Ownership,
    wanted_bytes: bool,
    same_pin: bool,
    writable: bool,
    shared: bool,
}

fn reference(observed: Observation, copy: bool, force: bool) -> Choice {
    use Choice::{Adopt, Changed, Foreign, Keep, Place, Replace, Shared, Vouch};
    use Ownership::{CheckedDamaged, CheckedIntact, Clean, Edited, Untracked};
    if !observed.exists {
        return Place;
    }
    match observed.ownership {
        CheckedDamaged => Replace,
        Clean | CheckedIntact => {
            if !observed.same_pin || (copy && !observed.writable) {
                Replace
            } else if matches!(observed.ownership, CheckedIntact) {
                Vouch
            } else {
                Keep
            }
        }
        Edited | Untracked if observed.wanted_bytes => {
            if copy && !observed.writable {
                Replace
            } else {
                Adopt
            }
        }
        Edited | Untracked if force => Replace,
        Untracked => Foreign,
        Edited if observed.shared => Shared,
        Edited => Changed,
    }
}

fn observation_case(project: &Project, bytes: [u8; 5]) {
    let [scenario, force, copy, writable, shared] = bytes;
    let scenario = scenario % 12;
    let (force, copy, writable, shared) = (
        force & 1 != 0,
        copy & 1 != 0,
        writable & 1 != 0,
        shared & 1 != 0,
    );
    let destination = project.root.join(PATH);
    let alias = project.root.join("alias");
    if alias.exists() {
        fs::remove_file(&alias).unwrap();
    }
    if destination.exists() {
        fs::remove_file(&destination).unwrap();
    }
    let ownership = match scenario {
        0..=2 => Ownership::Untracked,
        3 | 4 | 11 => Ownership::Clean,
        7 => Ownership::CheckedDamaged,
        8 | 9 => Ownership::CheckedIntact,
        _ => Ownership::Edited,
    };
    let version = match scenario {
        2 | 5 | 7 | 10 => 2,
        4 | 9 => 1,
        _ => 0,
    };
    let writable = writable && !matches!(scenario, 7..=9 | 11);
    let shared = shared && matches!(scenario, 5 | 10);
    if scenario != 0 {
        write_file(&destination, version, writable, 1_000_000_000);
        if shared {
            fs::hard_link(&destination, &alias).unwrap();
        }
    }
    let mut survey = Survey::read(
        project,
        "data",
        Threads::default(),
        &crate::stop::Stop::default(),
    )
    .unwrap();
    if !matches!(ownership, Ownership::Untracked) {
        let mut look = survey.disk[PATH];
        if matches!(scenario, 5 | 6) {
            look.modified += 1;
        }
        let previous_version = u8::from(!matches!(scenario, 3 | 8 | 11));
        survey.state.files.insert(
            PATH.to_owned(),
            Placed {
                dataset: "set".to_owned(),
                sha256: digest::sha256(&payload(previous_version)),
                method: if shared || matches!(scenario, 7..=9) {
                    Method::Hardlink
                } else {
                    Method::Copy
                },
                look,
                writable,
                checked: 1,
                adopted: false,
                racy_intact: std::sync::OnceLock::new(),
            },
        );
    }
    if scenario == 7 {
        survey.elsewhere.insert(PATH.to_owned());
    }
    if matches!(scenario, 8 | 9) {
        survey.vouched.insert(PATH.to_owned());
    }
    if scenario == 10 {
        survey.edited.insert(PATH.to_owned());
    }
    let file = locked(0);
    let target = Target {
        dataset: "set",
        path: PATH.to_owned(),
        sha256: file.sha256,
        size: 16,
        copy,
        holder: Holder::File(&file),
    };
    let expected = reference(
        Observation {
            exists: scenario != 0,
            ownership,
            wanted_bytes: version == 0,
            same_pin: matches!(scenario, 3 | 8 | 11),
            writable,
            shared,
        },
        copy,
        force,
    );
    let actual = decide(&target, &survey, force, &destination).unwrap();
    assert_eq!(choice(&actual), expected, "observation {bytes:?}");
}

/// twelve file states meet every mix of force, copy, writable and shared, each checked against
/// `reference`, a small model of who owns the file.
#[test]
fn every_observed_file_gets_the_decision_the_ownership_model_predicts() {
    let run = tempfile::tempdir().unwrap();
    let project = Project {
        root: run.path().join("project"),
    };
    fs::create_dir_all(&project.root).unwrap();
    for scenario in 0..12 {
        for flags in 0..16 {
            observation_case(
                &project,
                [scenario, flags, flags >> 1, flags >> 2, flags >> 3],
            );
        }
    }
}

/// no edit of the project's shows in a file `--verify` found changed under its placed size and
/// time, whatever it was placed as.
#[test]
fn a_damaged_file_is_replaced_without_force() {
    let run = tempfile::tempdir().unwrap();
    let project = Project {
        root: run.path().join("project"),
    };
    let destination = project.root.join(PATH);
    write_file(&destination, 0, true, 1_000_000_000);
    let file = locked(0);
    for copy in [false, true] {
        let mut survey = Survey::read(
            &project,
            "data",
            Threads::default(),
            &crate::stop::Stop::default(),
        )
        .unwrap();
        survey.state.files.insert(
            PATH.to_owned(),
            Placed {
                dataset: "set".to_owned(),
                sha256: file.sha256,
                method: Method::Copy,
                look: survey.disk[PATH],
                writable: true,
                checked: 1,
                adopted: false,
                racy_intact: std::sync::OnceLock::new(),
            },
        );
        survey.damaged.insert(PATH.to_owned());
        let target = Target {
            dataset: "set",
            path: PATH.to_owned(),
            sha256: file.sha256,
            size: 16,
            copy,
            holder: Holder::File(&file),
        };
        let actual = decide(&target, &survey, false, &destination).unwrap();
        assert_eq!(choice(&actual), Choice::Replace, "copy {copy}");
    }
}

/// sync removes, keeps or forgets only the records no target wants of datasets the run works on.
#[test]
fn only_unwanted_records_of_a_dataset_in_scope_are_let_go() {
    let run = tempfile::tempdir().unwrap();
    let project = Project {
        root: run.path().join("project"),
    };
    let file = locked(0);
    let named = |name: &str| Scope::Only(std::collections::BTreeSet::from([name.to_owned()]));
    for (scope, selected) in [
        (Scope::All, true),
        (named("set"), true),
        (named("other"), false),
    ] {
        for flags in 0_u8..64 {
            let [wanted, unknown, missing, edited, different, holds] =
                std::array::from_fn(|bit| flags & (1 << bit) != 0);
            let mut survey = Survey::read(
                &project,
                "data",
                Threads::default(),
                &crate::stop::Stop::default(),
            )
            .unwrap();
            let look = Look {
                size: 16,
                modified: 123,
            };
            survey.state.files.insert(
                PATH.to_owned(),
                Placed {
                    dataset: "set".to_owned(),
                    sha256: file.sha256,
                    method: Method::Copy,
                    look,
                    writable: true,
                    checked: 123,
                    adopted: false,
                    racy_intact: std::sync::OnceLock::new(),
                },
            );
            let seen = Look {
                modified: if different { 124 } else { 123 },
                ..look
            };
            if !missing {
                survey.disk.insert(PATH.to_owned(), seen);
            }
            if edited {
                survey.edited.insert(PATH.to_owned());
            }
            let wanted = if wanted {
                HashSet::from([PATH])
            } else {
                HashSet::new()
            };
            let unknown = if unknown {
                vec![("set", &file)]
            } else {
                Vec::new()
            };
            let actual = unwanted(&survey, &wanted, &unknown, &scope, |_, _, _| holds);
            let protected = !wanted.is_empty() || !unknown.is_empty() || !selected;
            if protected {
                assert!(actual.is_empty(), "{scope:?} {flags:06b}");
                continue;
            }
            assert_eq!(actual.len(), 1, "{scope:?} {flags:06b}");
            let actual = &actual[0].1;
            if missing {
                assert!(matches!(actual, Unwanted::Gone));
            } else if edited || (different && !holds) {
                assert!(matches!(actual, Unwanted::Keep), "{flags:06b}");
            } else {
                assert!(
                    matches!(actual, Unwanted::Remove(found) if *found == seen),
                    "{flags:06b}"
                );
            }
        }
    }
}
