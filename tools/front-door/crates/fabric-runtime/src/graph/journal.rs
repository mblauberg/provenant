use super::*;

/// Stored execution/checkpoint contract; older duplicate-pin records are rejected.
pub(super) const CHECKPOINT_SCHEMA_VERSION: u64 = 2;

/// Digest of a validated plan. Writers and readers share the domain and version.
pub(super) fn plan_digest(plan: &PlannedGraph) -> Result<provenant_core::Digest, CanonicalError> {
    CanonicalV1::record_digest("fabric-graph-plan", 1, plan)
}

/// Plan identity for a persisted document, without normalising its stored text.
pub fn graph_plan_digest(document: &GraphDocument) -> Result<provenant_core::Digest, JournalError> {
    let (plan, _) = validate_replay_plan(document, &replay_limits())
        .map_err(|_| JournalError::InvalidTopology)?;
    Ok(plan_digest(&plan)?)
}

/// Digest of the complete inline checkpoint and its bound execution.
/// Envelope extensions and prompt-store references are outside this payload.
pub fn checkpoint_digest(
    checkpoint: &GraphCheckpoint,
    execution: &GraphExecution,
) -> Result<provenant_core::Digest, CanonicalError> {
    CanonicalV1::record_digest("fabric-graph-checkpoint", 2, &(checkpoint, execution))
}

#[path = "events.rs"]
pub(super) mod events;
pub use events::{GraphEvent, GraphEventKind};

#[cfg(test)]
thread_local! { pub(super) static CHECKPOINT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(feature = "fixture")]
thread_local! { static READ_GRAPH_IDS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) }; }

/// Take the graph IDs read by this thread since the last call.
#[cfg(feature = "fixture")]
pub fn take_checkpoint_read_ids() -> Vec<String> {
    READ_GRAPH_IDS.with(|ids| std::mem::take(&mut *ids.borrow_mut()))
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CrashPoint {
    PeerOrigin,
    PendingMarker,
    TerminalCheckpoint,
    SentinelBefore,
    SentinelAfter,
}
#[cfg(test)]
thread_local! { pub(super) static CRASH_POINT: std::cell::Cell<Option<CrashPoint>> = const { std::cell::Cell::new(None) }; }
#[cfg(test)]
fn inject_crash(point: CrashPoint) {
    CRASH_POINT.with(|current| {
        if current.get() == Some(point) {
            current.set(None);
            std::panic::panic_any("injected journal interruption");
        }
    });
}

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("graph {graph_id}: {source}")]
    Graph {
        graph_id: String,
        #[source]
        source: Box<JournalError>,
    },
    #[error("invalid graph id for checkpoint path")]
    InvalidGraphId,
    #[error("journal was opened for a different graph")]
    GraphMismatch,
    #[error("graph name is reserved by a retained or incomplete generation")]
    NameReserved,
    #[error("graph checkpoint exceeds its byte limit")]
    TooLarge,
    #[error("checkpoint is visible but directory durability is unconfirmed")]
    DurabilityUnconfirmed(#[source] FsError),
    #[error(transparent)]
    Filesystem(#[from] FsError),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("checkpoint file read failed")]
    Io(#[from] std::io::Error),
    #[error("unsupported graph checkpoint envelope")]
    BadEnvelope,
    #[error("graph checkpoint digest does not match its contents")]
    BadDigest,
    #[error("graph checkpoint topology is incomplete or invalid")]
    InvalidTopology,
}

impl JournalError {
    /// Whether the checkpoint cannot be interpreted by this release.
    pub fn is_unreadable(&self) -> bool {
        if let Self::Graph { source, .. } = self {
            return source.is_unreadable();
        }
        matches!(
            self,
            Self::Decode(_)
                | Self::BadEnvelope
                | Self::BadDigest
                | Self::Canonical(_)
                | Self::InvalidTopology
        )
    }

    fn for_graph(self, graph_id: &str) -> Self {
        if matches!(self, Self::Graph { .. }) {
            self
        } else {
            Self::Graph {
                graph_id: graph_id.to_owned(),
                source: Box::new(self),
            }
        }
    }

    /// The underlying error, for typed recovery without discarding context.
    pub fn root_cause(&self) -> &Self {
        match self {
            Self::Graph { source, .. } => source.as_ref().root_cause(),
            other => other,
        }
    }

    pub fn is_lock_timeout(&self) -> bool {
        matches!(
            self.root_cause(),
            Self::Filesystem(FsError::LockTimeout { .. })
        )
    }

    /// Stable identifier independent of the displayed explanation.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Graph { source, .. } => source.code(),
            Self::InvalidGraphId => "invalid_graph_id",
            Self::GraphMismatch => "graph_mismatch",
            Self::NameReserved => "name_reserved",
            Self::TooLarge => "too_large",
            Self::DurabilityUnconfirmed(_) => "durability_unconfirmed",
            Self::Filesystem(_) => "filesystem",
            Self::Canonical(_) => "canonical",
            Self::Decode(_) => "decode",
            Self::Io(_) => "io",
            Self::BadEnvelope => "bad_envelope",
            Self::BadDigest => "bad_digest",
            Self::InvalidTopology => "invalid_topology",
        }
    }
}

/// Guidance for an unreadable checkpoint, with any recovery its error allows.
pub fn unreadable_graph_explanation(graph_id: &str, _error: &JournalError) -> String {
    unreadable_graph_guidance(graph_id)
}

/// A tick notice already recorded in the dedicated unreadable marker.
#[derive(Debug, Error)]
#[error("{}", unreadable_graph_guidance(.0))]
pub struct UnreadableCheckpointNotice(pub String);

/// Guidance for a graph whose checkpoint cannot be interpreted.
pub fn unreadable_graph_guidance(graph_id: &str) -> String {
    format!(
        "Graph {graph_id} state is unreadable. From the project and state directory that own it, run `provenant stop -- {graph_id}` to remove its record from the listing. There is no safe automatic repair. Other graphs and separately listed attempts remain available; inspect active attempts and writer fences separately."
    )
}

/// Safe, atomic checkpoint file journal. The held owner lock fences concurrent
/// graph writers; all filesystem access after open is descriptor-relative.
pub struct FileJournal {
    /// Envelope extensions sit outside the checkpoint digest.
    pub(super) extra: provenant_records::ExtraFields,
    pub(super) graph_id: String,
    state_root: StateRoot,
    project: ProjectId,
    pub(super) directory: Directory,
    pub(super) execution: Option<GraphExecution>,
    pub(super) _lock: LockGuard,
}

/// One graph or node cancellation scope and its optional reason.
#[derive(Clone, Copy, Debug)]
pub struct CancellationTarget<'a> {
    pub graph_id: &'a str,
    pub node: Option<&'a str>,
    pub reason: Option<&'a str>,
}

impl FileJournal {
    /// Resolve owner-managed observation paths without creating graph state.
    /// The event log is pre-commit; the checkpoint path signals the following
    /// authoritative publication, and the sidecar changes displayed status.
    pub fn observation_paths(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Vec<std::path::PathBuf>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = graph_directory(root, project)?;
            let path = directory.current_path()?;
            Ok(vec![
                path.join(events::name(graph_id)),
                path.join(checkpoint_file_name(graph_id)),
                path.join(advance_error_name(graph_id)),
            ])
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Replay the latest committed checkpoint marker into the shared journal.
    /// A failed index is retryable and never changes the authoritative graph.
    pub fn index_observation(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        use provenant_records::lane_contract::{
            Coverage, ObservationCursor, ObservationEvent, ObservationKind, ObservationLineage,
        };
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId.into());
        }
        let directory = graph_directory(root, project)?;
        let Some(stored) = load_checkpoint(&directory, graph_id)? else {
            return Ok(());
        };
        let source_digest =
            CanonicalV1::record_digest("fabric-graph-event", 1, &stored.checkpoint)?.to_string();
        let source_index = events::read(&directory, graph_id)?
            .iter()
            .enumerate()
            .rev()
            .find(|(_, event)| {
                event.kind == GraphEventKind::Checkpoint && event.digest == source_digest
            })
            .map(|(index, _)| index + 1)
            .unwrap_or(0);
        let clock = provenant_os::clock::SystemClock::new();
        let cancel = CancelToken::new();
        let request = provenant_fabric_store::Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = provenant_fabric_store::Store::open(root, &request)?;
        store.append_observation(&ObservationEvent {
            header: provenant_records::RecordHeader::V1,
            id: format!("graph-checkpoint:{graph_id}:{source_index}:{}", stored.digest),
            cursor: ObservationCursor { project, seq: 0 },
            lineage: ObservationLineage { attempt: None, parent_attempt: None, work: None, extra: Default::default() },
            kind: ObservationKind::GraphCheckpoint,
            coverage: Coverage::Partial,
            detail: serde_json::json!({"graph":graph_id,"digest":stored.digest,"source":"graph_checkpoint","source_index":source_index}),
            extra: Default::default(),
        }, provenant_core::WallTime::from_micros(0)?, &request)?;
        Ok(())
    }

    /// Persist cancellation without waiting for the graph's journal lock.
    pub fn request_cancellation(
        root: &StateRoot,
        project: ProjectId,
        target: CancellationTarget<'_>,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<String, JournalError> {
        let error_graph_id = target.graph_id.to_owned();
        super::cancellation_intent::record(root, project, target, budget, cancel, clock)
            .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Whether one particular stop request still awaits graph-lock acknowledgement.
    pub fn cancellation_request_pending(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        request_id: &str,
    ) -> Result<bool, JournalError> {
        let error_graph_id = graph_id.to_owned();
        super::cancellation_intent::pending(root, project, graph_id, request_id)
            .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Read complete event records in order, ignoring a torn final record.
    pub fn read_events(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Vec<GraphEvent>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            match graph_directory(root, project) {
                Ok(directory) => events::read(&directory, graph_id),
                Err(error) if error.is_not_found() => Ok(Vec::new()),
                Err(error) => Err(error.into()),
            }
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }
    /// A successful explicit delivery reopens only nodes fenced by that attempt.
    pub fn retry_delivered_attempt(
        root: &StateRoot,
        project: ProjectId,
        attempt: provenant_core::AttemptId,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), JournalError> {
        let attempt = attempt.to_string();
        for id in Self::graph_ids(root, project)? {
            let snapshot = match Self::read_checkpoint(root, project, &id) {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => continue,
                Err(error) => {
                    Self::set_advance_error(root, project, &id, Some(&error.to_string()))?;
                    continue;
                }
            };
            if snapshot.cancelled
                || !snapshot.nodes.iter().any(|node| {
                    node.state == NodeState::Failed
                        && node.attempts.last().is_some_and(|last| {
                            matches!(
                                last.outcome,
                                Some(AttemptOutcome::OtherFailure | AttemptOutcome::ReviewRequired)
                            ) && last
                                .attempt_ref
                                .as_deref()
                                .and_then(|reference| reference.rsplit('/').next())
                                == Some(attempt.as_str())
                        })
                })
            {
                continue;
            }
            let mut journal = Self::open(root, project, &id, budget, cancel, clock)?;
            let Some(mut checkpoint) = journal.load(&id)? else {
                continue;
            };
            if checkpoint.cancelled {
                continue;
            }
            let mut changed = false;
            let mut reopened_loops = std::collections::BTreeSet::new();
            for node in &mut checkpoint.nodes {
                if node.state != NodeState::Failed {
                    continue;
                }
                if let Some(last) = node.attempts.last_mut()
                    && matches!(
                        last.outcome,
                        Some(AttemptOutcome::OtherFailure | AttemptOutcome::ReviewRequired)
                    )
                    && last
                        .attempt_ref
                        .as_deref()
                        .and_then(|reference| reference.rsplit('/').next())
                        == Some(attempt.as_str())
                {
                    if let Some(round) = &node.round {
                        reopened_loops.insert(round.loop_id.clone());
                    }
                    node.state = NodeState::Running;
                    last.state = AttemptState::Starting;
                    last.outcome = None;
                    // The retry settles again and takes a fresh settlement order.
                    last.settled = 0;
                    changed = true;
                }
            }
            if changed {
                loops::reopen_after_delivery(&mut checkpoint, &reopened_loops);
                for node in &mut checkpoint.nodes {
                    if node.state == NodeState::Blocked {
                        node.state = NodeState::Pending;
                    }
                }
                journal.save(&checkpoint)?;
            }
        }
        Ok(())
    }
    pub fn open(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Self, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let root_dir = Directory::open_anchor(root.as_path())?;
            let fabric_dir = open_or_create(&root_dir, OsStr::new("fabric"))?;
            let graphs_dir = open_or_create(&fabric_dir, OsStr::new("graphs"))?;
            let graphs_dir = open_or_create(&graphs_dir, OsStr::new(&project.to_string()))?;
            let lock_name = format!("graph-{graph_id}.lock");
            let lock = graphs_dir.lock(
                OsStr::new(&lock_name),
                LockMode::Exclusive,
                budget,
                cancel,
                clock,
            )?;
            let stored = load_checkpoint(&graphs_dir, graph_id)?;
            let (extra, execution) = stored.map_or_else(
                || (Default::default(), None),
                |stored| (stored.extra, Some(stored.execution)),
            );
            Ok(Self {
                extra,
                graph_id: graph_id.to_owned(),
                state_root: root.clone(),
                project,
                directory: graphs_dir,
                execution,
                _lock: lock,
            })
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Try the graph lock once. Stop uses this after writing its independent
    /// intent so it can return a pending receipt while a launch owns the lock.
    pub fn try_open(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Self, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let root_dir = Directory::open_anchor(root.as_path())?;
            let fabric_dir = open_or_create(&root_dir, OsStr::new("fabric"))?;
            let graphs_dir = open_or_create(&fabric_dir, OsStr::new("graphs"))?;
            let graphs_dir = open_or_create(&graphs_dir, OsStr::new(&project.to_string()))?;
            let lock_name = format!("graph-{graph_id}.lock");
            let lock = graphs_dir.try_lock(OsStr::new(&lock_name), LockMode::Exclusive)?;
            let stored = load_checkpoint(&graphs_dir, graph_id)?;
            let (extra, execution) = stored.map_or_else(
                || (Default::default(), None),
                |stored| (stored.extra, Some(stored.execution)),
            );
            Ok(Self {
                extra,
                graph_id: graph_id.to_owned(),
                state_root: root.clone(),
                project,
                directory: graphs_dir,
                execution,
                _lock: lock,
            })
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Give a hand-built fixture the same plan identity as a real admitted graph.
    /// Production saves never repair or replace an incoming checkpoint digest.
    #[cfg(feature = "fixture")]
    pub fn prepare_fixture_checkpoint(
        &self,
        checkpoint: &mut GraphCheckpoint,
    ) -> Result<(), JournalError> {
        let execution = self.execution.as_ref().ok_or(JournalError::BadEnvelope)?;
        checkpoint.plan_digest = graph_plan_digest(&execution.document)?.to_string();
        Ok(())
    }

    pub fn bind(&mut self, execution: GraphExecution) -> Result<(), JournalError> {
        let error_graph_id = self.graph_id.clone();
        (|| {
            if execution.document.graph_id != self.graph_id || !execution.directory.is_absolute() {
                return Err(JournalError::GraphMismatch);
            }
            if self.execution.is_none() && self.has_retained_identity()? {
                return Err(JournalError::NameReserved);
            }
            if !execution.valid_bases() {
                return Err(JournalError::BadEnvelope);
            }
            if let Some(bound) = &self.execution {
                if bound.bases != execution.bases {
                    return Err(JournalError::GraphMismatch);
                }
                // The original host execution is part of the checkpoint. A caller
                // may resume from another checkout, but cannot replace that binding
                // or discard fields retained by a newer writer.
                return Ok(());
            }
            self.execution = Some(execution);
            Ok(())
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Update host-only launch input while the graph journal lock is held.
    pub fn set_requester_path(&mut self, path: Vec<u8>) -> Result<(), JournalError> {
        let error_graph_id = self.graph_id.clone();
        (|| {
            let execution = self.execution.as_mut().ok_or(JournalError::BadEnvelope)?;
            execution.requester_path = Some(path);
            Ok(())
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    pub fn integration_base(&self) -> Option<provenant_core::GitOid> {
        self.execution
            .as_ref()
            .and_then(|execution| execution.bases)
            .and_then(|bases| bases.integration)
    }

    /// Discover journals without creating directories. Each is read separately
    /// so a corrupt graph need not prevent another graph from advancing.
    pub fn graph_ids(root: &StateRoot, project: ProjectId) -> Result<Vec<String>, JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Vec::new();
        for entry in directory.entries()? {
            let name = entry?;
            if let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_prefix("graph-"))
                .and_then(|name| name.strip_suffix(".json"))
                && valid_identifier(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Names that have identified a graph generation, including collected
    /// checkpoints. Reservations are never released because retained attempt,
    /// chair and event stores continue to identify the graph by this name.
    pub fn reserved_graph_ids(
        root: &StateRoot,
        project: ProjectId,
    ) -> Result<Vec<String>, JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Self::graph_ids(root, project)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        ids.extend(Self::event_graph_ids(root, project)?);
        for entry in directory.entries()? {
            let name = entry?;
            if let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_prefix("graph-"))
                .and_then(|name| name.strip_suffix(".reserved"))
                && valid_identifier(id)
            {
                ids.insert(id.to_owned());
            }
        }
        Ok(ids.into_iter().collect())
    }

    /// Discover retained event logs, including graphs whose checkpoints were collected.
    pub fn event_graph_ids(
        root: &StateRoot,
        project: ProjectId,
    ) -> Result<Vec<String>, JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let mut ids = Vec::new();
        for entry in directory.entries()? {
            let name = entry?;
            if let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_suffix(".events.jsonl"))
                && valid_identifier(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Discover the derived pending set, rebuilding it when its sentinel is absent.
    pub fn pending_graph_ids(
        root: &StateRoot,
        project: ProjectId,
    ) -> Result<Vec<String>, JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let markers = open_or_create(&directory, OsStr::new("pending"))?;
        match markers.open_read(OsStr::new("complete")) {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {
                Self::rebuild_pending_in(root, project, &directory, &markers)?
            }
            Err(error) => return Err(error.into()),
        }
        let mut ids = Vec::new();
        for entry in markers.entries()? {
            let name = entry?;
            if let Some(id) = name.to_str().and_then(|s| s.strip_suffix(".pending"))
                && valid_identifier(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Rebuild by adding markers only. Concurrent saves own removals under their graph lock.
    pub fn rebuild_pending(root: &StateRoot, project: ProjectId) -> Result<(), JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let markers = open_or_create(&directory, OsStr::new("pending"))?;
        Self::rebuild_pending_in(root, project, &directory, &markers)
    }

    /// Collect old terminal checkpoints under their individual graph locks.
    pub fn collect_terminal(
        root: &StateRoot,
        project: ProjectId,
        before_ns: i128,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<usize, JournalError> {
        let mut removed = 0;
        for id in Self::graph_ids(root, project)? {
            let journal = match Self::open(
                root,
                project,
                &id,
                WaitBudget::nonblocking(clock.mono_now()),
                cancel,
                clock,
            ) {
                Ok(journal) => journal,
                Err(error) if error.is_lock_timeout() => continue,
                Err(error) if error.is_unreadable() => continue,
                Err(error) => return Err(error),
            };
            let Some(stored) = load_checkpoint(&journal.directory, &id)? else {
                continue;
            };
            let checkpoint = &stored.checkpoint;
            if stored_pending(&stored)
                || !checkpoint.asks.is_empty()
                || checkpoint.wake_at.is_some()
                || checkpoint.nodes.iter().any(|node| {
                    node.land
                        .as_ref()
                        .is_some_and(|land| land.wake_at.is_some())
                })
            {
                continue;
            }
            let name = journal.checkpoint_name();
            let metadata = journal
                .directory
                .metadata_at(&name, false)
                .map_err(|error| JournalError::from(error).for_graph(&id))?;
            if metadata.modified_ns >= before_ns {
                continue;
            }
            (|| -> Result<(), JournalError> {
                collect_scratch_documents(&stored.execution)?;
                journal.reserve_name()?;
                journal.directory.unlink(&name)?;
                prompt_store::collect(&journal.directory, &id)?;
                for sidecar in [unreadable_marker_name(&id), advance_error_name(&id)] {
                    match journal.directory.unlink(&sidecar) {
                        Ok(()) => {}
                        Err(error) if error.is_not_found() => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                let markers = open_or_create(&journal.directory, OsStr::new("pending"))?;
                remove_pending_marker(&markers, &id)?;
                journal
                    .directory
                    .sync()
                    .map_err(JournalError::DurabilityUnconfirmed)?;
                // No checkpoint mutation may follow lock retirement.
                journal
                    .directory
                    .retire_owner_lock(OsStr::new(&format!("graph-{id}.lock")), &journal._lock)?;
                Ok(())
            })()
            .map_err(|error| error.for_graph(&id))?;
            removed += 1;
        }
        removed += Self::collect_unreferenced_artifacts(root, project, cancel, clock)?;
        Ok(removed)
    }

    /// Resume cleanup when an earlier collection removed the checkpoint but
    /// stopped before deleting its prompt blobs or derived sidecars. Reserved
    /// graph names are durable, so the missing checkpoint is the proof that no
    /// live graph owns these derived files. The per-graph writer lock fences a
    /// concurrent admission or save while the absence is checked and artifacts
    /// are removed.
    fn collect_unreferenced_artifacts(
        root: &StateRoot,
        project: ProjectId,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<usize, JournalError> {
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let reserved = Self::reserved_graph_ids(root, project)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let checkpoints = Self::graph_ids(root, project)?
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>();
        let mut candidates = std::collections::BTreeSet::new();
        for entry in directory.entries()? {
            let name = entry?;
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(rest) = name.strip_prefix("graph-") else {
                continue;
            };
            let id = [".lock", ".unreadable", ".advance-error"]
                .iter()
                .find_map(|suffix| rest.strip_suffix(suffix))
                .or_else(|| {
                    rest.split_once(".prompt-").and_then(|(id, digest)| {
                        (digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
                            .then_some(id)
                    })
                });
            if let Some(id) = id.filter(|id| valid_identifier(id)) {
                candidates.insert(id.to_owned());
            }
        }
        match directory.open_verified_directory(OsStr::new("pending")) {
            Ok(markers) => {
                for entry in markers.entries()? {
                    let name = entry?;
                    if let Some(id) = name
                        .to_str()
                        .and_then(|name| name.strip_suffix(".pending"))
                        .filter(|id| valid_identifier(id))
                    {
                        candidates.insert(id.to_owned());
                    }
                }
            }
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }

        let mut recovered = 0;
        for id in candidates
            .into_iter()
            .filter(|id| reserved.contains(id) && !checkpoints.contains(id))
        {
            let journal = match Self::open(
                root,
                project,
                &id,
                WaitBudget::nonblocking(clock.mono_now()),
                cancel,
                clock,
            ) {
                Ok(journal) => journal,
                Err(error) if error.is_lock_timeout() => continue,
                Err(error) => return Err(error),
            };
            match journal.directory.open_read(&journal.checkpoint_name()) {
                Ok(_) => continue,
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(JournalError::from(error).for_graph(&id)),
            }

            (|| -> Result<(), JournalError> {
                prompt_store::collect(&journal.directory, &id)?;
                for sidecar in [unreadable_marker_name(&id), advance_error_name(&id)] {
                    match journal.directory.unlink(&sidecar) {
                        Ok(()) => {}
                        Err(error) if error.is_not_found() => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                match journal
                    .directory
                    .open_verified_directory(OsStr::new("pending"))
                {
                    Ok(markers) => remove_pending_marker(&markers, &id)?,
                    Err(error) if error.is_not_found() => {}
                    Err(error) => return Err(error.into()),
                }
                journal
                    .directory
                    .sync()
                    .map_err(JournalError::DurabilityUnconfirmed)?;
                journal
                    .directory
                    .retire_owner_lock(OsStr::new(&format!("graph-{id}.lock")), &journal._lock)?;
                Ok(())
            })()
            .map_err(|error| error.for_graph(&id))?;
            recovered += 1;
        }
        Ok(recovered)
    }

    fn rebuild_pending_in(
        root: &StateRoot,
        project: ProjectId,
        directory: &Directory,
        markers: &Directory,
    ) -> Result<(), JournalError> {
        for id in Self::graph_ids(root, project)? {
            match load_checkpoint(directory, &id) {
                Ok(Some(stored)) if stored_pending(&stored) => {
                    write_pending_marker(markers, &id).map_err(|error| error.for_graph(&id))?
                }
                // Leave load failures for the individual graph tick to report.
                Err(_) => {
                    write_pending_marker(markers, &id).map_err(|error| error.for_graph(&id))?
                }
                _ => {}
            }
        }
        #[cfg(test)]
        inject_crash(CrashPoint::SentinelBefore);
        markers
            .atomic_write(
                OsStr::new("complete"),
                ".sentinel.",
                b"1",
                PublishMode::Replace,
            )?
            .require_durable()
            .map_err(JournalError::DurabilityUnconfirmed)?;
        #[cfg(test)]
        inject_crash(CrashPoint::SentinelAfter);
        Ok(())
    }

    /// Remove a stale marker only while holding the graph's writer lock.
    pub fn clear_stale_pending(&mut self) -> Result<(), JournalError> {
        let error_graph_id = self.graph_id.clone();
        (|| {
            if load_checkpoint(&self.directory, &self.graph_id)?
                .as_ref()
                .is_none_or(|stored| !stored_pending(stored))
            {
                let markers = open_or_create(&self.directory, OsStr::new("pending"))?;
                remove_pending_marker(&markers, &self.graph_id)?;
            }
            Ok(())
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// The execution bound to a graph in any state, without taking the lock.
    pub fn execution(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphExecution>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            match graph_directory(root, project) {
                Ok(directory) => {
                    Ok(load_checkpoint(&directory, graph_id)?.map(|stored| stored.execution))
                }
                Err(error) if error.is_not_found() => Ok(None),
                Err(error) => Err(error.into()),
            }
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    pub fn pending_execution(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphExecution>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = graph_directory(root, project)?;
            Ok(load_checkpoint(&directory, graph_id)?
                .and_then(|stored| stored_pending(&stored).then_some(stored.execution)))
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Read an atomic checkpoint snapshot without creating directories or
    /// acquiring the writer lock. Concurrent saves publish whole files.
    pub fn read_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = graph_directory(root, project);
            match directory {
                Ok(directory) => {
                    Ok(load_checkpoint(&directory, graph_id)?.map(|stored| stored.checkpoint))
                }
                Err(error) if error.is_not_found() => Ok(None),
                Err(error) => Err(error.into()),
            }
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Read a status snapshot with minima recovered from its retained plan.
    pub fn read_status_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            Ok(Self::read_status_snapshot(root, project, graph_id)?
                .map(|(_, checkpoint)| checkpoint))
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Read the status document and checkpoint from one atomic stored record.
    /// The document is the declared topology authority for graph selectors.
    pub fn read_status_snapshot(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<(GraphExecution, GraphCheckpoint)>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = match graph_directory(root, project) {
                Ok(directory) => directory,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let Some(mut stored) = load_checkpoint(&directory, graph_id)? else {
                return Ok(None);
            };
            if stored.checkpoint.nodes.iter().any(|node| {
                node.state == NodeState::Succeeded
                    && node.min_successful.is_none()
                    && node.children.iter().any(|child| {
                        stored
                            .checkpoint
                            .nodes
                            .iter()
                            .any(|member| &member.id == child && member.state == NodeState::Failed)
                    })
            }) {
                super::recover_status_minima(&stored.execution.document, &mut stored.checkpoint)
                    .map_err(|_| JournalError::BadEnvelope)?;
            }
            Ok(Some((stored.execution, stored.checkpoint)))
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Read a checkpoint with compacted findings expanded for reporting.
    pub fn read_materialised_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = match graph_directory(root, project) {
                Ok(directory) => directory,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let Some(mut stored) = load_checkpoint(&directory, graph_id)? else {
                return Ok(None);
            };
            materialise_checkpoint_findings(&stored.execution.document, &mut stored.checkpoint)
                .map_err(|_| JournalError::BadEnvelope)?;
            Ok(Some(stored.checkpoint))
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// Corrupt checkpoints remain errors on every tick, independently of advance errors.
    /// Only a genuinely absent record returns `None`.
    pub fn read_checkpoint_for_tick(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, Box<dyn Error>> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId.for_graph(graph_id).into());
        }
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let bytes = match read_checkpoint_bytes(&directory, graph_id)
            .map_err(|error| error.for_graph(graph_id))?
        {
            Some(bytes) => bytes,
            None => {
                let _ = directory.unlink(&unreadable_marker_name(graph_id));
                return Ok(None);
            }
        };
        match decode_checkpoint_in(&directory, &bytes, graph_id) {
            Ok(stored) => {
                let _ = directory.unlink(&unreadable_marker_name(graph_id));
                Ok(Some(stored.checkpoint))
            }
            Err(error) if error.is_unreadable() => {
                let digest = CanonicalV1::file_digest(&bytes).to_string();
                let name = unreadable_marker_name(graph_id);
                let mut previous = String::new();
                if let Ok(mut file) = directory.open_read(&name) {
                    let _ = file.by_ref().take(72).read_to_string(&mut previous);
                }
                if previous != digest {
                    let _ = directory.atomic_write(
                        &name,
                        ".graph-unreadable.",
                        digest.as_bytes(),
                        PublishMode::Replace,
                    );
                }
                Err(UnreadableCheckpointNotice(graph_id.to_owned()).into())
            }
            Err(error) => Err(error.for_graph(graph_id).into()),
        }
    }

    /// Retire an unreadable record under its graph lock without releasing other fences.
    pub fn retire_unreadable(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
        cleanup: impl FnOnce(Option<&GraphCheckpoint>) -> Result<(), Box<dyn Error>>,
    ) -> Result<bool, Box<dyn Error>> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId.for_graph(graph_id).into());
        }
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        // A readable checkpoint needs no retirement. Inspect the atomic
        // snapshot first so an ordinary stop never waits on a live graph writer
        // before it can persist its independent cancellation request.
        match load_checkpoint(&directory, graph_id) {
            Ok(_) => return Ok(false),
            Err(error) if !error.is_unreadable() => return Err(error.into()),
            Err(_) => {}
        }
        let lock_name = format!("graph-{graph_id}.lock");
        let _lock = directory.lock(
            OsStr::new(&lock_name),
            LockMode::Exclusive,
            budget,
            cancel,
            clock,
        )?;
        match load_checkpoint(&directory, graph_id) {
            Err(error) if error.is_unreadable() => {
                let bytes = read_checkpoint_bytes(&directory, graph_id)
                    .map_err(|error| error.for_graph(graph_id))?;
                let recovered = bytes.as_deref().and_then(|bytes| {
                    let bytes = prompt_store::rehydrate(&directory, graph_id, bytes).ok()?;
                    decode_verified_checkpoint(&bytes, graph_id, bytes.len() as u64).ok()
                });
                cleanup(recovered.as_ref().map(|stored| &stored.checkpoint))?;
                directory.unlink(OsStr::new(&format!("graph-{graph_id}.json")))?;
                directory
                    .sync()
                    .map_err(JournalError::DurabilityUnconfirmed)?;
                let _ = directory.unlink(&unreadable_marker_name(graph_id));
                Ok(true)
            }
            Err(error) => Err(error.for_graph(graph_id).into()),
            Ok(_) => Ok(false),
        }
    }

    /// Record why an automatic advance failed, or clear the record with
    /// `None`. It takes no graph lock, because the failure may be that lock.
    pub fn set_advance_error(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        error: Option<&str>,
    ) -> Result<(), JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = graph_directory(root, project)?;
            let name = advance_error_name(graph_id);
            let Some(error) = error else {
                return match directory.unlink(&name) {
                    Err(error) if error.is_not_found() => Ok(()),
                    other => Ok(other?),
                };
            };
            let mut end = error.len().min(MAX_ADVANCE_ERROR_BYTES);
            while !error.is_char_boundary(end) {
                end -= 1;
            }
            directory.atomic_write(
                &name,
                ".graph-advance-error.",
                &error.as_bytes()[..end],
                PublishMode::Replace,
            )?;
            Ok(())
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    /// The recorded failure of the last automatic advance, if any.
    pub fn read_advance_error(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<String>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = match graph_directory(root, project) {
                Ok(directory) => directory,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let file = match directory.open_read(&advance_error_name(graph_id)) {
                Ok(file) => file,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let mut bytes = Vec::new();
            file.take(MAX_ADVANCE_ERROR_BYTES as u64)
                .read_to_end(&mut bytes)?;
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    pub(super) fn checkpoint_name(&self) -> OsString {
        checkpoint_file_name(&self.graph_id)
    }

    /// The immutable origin binding retained with the graph's name after
    /// collection removes its checkpoint. It is copied from document metadata
    /// under the same journal lock before the first attempt can launch.
    pub fn peer_origin(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<provenant_records::WorkKey>, JournalError> {
        let error_graph_id = graph_id.to_owned();
        (|| {
            if !valid_identifier(graph_id) {
                return Err(JournalError::InvalidGraphId);
            }
            let directory = match graph_directory(root, project) {
                Ok(directory) => directory,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            let bytes = match directory
                .read_stable(OsStr::new(&format!("peer-origin-{graph_id}.json")), 4096)
            {
                Ok(bytes) => bytes,
                Err(error) if error.is_not_found() => return Ok(None),
                Err(error) => return Err(error.into()),
            };
            serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| JournalError::BadEnvelope)
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    fn reserve_name(&self) -> Result<(), JournalError> {
        // Reserve first: a crash while publishing retained metadata must not
        // make a partially admitted launch available to different input.
        let name: OsString = format!("graph-{}.reserved", self.graph_id).into();
        self.directory
            .atomic_write(&name, ".graph-name.", b"reserved\n", PublishMode::Replace)?
            .require_durable()
            .map_err(JournalError::DurabilityUnconfirmed)?;
        if let Some(origin) = self
            .execution
            .as_ref()
            .and_then(|execution| execution.document.peer_origin_work_key.as_ref())
        {
            let origin = origin.clone();
            let name = format!("peer-origin-{}.json", self.graph_id);
            match self.directory.read_stable(OsStr::new(&name), 4096) {
                Ok(bytes) => {
                    let retained: provenant_records::WorkKey =
                        serde_json::from_slice(&bytes).map_err(|_| JournalError::BadEnvelope)?;
                    if retained != origin {
                        return Err(JournalError::GraphMismatch);
                    }
                }
                Err(error) if error.is_not_found() => {
                    let bytes = CanonicalV1::bytes(&origin)?;
                    self.directory
                        .atomic_write(
                            OsStr::new(&name),
                            ".peer-origin.",
                            bytes.as_bytes(),
                            PublishMode::NoReplace,
                        )?
                        .require_durable()
                        .map_err(JournalError::DurabilityUnconfirmed)?;
                }
                Err(error) => return Err(error.into()),
            }
            #[cfg(test)]
            inject_crash(CrashPoint::PeerOrigin);
        }
        Ok(())
    }

    fn has_retained_identity(&self) -> Result<bool, JournalError> {
        for name in [
            OsString::from(format!("graph-{}.reserved", self.graph_id)),
            OsString::from(format!("peer-origin-{}.json", self.graph_id)),
            OsString::from(format!("{}.events.jsonl", self.graph_id)),
        ] {
            match self.directory.open_read(&name) {
                Ok(_) => return Ok(true),
                Err(error) if error.is_not_found() => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(false)
    }
}

impl GraphJournal for FileJournal {
    type Error = JournalError;

    fn load(&mut self, graph_id: &str) -> Result<Option<GraphCheckpoint>, Self::Error> {
        let error_graph_id = self.graph_id.clone();
        (|| {
            if graph_id != self.graph_id {
                return Err(JournalError::GraphMismatch);
            }
            Ok(load_checkpoint(&self.directory, graph_id)?.map(|stored| stored.checkpoint))
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    fn apply_cancellation(
        &mut self,
        document: &GraphDocument,
        checkpoint: &mut GraphCheckpoint,
    ) -> Result<bool, Self::Error> {
        let error_graph_id = self.graph_id.clone();
        super::cancellation_intent::acknowledge(self, document, checkpoint)
            .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
        let error_graph_id = self.graph_id.clone();
        (|| {
            if checkpoint.graph_id != self.graph_id {
                return Err(JournalError::GraphMismatch);
            }
            let execution = self.execution.as_ref().ok_or(JournalError::BadEnvelope)?;
            if execution.bases != checkpoint.bases {
                return Err(JournalError::GraphMismatch);
            }
            let stored = StoredCheckpoint {
                extra: self.extra.clone(),
                schema_version: CHECKPOINT_SCHEMA_VERSION,
                digest: checkpoint_digest(checkpoint, execution)?.to_string(),
                checkpoint: checkpoint.clone(),
                execution: execution.clone(),
            };
            let inline = CanonicalV1::bytes(&stored)?;
            decode_checkpoint_limited(
                inline.as_bytes(),
                &self.graph_id,
                inline.as_bytes().len() as u64,
            )?;
            // Prompts live in the store, so the limit applies to the digest form.
            let mut compact = serde_json::to_value(&stored)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            prompt_store::externalise(&self.directory, &self.graph_id, &mut compact)?;
            let bytes = CanonicalV1::bytes(&compact)?;
            decode_checkpoint_in(&self.directory, bytes.as_bytes(), &self.graph_id)?;
            if bytes.as_bytes().len() as u64 > MAX_CHECKPOINT_BYTES {
                return Err(JournalError::TooLarge);
            }
            self.reserve_name()?;
            let previous = load_checkpoint(&self.directory, &self.graph_id)?;
            events::append_changes(
                &self.directory,
                &self.graph_id,
                previous.as_ref().map(|stored| &stored.checkpoint),
                checkpoint,
                Some(&execution.document),
            )?;
            let markers = open_or_create(&self.directory, OsStr::new("pending"))?;
            let pending = stored_pending(&stored);
            if pending {
                write_pending_marker(&markers, &self.graph_id)?;
                #[cfg(test)]
                inject_crash(CrashPoint::PendingMarker);
            }
            self.directory
                .atomic_write(
                    &self.checkpoint_name(),
                    ".graph-checkpoint.",
                    bytes.as_bytes(),
                    PublishMode::Replace,
                )?
                .require_durable()
                .map_err(JournalError::DurabilityUnconfirmed)?;
            if !pending {
                #[cfg(test)]
                inject_crash(CrashPoint::TerminalCheckpoint);
                remove_pending_marker(&markers, &self.graph_id)?;
            }
            // Index only after authoritative checkpoint publication. Failed
            // hints are healed by the host tick and waiter gap reconciliation.
            #[allow(clippy::disallowed_methods)]
            if let Err(error) =
                Self::index_observation(&self.state_root, self.project, &self.graph_id)
            {
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: graph observation indexing deferred: {error}"
                );
            }
            Ok(())
        })()
        .map_err(|error: JournalError| error.for_graph(&error_graph_id))
    }
}

fn stored_pending(stored: &StoredCheckpoint) -> bool {
    let checkpoint = &stored.checkpoint;
    next_step::pending(checkpoint)
        || checkpoint.pending_steers.has_pending()
        || checkpoint
            .nodes
            .iter()
            .any(|node| matches!(node.state, NodeState::Pending | NodeState::Running))
        || !checkpoint.cancelled
            && !checkpoint.delivered
            && stored.execution.document.delivery == Some(provenant_records::run::DeliveryMode::Pr)
            && checkpoint.successful()
        || !checkpoint.asks.is_empty()
}

/// Remove scratch documents equal to this graph's retained document. The
/// execution directory may already have been removed.
fn collect_scratch_documents(execution: &GraphExecution) -> Result<(), JournalError> {
    let directory = match Directory::open_anchor(&execution.directory) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let mut directory = directory;
    for component in [".provenant", "scratch", "graphs"] {
        directory = match directory.open_verified_directory(OsStr::new(component)) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error.into()),
        };
    }
    let mut metadata = directory.metadata()?;
    if metadata.uid == provenant_os::fs::effective_uid() && metadata.mode & 0o077 != 0 {
        directory.make_private()?;
        metadata = directory.metadata()?;
    }
    if metadata.uid != provenant_os::fs::effective_uid() || metadata.mode & 0o077 != 0 {
        return Ok(());
    }
    let mut removed = false;
    for entry in directory.entries()? {
        let name = entry?;
        let Ok(metadata) = directory.metadata_at(&name, false) else {
            continue;
        };
        if metadata.uid != provenant_os::fs::effective_uid() || metadata.links != 1 {
            continue;
        }
        let Ok(bytes) = directory.read_stable(
            &name,
            crate::config::GraphLimits::document_bytes_bound().maximum_u64(),
        ) else {
            continue;
        };
        if serde_json::from_slice::<GraphDocument>(&bytes)
            .ok()
            .as_ref()
            != Some(&execution.document)
        {
            continue;
        }
        if !directory
            .metadata_at(&name, false)
            .is_ok_and(|current| metadata.unchanged(&current))
        {
            continue;
        }
        match directory.unlink(&name) {
            Ok(()) => removed = true,
            Err(error) if error.is_not_found() => {}
            Err(error) => return Err(error.into()),
        }
    }
    if removed {
        directory
            .sync()
            .map_err(JournalError::DurabilityUnconfirmed)?;
    }
    Ok(())
}

fn pending_marker_name(graph_id: &str) -> OsString {
    format!("{graph_id}.pending").into()
}

fn write_pending_marker(markers: &Directory, graph_id: &str) -> Result<(), JournalError> {
    markers
        .atomic_write(
            &pending_marker_name(graph_id),
            ".pending.",
            b"1",
            PublishMode::Replace,
        )?
        .require_durable()
        .map_err(JournalError::DurabilityUnconfirmed)
}

fn remove_pending_marker(markers: &Directory, graph_id: &str) -> Result<(), JournalError> {
    match markers.unlink(&pending_marker_name(graph_id)) {
        Ok(()) => markers.sync().map_err(JournalError::DurabilityUnconfirmed),
        Err(error) if error.is_not_found() => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(super) fn unreadable_marker_name(graph_id: &str) -> OsString {
    format!("graph-{graph_id}.unreadable").into()
}

pub(super) fn advance_error_name(graph_id: &str) -> OsString {
    format!("graph-{graph_id}.advance-error").into()
}

fn checkpoint_file_name(graph_id: &str) -> OsString {
    format!("graph-{graph_id}.json").into()
}

pub(super) fn graph_directory(root: &StateRoot, project: ProjectId) -> Result<Directory, FsError> {
    let root = Directory::open_anchor(root.as_path())?;
    let fabric = root.open_verified_directory(OsStr::new("fabric"))?;
    let graphs = fabric.open_verified_directory(OsStr::new("graphs"))?;
    graphs.open_verified_directory(OsStr::new(&project.to_string()))
}

pub(super) fn load_checkpoint(
    directory: &Directory,
    graph_id: &str,
) -> Result<Option<StoredCheckpoint>, JournalError> {
    (|| {
        read_checkpoint_bytes(directory, graph_id)?
            .map(|bytes| decode_checkpoint_in(directory, &bytes, graph_id))
            .transpose()
    })()
    .map_err(|error: JournalError| error.for_graph(graph_id))
}

/// Decode a checkpoint whose prompts may be held in the prompt store.
pub(super) fn decode_checkpoint_in(
    directory: &Directory,
    bytes: &[u8],
    graph_id: &str,
) -> Result<StoredCheckpoint, JournalError> {
    let bytes = prompt_store::rehydrate(directory, graph_id, bytes)?;
    decode_checkpoint_limited(&bytes, graph_id, bytes.len() as u64)
}

pub(super) fn read_checkpoint_bytes(
    directory: &Directory,
    graph_id: &str,
) -> Result<Option<Vec<u8>>, JournalError> {
    #[cfg(test)]
    CHECKPOINT_READS.with(|reads| reads.set(reads.get() + 1));
    #[cfg(feature = "fixture")]
    READ_GRAPH_IDS.with(|ids| ids.borrow_mut().push(graph_id.to_owned()));
    let file = match directory.open_read(OsStr::new(&format!("graph-{graph_id}.json"))) {
        Ok(file) => file,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = Vec::new();
    let mut reader = file.take(MAX_CHECKPOINT_BYTES + 1);
    reader.read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_CHECKPOINT_BYTES {
        return Err(JournalError::TooLarge);
    }
    Ok(Some(bytes))
}

#[cfg(test)]
pub(super) fn decode_checkpoint(
    bytes: &[u8],
    graph_id: &str,
) -> Result<StoredCheckpoint, JournalError> {
    decode_checkpoint_limited(bytes, graph_id, MAX_CHECKPOINT_BYTES)
}

fn replay_limits() -> crate::config::GraphLimits {
    crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        fan_out_width: usize::MAX,
        expansion_depth: usize::MAX,
        ..Default::default()
    }
}

fn decode_checkpoint_limited(
    bytes: &[u8],
    graph_id: &str,
    max_bytes: u64,
) -> Result<StoredCheckpoint, JournalError> {
    let stored = decode_verified_checkpoint(bytes, graph_id, max_bytes.max(MAX_CHECKPOINT_BYTES))?;
    let limits = replay_limits();
    let (base, _) = validate_replay_plan(&stored.execution.document, &limits)
        .map_err(|_| JournalError::InvalidTopology)?;
    let plan_digest = plan_digest(&base)?;
    if stored.checkpoint.plan_digest != plan_digest.to_string() {
        return Err(JournalError::BadDigest);
    }
    plans::validate_persisted_topology(
        &stored.execution.document,
        &stored.checkpoint.extensions,
        &stored.checkpoint.expansions,
        &limits,
    )
    .map_err(|_| JournalError::InvalidTopology)?;
    Ok(stored)
}

fn decode_verified_checkpoint(
    bytes: &[u8],
    graph_id: &str,
    max_bytes: u64,
) -> Result<StoredCheckpoint, JournalError> {
    let stored: StoredCheckpoint = provenant_records::decode(
        bytes,
        Limits {
            max_bytes,
            max_depth: 64,
            max_members: MAX_CHECKPOINT_MEMBERS,
        },
    )?;
    if stored.schema_version != CHECKPOINT_SCHEMA_VERSION {
        return Err(DecodeError::UnsupportedVersion {
            found: stored.schema_version,
            line: 1,
            column: 1,
        }
        .into());
    }
    for (found, supported) in [
        (stored.checkpoint.schema_version, 2),
        (stored.execution.document.schema_version, 2),
    ] {
        if found > supported {
            return Err(DecodeError::UnsupportedVersion {
                found,
                line: 1,
                column: 1,
            }
            .into());
        }
    }
    if stored.checkpoint.schema_version != 2 {
        return Err(JournalError::BadEnvelope);
    }
    let document = &stored.execution.document;
    let has_v2 = document_requires_review_fields(document)
        || stored
            .execution
            .bases
            .is_some_and(|bases| bases.review.is_some())
        || stored
            .checkpoint
            .expansions
            .iter()
            .any(|expansion| nodes_have_v2(&expansion.nodes));
    // Matches validate_schema: version 2 may omit every v2 field.
    if !matches!(document.schema_version, 1 | 2)
        || document.schema_version < u64::from(minimum_schema_version(document))
        || document.schema_version == 1 && has_v2
    {
        return Err(JournalError::BadEnvelope);
    }
    let digest = checkpoint_digest(&stored.checkpoint, &stored.execution)?;
    if digest.to_string() != stored.digest {
        return Err(JournalError::BadDigest);
    }
    if !stored.execution.valid_bases() {
        return Err(JournalError::BadEnvelope);
    }
    if stored.checkpoint.graph_id != graph_id
        || stored.execution.document.graph_id != graph_id
        || !stored.execution.directory.is_absolute()
    {
        return Err(JournalError::GraphMismatch);
    }

    let mut stored = stored;
    stored.checkpoint.bases = stored.execution.bases;
    Ok(stored)
}

#[derive(Serialize, Deserialize)]
// CHECKPOINT_SCHEMA_VERSION owns stored layout changes; older records are rejected.
pub(super) struct StoredCheckpoint {
    /// Envelope extensions sit outside the checkpoint digest.
    #[serde(flatten)]
    pub extra: provenant_records::ExtraFields,
    pub(super) schema_version: u64,
    pub(super) digest: String,
    pub(super) checkpoint: GraphCheckpoint,
    pub(super) execution: GraphExecution,
}

/// Review settlements and graph-specific sources that could not be read.
#[derive(Debug, Default)]
pub struct LandingReviews {
    pub reviews: Vec<ReviewSettlement>,
    pub unusable: Vec<String>,
}

/// Read typed Provenant review settlements covering exactly `head`.
/// The graph settlement writes this projection; the query never reads verdict prose.
pub fn landing_reviews(
    root: &StateRoot,
    project: ProjectId,
    head: provenant_core::GitOid,
) -> Result<LandingReviews, JournalError> {
    let mut result = LandingReviews::default();
    let ids = match FileJournal::graph_ids(root, project) {
        Ok(ids) => ids,
        Err(error) => {
            result.unusable.push(format!(
                "project {project}: cannot enumerate graph review sources: {error}"
            ));
            return Ok(result);
        }
    };
    for id in ids {
        let checkpoint = match FileJournal::read_checkpoint(root, project, &id) {
            Ok(Some(checkpoint)) => checkpoint,
            Ok(None) => continue,
            Err(error) => {
                result.unusable.push(error.for_graph(&id).to_string());
                continue;
            }
        };
        for node in &checkpoint.nodes {
            if node.state == NodeState::Succeeded
                && let Some(review) = node
                    .review_settlement
                    .as_ref()
                    .filter(|review| review.head == head)
            {
                result.reviews.push(review.clone());
            }
        }
    }
    Ok(result)
}

pub(super) fn open_or_create(directory: &Directory, name: &OsStr) -> Result<Directory, FsError> {
    match directory.open_verified_directory(name) {
        Ok(child) => Ok(child),
        Err(error) if error.is_not_found() => {
            match directory.mkdir(name, 0o700) {
                Ok(()) => directory.sync()?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(error),
            }
            directory.open_verified_directory(name)
        }
        Err(error) => Err(error),
    }
}
