//! a watch on the calls that give a file a name, by a link or a rename. on a `ReFS` drive, such as
//! a Dev Drive, windows can deadlock inside such a call: its thread never returns, no kill ends
//! it, and a run would stop without a word. the watch ends the run instead, saying what is stuck.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, Once, OnceLock, PoisonError};
use std::time::Duration;

use crate::error::{Error, Kind};

/// how often the watch looks at the calls under way.
const TICK: Duration = Duration::from_secs(1);

/// how many looks may find one name change under way on windows before the run ends: 90 s, where
/// one takes milliseconds and a network drive gives up on a server that stopped answering after a
/// minute. looks rather than a clock, so time the machine spent asleep never counts.
const LIMIT: u32 = 90;

/// a name change under way: the file it names, and how many looks have found it so.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Call {
    file: PathBuf,
    looks: u32,
}

struct Watch {
    limit: u32,
    calls: Mutex<Calls>,
    /// every call waits forever before it runs, as one windows deadlocked in does. only a debug
    /// build's CLI tests ask for it.
    hangs: bool,
}

#[derive(Default)]
struct Calls {
    next: u64,
    under_way: BTreeMap<u64, Call>,
}

/// a call the watch sees until it is dropped.
struct Entered<'w> {
    watch: &'w Watch,
    id: u64,
}

static WATCH: OnceLock<Option<Watch>> = OnceLock::new();
static WATCHING: Once = Once::new();
static END: OnceLock<Box<dyn Fn(Error) + Send + Sync>> = OnceLock::new();

/// runs `call`, which gives `file` a name by a link or a rename, where the watch sees it.
pub(crate) fn naming<T>(file: &Path, call: impl FnOnce() -> T) -> T {
    let Some(watch) = watch() else {
        return call();
    };
    let _entered = watch.enter(file);
    if watch.hangs {
        loop {
            std::thread::park();
        }
    }
    call()
}

/// what ends the run once a name change has stalled: `end` is given the error to tell, and does
/// not return. until it is set, a stall is waited out as before.
pub(crate) fn when_stalled(end: impl Fn(Error) + Send + Sync + 'static) {
    let _ = END.set(Box::new(end));
}

/// the watch, started with the first call that needs it. none where nothing deadlocks this way.
fn watch() -> Option<&'static Watch> {
    let watch = WATCH.get_or_init(Watch::chosen).as_ref()?;
    WATCHING.call_once(|| {
        // without its thread the watch only records calls, and a stall is waited out as before
        let _ = std::thread::Builder::new()
            .name("stall watch".to_owned())
            .spawn(move || watch.keep());
    });
    Some(watch)
}

impl Watch {
    /// the watch windows needs. in a debug build `FETCHLOOM_TEST_STALL` sets its limit in looks,
    /// on any system, and makes every call hang, for CLI tests.
    fn chosen() -> Option<Watch> {
        #[cfg(debug_assertions)]
        if let Some(looks) = std::env::var("FETCHLOOM_TEST_STALL")
            .ok()
            .and_then(|text| text.parse().ok())
        {
            return Some(Watch::new(looks, true));
        }
        cfg!(windows).then(|| Watch::new(LIMIT, false))
    }

    fn new(limit: u32, hangs: bool) -> Watch {
        Watch {
            limit,
            calls: Mutex::default(),
            hangs,
        }
    }

    fn calls(&self) -> MutexGuard<'_, Calls> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn enter(&self, file: &Path) -> Entered<'_> {
        let mut calls = self.calls();
        let id = calls.next;
        calls.next += 1;
        calls.under_way.insert(
            id,
            Call {
                file: file.to_path_buf(),
                looks: 0,
            },
        );
        Entered { watch: self, id }
    }

    /// counts a look at every call under way, and the calls that look found stalled.
    fn look(&self) -> Option<Call> {
        let mut calls = self.calls();
        for call in calls.under_way.values_mut() {
            call.looks += 1;
        }
        stalled(calls.under_way.values(), self.limit).cloned()
    }

    /// looks at the calls under way each tick, and ends the run at the first that stalled.
    fn keep(&self) {
        loop {
            std::thread::sleep(TICK);
            if let (Some(call), Some(end)) = (self.look(), END.get()) {
                end(stalled_error(&call, TICK * self.limit));
            }
        }
    }
}

impl Drop for Entered<'_> {
    fn drop(&mut self) {
        self.watch.calls().under_way.remove(&self.id);
    }
}

/// of `calls`, the one the most looks found under way, once `limit` looks or more have.
fn stalled<'c>(calls: impl IntoIterator<Item = &'c Call>, limit: u32) -> Option<&'c Call> {
    calls
        .into_iter()
        .max_by_key(|call| call.looks)
        .filter(|call| call.looks >= limit)
}

/// what a run ends with once `call` has not returned in `waited`.
fn stalled_error(call: &Call, waited: Duration) -> Error {
    Error::new(
        Kind::LocalIo,
        format!(
            "windows has not finished giving {} its name after {}, so fetchloom stopped",
            call.file.display(),
            crate::units::seconds(waited)
        ),
    )
    .help(
        "on a ReFS drive, such as a Dev Drive, windows can lock up while it names a file, and only a restart ends that. restart windows, then run the command again: the files fetchloom finished are kept, and this window may not close before the restart",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(file: &str, looks: u32) -> Call {
        Call {
            file: PathBuf::from(file),
            looks,
        }
    }

    /// a call stalls once as many looks as the limit found it under way, and the one found under
    /// way longest is named.
    #[test]
    fn the_call_under_way_longest_stalls_at_the_limit() {
        let limit = 90;
        let later = call("data/later.csv", 60);
        for (looks, stuck) in [(0, false), (1, false), (89, false), (90, true), (600, true)] {
            let first = call("cache/objects/9d/first", looks);
            let calls = [later.clone(), first.clone()];
            let expected = if stuck { Some(&first) } else { None };
            assert_eq!(stalled(&calls, limit), expected, "{looks} looks");
        }
        assert_eq!(stalled([], limit), None);
    }

    /// each look counts once for every call still under way, and a call that returned is gone.
    #[test]
    fn a_call_is_watched_only_while_it_runs() {
        let watch = Watch::new(2, false);
        let first = watch.enter(Path::new("data/a.csv"));
        assert_eq!(watch.look(), None);
        let second = watch.enter(Path::new("data/b.csv"));
        assert_eq!(watch.look(), Some(call("data/a.csv", 2)));
        drop(first);
        assert_eq!(watch.look(), Some(call("data/b.csv", 2)));
        drop(second);
        assert_eq!(watch.look(), None);
        assert!(watch.calls().under_way.is_empty());
    }

    /// the error names the file and the wait, then says a restart is the way out.
    #[test]
    fn a_stall_says_what_is_stuck_and_that_a_restart_frees_it() {
        let error = stalled_error(&call(r"D:\cache\objects\9d\9d73", LIMIT), TICK * LIMIT);
        assert_eq!(error.kind, Kind::LocalIo);
        assert_eq!(
            error.message,
            r"windows has not finished giving D:\cache\objects\9d\9d73 its name after 1 min 30 s, so fetchloom stopped"
        );
        assert!(
            error
                .help
                .as_deref()
                .is_some_and(|help| help.contains("restart windows, then run the command again")),
            "{:?}",
            error.help
        );
    }
}
