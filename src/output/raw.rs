//! a question's hold on the terminal it reads keys from: raw, as console's read of one key sets
//! it, from before the question shows until its answer is read. console puts back the mode it
//! found after each key, so without the hold the terminal itself would echo a key typed before
//! the first read or between two, and a hidden key would show.

#[cfg(unix)]
use std::os::fd::{AsFd as _, BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::sync::{Mutex, PoisonError};

/// the mode the terminal of the question under way was in, beside a handle on that terminal,
/// for [`release`] to put back.
#[cfg(unix)]
static FOUND: Mutex<Option<(OwnedFd, rustix::termios::Termios)>> = Mutex::new(None);

/// runs `ask`, which shows a question and reads its answer, with the terminal console reads
/// keys from held raw throughout: stdin when it is a terminal, else the process's own.
#[cfg(unix)]
pub(super) fn question<R>(ask: impl FnOnce() -> R) -> R {
    use std::io::IsTerminal as _;
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return question_at(stdin.as_fd(), ask);
    }
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
    {
        Ok(terminal) => question_at(terminal.as_fd(), ask),
        Err(_) => ask(),
    }
}

/// runs `ask`: windows reads key events, which its console never echoes.
#[cfg(not(unix))]
pub(super) fn question<R>(ask: impl FnOnce() -> R) -> R {
    ask()
}

/// runs `ask` with `terminal` held raw from before it starts until it ends, on every way out.
#[cfg(unix)]
fn question_at<R>(terminal: BorrowedFd<'_>, ask: impl FnOnce() -> R) -> R {
    let _held = Held::on(terminal);
    ask()
}

/// a question's hold on its terminal, let go when dropped.
#[cfg(unix)]
struct Held(bool);

#[cfg(unix)]
impl Held {
    /// holds `terminal` raw, as console's read of one key does: no echo, no line editing, and
    /// ctrl-c a key rather than a signal, with output processed as it was. none when another
    /// question holds it already, or it is no terminal.
    fn on(terminal: BorrowedFd<'_>) -> Held {
        use rustix::termios::{OptionalActions, tcgetattr, tcsetattr};
        let mut found = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
        if found.is_some() {
            return Held(false);
        }
        let (Ok(mode), Ok(kept)) = (tcgetattr(terminal), rustix::io::dup(terminal)) else {
            return Held(false);
        };
        let mut raw = mode.clone();
        raw.make_raw();
        raw.output_modes = mode.output_modes;
        // at once rather than after output drains: a bsd pty waits until its reader takes what is
        // pending, and the output modes stay as they were, so nothing written reads differently
        if tcsetattr(terminal, OptionalActions::Now, &raw).is_err() {
            return Held(false);
        }
        *found = Some((kept, mode));
        Held(true)
    }
}

#[cfg(unix)]
impl Drop for Held {
    fn drop(&mut self) {
        if self.0 {
            release();
        }
    }
}

/// puts the terminal of the question under way back as the question found it. the hold's drop
/// calls it, and so do the ways out that skip that drop: a second interrupt and a panic.
#[cfg(unix)]
pub(crate) fn release() {
    let found = FOUND.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some((terminal, mode)) = found {
        let _ = rustix::termios::tcsetattr(&terminal, rustix::termios::OptionalActions::Now, &mode);
    }
}

/// nothing to put back: windows holds no terminal for a question.
#[cfg(not(unix))]
pub(crate) fn release() {}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::os::fd::{AsFd as _, OwnedFd};

    use rustix::termios::{LocalModes, Termios, tcgetattr};

    /// a pty pair: the end a person types into and reads from, and the terminal fetchloom asks on.
    fn pair() -> (OwnedFd, OwnedFd) {
        use rustix::fs::{Mode, OFlags};
        use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
        let person = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        grantpt(&person).unwrap();
        unlockpt(&person).unwrap();
        let name = ptsname(&person, Vec::new()).unwrap();
        let terminal = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY,
            Mode::empty(),
        )
        .unwrap();
        (person, terminal)
    }

    /// what the terminal has sent the person so far, without waiting for more.
    fn shown(person: &OwnedFd) -> Vec<u8> {
        rustix::fs::fcntl_setfl(person, rustix::fs::OFlags::NONBLOCK).unwrap();
        let mut shown = Vec::new();
        let mut bytes = [0; 256];
        while let Ok(read @ 1..) = rustix::io::read(person, &mut bytes) {
            shown.extend_from_slice(&bytes[..read]);
        }
        shown
    }

    /// a line read from `terminal` a byte at a time, up to enter.
    fn line(terminal: &OwnedFd) -> String {
        let mut line = Vec::new();
        let mut byte = [0];
        while rustix::io::read(terminal, &mut byte).unwrap() == 1
            && !matches!(byte[0], b'\r' | b'\n')
        {
            line.push(byte[0]);
        }
        String::from_utf8(line).unwrap()
    }

    /// asserts `terminal` is back in the mode it was `found` in. macos adds `PENDIN` as a terminal
    /// returns to line mode, a note to reprint pending input that its next read clears.
    #[track_caller]
    fn restored(terminal: &OwnedFd, found: &Termios) {
        let modes = |mode: &Termios| {
            (
                mode.local_modes - LocalModes::PENDIN,
                mode.input_modes,
                mode.output_modes,
                mode.control_modes,
            )
        };
        assert_eq!(modes(&tcgetattr(terminal).unwrap()), modes(found));
    }

    /// echo is off before the prompt goes out, so a key typed at once is never shown, and the mode
    /// the terminal had comes back after the answer, an error, a panic or a release on the way
    /// out, as a second interrupt makes.
    #[test]
    fn a_question_holds_its_terminal_raw_from_before_its_prompt_until_its_answer() {
        let (person, terminal) = pair();
        let found = tcgetattr(&terminal).unwrap();
        let cooked = LocalModes::ECHO | LocalModes::ICANON | LocalModes::ISIG;
        assert!(found.local_modes.contains(cooked));
        let typed = super::question_at(terminal.as_fd(), || {
            let mode = tcgetattr(&terminal).unwrap();
            assert!(
                !mode.local_modes.intersects(cooked),
                "the prompt went out with {:?}",
                mode.local_modes & cooked
            );
            assert_eq!(mode.output_modes, found.output_modes);
            rustix::io::write(&terminal, b"key ").unwrap();
            // typed as soon as the prompt shows, before anything reads it
            rustix::io::write(&person, b"hf_secret\r").unwrap();
            line(&terminal)
        });
        assert_eq!(typed, "hf_secret");
        assert_eq!(String::from_utf8_lossy(&shown(&person)), "key ");
        restored(&terminal, &found);

        let failed: std::io::Result<()> = super::question_at(terminal.as_fd(), || {
            Err(std::io::ErrorKind::BrokenPipe.into())
        });
        assert!(failed.is_err());
        restored(&terminal, &found);

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::question_at(terminal.as_fd(), || {
                panic!("a bug in the middle of a question")
            })
        }));
        assert!(panicked.is_err());
        restored(&terminal, &found);

        super::question_at(terminal.as_fd(), || {
            super::release();
            restored(&terminal, &found);
        });
        restored(&terminal, &found);
    }
}
