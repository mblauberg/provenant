//! Metadata-only usage attribution. Inert library: no command or live state owner.
#![forbid(unsafe_code)]

pub mod changes;
pub mod costs;
pub mod delivery;
#[cfg(test)]
mod differential;
pub mod hosts;
pub mod ingest;
mod pending;
pub use pending::{decode_json, Numeric};
pub mod report;

pub use pending::{Event, Observation, Usage};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid numeric report cell: {field}")]
    InvalidReportNumber { field: &'static str },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("JSON input exceeds {limit} bytes")]
    InputTooLarge { limit: usize },
    #[error("JSON integer outside i64/u64 range")]
    NumberOutOfRange,
    #[error("invalid timestamp: {0}")]
    Timestamp(String),
    #[error("{0}")]
    Invalid(String),
}
/// Maximum bytes read for one JSON document or JSONL record.
pub const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;

pub type Result<T> = std::result::Result<T, Error>;

/// Explicit per-ledger identity, replacing the Python module's cached globals.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostIdentity {
    pub current: String,
    pub aliases: BTreeSet<String>,
}
impl HostIdentity {
    pub fn new(current: String, aliases: BTreeSet<String>, peers: &BTreeSet<String>) -> Self {
        let mut aliases: BTreeSet<_> = aliases.difference(peers).cloned().collect();
        aliases.insert(current.clone());
        Self { current, aliases }
    }
    pub fn canonical<'a>(&'a self, host: &'a str) -> &'a str {
        if self.aliases.contains(host) {
            &self.current
        } else {
            host
        }
    }
    pub fn keys(&self, host: &str) -> Vec<String> {
        if self.canonical(host) == self.current {
            self.aliases.iter().cloned().collect()
        } else {
            vec![host.to_owned()]
        }
    }
}

/// The existing ledger owner supplies deduplication, file ownership and event IDs.
/// A parser cannot create schema, migrate state, read credentials or discover files.
pub trait Sink {
    fn usage(&mut self, row: Usage, origin: &std::path::Path) -> Result<()>;
    fn observation(&mut self, row: Observation) -> Result<()>;
    /// Append an event, or merge attributes on an ID conflict when `row.refresh` is true.
    fn event(&mut self, row: Event) -> Result<()>;
    fn update_event(
        &mut self,
        kind: &str,
        source_ref: &str,
        attrs: serde_json::Value,
    ) -> Result<()>;
}

/// Parse the ISO values accepted by reports, treating naive dates/times as UTC.
pub fn parse_time(value: &str) -> Result<jiff::Timestamp> {
    pending::parse_time(value)
}

/// Python's non-negative number check excludes booleans.
pub fn number(value: &serde_json::Value) -> Option<Numeric> {
    Numeric::from_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_rename_keeps_peers_distinct() {
        let id = HostIdentity::new(
            "current".into(),
            BTreeSet::from(["old".into(), "peer".into()]),
            &BTreeSet::from(["peer".into()]),
        );
        assert_eq!(id.canonical("old"), "current");
        assert_eq!(id.canonical("peer"), "peer");
        assert_eq!(id.keys("old"), vec!["current", "old"]);
    }
    #[test]
    fn dates_and_offsets_are_utc() {
        assert_eq!(
            parse_time("2026-01-01").unwrap(),
            parse_time("2026-01-01T01:00:00+01:00").unwrap()
        );
        assert_eq!(
            parse_time("2026-01-01T00:00:00").unwrap(),
            parse_time("2026-01-01Z").unwrap()
        );
        assert_eq!(number(&serde_json::json!(true)), None);
    }

    #[test]
    fn review_regression_number_preserves_integer_spelling() {
        for value in [
            serde_json::json!(0),
            serde_json::json!(300),
            serde_json::json!(9007199254740993u64),
            serde_json::json!(0.0),
        ] {
            assert_eq!(
                serde_json::to_string(&number(&value)).unwrap(),
                value.to_string()
            );
        }
    }

    #[test]
    fn review_regression_event_carries_every_field_and_refresh() {
        let value = serde_json::json!({"kind":"lane.attempt","ts":"2026-01-01T00:00:00Z","source_ref":"receipt","session":"s","run_id":"r","lane":"l","task_id":"t","pr":1295,"provider":"openai","account":"a","model":"m","harness_rev":"rev","refresh":true,"attrs":{}});
        let event: Event = serde_json::from_value(value.clone()).unwrap();
        let mut sink = ingest::tests::Capture::default();
        sink.event(event).unwrap();
        assert_eq!(serde_json::to_value(&sink.events[0]).unwrap(), value);
    }
}
