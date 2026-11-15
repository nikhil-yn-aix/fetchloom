//! the `cache_budget` setting: a run whose downloads would take the cache's files past it asks
//! first, as for a disk without room. nothing is deleted to keep within it.

use crate::cache::{Cache, Scan};
use crate::error::{Error, Kind};
use crate::output::{Output, Warning};
use crate::project::lock::Lock;
use crate::stop::Stop;
use crate::threads::Threads;
use crate::units;

/// how many of the project's largest datasets a message about the budget names.
pub(super) const LARGEST: usize = 3;

/// downloads that would take the cache past its budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::cli) struct Past {
    /// what the downloads bring.
    pub(super) adding: u64,
    /// what the cache's files would hold once they are in.
    pub(super) total: u64,
    pub(super) budget: u64,
}

/// whether downloading `adding` bytes takes the cache past `budget`, reading what its files hold
/// with `held` only when a budget is set and something downloads. a cache `held` cannot read is
/// taken to fit, as a disk that does not tell its free space is.
pub(in crate::cli) fn weigh(
    budget: Option<u64>,
    adding: u64,
    held: impl FnOnce() -> Option<u64>,
) -> Option<Past> {
    let budget = budget.filter(|_| adding > 0)?;
    let total = held()?.saturating_add(adding);
    (total > budget).then_some(Past {
        adding,
        total,
        budget,
    })
}

/// what the cache's files take on its disk, as `fetchloom cache` holds them against the budget,
/// and the scan that read them; none when the cache cannot be read.
pub(in crate::cli) fn held(cache: &Cache, threads: Threads, stop: &Stop) -> Option<(u64, Scan)> {
    let found = cache.scan_objects(threads, stop).ok()?;
    let held = found
        .objects
        .iter()
        .map(|object| object.disk.unwrap_or(object.size))
        .fold(0, u64::saturating_add);
    Some((held, found))
}

/// the `most` largest datasets `lock` pins, by their pinned files' bytes, largest first.
pub(super) fn largest(lock: &Lock, most: usize) -> Vec<(String, u64)> {
    let fetched = lock.datasets.iter().map(|locked| {
        let sizes = locked.files.iter().map(|file| file.size);
        (&locked.name, sizes.fold(0, u64::saturating_add))
    });
    let made = lock.derived.iter().map(|derived| {
        let sizes = derived.files.iter().map(|file| file.size);
        (&derived.name, sizes.fold(0, u64::saturating_add))
    });
    let mut sized: Vec<(String, u64)> = fetched
        .chain(made)
        .filter(|(_, size)| *size > 0)
        .map(|(name, size)| (name.clone(), size))
        .collect();
    sized.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sized.truncate(most);
    sized
}

/// what a person reads when `doing` would take the cache past its budget, and what frees room.
pub(in crate::cli) struct Over<'a> {
    pub(in crate::cli) doing: &'a str,
    pub(in crate::cli) past: Past,
    /// what `cache gc` would free now, and what only `cache gc --now` would, as a file a day old
    /// at most; none when a project gc reads is out of reach.
    pub(in crate::cli) freeable: Option<(u64, u64)>,
    /// the project's largest datasets, with their bytes, largest first.
    pub(in crate::cli) largest: Vec<(String, u64)>,
}

impl Over<'_> {
    /// what the downloads do to the cache, as `would bring` or, once they go on, `brings`.
    fn said(&self, bring: &str) -> String {
        format!(
            "{} {bring} the cache to {}, past the {} cache_budget",
            self.doing,
            units::size(self.past.total),
            units::size(self.past.budget)
        )
    }

    /// what would free room in the cache now, each on a line of its own.
    fn room(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match self.freeable {
            Some((0, 0)) => lines.push("`fetchloom cache gc` would free nothing now".to_owned()),
            Some((aged, 0)) => lines.push(format!(
                "`fetchloom cache gc` would free {} now",
                units::size(aged)
            )),
            Some((aged, young)) => lines.push(format!(
                "`fetchloom cache gc --now` would free {} now",
                units::size(aged.saturating_add(young))
            )),
            None => {}
        }
        let named: Vec<String> = self
            .largest
            .iter()
            .map(|(name, bytes)| format!("{name} ({})", units::size(*bytes)))
            .collect();
        match named.as_slice() {
            [] => {}
            [one] => lines.push(format!("this project's largest dataset is {one}")),
            [first @ .., last] => lines.push(format!(
                "this project's largest datasets are {} and {last}",
                first.join(", ")
            )),
        }
        lines
    }

    /// the commands that free a dataset's files: dropping it here, which keeps it pinned, or
    /// removing it, then gc, which deletes only what no project needs, at once with `--now`, as
    /// gc alone keeps a file a day.
    fn freeing() -> String {
        format!(
            "`{}` frees a dataset's files here and keeps it pinned, or `{}` and then `fetchloom cache gc --now` free them and take it out of the project",
            crate::suggested::command("drop <name>"),
            crate::suggested::command("remove <name>")
        )
    }
}

/// asks whether `over` may go on, as for a disk without room: a person at a terminal answers, a
/// run no one can answer stops with what frees room, and `--yes` goes on with a warning.
pub(in crate::cli) fn ask(output: &Output, yes: bool, over: &Over<'_>) -> Result<(), Error> {
    let room = over.room();
    if yes {
        let mut help = room.clone();
        help.push(Over::freeing());
        output.warn_detailed(
            Warning::CacheOverBudget,
            &over.said("brings"),
            Some(&help.join("; ")),
            &room,
        );
        return Ok(());
    }
    let said = over.said("would bring");
    let cancel = format!("{} ({})", over.doing, units::size(over.past.adding));
    let question = match room.as_slice() {
        [] => format!("{said}. go on anyway?"),
        room => format!("{said}. {}. go on anyway?", room.join("; ")),
    };
    match output
        .confirm(&question)
        .map_err(|error| error.cancelling(&cancel))?
    {
        Some(true) => Ok(()),
        Some(false) => Err(Error::new(
            Kind::Unconfirmed,
            format!("did not start {}", over.doing),
        )
        .cancelled(cancel)),
        None => Err(room
            .into_iter()
            .fold(Error::new(Kind::Unconfirmed, said), Error::detail)
            .help(format!(
                "{}, or raise cache_budget in the settings file or FETCHLOOM_CACHE_BUDGET, then run the same command again. add --yes to go on anyway",
                Over::freeing()
            ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::lock::{Locked, LockedFile};

    const GIB: u64 = 1 << 30;

    /// the cache is read only when a budget is set and something downloads, a run that lands
    /// exactly on the budget is within it, and a cache that cannot be read stops nothing.
    #[test]
    fn only_downloads_past_a_set_budget_count() {
        let unread = || -> Option<u64> { panic!("the cache was read") };
        assert_eq!(weigh(None, GIB, unread), None);
        assert_eq!(weigh(Some(GIB), 0, unread), None);
        for (held, adding, past) in [
            (150 * GIB, 50 * GIB, None),
            (
                150 * GIB,
                50 * GIB + 1,
                Some(Past {
                    adding: 50 * GIB + 1,
                    total: 200 * GIB + 1,
                    budget: 200 * GIB,
                }),
            ),
            (
                250 * GIB,
                1,
                Some(Past {
                    adding: 1,
                    total: 250 * GIB + 1,
                    budget: 200 * GIB,
                }),
            ),
            (
                u64::MAX,
                GIB,
                Some(Past {
                    adding: GIB,
                    total: u64::MAX,
                    budget: 200 * GIB,
                }),
            ),
        ] {
            let weighed = weigh(Some(200 * GIB), adding, || Some(held));
            assert_eq!(weighed, past, "{held} held, {adding} adding");
        }
        assert_eq!(weigh(Some(GIB), 2 * GIB, || None), None);
    }

    fn dataset(name: &str, sizes: &[u64]) -> Locked {
        let files = sizes
            .iter()
            .enumerate()
            .map(|(index, size)| LockedFile {
                path: format!("{index}.bin"),
                size: *size,
                sha256: crate::digest::sha256(name.as_bytes()),
                md5: None,
                url: format!("https://example.org/{name}/{index}.bin"),
                unpacks_to: None,
            })
            .collect();
        Locked {
            name: name.to_owned(),
            reference: format!("https://example.org/{name}"),
            spec: String::new(),
            extract: true,
            web_page: false,
            resolved: format!("https://example.org/{name}"),
            title: None,
            license: None,
            doi: None,
            access: crate::source::Access::Public,
            timestamp: "2026-11-15T00:00:00Z".to_owned(),
            files,
        }
    }

    /// a dataset counts all its files, an empty one is no candidate, and a tie keeps name order.
    #[test]
    fn the_largest_datasets_come_first_by_all_their_files() {
        let lock = Lock {
            datasets: vec![
                dataset("coco", &[20 * GIB]),
                dataset("empty", &[0]),
                dataset("imagenet", &[100 * GIB, 80 * GIB]),
                dataset("iris", &[4608]),
                dataset("mnist", &[4608]),
            ],
            derived: Vec::new(),
            stated: Vec::new(),
        };
        assert_eq!(
            largest(&lock, 3),
            [
                ("imagenet".to_owned(), 180 * GIB),
                ("coco".to_owned(), 20 * GIB),
                ("iris".to_owned(), 4608),
            ]
        );
        assert_eq!(largest(&Lock::default(), 3), []);
    }

    /// the message names gc's share and the largest datasets; the help names what frees room.
    #[test]
    fn a_run_past_the_budget_is_told_what_frees_room() {
        let over = |freeable, largest: &[(&str, u64)]| Over {
            doing: "this sync",
            past: Past {
                adding: 12 * GIB,
                total: 212 * GIB,
                budget: 200 * GIB,
            },
            freeable,
            largest: largest
                .iter()
                .map(|(name, size)| ((*name).to_owned(), *size))
                .collect(),
        };
        let told = over(
            Some((3 * GIB, 0)),
            &[("imagenet", 180 * GIB), ("coco", 20 * GIB)],
        );
        assert_eq!(
            told.said("would bring"),
            "this sync would bring the cache to 212.0 GiB, past the 200.0 GiB cache_budget"
        );
        assert_eq!(
            told.room(),
            [
                "`fetchloom cache gc` would free 3.0 GiB now",
                "this project's largest datasets are imagenet (180.0 GiB) and coco (20.0 GiB)"
            ]
        );
        assert_eq!(
            over(Some((0, 0)), &[("iris", 4608)]).room(),
            [
                "`fetchloom cache gc` would free nothing now",
                "this project's largest dataset is iris (4.5 KiB)"
            ]
        );
        // gc keeps a file downloaded today for a day, so only --now frees it now
        assert_eq!(
            over(Some((GIB, 2 * GIB)), &[]).room(),
            ["`fetchloom cache gc --now` would free 3.0 GiB now"]
        );
        assert_eq!(over(None, &[]).room(), Vec::<String>::new());
    }

    /// `--yes` goes on and warns with what frees room; no one to ask stops with the same facts.
    #[test]
    fn yes_goes_on_with_a_warning_and_no_answer_stops() {
        let over = Over {
            doing: "this sync",
            past: Past {
                adding: 12 * GIB,
                total: 212 * GIB,
                budget: 200 * GIB,
            },
            freeable: Some((3 * GIB, 0)),
            largest: vec![("imagenet".to_owned(), 180 * GIB)],
        };
        let style = crate::output::Style {
            color: false,
            unicode: false,
            interactive: false,
            watched: false,
            links: false,
            accessible: false,
        };
        let output = Output::new(style, crate::output::Level::Quiet, true, false);
        ask(&output, true, &over).unwrap();
        assert_eq!(
            output.warned(),
            [
                "this sync brings the cache to 212.0 GiB, past the 200.0 GiB cache_budget; `fetchloom cache gc` would free 3.0 GiB now; this project's largest dataset is imagenet (180.0 GiB); `fetchloom drop <name>` frees a dataset's files here and keeps it pinned, or `fetchloom remove <name>` and then `fetchloom cache gc --now` free them and take it out of the project"
            ]
        );
        let error = ask(&output, false, &over).unwrap_err();
        assert_eq!(error.kind, Kind::Unconfirmed);
        assert_eq!(
            error.message,
            "this sync would bring the cache to 212.0 GiB, past the 200.0 GiB cache_budget"
        );
        assert_eq!(
            error.details,
            [
                "`fetchloom cache gc` would free 3.0 GiB now",
                "this project's largest dataset is imagenet (180.0 GiB)"
            ]
        );
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.ends_with("add --yes to go on anyway")),
            "{error:?}"
        );
    }
}
