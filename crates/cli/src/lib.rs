//! The command surface, configuration precedence, and the observers behind it.

pub mod cache;
pub mod config;

use clap_complete as _;
use ctrlc as _;
#[cfg(test)]
use fetchloom_faults as _;
#[cfg(all(test, unix))]
use rustix as _;
#[cfg(test)]
use tempfile as _;
#[cfg(all(test, windows))]
use windows_sys as _;
pub mod doctor;
pub mod explain;
pub mod hint;
pub mod inference;
pub mod locked;
pub mod logging;
pub mod materialize;
pub mod observer;
pub mod planning;
pub mod policy;
pub mod repair;
pub mod resolve;
pub mod run;
pub mod settings;
pub mod style;
pub mod surface;
pub mod terminal;
pub mod why;

/// Where a failure is written, and the event stream it also enters.
pub struct Reporter<'a> {
    json: bool,
    observer: &'a dyn fetchloom_engine::seam::observer::Observer,
    sequence: &'a fetchloom_engine::event::Sequence,
}

impl<'a> Reporter<'a> {
    /// Builds a reporter over the stream a run is already writing.
    #[must_use]
    pub fn new(
        json: bool,
        observer: &'a dyn fetchloom_engine::seam::observer::Observer,
        sequence: &'a fetchloom_engine::event::Sequence,
    ) -> Self {
        Self {
            json,
            observer,
            sequence,
        }
    }

    /// Reports whether the result is machine readable.
    #[must_use]
    pub fn json(&self) -> bool {
        self.json
    }

    /// Writes a failure where the caller asked for it, puts it on the event
    /// stream, and returns its exit code.
    #[must_use]
    pub fn report(
        &self,
        error: &fetchloom_engine::error::Error,
    ) -> fetchloom_engine::outcome::ExitCode {
        use fetchloom_engine::error::Layer;
        use fetchloom_engine::event::{Event, EventPayload};

        if error.layer() == Layer::Extract {
            self.observer.emit(&Event::new(
                self.sequence,
                EventPayload::ExtractReject {
                    path: error.member().unwrap_or_default().to_owned(),
                    error: error.clone(),
                },
            ));
        }
        self.observer.emit(&Event::new(
            self.sequence,
            EventPayload::Failure {
                error: error.clone(),
            },
        ));
        if self.json {
            match serde_json::to_string(error) {
                Ok(body) => println!("{body}"),
                Err(_) => eprintln!("{}", crate::style::failure(&error.to_string())),
            }
        } else {
            eprintln!("{}", crate::style::failure(&error.to_string()));
        }
        fetchloom_engine::outcome::ExitCode::from(error.layer())
    }
}

#[cfg(test)]
use flate2 as _;
