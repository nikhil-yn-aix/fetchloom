//! stopping a run cleanly. the first ctrl-c asks every task to wind down and keep what it has.

use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock, PoisonError};

use tokio::sync::watch;

use crate::error::{Error, Kind};

pub(crate) struct Stop {
    asked: watch::Sender<bool>,
    /// what was heard and marked so far, as the bits below. a ctrl-c and the mark that the work
    /// is finishing each change it in one step, so each sees the other, whichever comes first.
    state: AtomicU8,
    /// downloads of this run that have yet to receive every byte, and those whose every byte is
    /// in while the disk keeps them. a first ctrl-c with only the second left lets the run finish.
    receiving: AtomicUsize,
    writing: AtomicUsize,
    /// told when the work marks itself as finishing after a stop was asked, which then no longer
    /// ends it.
    late: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// what a run that quits at once undoes first, as a first add takes back the project folder
    /// it started before it wrote anything there.
    undo: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// a stop was asked, by a ctrl-c or by the work itself.
const ASKED: u8 = 1;
/// the run marked its work as finishing, which it does after its last look for a stop.
const FINISHED: u8 = 2;
/// a first ctrl-c let the run finish instead of stopping it.
const LET_FINISH: u8 = 4;

/// what a first ctrl-c does to the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// the work stops: [`Stop::hear`] asked it, and the caller tells the person while every
    /// other line waits, so no line the stopping work prints comes before it.
    Stop,
    /// the work had marked itself as finishing, so it runs to its end.
    Started,
    /// every byte of this many downloads is in and the disk is keeping them, with nothing else
    /// left to download, so the run finishes rather than download them again next time.
    Downloaded(usize),
}

impl Default for Stop {
    fn default() -> Stop {
        Stop {
            asked: watch::Sender::new(false),
            state: AtomicU8::new(0),
            receiving: AtomicUsize::new(0),
            writing: AtomicUsize::new(0),
            late: OnceLock::new(),
            undo: Mutex::new(None),
        }
    }
}

impl Stop {
    /// the error work that a stop ended returns.
    pub(crate) fn stopped() -> Error {
        Error::new(Kind::Interrupted, "stopped at your request")
    }

    pub(crate) fn check(&self) -> Result<(), Error> {
        if self.is_asked() {
            Err(Stop::stopped())
        } else {
            Ok(())
        }
    }

    /// asks every task to stop. a run that marked its work as finishing finishes anyway, so its
    /// work never sees the stop, which [`Stop::on_late`] tells, unless a first ctrl-c already let
    /// it finish.
    pub(crate) fn ask(&self) {
        let mut late = false;
        // under the watch's lock, so a mark at the same moment comes wholly before or after
        self.asked.send_if_modified(|asked| {
            let before = self.state.fetch_or(ASKED, Ordering::SeqCst);
            if before & FINISHED != 0 {
                late = before & (ASKED | LET_FINISH) == 0;
                return false;
            }
            !std::mem::replace(asked, true)
        });
        if late {
            self.tell_late();
        }
    }

    /// decides what a first ctrl-c does: a run that marked its work as finishing, or whose only
    /// downloads left have every byte in, finishes; any other stops, and the stop is asked here,
    /// before the caller tells the person, so once that line shows the work sees it. the choice
    /// and the ask are made under the watch's lock, so a mark at the same moment comes wholly
    /// before or after them.
    pub(crate) fn hear(&self) -> Heard {
        let mut heard = Heard::Stop;
        self.asked.send_if_modified(|asked| {
            if self.state.load(Ordering::SeqCst) & FINISHED != 0 {
                heard = Heard::Started;
            } else if let Some(writing) = self.only_writing() {
                heard = Heard::Downloaded(writing);
            } else {
                self.state.fetch_or(ASKED, Ordering::SeqCst);
                return !std::mem::replace(asked, true);
            }
            self.state.fetch_or(LET_FINISH, Ordering::SeqCst);
            false
        });
        heard
    }

    /// how many downloads the disk is keeping when nothing else is left to download.
    fn only_writing(&self) -> Option<usize> {
        let writing = self.writing.load(Ordering::SeqCst);
        (writing > 0 && self.receiving.load(Ordering::SeqCst) == 0).then_some(writing)
    }

    /// resolves once a stop is asked, at once when it already was.
    pub(crate) async fn asked(&self) {
        let mut receiver = self.asked.subscribe();
        let _ = receiver.wait_for(|asked| *asked).await;
    }

    /// whether a stop was asked, for work that runs on its own thread.
    pub(crate) fn is_asked(&self) -> bool {
        *self.asked.borrow()
    }

    /// marks the point past which the run completes its work, whatever is asked. a command marks
    /// it once it last looked for a stop where it could still stop, so a stop asked later, as by
    /// ctrl-c while it prints its last lines, leaves it finished. one asked in between, after
    /// that look, is told late through [`Stop::on_late`] and taken back, so no work after the
    /// mark stops for it.
    pub(crate) fn finish(&self) {
        let mut late = false;
        self.asked.send_if_modified(|asked| {
            let before = self.state.fetch_or(FINISHED, Ordering::SeqCst);
            late = before & (ASKED | FINISHED | LET_FINISH) == ASKED;
            std::mem::replace(asked, false)
        });
        if late {
            self.tell_late();
        }
        #[cfg(debug_assertions)]
        held_at_finish();
    }

    /// what to tell when a stop is asked of a run that finishes anyway, as when the work marks
    /// itself as finishing between a ctrl-c and the stop it asks.
    pub(crate) fn on_late(&self, tell: impl Fn() + Send + Sync + 'static) {
        let _ = self.late.set(Box::new(tell));
    }

    fn tell_late(&self) {
        if let Some(tell) = self.late.get() {
            tell();
        }
    }

    /// fails when a stop was asked and the run had not marked its work as finishing.
    pub(crate) fn check_unfinished(&self) -> Result<(), Error> {
        if self.state.load(Ordering::SeqCst) & FINISHED != 0 {
            Ok(())
        } else {
            self.check()
        }
    }

    /// what to undo should the run quit at once, until [`Stop::keep_at_quit`].
    pub(crate) fn undo_at_quit(&self, undo: impl FnOnce() + Send + 'static) {
        *self.undo.lock().unwrap_or_else(PoisonError::into_inner) = Some(Box::new(undo));
    }

    /// keeps what [`Stop::undo_at_quit`] would undo, once the run writes what it set out to.
    pub(crate) fn keep_at_quit(&self) {
        self.undo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// undoes what the run asked to, as it quits at once.
    pub(crate) fn quitting(&self) {
        let undo = self
            .undo
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(undo) = undo {
            undo();
        }
    }

    /// counts `downloads` more that this run has yet to receive.
    pub(crate) fn expect(&self, downloads: usize) {
        self.receiving.fetch_add(downloads, Ordering::SeqCst);
    }

    /// counts a download whose every byte is in as one the disk keeps, or, `again`, one that
    /// receives again, as a download that starts over does.
    pub(crate) fn arrived(&self, again: bool) {
        let (from, to) = if again {
            (&self.writing, &self.receiving)
        } else {
            (&self.receiving, &self.writing)
        };
        to.fetch_add(1, Ordering::SeqCst);
        let _ = from.try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            count.checked_sub(1)
        });
    }

    /// counts a download as ended: kept, when it was `writing`, or ended before every byte came.
    pub(crate) fn ended(&self, writing: bool) {
        let count = if writing {
            &self.writing
        } else {
            &self.receiving
        };
        let _ = count.try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
            count.checked_sub(1)
        });
    }
}

/// holds a run right after it marks its work as finishing: with `FETCHLOOM_TEST_HOLD_FINISH`
/// naming a file, it writes `<file>.held`, then waits until that file appears, so a CLI test can
/// land a stop past the mark. release builds never read it.
#[cfg(debug_assertions)]
fn held_at_finish() {
    let Some(release) =
        std::env::var_os("FETCHLOOM_TEST_HOLD_FINISH").map(std::path::PathBuf::from)
    else {
        return;
    };
    let mut held = release.clone().into_os_string();
    held.push(".held");
    let _ = std::fs::write(held, b"");
    while !release.exists() {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;

    use super::*;

    /// a first ctrl-c stops work that has not marked itself as finishing, asking the stop before
    /// the caller tells the person, and lets work that has run to its end, without asking a stop
    /// that would end it anyway.
    #[test]
    fn a_ctrl_c_after_the_mark_lets_the_work_finish() {
        let stop = Stop::default();
        assert_eq!(stop.hear(), Heard::Stop);
        assert!(
            stop.is_asked(),
            "the work sees the stop before the person is told"
        );
        let finished = Stop::default();
        finished.finish();
        assert_eq!(finished.hear(), Heard::Started);
        assert!(!finished.is_asked());
        assert!(finished.check_unfinished().is_ok());
    }

    /// a stop asked of work that marked itself as finishing, or marked after the stop, is told
    /// late, once, whichever of the two the run does first, while one a first ctrl-c already let
    /// finish needs no more telling.
    #[test]
    fn a_stop_the_work_finishes_anyway_is_told_late() {
        let told = Arc::new(AtomicUsize::new(0));
        let counting = || {
            let stop = Stop::default();
            let counted = told.clone();
            stop.on_late(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            });
            stop
        };
        let between = counting();
        assert_eq!(between.hear(), Heard::Stop);
        between.finish();
        between.ask();
        assert_eq!(told.load(Ordering::SeqCst), 1);
        assert!(between.check_unfinished().is_ok());
        assert!(
            between.check().is_ok(),
            "the work past the mark never sees it"
        );
        let after = counting();
        after.ask();
        assert!(after.is_asked());
        after.finish();
        after.finish();
        assert_eq!(told.load(Ordering::SeqCst), 2);
        assert!(
            !after.is_asked(),
            "the mark takes back a stop the work did not look at"
        );
        let let_finish = counting();
        let_finish.finish();
        assert_eq!(let_finish.hear(), Heard::Started);
        let_finish.ask();
        assert_eq!(told.load(Ordering::SeqCst), 2);
    }

    /// a ctrl-c while every byte of the run's downloads is in, and the disk only keeps them,
    /// lets the run finish, since stopping would download them again; a download still
    /// receiving, or one yet to start, makes it stop.
    #[test]
    fn a_ctrl_c_while_the_disk_keeps_every_byte_lets_the_run_finish() {
        let stop = Stop::default();
        stop.expect(2);
        stop.arrived(false);
        assert_eq!(stop.only_writing(), None, "one download still receives");
        stop.arrived(false);
        assert_eq!(stop.hear(), Heard::Downloaded(2));
        assert!(!stop.is_asked());
        let restarted = Stop::default();
        restarted.expect(1);
        restarted.arrived(false);
        restarted.arrived(true);
        assert_eq!(
            restarted.hear(),
            Heard::Stop,
            "a download that starts over receives"
        );
        let kept = Stop::default();
        kept.expect(1);
        kept.arrived(false);
        kept.ended(true);
        assert_eq!(kept.hear(), Heard::Stop, "nothing is left to keep");
    }
}
