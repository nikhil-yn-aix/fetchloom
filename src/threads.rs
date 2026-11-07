//! the optional ceiling on local worker groups.

use std::any::Any;
use std::cell::Cell;
use std::ffi::OsString;
use std::io;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, ScopedJoinHandle};

use crate::error::{Error, Kind};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Threads(Option<NonZeroUsize>);

impl Threads {
    pub(crate) fn read(var: &dyn Fn(&str) -> Option<OsString>) -> Result<Threads, Error> {
        let Some(value) = var("FETCHLOOM_THREADS").filter(|value| !value.is_empty()) else {
            return Ok(Self::default());
        };
        value
            .to_str()
            .and_then(|text| text.trim().parse::<NonZeroUsize>().ok())
            .filter(|threads| threads.get() <= 256)
            .map(|threads| Self(Some(threads)))
            .ok_or_else(|| {
                Error::new(
                    Kind::Usage,
                    format!(
                        "FETCHLOOM_THREADS is \"{}\", which is not a number from 1 to 256",
                        value.display()
                    ),
                )
                .help("unset FETCHLOOM_THREADS to use the default worker counts, or set it to a number from 1 to 256")
            })
    }

    pub(crate) fn is_limited(self) -> bool {
        self.0.is_some()
    }

    pub(crate) fn limit(self, requested: usize) -> usize {
        self.0.map_or(requested, |cap| requested.min(cap.get()))
    }

    /// shares the archive budget between readers and their writers. zero writers means the
    /// reader stores members itself, so a cap of one never needs a second thread to drain a queue.
    pub(crate) fn archives(self, requested: usize) -> (usize, Option<usize>) {
        let Some(cap) = self.0 else {
            return (requested, None);
        };
        let readers = requested.max(1).min(cap.get());
        (readers, Some(cap.get() / readers - 1))
    }
}

/// work on a scoped thread, or retained for the caller when that thread could not start.
#[must_use = "join the worker to run work retained after a spawn failure"]
pub(crate) enum Scoped<'scope, F, T> {
    Thread(ScopedJoinHandle<'scope, T>),
    Caller(Arc<Mutex<F>>),
}

impl<F: FnMut() -> T, T> Scoped<'_, F, T> {
    pub(crate) fn is_threaded(&self) -> bool {
        matches!(self, Self::Thread(_))
    }

    /// finishes this work. unstarted work runs after its producer can close the queue.
    pub(crate) fn join(self) -> T {
        match self {
            Self::Thread(worker) => worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            Self::Caller(work) => run_job(&work),
        }
    }
}

/// starts work without losing its owned input when the OS refuses another thread.
pub(crate) fn scoped<'scope, F, T>(
    scope: &'scope thread::Scope<'scope, '_>,
    work: F,
) -> Scoped<'scope, F, T>
where
    F: FnMut() -> T + Send + 'scope,
    T: Send + 'scope,
{
    scoped_with(work, |job| {
        #[cfg(test)]
        if REFUSE_AFTER.get().is_some_and(|left| {
            REFUSE_AFTER.set(Some(left.saturating_sub(1)));
            left == 0
        }) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        thread::Builder::new().spawn_scoped(scope, move || run_job(&job))
    })
}

fn scoped_with<'scope, F, T>(
    work: F,
    start: impl FnOnce(Arc<Mutex<F>>) -> io::Result<ScopedJoinHandle<'scope, T>>,
) -> Scoped<'scope, F, T> {
    let job = Arc::new(Mutex::new(work));
    match start(Arc::clone(&job)) {
        Ok(worker) => Scoped::Thread(worker),
        Err(error) => {
            tracing::debug!(%error, "keeping an unstarted worker's work on the calling thread");
            Scoped::Caller(job)
        }
    }
}

fn run_job<F: FnMut() -> T, T>(job: &Mutex<F>) -> T {
    (*job.lock().unwrap_or_else(PoisonError::into_inner))()
}

#[cfg(test)]
thread_local! {
    static REFUSE_AFTER: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) struct Refusing(Option<usize>);

#[cfg(test)]
impl Drop for Refusing {
    fn drop(&mut self) {
        REFUSE_AFTER.set(self.0);
    }
}

#[cfg(test)]
pub(crate) fn refuse_after(successful: usize) -> Refusing {
    Refusing(REFUSE_AFTER.replace(Some(successful)))
}

thread_local! {
    static STARTING_RUNTIME: Cell<bool> = const { Cell::new(false) };
}

struct StartingRuntime(bool);

impl Drop for StartingRuntime {
    fn drop(&mut self) {
        STARTING_RUNTIME.set(self.0);
    }
}

/// tokio panics on worker-start failures rather than returning its builder's I/O result.
pub(crate) fn start_runtime<T>(start: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
    let _starting = StartingRuntime(STARTING_RUNTIME.replace(true));
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(start)) {
        Ok(result) => result,
        Err(panic) => match runtime_failure(panic.as_ref()) {
            Some(reason) => Err(io::Error::other(format!(
                "cannot spawn a runtime worker: {reason}"
            ))),
            None => std::panic::resume_unwind(panic),
        },
    }
}

/// whether the entry-point panic hook should leave this failure to `start_runtime`.
pub(crate) fn expected_runtime_failure(panic: &(dyn Any + Send)) -> bool {
    STARTING_RUNTIME.get() && runtime_failure(panic).is_some()
}

fn runtime_failure(panic: &(dyn Any + Send)) -> Option<&str> {
    panic
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| panic.downcast_ref::<&str>().copied())?
        .strip_prefix("OS can't spawn worker thread: ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_values_keep_defaults_and_accept_the_bounds() {
        for value in [None, Some("")] {
            let threads = Threads::read(&|_| value.map(Into::into)).unwrap();
            assert!(!threads.is_limited());
            assert_eq!(threads.limit(512), 512);
            assert_eq!(threads.archives(4), (4, None));
        }
        for (text, cap) in [("1", 1), (" 8 ", 8), ("256", 256)] {
            let threads = Threads::read(&|_| Some(text.into())).unwrap();
            assert!(threads.is_limited());
            assert_eq!(threads.limit(512), cap);
            assert_eq!(threads.limit(1), 1);
        }
    }

    #[test]
    fn invalid_thread_values_are_settings_errors() {
        for text in [
            " ",
            "0",
            "257",
            "-1",
            "1.5",
            "1K",
            "many",
            "999999999999999999999",
        ] {
            let error = Threads::read(&|_| Some(text.into())).unwrap_err();
            assert_eq!(error.kind, Kind::Usage);
            assert_eq!(error.kind.exit_code(), 2);
            assert!(error.message.contains("FETCHLOOM_THREADS"));
            assert!(error.help.is_some());
        }
    }

    #[test]
    fn archives_share_the_cap_between_readers_and_writers() {
        for cap in 1..=256 {
            let threads = Threads::read(&|_| Some(cap.to_string().into())).unwrap();
            for requested in 1..=4 {
                let (readers, writers) = threads.archives(requested);
                assert!(readers >= 1 && readers <= requested);
                assert!(readers * (writers.unwrap() + 1) <= cap);
            }
        }
        let one = Threads::read(&|_| Some("1".into())).unwrap();
        assert_eq!(one.archives(4), (1, Some(0)));
    }

    #[test]
    fn a_refused_worker_keeps_owned_input_until_joined() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let mut input = vec![3, 1, 4];
        let caller = thread::current().id();
        let worker = scoped_with(
            move || {
                assert_eq!(thread::current().id(), caller);
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                std::mem::take(&mut input)
            },
            |_| Err(io::ErrorKind::WouldBlock.into()),
        );
        assert!(!worker.is_threaded());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(worker.join(), [3, 1, 4]);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn refused_workers_preserve_started_results_and_order() {
        let _refusing = refuse_after(1);
        let caller = thread::current().id();
        let results = thread::scope(|scope| {
            let workers: Vec<_> = (0..4)
                .map(|value| scoped(scope, move || (value, thread::current().id())))
                .collect();
            assert!(workers[0].is_threaded());
            assert!(workers[1..].iter().all(|worker| !worker.is_threaded()));
            workers.into_iter().map(Scoped::join).collect::<Vec<_>>()
        });
        assert_ne!(results[0].1, caller);
        for (index, (value, thread)) in results.into_iter().enumerate() {
            assert_eq!(value, index);
            if index != 0 {
                assert_eq!(thread, caller);
            }
        }
    }

    #[test]
    #[should_panic(expected = "job failed")]
    fn a_refused_worker_does_not_hide_job_panics() {
        let worker = scoped_with(
            || panic!("job failed"),
            |_| Err(io::ErrorKind::WouldBlock.into()),
        );
        worker.join();
    }

    #[test]
    #[should_panic(expected = "job failed")]
    fn a_started_worker_does_not_hide_job_panics() {
        thread::scope(|scope| scoped(scope, || panic!("job failed")).join());
    }

    #[test]
    fn runtime_thread_refusal_becomes_an_io_error() {
        let message =
            "OS can't spawn worker thread: Resource temporarily unavailable (os error 11)";
        assert!(!expected_runtime_failure(&message));
        let result: io::Result<()> = start_runtime(|| {
            assert!(expected_runtime_failure(&message));
            std::panic::panic_any(message);
        });
        let error = result.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("cannot spawn a runtime worker"));
        assert!(!expected_runtime_failure(&message));
    }

    #[test]
    fn runtime_errors_and_unrelated_panics_are_not_reclassified() {
        let result: io::Result<()> = start_runtime(|| Err(io::ErrorKind::PermissionDenied.into()));
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        let result = std::panic::catch_unwind(|| {
            let _: io::Result<()> = start_runtime(|| std::panic::panic_any("another bug"));
        });
        assert_eq!(
            result.unwrap_err().downcast_ref::<&str>(),
            Some(&"another bug")
        );
        assert!(!expected_runtime_failure(
            &"OS can't spawn worker thread: refused"
        ));
    }
}
