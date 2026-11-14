use super::*;

/// Stored execution/checkpoint contract; older duplicate-pin records are rejected.
pub(super) const CHECKPOINT_SCHEMA_VERSION: u64 = 2;

#[path = "events.rs"]
mod events;
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
        matches!(
            self,
            Self::Decode(_)
                | Self::BadEnvelope
                | Self::BadDigest
                | Self::Canonical(_)
                | Self::InvalidTopology
        )
    }

    /// Stable identifier independent of the displayed explanation.
    pub fn code(&self) -> &'static str {
        match self {
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
        super::cancellation_intent::record(root, project, target, budget, cancel, clock)
    }

    /// Whether one particular stop request still awaits graph-lock acknowledgement.
    pub fn cancellation_request_pending(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
        request_id: &str,
    ) -> Result<bool, JournalError> {
        super::cancellation_intent::pending(root, project, graph_id, request_id)
    }

    /// Read complete event records in order, ignoring a torn final record.
    pub fn read_events(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Vec<GraphEvent>, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        match graph_directory(root, project) {
            Ok(directory) => events::read(&directory, graph_id),
            Err(error) if error.is_not_found() => Ok(Vec::new()),
            Err(error) => Err(error.into()),
        }
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
                    if error.is_unreadable() && Self::unreadable_settled(root, project, &id)? {
                        continue;
                    }
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
            directory: graphs_dir,
            execution,
            _lock: lock,
        })
    }

    /// Try the graph lock once. Stop uses this after writing its independent
    /// intent so it can return a pending receipt while a launch owns the lock.
    pub fn try_open(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Self, JournalError> {
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
            directory: graphs_dir,
            execution,
            _lock: lock,
        })
    }

    pub fn bind(&mut self, execution: GraphExecution) -> Result<(), JournalError> {
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
                Err(JournalError::Filesystem(FsError::LockTimeout { .. })) => continue,
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
            let metadata = journal.directory.metadata_at(&name, false)?;
            if metadata.modified_ns >= before_ns {
                continue;
            }
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
            removed += 1;
        }
        Ok(removed)
    }

    fn rebuild_pending_in(
        root: &StateRoot,
        project: ProjectId,
        directory: &Directory,
        markers: &Directory,
    ) -> Result<(), JournalError> {
        for id in Self::graph_ids(root, project)? {
            match load_checkpoint(directory, &id) {
                Ok(Some(stored)) if stored_pending(&stored) => write_pending_marker(markers, &id)?,
                // Leave load failures for the individual graph tick to report.
                Err(error)
                    if error.is_unreadable() && Self::unreadable_settled(root, project, &id)? => {}
                Err(_) => write_pending_marker(markers, &id)?,
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
        if load_checkpoint(&self.directory, &self.graph_id)?
            .as_ref()
            .is_none_or(|stored| !stored_pending(stored))
        {
            let markers = open_or_create(&self.directory, OsStr::new("pending"))?;
            remove_pending_marker(&markers, &self.graph_id)?;
        }
        Ok(())
    }

    /// The execution bound to a graph in any state, without taking the lock.
    pub fn execution(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphExecution>, JournalError> {
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
    }

    pub fn pending_execution(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphExecution>, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let directory = graph_directory(root, project)?;
        Ok(load_checkpoint(&directory, graph_id)?
            .and_then(|stored| stored_pending(&stored).then_some(stored.execution)))
    }

    /// Read an atomic checkpoint snapshot without creating directories or
    /// acquiring the writer lock. Concurrent saves publish whole files.
    pub fn read_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
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
    }

    /// Read a status snapshot with minima recovered from its retained plan.
    pub fn read_status_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
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
        Ok(Some(stored.checkpoint))
    }

    /// Read a checkpoint with compacted findings expanded for reporting.
    pub fn read_materialised_checkpoint(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, JournalError> {
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
    }

    /// Report each unreadable byte snapshot once, independently of advance errors.
    pub fn read_checkpoint_for_tick(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<GraphCheckpoint>, Box<dyn Error>> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId.into());
        }
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let bytes = match read_checkpoint_bytes(&directory, graph_id)? {
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
                if checkpoint_json_settled(&bytes) {
                    return Ok(None);
                }
                let digest = CanonicalV1::file_digest(&bytes).to_string();
                let name = unreadable_marker_name(graph_id);
                let mut previous = String::new();
                if let Ok(mut file) = directory.open_read(&name) {
                    let _ = file.by_ref().take(72).read_to_string(&mut previous);
                }
                if previous == digest {
                    return Ok(None);
                }
                let _ = directory.atomic_write(
                    &name,
                    ".graph-unreadable.",
                    digest.as_bytes(),
                    PublishMode::Replace,
                );
                Err(UnreadableCheckpointNotice(graph_id.to_owned()).into())
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Check only the lifecycle fields of an otherwise unreadable checkpoint.
    pub fn unreadable_settled(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<bool, JournalError> {
        if !valid_identifier(graph_id) {
            return Err(JournalError::InvalidGraphId);
        }
        let directory = match graph_directory(root, project) {
            Ok(directory) => directory,
            Err(error) if error.is_not_found() => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        Ok(read_checkpoint_bytes(&directory, graph_id)?
            .is_some_and(|bytes| checkpoint_json_settled(&bytes)))
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
            return Err(JournalError::InvalidGraphId.into());
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
                let bytes = read_checkpoint_bytes(&directory, graph_id)?;
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
            Err(error) => Err(error.into()),
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
    }

    /// The recorded failure of the last automatic advance, if any.
    pub fn read_advance_error(
        root: &StateRoot,
        project: ProjectId,
        graph_id: &str,
    ) -> Result<Option<String>, JournalError> {
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
            .and_then(|execution| execution.document.extra.get("peer_origin_work_key"))
        {
            let origin: provenant_records::WorkKey =
                serde_json::from_value(origin.clone()).map_err(|_| JournalError::BadEnvelope)?;
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
        if graph_id != self.graph_id {
            return Err(JournalError::GraphMismatch);
        }
        Ok(load_checkpoint(&self.directory, graph_id)?.map(|stored| stored.checkpoint))
    }

    fn apply_cancellation(
        &mut self,
        document: &GraphDocument,
        checkpoint: &mut GraphCheckpoint,
    ) -> Result<bool, Self::Error> {
        super::cancellation_intent::acknowledge(self, document, checkpoint)
    }

    fn save(&mut self, checkpoint: &GraphCheckpoint) -> Result<(), Self::Error> {
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
            digest: CanonicalV1::record_digest(
                "fabric-graph-checkpoint",
                2,
                &(checkpoint, execution),
            )?
            .to_string(),
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
        Ok(())
    }
}

fn stored_pending(stored: &StoredCheckpoint) -> bool {
    let checkpoint = &stored.checkpoint;
    next_step::pending(checkpoint)
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

fn checkpoint_json_settled(bytes: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let Some(checkpoint) = value
        .get("checkpoint")
        .and_then(serde_json::Value::as_object)
    else {
        return false;
    };
    if checkpoint.get("cancelled") == Some(&serde_json::Value::Bool(true)) {
        return true;
    }
    if checkpoint
        .get("cancelled")
        .is_some_and(|value| value != false)
    {
        return false;
    }
    let Some(nodes) = checkpoint
        .get("nodes")
        .and_then(serde_json::Value::as_array)
    else {
        return false;
    };
    let terminal = nodes.iter().all(|node| {
        matches!(
            node.get("state").and_then(serde_json::Value::as_str),
            Some("succeeded" | "failed" | "blocked" | "skipped")
        )
    });
    // A link means an inbox request that is still open or unread.
    let asks_closed = match checkpoint.get("asks") {
        None => true,
        Some(asks) => asks.as_array().is_some_and(Vec::is_empty),
    };
    if !terminal || !asks_closed {
        return false;
    }
    let pr_delivery_pending = value
        .pointer("/execution/document/delivery")
        .and_then(serde_json::Value::as_str)
        == Some("pr")
        && checkpoint.get("delivered") != Some(&serde_json::Value::Bool(true));
    !pr_delivery_pending
}

pub(super) fn load_checkpoint(
    directory: &Directory,
    graph_id: &str,
) -> Result<Option<StoredCheckpoint>, JournalError> {
    read_checkpoint_bytes(directory, graph_id)?
        .map(|bytes| decode_checkpoint_in(directory, &bytes, graph_id))
        .transpose()
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

fn decode_checkpoint_limited(
    bytes: &[u8],
    graph_id: &str,
    max_bytes: u64,
) -> Result<StoredCheckpoint, JournalError> {
    let stored = decode_verified_checkpoint(bytes, graph_id, max_bytes.max(MAX_CHECKPOINT_BYTES))?;
    let limits = crate::config::GraphLimits {
        nodes: usize::MAX,
        edges: usize::MAX,
        routes_per_node: u16::MAX as usize,
        node_prompt_bytes: usize::MAX,
        graph_prompt_bytes: usize::MAX,
        fan_out_width: usize::MAX,
        expansion_depth: usize::MAX,
        ..Default::default()
    };
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
    let digest = CanonicalV1::record_digest(
        "fabric-graph-checkpoint",
        2,
        &(&stored.checkpoint, &stored.execution),
    )?;
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

/// Read typed Provenant review settlements covering exactly `head`.
/// The graph settlement writes this projection; the query never reads verdict prose.
pub fn landing_reviews(
    root: &StateRoot,
    project: ProjectId,
    head: provenant_core::GitOid,
) -> Result<Vec<ReviewSettlement>, JournalError> {
    let mut reviews = Vec::new();
    for id in FileJournal::graph_ids(root, project)? {
        let Some(checkpoint) = FileJournal::read_checkpoint(root, project, &id)? else {
            continue;
        };
        for node in &checkpoint.nodes {
            if node.state == NodeState::Succeeded
                && let Some(review) = node
                    .review_settlement
                    .as_ref()
                    .filter(|review| review.head == head)
            {
                reviews.push(review.clone());
            }
        }
    }
    Ok(reviews)
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
