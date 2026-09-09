//! The live view, which is a consumer of the event stream and has no other
//! input.
//!
//! It depends on the engine's event types and nothing else, so the contract is
//! mechanical: no filesystem, network, cache or run is reachable from here.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use fetchloom_engine::event::{Event, EventPayload};

const BAR_WIDTH: usize = 24;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct HostState {
    transfers: u64,
    retries: u64,
    bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveView {
    dataset: Option<String>,
    source: Option<String>,
    reason: Option<String>,
    hosts: BTreeMap<String, HostState>,
    bytes: u64,
    entries: u64,
    rejected: Vec<String>,
    cache_hits: u64,
    cache_misses: u64,
    verified: u64,
    degradations: Vec<String>,
    failures: Vec<String>,
    finished: bool,
}

impl LiveView {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, event: &Event) {
        if let Some(dataset) = event.dataset() {
            self.dataset = Some(dataset.to_owned());
        }
        match event.payload() {
            EventPayload::SourceSelected { source, reason } => {
                self.source = Some(source.to_string());
                self.reason = Some(reason.clone());
            }
            EventPayload::SourceFailover { to, reason, .. } => {
                self.source = Some(to.to_string());
                self.reason = Some(reason.clone());
            }
            EventPayload::TransferStart { host, .. } => {
                self.hosts.entry(host.to_string()).or_default().transfers += 1;
            }
            EventPayload::TransferRetry { host, .. } => {
                self.hosts.entry(host.to_string()).or_default().retries += 1;
            }
            EventPayload::TransferProgress { bytes } => {
                self.bytes = *bytes;
            }
            EventPayload::TransferEnd { host, bytes, .. } => {
                self.hosts.entry(host.to_string()).or_default().bytes += *bytes;
                self.bytes = *bytes;
            }
            EventPayload::CacheHit { .. } => self.cache_hits += 1,
            EventPayload::CacheMiss { .. } => self.cache_misses += 1,
            EventPayload::VerifyEnd { .. } => self.verified += 1,
            EventPayload::ExtractReject { path, error } => {
                self.rejected
                    .push(format!("{path}: {}", error.next_action()));
            }
            EventPayload::ExtractEnd { entries, bytes, .. } => {
                self.entries = *entries;
                self.bytes = *bytes;
            }
            EventPayload::Degrade {
                requested,
                used,
                reason,
            } => {
                self.degradations
                    .push(format!("{requested} became {used}: {reason}"));
            }
            EventPayload::Failure { error } => {
                self.failures
                    .push(format!("{}: {}", error.kind(), error.next_action()));
            }
            EventPayload::RunEnd { .. } => self.finished = true,
            _ => {}
        }
    }

    #[must_use]
    pub fn finished(&self) -> bool {
        self.finished
    }

    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let dataset = self.dataset.as_deref().unwrap_or("?");
        let _ = writeln!(out, "dataset  {dataset}");
        match (&self.source, &self.reason) {
            (Some(source), Some(reason)) => {
                let _ = writeln!(out, "source   {source}  ({reason})");
            }
            (Some(source), None) => {
                let _ = writeln!(out, "source   {source}");
            }
            _ => {
                let _ = writeln!(out, "source   ?");
            }
        }

        let widest = self
            .hosts
            .values()
            .map(|state| state.bytes)
            .max()
            .unwrap_or(0);
        for (host, state) in &self.hosts {
            let _ = writeln!(
                out,
                "  {host:<28} {:<BAR_WIDTH$} {:>12}  {} in flight  {} retries",
                bar(state.bytes, widest),
                human(state.bytes),
                state.transfers,
                state.retries
            );
        }

        let _ = writeln!(
            out,
            "cache    {} hit  {} miss     verified {}",
            self.cache_hits, self.cache_misses, self.verified
        );
        let _ = writeln!(
            out,
            "entries  {}          bytes {}",
            self.entries,
            human(self.bytes)
        );
        for rejected in &self.rejected {
            let _ = writeln!(out, "rejected {rejected}");
        }
        for degradation in &self.degradations {
            let _ = writeln!(out, "degraded {degradation}");
        }
        for failure in &self.failures {
            let _ = writeln!(out, "error    {failure}");
        }
        out
    }
}

fn bar(value: u64, widest: u64) -> String {
    if widest == 0 {
        return " ".repeat(BAR_WIDTH);
    }
    let filled = usize::try_from(value.saturating_mul(BAR_WIDTH as u64) / widest)
        .unwrap_or(0)
        .min(BAR_WIDTH);
    let mut bar = "#".repeat(filled);
    bar.push_str(&" ".repeat(BAR_WIDTH - filled));
    bar
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    #[expect(
        clippy::cast_precision_loss,
        reason = "a byte count is shown to one decimal place, where the loss is invisible"
    )]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::LiveView;
    use fetchloom_engine::event::{Event, EventPayload, Sequence};

    #[test]
    fn a_view_that_has_seen_nothing_shows_nothing_it_was_not_told() {
        let view = LiveView::new();
        let rendered = view.render();
        assert!(rendered.contains("dataset  ?"));
        assert!(rendered.contains("source   ?"));
        assert!(!view.finished());
    }

    #[test]
    fn a_view_shows_only_what_events_carried() {
        let sequence = Sequence::new();
        let mut view = LiveView::new();
        let digest = fetchloom_engine::hashing::hash_bytes(b"one");
        view.observe(&Event::new(&sequence, EventPayload::CacheHit { digest }));
        let rendered = view.render();
        assert!(rendered.contains("1 hit"), "{rendered}");
        assert!(rendered.contains("0 miss"), "{rendered}");
    }

    #[test]
    fn a_run_end_finishes_the_view() {
        let sequence = Sequence::new();
        let mut view = LiveView::new();
        assert!(!view.finished());
        view.observe(&Event::new(
            &sequence,
            EventPayload::RunEnd { duration_ms: 1 },
        ));
        assert!(view.finished());
    }

    #[test]
    fn a_bar_is_a_share_of_the_widest_and_never_wider() {
        assert_eq!(super::bar(0, 0).trim(), "");
        assert_eq!(super::bar(10, 10).trim().len(), super::BAR_WIDTH);
        assert!(super::bar(5, 10).trim().len() < super::BAR_WIDTH);
        assert_eq!(super::bar(20, 10).len(), super::BAR_WIDTH);
    }
}
