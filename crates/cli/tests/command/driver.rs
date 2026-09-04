//! Building the command line, settings and observer a command body is called with.

#![expect(
    clippy::unwrap_used,
    reason = "test setup, where a failure to build the input is the assertion"
)]

use std::path::Path;

use clap::Parser as _;
use fetchloom_cli::surface::{Command, CommandLine, TransferFlags};
use fetchloom_cli::{config, settings};
use fetchloom_engine::event::{EventPayload, Sequence};
use fetchloom_faults::RecordingObserver;

use crate::support::FakeEnvironment;

pub struct Driver {
    pub parsed: CommandLine,
    pub resolved: settings::Settings,
    pub observer: RecordingObserver,
    pub sequence: Sequence,
}

pub struct Degradation {
    pub requested: String,
    pub used: String,
    pub reason: String,
}

impl Driver {
    pub fn of(arguments: &[&str]) -> Self {
        let mut argv = vec!["fetchloom", "--no-config"];
        argv.extend_from_slice(arguments);
        let parsed = CommandLine::try_parse_from(&argv).unwrap();
        let transfer = transfer_of(&parsed.command);
        let discovered = config::discover(Path::new("."), None, true).unwrap();
        let resolved = settings::resolve_all(
            &parsed.global,
            &transfer,
            &discovered,
            &FakeEnvironment::default(),
        )
        .unwrap();
        Self {
            parsed,
            resolved,
            observer: RecordingObserver::new(),
            sequence: Sequence::new(),
        }
    }

    pub fn transfer(&self) -> TransferFlags {
        transfer_of(&self.parsed.command)
    }

    pub fn degradations(&self) -> Vec<Degradation> {
        self.observer
            .events()
            .iter()
            .filter_map(|event| match event.payload() {
                EventPayload::Degrade {
                    requested,
                    used,
                    reason,
                } => Some(Degradation {
                    requested: requested.clone(),
                    used: used.clone(),
                    reason: reason.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    pub fn lock_degradation(&self) -> Option<Degradation> {
        self.degradations()
            .into_iter()
            .find(|degradation| degradation.requested.starts_with("a lock pinning what"))
    }
}

fn transfer_of(command: &Command) -> TransferFlags {
    match command {
        Command::Get { transfer, .. }
        | Command::Plan { transfer, .. }
        | Command::Apply { transfer, .. }
        | Command::Repair { transfer, .. } => (**transfer).clone(),
        _ => TransferFlags::default(),
    }
}
