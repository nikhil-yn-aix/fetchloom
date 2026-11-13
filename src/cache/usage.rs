//! where a cache's space goes, which `fetchloom cache` reports and the weekly reminder reads, so
//! the two cannot disagree.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use super::upkeep::{GRACE, Needed, Scan, Use};
use crate::digest::Sha256;
use crate::error::Error;
use crate::place::Method;
use crate::project::Project;
use crate::project::state::{Look, Placed};
use crate::stop::Stop;
use crate::threads::Threads;

/// where a cache's space goes, in payload bytes as gc counts them. they estimate what the disk
/// holds: the rest of a file's last block and a folder's own space are not counted.
#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct Usage {
    pub(crate) objects: u64,
    pub(crate) bytes: u64,
    /// what the registered projects need.
    pub(crate) needed: u64,
    /// of `needed`, what has another name outside the cache too, as a project's hardlink gives
    /// it, so deleting it from the cache would free nothing.
    pub(crate) linked: u64,
    /// what no registered project needs.
    pub(crate) unneeded: u64,
    /// of `unneeded`, what `cache gc` frees now: a day old, with no other name.
    pub(crate) freeable: u64,
    /// of `unneeded`, what gc frees once it is a day old.
    #[serde(skip)]
    pub(crate) young: u64,
    /// how many objects no registered project needs, which may all be empty files.
    #[serde(skip)]
    pub(crate) unneeded_objects: u64,
    /// files in registered projects that are copies of cached files, each a file of its own:
    /// placed as a copy or a clone, or placed as a hardlink that has lost its other name.
    pub(crate) copies: u64,
    /// of `copies`, the files placed as hardlinks, whose link a copy of the project broke.
    pub(crate) unlinked_copies: u64,
    /// of `copies`, the clones, which share their blocks with the cache's copy until either
    /// changes, so `copies - cloned` is what takes room twice.
    pub(crate) cloned: u64,
    /// how many projects hold copies.
    #[serde(skip)]
    pub(crate) copied_projects: u64,
    /// of `needed`, what only builds read: files of datasets with `place = false` that every
    /// recipe reading them was built from. none when the needs walk did not tell uses apart.
    pub(crate) inputs_only: Option<u64>,
}

impl Usage {
    /// what `scan`'s objects hold against what `needed` keeps, as gc would count them at `now`,
    /// and which files placed in `projects` are copies of them, looked at on `threads`. a windows
    /// listing counts no names, so there each object is opened once.
    pub(crate) fn measure(
        scan: &Scan,
        needed: &Needed,
        projects: &[PathBuf],
        now: SystemTime,
        threads: Threads,
        stop: &Stop,
    ) -> Result<Usage, Error> {
        let mut usage = Usage {
            inputs_only: needed.told.then_some(0),
            ..Usage::default()
        };
        for object in &scan.objects {
            usage.objects += 1;
            usage.bytes += object.size;
            let linked = object.has_other_names();
            if needed.objects.contains(&object.digest) {
                usage.needed += object.size;
                if linked {
                    usage.linked += object.size;
                }
                if let (Some(inputs), Some(Use::Input(_))) =
                    (&mut usage.inputs_only, needed.uses.get(&object.digest))
                {
                    *inputs += object.size;
                }
                continue;
            }
            usage.unneeded += object.size;
            usage.unneeded_objects += 1;
            if linked {
                continue;
            }
            // as gc ages it: a time ahead of the clock, as FAT's rounding gives, is young
            if now
                .duration_since(object.changed)
                .is_ok_and(|age| age >= GRACE)
            {
                usage.freeable += object.size;
            } else {
                usage.young += object.size;
            }
        }
        let held: HashSet<Sha256> = scan.objects.iter().map(|object| object.digest).collect();
        for root in projects {
            let before = usage.copies;
            usage.count_copies(root, &held, threads, stop)?;
            if usage.copies > before {
                usage.copied_projects += 1;
            }
        }
        Ok(usage)
    }

    /// adds the files placed in the project at `root` that copy objects in `held`, one look each,
    /// on a few threads when there are many, since each look waits on the disk.
    fn count_copies(
        &mut self,
        root: &Path,
        held: &HashSet<Sha256>,
        threads: Threads,
        stop: &Stop,
    ) -> Result<(), Error> {
        let (state, _) = Project {
            root: root.to_path_buf(),
        }
        .state();
        // a file whose object the cache no longer holds is the only copy of its bytes
        let files: Vec<(&String, &Placed)> = state
            .files
            .iter()
            .filter(|(_, placed)| held.contains(&placed.sha256))
            .collect();
        let found = crate::place::run_all(&files, threads, |(path, placed)| {
            (!stop.is_asked())
                .then(|| copied(&root.join(path), placed))
                .flatten()
        });
        stop.check()?;
        for ((_, placed), method) in files.into_iter().zip(found) {
            let size = placed.look.size;
            match method {
                Some(Method::Copy) => self.copies += size,
                Some(Method::Reflink) => {
                    self.copies += size;
                    self.cloned += size;
                }
                Some(Method::Hardlink) => {
                    self.copies += size;
                    self.unlinked_copies += size;
                }
                None => {}
            }
        }
        Ok(())
    }
}

/// how the file at `path` is a copy of its cached object, if it is: a copy, a clone or a hardlink
/// that lost its other name, still looking as `placed` records, so an edit is never counted.
fn copied(path: &Path, placed: &Placed) -> Option<Method> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || Look::of(&metadata) != placed.look {
        return None;
    }
    match placed.method {
        Method::Hardlink if file_has_other_names(path, &metadata) => None,
        method => Some(method),
    }
}

/// whether the file at `path`, which `metadata` describes, has another name. unix metadata
/// counts names; windows opens the file to count them.
fn file_has_other_names(path: &Path, metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        let _ = path;
        std::os::unix::fs::MetadataExt::nlink(metadata) > 1
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        crate::place::has_other_names(path)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::Cache;
    use super::super::tests::store;
    use super::super::upkeep::Object;
    use super::*;
    use crate::digest;
    use crate::place::Method;
    use crate::project::state::{Look, Placed, State};

    /// an object of `size` bytes, last changed at `changed`, with `names` names.
    fn object(name: &str, size: u64, changed: SystemTime, names: u64) -> Object {
        Object {
            digest: digest::sha256(name.as_bytes()),
            size,
            path: PathBuf::from(name),
            changed,
            names: Some(names),
        }
    }

    #[test]
    fn an_object_with_other_names_frees_nothing() {
        let now = SystemTime::now();
        let old = now - Duration::from_hours(48);
        let young = now - Duration::from_hours(1);
        let scan = Scan {
            objects: vec![
                object("unneeded", 1, old, 1),
                object("unneeded and linked by get", 10, old, 2),
                object("unneeded and young", 100, young, 1),
                object("needed and linked", 1_000, old, 2),
                object("needed", 10_000, young, 1),
            ],
            unreadable: Vec::new(),
        };
        let mut needed = Needed::default();
        for name in ["needed and linked", "needed"] {
            needed.objects.insert(digest::sha256(name.as_bytes()));
        }
        assert_eq!(
            Usage::measure(
                &scan,
                &needed,
                &[],
                now,
                Threads::default(),
                &Stop::default()
            )
            .unwrap(),
            Usage {
                objects: 5,
                bytes: 11_111,
                needed: 11_000,
                linked: 1_000,
                unneeded: 111,
                freeable: 1,
                young: 100,
                unneeded_objects: 3,
                inputs_only: None,
                ..Usage::default()
            }
        );
    }

    /// what every project only reads in builds is counted apart once the needs walk told uses;
    /// an object one project places is no input, though another only reads it in builds.
    #[test]
    fn inputs_only_counts_what_every_project_only_reads_in_builds() {
        let now = SystemTime::now();
        let scan = Scan {
            objects: vec![object("input", 1, now, 1), object("placed", 10, now, 1)],
            unreadable: Vec::new(),
        };
        let mut needed = Needed {
            told: true,
            ..Needed::default()
        };
        let input = Use::Input(super::super::upkeep::Supply::Source);
        needed.add(digest::sha256(b"input"), input);
        needed.add(digest::sha256(b"placed"), input);
        needed.add(digest::sha256(b"placed"), Use::Placed);
        let usage = Usage::measure(
            &scan,
            &needed,
            &[],
            now,
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(usage.needed, 11);
        assert_eq!(usage.inputs_only, Some(1));
    }

    /// a cache holding each of `held`, and a project whose state records each of `placed` by path,
    /// method, bytes and whether it still links the cache's copy, where the drive makes links.
    /// returns the folder holding both and what `measure` finds.
    fn report(
        placed: &[(&str, Method, &[u8], bool)],
        held: &[&[u8]],
    ) -> (tempfile::TempDir, Usage) {
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        for bytes in held {
            store(&cache, bytes);
        }
        let root = folder.path().join("project");
        let mut state = State::default();
        for (path, method, bytes, linked) in placed {
            let file = root.join(path);
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            let digest = digest::sha256(bytes);
            if !(*linked && fs::hard_link(cache.object_path(&digest), &file).is_ok()) {
                fs::write(&file, bytes).unwrap();
            }
            state.files.insert(
                (*path).to_owned(),
                Placed {
                    dataset: "set".to_owned(),
                    sha256: digest,
                    method: *method,
                    look: Look::of(&fs::symlink_metadata(&file).unwrap()),
                    writable: false,
                    checked: 0,
                    adopted: false,
                    racy_intact: std::sync::OnceLock::new(),
                },
            );
        }
        fs::create_dir_all(root.join(".fetchloom")).unwrap();
        fs::write(root.join(".fetchloom").join("state"), state.render()).unwrap();
        let usage = Usage::measure(
            &cache
                .scan_objects(Threads::default(), &Stop::default())
                .unwrap(),
            &Needed::default(),
            &[root],
            SystemTime::now(),
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();
        (folder, usage)
    }

    #[test]
    fn a_hardlink_record_with_one_name_is_an_unlinked_copy() {
        let (folder, usage) = report(
            &[
                (
                    "data/set/copied.csv",
                    Method::Hardlink,
                    b"rsync -a\n",
                    false,
                ),
                ("data/set/linked.csv", Method::Hardlink, b"linked\n", true),
            ],
            &[b"rsync -a\n", b"linked\n"],
        );
        // a drive without links leaves the second one a file of its own too
        let unlinked = if super::super::tests::hardlinks(folder.path()) {
            9
        } else {
            9 + 7
        };
        assert_eq!(usage.copies, unlinked);
        assert_eq!(usage.unlinked_copies, unlinked);
        assert_eq!(usage.copied_projects, 1);
    }

    #[test]
    fn an_object_gone_from_the_cache_is_no_copy() {
        let (_folder, usage) = report(
            &[
                ("data/set/held.csv", Method::Copy, b"held here\n", false),
                ("data/set/gone.csv", Method::Copy, b"gone\n", false),
            ],
            &[b"held here\n"],
        );
        assert_eq!(usage.copies, 10);
        assert_eq!(usage.unlinked_copies, 0);
    }

    /// a clone shares its blocks with the cache's copy, so json tells it apart from a copy that
    /// takes room twice.
    #[test]
    fn a_clone_counts_in_copies_and_in_cloned() {
        let (_folder, usage) = report(
            &[("data/set/cloned.csv", Method::Reflink, b"cloned\n", false)],
            &[b"cloned\n"],
        );
        let result = serde_json::to_value(&usage).unwrap();
        assert_eq!(result["copies"], 7, "{result}");
        assert_eq!(result["cloned"], 7, "{result}");
    }

    #[test]
    fn a_plain_copy_counts_only_in_copies() {
        let (_folder, usage) = report(
            &[("data/set/copied.csv", Method::Copy, b"copied\n", false)],
            &[b"copied\n"],
        );
        let result = serde_json::to_value(&usage).unwrap();
        assert_eq!(result["copies"], 7, "{result}");
        assert_eq!(result["cloned"], 0, "{result}");
    }

    #[test]
    fn a_file_changed_since_it_was_placed_is_no_copy() {
        let (folder, before) = report(
            &[("data/set/edited.csv", Method::Copy, b"pinned\n", false)],
            &[b"pinned\n"],
        );
        assert_eq!(before.copies, 7);
        let project = folder.path().join("project");
        fs::write(project.join("data/set/edited.csv"), b"edited by hand\n").unwrap();
        let cache = Cache::open(folder.path().join("cache")).unwrap();
        let after = Usage::measure(
            &cache
                .scan_objects(Threads::default(), &Stop::default())
                .unwrap(),
            &Needed::default(),
            &[project],
            SystemTime::now(),
            Threads::default(),
            &Stop::default(),
        )
        .unwrap();
        assert_eq!(after.copies, 0, "an edit is the person's file, not a copy");
    }
}
