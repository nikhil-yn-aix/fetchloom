//! a terminal for tests that keeps what progress bars draw on it.

use std::io;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// a terminal 80 columns wide that records each thing drawn on it, as `line …`, `text …`,
/// `up 1` or `clear`, and wakes a test waiting for a line at each draw.
#[derive(Clone, Debug, Default)]
pub(crate) struct Screen(Arc<(Mutex<Vec<String>>, Condvar)>);

impl Screen {
    fn record(&self, what: String) {
        let (records, drawn) = &*self.0;
        records.lock().unwrap().push(what);
        drawn.notify_all();
    }

    /// every record so far, in the order drawn.
    pub(crate) fn records(&self) -> Vec<String> {
        self.0.0.lock().unwrap().clone()
    }

    /// the text drawn so far, each line and text in order, without the moves between them.
    pub(crate) fn text(&self) -> String {
        text(&self.0.0.lock().unwrap())
    }

    /// whether the text drawn holds `wanted`, waiting for it up to 30 s, which only a broken
    /// test reaches.
    pub(crate) fn shows(&self, wanted: &str) -> bool {
        let (records, drawn) = &*self.0;
        let (records, _) = drawn
            .wait_timeout_while(
                records.lock().unwrap(),
                Duration::from_secs(30),
                |records| !text(records).contains(wanted),
            )
            .unwrap();
        text(&records).contains(wanted)
    }
}

fn text(records: &[String]) -> String {
    records
        .iter()
        .filter_map(|record| {
            record
                .strip_prefix("line ")
                .or_else(|| record.strip_prefix("text "))
        })
        .collect()
}

impl indicatif::TermLike for Screen {
    fn width(&self) -> u16 {
        80
    }
    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.record(format!("up {n}"));
        Ok(())
    }
    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.record(format!("down {n}"));
        Ok(())
    }
    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.record(format!("right {n}"));
        Ok(())
    }
    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.record(format!("left {n}"));
        Ok(())
    }
    fn write_line(&self, line: &str) -> io::Result<()> {
        self.record(format!("line {line}"));
        Ok(())
    }
    fn write_str(&self, text: &str) -> io::Result<()> {
        self.record(format!("text {text}"));
        Ok(())
    }
    fn clear_line(&self) -> io::Result<()> {
        self.record("clear".to_owned());
        Ok(())
    }
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}
