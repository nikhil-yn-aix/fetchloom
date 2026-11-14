//! what this machine remembers of each host's speed: how fast transfers big enough to measure
//! received, and how long each file waited for its answer, smoothed over recent runs. it lives in
//! a small file of fetchloom's state folder, apart from the cache, which a lab may share between
//! machines of different speeds. the file holds host names and numbers only, never leaves the
//! machine, and is started again from nothing when it cannot be read.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::estimate::Speed;

/// the file in the state folder.
pub(crate) const FILE: &str = "speeds.json";
/// the most hosts the file keeps; those heard from longest ago go first.
const MOST_HOSTS: usize = 256;
/// a transfer counts toward its host's rate once it received this much, since a shorter one is
/// mostly its connection getting up to speed.
pub(crate) const MEASURED: u64 = 1 << 20;
/// how much a run moves what a host's speed was: a third, so a few recent runs decide it.
const WEIGHT: f64 = 1.0 / 3.0;

/// fetchloom's folder for what it remembers on this machine: `$XDG_STATE_HOME/fetchloom`, else
/// `~/.local/state/fetchloom` on linux and other unix systems, the settings folder on macos and
/// `%LOCALAPPDATA%\fetchloom\state` on windows, none without a home.
pub(crate) fn folder(var: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let set = |name: &str| {
        var(name)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    if cfg!(windows) {
        return set("LOCALAPPDATA").map(|local| local.join("fetchloom").join("state"));
    }
    if cfg!(target_os = "macos") {
        return set("HOME").map(|home| home.join("Library/Application Support/fetchloom"));
    }
    // the XDG spec has a relative value ignored, which would move the file with the working folder
    set("XDG_STATE_HOME")
        .filter(|path| path.is_absolute())
        .or_else(|| set("HOME").map(|home| home.join(".local").join("state")))
        .map(|state| state.join("fetchloom"))
}

/// what one run heard from one host.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Heard {
    /// answers, and the seconds they took from request to headers, every hop included.
    answers: u64,
    waited: f64,
    /// bytes received by transfers big enough to measure, and the seconds they took.
    bytes: u64,
    receiving: f64,
}

/// this run's transfers by host, kept in memory and written once as the run ends.
#[derive(Debug, Default)]
pub(crate) struct Run {
    hosts: Mutex<HashMap<String, Heard>>,
}

impl Run {
    /// an answer from `host` that took `waited` from its request to its headers.
    pub(crate) fn answered(&self, host: &str, waited: Duration) {
        self.hear(host, |heard| {
            heard.answers += 1;
            heard.waited += waited.as_secs_f64();
        });
    }

    /// `bytes` from `host` that took `took` from the answer to the last byte, counted only when
    /// they are enough to measure.
    pub(crate) fn received(&self, host: &str, bytes: u64, took: Duration) {
        if bytes < MEASURED || took.is_zero() {
            return;
        }
        self.hear(host, |heard| {
            heard.bytes = heard.bytes.saturating_add(bytes);
            heard.receiving += took.as_secs_f64();
        });
    }

    fn hear(&self, host: &str, change: impl FnOnce(&mut Heard)) {
        let mut hosts = self.hosts.lock().unwrap_or_else(PoisonError::into_inner);
        change(hosts.entry(host.to_owned()).or_default());
    }

    fn taken(&self) -> HashMap<String, Heard> {
        std::mem::take(&mut *self.hosts.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// a run's hold on the memory: the file, read once when an estimate first needs it, and what the
/// run's transfers heard, written once as it ends.
#[derive(Debug, Default)]
pub(crate) struct Speeds {
    path: Option<PathBuf>,
    memory: std::sync::OnceLock<Memory>,
    pub(crate) run: std::sync::Arc<Run>,
}

impl Speeds {
    /// the memory in `folder`, or none without one, which reads and writes nothing.
    pub(crate) fn at(folder: Option<PathBuf>) -> Speeds {
        Speeds {
            path: folder.map(|folder| folder.join(FILE)),
            ..Speeds::default()
        }
    }

    /// the speed remembered for `host`, as the run found the file.
    pub(crate) fn speed(&self, host: &str) -> Option<Speed> {
        let path = self.path.as_deref()?;
        self.memory.get_or_init(|| Memory::read(path)).speed(host)
    }

    /// adds what the run heard to the file. a failure costs only what the run heard, so it is
    /// logged, not told.
    pub(crate) fn save(&self) {
        let Some(path) = &self.path else { return };
        if let Err(error) = Memory::save(path, &self.run, SystemTime::now()) {
            tracing::debug!(%error, path = %path.display(), "could not remember the hosts' speeds");
        }
    }
}

/// a host as the file remembers it: bytes a second over one stream, once a transfer was big
/// enough to tell, the seconds an answer takes, and when a run last heard from it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct Remembered {
    rate: Option<f64>,
    overhead: f64,
    /// seconds since 1970.
    heard: u64,
}

/// the hosts the file remembers.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Memory {
    hosts: BTreeMap<String, Remembered>,
}

impl Memory {
    /// the file at `path`; anything it cannot read is no memory, which the next write replaces.
    pub(crate) fn read(path: &Path) -> Memory {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// the speed remembered for `host`, once a transfer from it was big enough to tell its rate.
    pub(crate) fn speed(&self, host: &str) -> Option<Speed> {
        let remembered = self.hosts.get(host)?;
        Some(Speed {
            rate: remembered.rate?,
            overhead: remembered.overhead,
        })
    }

    /// adds what `run` heard at `now`, keeping the hosts heard from most recently.
    fn learn(&mut self, run: HashMap<String, Heard>, now: u64) {
        for (host, heard) in run {
            let old = self.hosts.get(&host).copied();
            if let Some(remembered) = smoothed(old, heard, now) {
                self.hosts.insert(host, remembered);
            }
        }
        if self.hosts.len() > MOST_HOSTS {
            let mut ages: Vec<(u64, String)> = self
                .hosts
                .iter()
                .map(|(host, remembered)| (remembered.heard, host.clone()))
                .collect();
            ages.sort_unstable();
            for (_, host) in ages.into_iter().take(self.hosts.len() - MOST_HOSTS) {
                self.hosts.remove(&host);
            }
        }
    }

    /// adds what `run` heard to the file at `path`, written whole under another name and then
    /// moved into place, so a reader never sees half of it. a run that heard nothing writes
    /// nothing; two runs ending at once keep the later one's.
    ///
    /// # Errors
    ///
    /// the folder or file could not be written.
    pub(crate) fn save(path: &Path, run: &Run, now: SystemTime) -> std::io::Result<()> {
        let heard = run.taken();
        if heard.is_empty() {
            return Ok(());
        }
        let mut memory = Memory::read(path);
        let seconds = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        memory.learn(heard, seconds);
        let text = serde_json::to_vec(&memory).map_err(std::io::Error::other)?;
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let written = path.with_file_name(format!("{FILE}.{}.tmp", std::process::id()));
        std::fs::write(&written, text)?;
        std::fs::rename(&written, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&written);
        })
    }
}

/// a host's speed after a run that `heard` this, from what it was, `old`: each new figure moves
/// the old one a third of the way, the rate as seconds a byte, so one fast run cannot hide slow
/// ones. none when the run heard no answer from it.
fn smoothed(old: Option<Remembered>, heard: Heard, now: u64) -> Option<Remembered> {
    if heard.answers == 0 && heard.bytes == 0 {
        return None;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "an average needs no exact count"
    )]
    let overhead = (heard.answers > 0).then(|| heard.waited / heard.answers as f64);
    #[expect(clippy::cast_precision_loss, reason = "a rate needs no exact count")]
    let rate =
        (heard.bytes > 0 && heard.receiving > 0.0).then(|| heard.bytes as f64 / heard.receiving);
    let blend = |old: f64, new: f64| old + (new - old) * WEIGHT;
    let remembered = match old {
        None => Remembered {
            rate,
            overhead: overhead.unwrap_or_default(),
            heard: now,
        },
        Some(old) => Remembered {
            rate: match (old.rate, rate) {
                (Some(old), Some(new)) => Some(1.0 / blend(1.0 / old, 1.0 / new)),
                (old, new) => new.or(old),
            },
            overhead: overhead.map_or(old.overhead, |new| blend(old.overhead, new)),
            heard: now,
        },
    };
    Some(remembered)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIB: u64 = 1 << 20;
    /// a MiB, as rates and sizes in bytes a second count it.
    const A_MIB: f64 = 1_048_576.0;

    fn heard(answers: u64, waited: f64, bytes: u64, receiving: f64) -> Heard {
        Heard {
            answers,
            waited,
            bytes,
            receiving,
        }
    }

    /// a first run sets the speed; a later one moves it a third of the way, the rate as seconds
    /// a byte: 1 MiB/s then 4 MiB/s gives 1 / (1 - (1 - 1/4) / 3) = 1.33 MiB/s, not 2.
    #[test]
    fn each_run_moves_the_speed_a_third_of_the_way() {
        let first = smoothed(None, heard(2, 1.0, 4 * MIB, 4.0), 10).unwrap();
        assert_eq!(
            first,
            Remembered {
                rate: Some(A_MIB),
                overhead: 0.5,
                heard: 10
            }
        );
        let second = smoothed(Some(first), heard(1, 2.0, 4 * MIB, 1.0), 20).unwrap();
        let rate = second.rate.unwrap() / A_MIB;
        assert!((rate - 4.0 / 3.0).abs() < 1e-9, "{rate}");
        assert!((second.overhead - 1.0).abs() < 1e-9, "{}", second.overhead);
        assert_eq!(second.heard, 20);
    }

    /// small files tell the wait for an answer but no rate, which keeps what it was.
    #[test]
    fn a_run_of_small_files_keeps_the_rate() {
        let run = Run::default();
        run.answered("zenodo.org", Duration::from_millis(300));
        run.received("zenodo.org", MEASURED - 1, Duration::from_millis(5));
        let fresh = smoothed(None, run.taken()["zenodo.org"], 1).unwrap();
        assert_eq!(fresh.rate, None);
        let old = Remembered {
            rate: Some(2.0 * A_MIB),
            overhead: 0.3,
            heard: 0,
        };
        let kept = smoothed(Some(old), heard(1, 0.3, 0, 0.0), 1).unwrap();
        assert_eq!(kept.rate, old.rate);
        assert_eq!(smoothed(Some(old), Heard::default(), 1), None);
    }

    /// the file keeps the hosts heard from most recently, is rebuilt when unreadable and is
    /// written only by a run that heard something.
    #[test]
    fn the_file_is_bounded_rebuilt_and_written_once() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("state").join(FILE);
        Memory::save(&path, &Run::default(), SystemTime::UNIX_EPOCH).unwrap();
        assert!(!path.exists(), "a run that heard nothing writes nothing");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{not json").unwrap();
        let run = Run::default();
        run.answered("zenodo.org", Duration::from_millis(200));
        run.received("zenodo.org", 8 * MIB, Duration::from_secs(2));
        Memory::save(&path, &run, SystemTime::UNIX_EPOCH).unwrap();
        let read = Memory::read(&path);
        let speed = read.speed("zenodo.org").unwrap();
        assert!((speed.rate - 4.0 * A_MIB).abs() < 1e-6);
        assert!(read.speed("huggingface.co").is_none());
        let mut memory = Memory::default();
        let hosts = (0..=MOST_HOSTS as u64)
            .map(|age| (format!("host{age}.org"), heard(1, 0.1, 0, 0.0)))
            .collect::<Vec<_>>();
        for (age, (host, heard)) in hosts.into_iter().enumerate() {
            memory.learn(HashMap::from([(host, heard)]), age as u64);
        }
        assert_eq!(memory.hosts.len(), MOST_HOSTS);
        assert!(!memory.hosts.contains_key("host0.org"), "the oldest goes");
        assert!(memory.hosts.contains_key("host256.org"));
    }

    #[test]
    fn the_state_folder_follows_each_system() {
        let var = |name: &str| match name {
            "HOME" => Some("/home/ada".into()),
            "XDG_STATE_HOME" => Some("relative".into()),
            "LOCALAPPDATA" => Some("C:\\Users\\ada\\AppData\\Local".into()),
            _ => None,
        };
        let expected = if cfg!(windows) {
            PathBuf::from("C:\\Users\\ada\\AppData\\Local")
                .join("fetchloom")
                .join("state")
        } else if cfg!(target_os = "macos") {
            PathBuf::from("/home/ada/Library/Application Support/fetchloom")
        } else {
            PathBuf::from("/home/ada/.local/state/fetchloom")
        };
        assert_eq!(folder(&var), Some(expected));
        assert_eq!(folder(&|_| None), None);
    }
}
