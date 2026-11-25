//! Host-wide durable ownership for delivery queue items.
//!
//! A queue reservation is written before any item graph is launched. Pending
//! and launching rows remain owners across restart; only an explicit terminal
//! graph observation releases an identity for a later submission.

use provenant_core::{CancelToken, Clock, WaitBudget, WallTime};
use provenant_os::fs::{Directory, LockMode, PublishMode};
use provenant_records::Sourced;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::{BTreeMap, BTreeSet}, ffi::OsStr, path::Path};

const REGISTRY_FILE: &str = "registry.json";
const REGISTRY_LOCK: &str = "registry.lock";
const REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
const REGISTRY_SCHEMA: u64 = 1;

/// Canonical tracker identity. The selector resolver owns tracker-specific
/// item canonicalisation; project and tracker names are folded here.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct QueueIdentity {
    pub project: String,
    pub tracker: String,
    pub item: String,
}

impl QueueIdentity {
    pub(super) fn new(project: &str, tracker: &str, item: &str) -> Result<Self, String> {
        let project = project.trim().to_ascii_lowercase();
        let tracker = tracker.trim().to_ascii_lowercase();
        let item = item.trim().to_owned();
        if [project.as_str(), tracker.as_str(), item.as_str()]
            .iter()
            .any(|part| part.is_empty() || part.contains('\0'))
        {
            return Err("queue identity components must be non-empty text".into());
        }
        Ok(Self { project, tracker, item })
    }
}

/// One tracker result retained from the resolution of a selector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct QueueSnapshotItem {
    pub identity: QueueIdentity,
    /// Bounded tracker projection captured before graph admission.
    pub data: Value,
}

/// Immutable resolution of a tracker selector. Later tracker changes do not
/// alter this queue's worklist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct QueueSnapshot {
    pub selector: String,
    pub source: String,
    pub resolved_at: WallTime,
    pub items: Vec<QueueSnapshotItem>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum QueueItemState {
    Pending,
    Launching,
    Active,
    Terminal,
}

/// One ordered item graph in a durable queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct QueueItem {
    pub identity: QueueIdentity,
    pub graph_id: String,
    pub snapshot_index: usize,
    /// Canonical replay input with wait/explain/detached controls removed.
    pub launch_request: Value,
    /// Caller checkout used to rebuild the launch context after restart.
    pub execution_directory: String,
    /// Sealed, validated custody for an explicitly adopted pull request.
    pub adoption: Option<Value>,
    /// Digest of the canonical replay input and execution directory.
    pub launch_digest: String,
    pub state: QueueItemState,
    pub terminal_outcome: Option<String>,
    pub overflow_reason: Option<String>,
}

/// A persisted queue and its ordered item graphs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct QueueRecord {
    pub queue_id: String,
    /// Stable digest of user-selected inputs, computed before tracker reads.
    pub submission_digest: String,
    pub project: String,
    pub tracker: String,
    pub snapshot: QueueSnapshot,
    pub max_parallel: Sourced<usize>,
    pub admitted_at: WallTime,
    /// Emit the advisory warning at this deadline while items wait at cap.
    pub warning_at: WallTime,
    /// The cap is advisory after this persisted deadline.
    pub overflow_at: WallTime,
    pub wait_warning: Option<String>,
    pub overflow_reason: Option<String>,
    pub items: Vec<QueueItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Registry {
    schema_version: u64,
    generation: u64,
    queues: BTreeMap<String, QueueRecord>,
}

impl Default for Registry {
    fn default() -> Self {
        Self { schema_version: REGISTRY_SCHEMA, generation: 0, queues: BTreeMap::new() }
    }
}

#[derive(Clone, Debug)]
pub(super) struct QueueItemSpec {
    pub identity: QueueIdentity,
    pub graph_id: String,
    pub launch_request: Value,
    pub execution_directory: String,
    pub adoption: Option<Value>,
}

#[derive(Clone, Debug)]
pub(super) struct QueueSubmission {
    pub queue_id: String,
    pub submission_digest: String,
    pub project: String,
    pub tracker: String,
    pub snapshot: QueueSnapshot,
    pub max_parallel: Sourced<usize>,
    pub admitted_at: WallTime,
    pub warning_at: WallTime,
    pub overflow_at: WallTime,
    pub items: Vec<QueueItemSpec>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExistingOwner {
    pub identity: QueueIdentity,
    pub queue_id: String,
    pub graph_id: String,
    pub state: QueueItemState,
}

#[derive(Clone, Debug)]
pub(super) enum QueueAdmission {
    Admitted { queue: QueueRecord, replayed: bool },
    Owned(Vec<ExistingOwner>),
}

#[derive(Clone, Debug, Default)]
pub(super) struct QueueSchedule {
    pub launch: Vec<(String, QueueItem)>,
    pub warnings: Vec<String>,
    pub waiting: Vec<String>,
}

/// Serialises all queue ownership decisions in the host state domain.
pub(super) struct QueueRegistry {
    directory: Directory,
}

impl QueueRegistry {
    pub(super) fn open(state: &Path) -> Result<Self, String> {
        let path = state.join("fabric/delivery-queue");
        let directory = provenant_config::private_directory(&path, true)
            .map_err(|error| format!("delivery queue state unavailable: {error}"))?;
        Ok(Self { directory })
    }

    fn locked<T>(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
        operation: impl FnOnce(&mut Registry) -> Result<T, String>,
    ) -> Result<T, String> {
        let _guard = self.directory
            .lock(OsStr::new(REGISTRY_LOCK), LockMode::Exclusive, budget, cancel, clock)
            .map_err(|error| format!("delivery queue registry lock failed: {error}"))?;
        let mut registry = match self.directory.read_bounded(OsStr::new(REGISTRY_FILE), REGISTRY_BYTES) {
            Ok(bytes) => serde_json::from_slice::<Registry>(&bytes)
                .map_err(|error| format!("delivery queue registry is corrupt: {error}"))?,
            Err(error) if error.is_not_found() => Registry::default(),
            Err(error) => return Err(format!("delivery queue registry read failed: {error}")),
        };
        if registry.schema_version != REGISTRY_SCHEMA {
            return Err(format!("unsupported delivery queue registry schema {}", registry.schema_version));
        }
        let before = registry.clone();
        let result = operation(&mut registry)?;
        if registry == before {
            return Ok(result);
        }
        registry.generation = registry.generation.checked_add(1)
            .ok_or_else(|| "delivery queue registry generation exhausted".to_owned())?;
        let encoded = serde_json::to_vec(&registry)
            .map_err(|error| format!("delivery queue registry encode failed: {error}"))?;
        if encoded.len() as u64 > REGISTRY_BYTES {
            return Err("delivery queue registry reached its storage bound".into());
        }
        self.directory.atomic_write(
            OsStr::new(REGISTRY_FILE),
            "delivery-queue-registry",
            &encoded,
            PublishMode::Replace,
        ).and_then(|publication| publication.require_durable())
            .map_err(|error| format!("delivery queue registry write failed: {error}"))?;
        Ok(result)
    }

    /// Look up a retried operation before resolving its selector again. The
    /// operation digest is bound before network reads; a match returns the
    /// original tracker snapshot and graph IDs even if the tracker has changed.
    pub(super) fn find_operation(
        &self,
        queue_id: &str,
        submission_digest: &str,
        project: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Option<QueueRecord>, String> {
        self.locked(budget, cancel, clock, |registry| {
            let Some(existing) = registry.queues.get(queue_id) else { return Ok(None); };
            if existing.project == project.to_ascii_lowercase()
                && existing.submission_digest == submission_digest
            {
                return Ok(Some(existing.clone()));
            }
            Err(format!("delivery queue id `{queue_id}` already names different input"))
        })
    }

    /// Atomically reserve every selected identity before any item graph starts.
    /// A queue id replay is accepted only when it carries the exact same record.
    pub(super) fn admit(
        &self,
        mut submission: QueueSubmission,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueAdmission, String> {
        sanitize_submission(&mut submission)?;
        validate_submission(&submission)?;
        self.locked(budget, cancel, clock, |registry| {
            let items: Vec<QueueItem> = submission.items.iter().enumerate().map(|(index, item)| QueueItem {
                identity: item.identity.clone(),
                graph_id: item.graph_id.clone(),
                snapshot_index: index,
                launch_request: item.launch_request.clone(),
                execution_directory: item.execution_directory.clone(),
                adoption: item.adoption.clone(),
                launch_digest: launch_digest(&item.launch_request, &item.execution_directory, &item.adoption),
                state: QueueItemState::Pending,
                terminal_outcome: None,
                overflow_reason: None,
            }).collect();
            let queue = QueueRecord {
                queue_id: submission.queue_id.clone(),
                submission_digest: submission.submission_digest.clone(),
                project: submission.project.clone(),
                tracker: submission.tracker.clone(),
                snapshot: submission.snapshot.clone(),
                max_parallel: submission.max_parallel.clone(),
                admitted_at: submission.admitted_at,
                warning_at: submission.warning_at,
                overflow_at: submission.overflow_at,
                wait_warning: None,
                overflow_reason: None,
                items,
            };
            if let Some(existing) = registry.queues.get(&submission.queue_id) {
                if same_submission(existing, &queue) {
                    return Ok(QueueAdmission::Admitted { queue: existing.clone(), replayed: true });
                }
                return Err(format!("delivery queue id `{}` already names different input", submission.queue_id));
            }
            for item in &queue.items {
                if registry.queues.values().any(|existing| {
                    existing.project == queue.project
                        && existing.items.iter().any(|prior| prior.graph_id == item.graph_id)
                }) {
                    return Err(format!("queue item graph id `{}` is already reserved", item.graph_id));
                }
            }
            let selected: BTreeSet<_> = queue.items.iter().map(|item| item.identity.clone()).collect();
            let owners: Vec<_> = registry.queues.values().flat_map(|current| {
                current.items.iter().filter(|item| {
                    selected.contains(&item.identity) && item.state != QueueItemState::Terminal
                }).map(move |item| ExistingOwner {
                    identity: item.identity.clone(),
                    queue_id: current.queue_id.clone(),
                    graph_id: item.graph_id.clone(),
                    state: item.state,
                })
            }).collect();
            if !owners.is_empty() {
                return Ok(QueueAdmission::Owned(owners));
            }
            registry.queues.insert(queue.queue_id.clone(), queue.clone());
            Ok(QueueAdmission::Admitted { queue, replayed: false })
        })
    }

    /// Claim ordered pending item graphs under the queue's concurrency target.
    /// Saturated queues warn before `overflow_at`; after it, the cap is advisory
    /// and every waiting item is returned with a persisted overflow reason.
    pub(super) fn claim_runnable(
        &self,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueSchedule, String> {
        self.claim_runnable_for(None, now, budget, cancel, clock)
    }

    /// Claim one queue immediately after atomic submission without marking
    /// unrelated queues as launched before their owning project can run them.
    pub(super) fn claim_queue(
        &self,
        queue_id: &str,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueSchedule, String> {
        self.claim_runnable_for(Some(queue_id), now, budget, cancel, clock)
    }

    fn claim_runnable_for(
        &self,
        only_queue: Option<&str>,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueSchedule, String> {
        self.locked(budget, cancel, clock, |registry| {
            let mut schedule = QueueSchedule::default();
            for (queue_id, queue) in &mut registry.queues {
                if only_queue.is_some_and(|only| only != queue_id) { continue; }
                let active = queue.items.iter().filter(|item| matches!(item.state, QueueItemState::Launching | QueueItemState::Active)).count();
                let pending = queue.items.iter().filter(|item| item.state == QueueItemState::Pending).count();
                if pending > 0 && active >= queue.max_parallel.value {
                    if now < queue.overflow_at {
                        if now >= queue.warning_at && queue.wait_warning.is_none() {
                            let warning = format!(
                                "queue {} is waiting at max_parallel={} from {} ({}) before advisory overflow at {}",
                                queue.queue_id,
                                queue.max_parallel.value,
                                queue.max_parallel.source.layer.as_str(),
                                queue.max_parallel.source.origin.as_deref().unwrap_or("no origin recorded"),
                                queue.overflow_at,
                            );
                            queue.wait_warning = Some(warning.clone());
                            schedule.warnings.push(warning);
                        }
                        schedule.waiting.push(format!(
                            "queue {} has {pending} pending items waiting for active capacity {}/{}",
                            queue.queue_id, active, queue.max_parallel.value
                        ));
                        schedule.launch.extend(queue.items.iter().filter(|item| item.state == QueueItemState::Launching).cloned().map(|item| (queue_id.clone(), item)));
                        continue;
                    }
                    if queue.overflow_reason.is_none() {
                        queue.overflow_reason = Some(format!(
                            "max_parallel={} from {} ({}) exceeded after bounded wait; advisory overflow began at {}",
                            queue.max_parallel.value,
                            queue.max_parallel.source.layer.as_str(),
                            queue.max_parallel.source.origin.as_deref().unwrap_or("no origin recorded"),
                            queue.overflow_at,
                        ));
                    }
                }
                let capacity = queue.max_parallel.value.saturating_sub(active);
                let overflow = active >= queue.max_parallel.value;
                let mut claimed = 0;
                for item in queue.items.iter_mut().filter(|item| item.state == QueueItemState::Pending) {
                    if !overflow && claimed >= capacity {
                        break;
                    }
                    item.state = QueueItemState::Launching;
                    if overflow {
                        item.overflow_reason = queue.overflow_reason.clone();
                    }
                    claimed += 1;
                }
                schedule.launch.extend(queue.items.iter().filter(|item| item.state == QueueItemState::Launching).cloned().map(|item| (queue_id.clone(), item)));
            }
            Ok(schedule)
        })
    }

    /// Record that a persisted graph has been accepted for execution.
    pub(super) fn mark_active(
        &self,
        queue_id: &str,
        identity: &QueueIdentity,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        self.update_item(queue_id, identity, graph_id, budget, cancel, clock, |item| {
            if item.state == QueueItemState::Terminal { return Err("terminal queue item cannot become active".into()); }
            if item.state == QueueItemState::Pending { return Err("queue item must be claimed before launch acceptance".into()); }
            item.state = QueueItemState::Active;
            Ok(())
        })
    }

    /// Requeue a launch only after its caller proves no graph was persisted.
    pub(super) fn release_unlaunched(
        &self,
        queue_id: &str,
        identity: &QueueIdentity,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        self.update_item(queue_id, identity, graph_id, budget, cancel, clock, |item| {
            if item.state == QueueItemState::Active || item.state == QueueItemState::Terminal {
                return Err("only an unaccepted queue item can be requeued".into());
            }
            item.state = QueueItemState::Pending;
            Ok(())
        })
    }

    /// Terminal settlement must come from a conclusive graph observation.
    pub(super) fn mark_terminal(
        &self,
        queue_id: &str,
        identity: &QueueIdentity,
        graph_id: &str,
        outcome: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        if outcome.trim().is_empty() { return Err("terminal outcome must be non-empty".into()); }
        self.update_item(queue_id, identity, graph_id, budget, cancel, clock, |item| {
            if item.state == QueueItemState::Pending {
                return Err("pending queue item has no graph to settle".into());
            }
            if item.state == QueueItemState::Terminal {
                if item.terminal_outcome.as_deref() == Some(outcome) { return Ok(()); }
                return Err("terminal queue outcome is already recorded".into());
            }
            item.state = QueueItemState::Terminal;
            item.terminal_outcome = Some(outcome.to_owned());
            Ok(())
        })
    }

    pub(super) fn queues(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Vec<QueueRecord>, String> {
        self.locked(budget, cancel, clock, |registry| Ok(registry.queues.values().cloned().collect()))
    }

    fn update_item(
        &self,
        queue_id: &str,
        identity: &QueueIdentity,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
        update: impl FnOnce(&mut QueueItem) -> Result<(), String>,
    ) -> Result<(), String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry.queues.get_mut(queue_id).ok_or_else(|| format!("unknown queue `{queue_id}`"))?;
            let item = queue.items.iter_mut().find(|item| &item.identity == identity && item.graph_id == graph_id)
                .ok_or_else(|| "queue item identity or graph id changed".to_owned())?;
            update(item)
        })
    }
}

fn validate_submission(submission: &QueueSubmission) -> Result<(), String> {
    if submission.queue_id.trim().is_empty() || submission.queue_id.contains('\0') {
        return Err("queue id must be non-empty text".into());
    }
    if submission.max_parallel.value == 0 {
        return Err("queue max_parallel must be positive".into());
    }
    if !submission.submission_digest.starts_with("sha256:") {
        return Err("queue submission digest must be a canonical sha256 digest".into());
    }
    if submission.warning_at > submission.overflow_at {
        return Err("queue warning deadline must not follow its overflow deadline".into());
    }
    if submission.project.trim().is_empty() || submission.tracker.trim().is_empty() {
        return Err("queue project and tracker must be non-empty".into());
    }
    if submission.items.is_empty() || submission.items.len() != submission.snapshot.items.len() {
        return Err("queue snapshot and item graph count must match and be non-empty".into());
    }
    if submission.snapshot.selector.trim().is_empty() || submission.snapshot.source.trim().is_empty() {
        return Err("queue tracker snapshot must record its selector and source".into());
    }
    let mut identities = BTreeSet::new();
    let mut graph_ids = BTreeSet::new();
    for (index, (item, snapshot)) in submission.items.iter().zip(&submission.snapshot.items).enumerate() {
        if item.identity != snapshot.identity {
            return Err(format!("queue snapshot identity at position {index} does not match its item graph"));
        }
        if item.identity.project != submission.project.to_ascii_lowercase()
            || item.identity.tracker != submission.tracker.to_ascii_lowercase()
            || [item.identity.project.as_str(), item.identity.tracker.as_str(), item.identity.item.as_str()]
                .iter().any(|part| part.trim().is_empty() || part.contains('\0'))
        {
            return Err("queue item identity does not match its project and tracker".into());
        }
        if !identities.insert(item.identity.clone()) {
            return Err("queue submission contains a duplicate tracker identity".into());
        }
        if item.graph_id.trim().is_empty() || item.graph_id.contains('\0') || !graph_ids.insert(&item.graph_id) {
            return Err("queue item graph ids must be non-empty and unique".into());
        }
        let canonical_graph_id = provenant_fabric_runtime::graph::template::fold_graph_name(&item.graph_id)
            .map_err(|_| "queue item graph id is invalid".to_owned())?;
        if canonical_graph_id != item.graph_id {
            return Err("queue item graph id must already be canonical".into());
        }
        if item.execution_directory.trim().is_empty() || item.execution_directory.contains('\0') {
            return Err("queue item execution directory must be non-empty".into());
        }
        validate_launch_request(&item.launch_request)?;
        validate_launch_request(&snapshot.data)?;
        if let Some(adoption) = &item.adoption {
            validate_launch_request(adoption)?;
        }
    }
    Ok(())
}

fn same_submission(existing: &QueueRecord, proposed: &QueueRecord) -> bool {
    existing.queue_id == proposed.queue_id
        && existing.submission_digest == proposed.submission_digest
        && existing.project == proposed.project
        && existing.tracker == proposed.tracker
}

fn launch_digest(request: &Value, execution_directory: &str, adoption: &Option<Value>) -> String {
    let bytes = serde_json::to_vec(&(request, execution_directory, adoption)).unwrap_or_default();
    provenant_records::CanonicalV1::file_digest(&bytes).to_string()
}

fn sanitize_submission(submission: &mut QueueSubmission) -> Result<(), String> {
    submission.snapshot.selector = sanitize_persisted_text(&submission.snapshot.selector)?;
    submission.snapshot.source = sanitize_persisted_text(&submission.snapshot.source)?;
    for item in &mut submission.items {
        item.launch_request = sanitize_persisted_value(&item.launch_request)?;
        item.adoption = item.adoption.as_ref().map(sanitize_persisted_value).transpose()?;
    }
    for item in &mut submission.snapshot.items {
        item.data = sanitize_persisted_value(&item.data)?;
    }
    Ok(())
}

fn sanitize_persisted_text(value: &str) -> Result<String, String> {
    let safe = provenant_redact::publication_text(value.as_bytes(), [])
        .map_err(|_| "queue input could not be sanitised for durable storage".to_owned())?;
    String::from_utf8(safe)
        .map_err(|_| "queue input sanitisation produced invalid text".to_owned())
}

fn sanitize_persisted_value(value: &Value) -> Result<Value, String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|_| "queue input could not be encoded for sanitisation".to_owned())?;
    let safe = provenant_redact::publication_text(&encoded, [])
        .map_err(|_| "queue input could not be sanitised for durable storage".to_owned())?;
    serde_json::from_slice(&safe)
        .map_err(|_| "queue input sanitisation produced invalid structured data".to_owned())
}

fn validate_launch_request(value: &Value) -> Result<(), String> {
    validate_persistable_object(value)?;
    for text in string_values(value) {
        if provenant_redact::mode().masks_egress() {
            match provenant_redact::matches_credential_value_for_retry(text.as_bytes()) {
                Ok(true) => return Err("queue launch content matches a configured credential; remove it before queue admission".into()),
                Ok(false) => {}
                Err(_) => return Err("queue launch content could not be checked against configured credentials".into()),
            }
        }
    }
    Ok(())
}

fn validate_persistable_object(value: &Value) -> Result<(), String> {
    fn visit(value: &Value) -> Result<(), String> {
        match value {
            Value::Object(fields) => {
                for (name, child) in fields {
                    let folded = name.to_ascii_lowercase();
                    if matches!(folded.as_str(), "token" | "access_token" | "refresh_token" | "password" | "secret" | "credentials" | "authorization" | "api_key" | "private_key") {
                        return Err(format!("queue launch request contains forbidden secret field `{name}`"));
                    }
                    if matches!(folded.as_str(), "wait_seconds" | "explain" | "detached") {
                        return Err(format!("queue launch request contains ephemeral field `{name}`"));
                    }
                    visit(child)?;
                }
            }
            Value::Array(values) => for child in values { visit(child)?; },
            _ => {}
        }
        Ok(())
    }
    if !value.is_object() {
        return Err("persisted queue input must be an object".into());
    }
    visit(value)
}

fn string_values(value: &Value) -> Vec<&str> {
    fn collect<'a>(value: &'a Value, rows: &mut Vec<&'a str>) {
        match value {
            Value::String(text) => rows.push(text),
            Value::Array(values) => values.iter().for_each(|value| collect(value, rows)),
            Value::Object(fields) => fields.values().for_each(|value| collect(value, rows)),
            _ => {}
        }
    }
    let mut rows = Vec::new();
    collect(value, &mut rows);
    rows
}
