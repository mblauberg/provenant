//! The event-journal side of the one CI observation transport.
//!
//! When a current PR observation has reached the host journal, the observer
//! reads check state from it; otherwise it reads the forge through the
//! selected port. Keeping both transports in this module means no waiter opens
//! a journal or shells out to `gh` on its own.
//!
//! Spec: docs/specs/cli.md#ci-observations

use crate::event_delivery::Journal;
use provenant_core::CancelToken;
use serde_json::Value;
use std::{path::Path, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// A journal-backed reader of PR check state, owned by the observer module.
pub(crate) struct PrJournal(Journal);

impl PrJournal {
    /// Open the project's observation journal.
    pub(crate) fn open(state: &Path, project: &Path) -> Result<Self> {
        Journal::open(state, project).map(Self)
    }

    /// Subscribe this reader to one pull request, so later host poller ticks
    /// append observations it can replay.
    pub(crate) fn register(&self, repository: &str, number: u64, head: Option<&str>) -> Result<()> {
        self.0.register_pr(repository, number, head)
    }

    /// Whether the host poller's last read of this pull request is recent
    /// enough that a waiter need not read the forge itself.
    pub(crate) fn source_current(
        &self,
        repository: &str,
        number: u64,
        head: Option<&str>,
    ) -> Result<bool> {
        self.0.source_current(repository, number, head)
    }

    /// The next current observation for the pull request, waiting up to
    /// `maximum` for the host poller. A webhook hint carries identifiers only;
    /// the source store stays authoritative.
    pub(crate) fn next(
        &mut self,
        repository: &str,
        number: u64,
        maximum: Duration,
        cancel: &CancelToken,
    ) -> Result<Option<Value>> {
        self.0.next_pr(repository, number, maximum, cancel)
    }
}
