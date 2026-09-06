//! The command surface, configuration precedence, and the observers behind it.

use clap_complete as _;
use ctrlc as _;
#[cfg(test)]
use fetchloom_faults as _;
#[cfg(test)]
use flate2 as _;
#[cfg(all(test, unix))]
use rustix as _;
#[cfg(test)]
use tempfile as _;
#[cfg(all(test, windows))]
use windows_sys as _;

pub mod cache;
pub mod command;
pub mod config;
pub mod discover;
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

pub struct Reporter<'a> {
    json: bool,
    observer: &'a dyn fetchloom_engine::seam::observer::Observer,
    sequence: &'a fetchloom_engine::event::Sequence,
}

impl<'a> Reporter<'a> {
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

    #[must_use]
    pub fn json(&self) -> bool {
        self.json
    }

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
            let code = fetchloom_engine::outcome::ExitCode::from(error.layer());
            eprintln!("{}", crate::style::failure(error.next_action()));
            eprintln!(
                "{}",
                crate::style::dimmed(&format!("{}, exit {}", error.kind(), code.code()))
            );
        }
        fetchloom_engine::outcome::ExitCode::from(error.layer())
    }
}
