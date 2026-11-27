//! Host-wide durable ownership for delivery queue items.
//!
//! A queue reservation is written before any item graph is launched. Pending
//! and launching rows remain owners across restart; only an explicit terminal
//! graph observation releases an identity for a later submission.

use provenant_core::{CancelToken, Clock, WaitBudget, WallTime};
use provenant_fabric_runtime::graph::QueueKnobs;
use provenant_os::fs::{Directory, LockMode, PublishMode};
use provenant_records::Sourced;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::Path,
};

const REGISTRY_FILE: &str = "registry.json";
const REGISTRY_LOCK: &str = "registry.lock";
const REGISTRY_BYTES: u64 = 16 * 1024 * 1024;
const REGISTRY_SCHEMA: u64 = 2;
const MAX_QUEUE_CAP_WAIT_SECONDS: u64 = 14_400;

/// Canonical tracker identity. The selector resolver owns tracker-specific
/// item canonicalisation; project and tracker names are folded here.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        Ok(Self {
            project,
            tracker,
            item,
        })
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
    /// A persisted graph is paused and still owned, but holds no parallel slot.
    Parked,
    /// A parked graph atomically reacquired a slot and must resume its existing checkpoint.
    Resuming,
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
    /// A deterministic pre-effect refusal, retained while ownership stays active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
    pub overflow_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct QueueItemMarker {
    pub queue_id: String,
    pub identity: QueueIdentity,
    pub graph_id: String,
    pub launch_digest: String,
}

#[derive(Clone, Debug)]
pub(super) struct AuthorisedQueueLaunch {
    pub queue: QueueRecord,
    pub item: QueueItem,
    pub snapshot_item: QueueSnapshotItem,
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
    pub knobs: QueueKnobs,
    pub cap_wait: Sourced<u64>,
    pub admitted_at: WallTime,
    /// Emit the advisory warning at this deadline while items wait at cap.
    pub warning_at: WallTime,
    /// The cap is advisory after this persisted deadline.
    pub overflow_at: WallTime,
    pub wait_warning: Option<String>,
    #[serde(default)]
    pub wait_warning_emitted: bool,
    pub overflow_reason: Option<String>,
    pub items: Vec<QueueItem>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Registry {
    schema_version: u64,
    generation: u64,
    queues: BTreeMap<String, QueueRecord>,
}

#[derive(Deserialize)]
struct RegistryHeader {
    schema_version: u64,
}

impl Default for Registry {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA,
            generation: 0,
            queues: BTreeMap::new(),
        }
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
    pub knobs: QueueKnobs,
    pub cap_wait: Sourced<u64>,
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
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) enum QueueAdmission {
    Admitted {
        queue: Box<QueueRecord>,
        replayed: bool,
    },
    Owned(Vec<ExistingOwner>),
}

#[derive(Clone, Debug, Default)]
pub(super) struct QueueSchedule {
    pub launch: Vec<(String, QueueItem)>,
    pub warnings: Vec<String>,
    pub waiting: Vec<String>,
}

pub(super) fn bounded_wait_deadlines(
    admitted_at: WallTime,
    cap_wait: &Sourced<u64>,
) -> Result<(WallTime, WallTime), String> {
    if !(1..=MAX_QUEUE_CAP_WAIT_SECONDS).contains(&cap_wait.value) {
        return Err("queue cap wait must be 1..=14400 seconds".into());
    }
    let overflow_delta = i64::try_from(cap_wait.value)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000))
        .ok_or("queue overflow wait is out of range")?;
    let warning_delta = i64::try_from(cap_wait.value / 2)
        .ok()
        .and_then(|seconds| seconds.checked_mul(1_000_000))
        .ok_or("queue warning wait is out of range")?;
    let warning = admitted_at
        .as_micros()
        .checked_add(warning_delta)
        .ok_or("queue warning deadline overflow")?;
    let overflow = admitted_at
        .as_micros()
        .checked_add(overflow_delta)
        .ok_or("queue overflow deadline overflow")?;
    Ok((
        WallTime::from_micros(warning).map_err(|_| "queue warning deadline is invalid")?,
        WallTime::from_micros(overflow).map_err(|_| "queue overflow deadline is invalid")?,
    ))
}

/// Directory and lock used for every delivery queue registry operation.
pub(crate) fn registry_lock_path(state: &Path) -> std::path::PathBuf {
    state.join("fabric/delivery-queue").join(REGISTRY_LOCK)
}

/// Serialises all queue ownership decisions in the host state domain.
pub(super) struct QueueRegistry {
    directory: Directory,
}

impl QueueRegistry {
    pub(super) fn open(state: &Path) -> Result<Self, String> {
        let lock = registry_lock_path(state);
        let path = lock.parent().expect("registry lock has a parent");
        let directory = provenant_config::private_directory(path, true)
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
        let _guard = self
            .directory
            .lock(
                OsStr::new(REGISTRY_LOCK),
                LockMode::Exclusive,
                budget,
                cancel,
                clock,
            )
            .map_err(|error| format!("delivery queue registry lock failed: {error}"))?;
        let mut registry = match self
            .directory
            .read_bounded(OsStr::new(REGISTRY_FILE), REGISTRY_BYTES)
        {
            Ok(bytes) => {
                let header = serde_json::from_slice::<RegistryHeader>(&bytes)
                    .map_err(|error| format!("delivery queue registry is corrupt: {error}"))?;
                if header.schema_version != REGISTRY_SCHEMA {
                    return Err(format!(
                        "unsupported delivery queue registry schema {}; expected {REGISTRY_SCHEMA}",
                        header.schema_version
                    ));
                }
                serde_json::from_slice::<Registry>(&bytes)
                    .map_err(|error| format!("delivery queue registry is corrupt: {error}"))?
            }
            Err(error) if error.is_not_found() => Registry::default(),
            Err(error) => return Err(format!("delivery queue registry read failed: {error}")),
        };
        let before = registry.clone();
        let result = operation(&mut registry)?;
        if registry == before {
            return Ok(result);
        }
        registry.generation = registry
            .generation
            .checked_add(1)
            .ok_or_else(|| "delivery queue registry generation exhausted".to_owned())?;
        let encoded = serde_json::to_vec(&registry)
            .map_err(|error| format!("delivery queue registry encode failed: {error}"))?;
        if encoded.len() as u64 > REGISTRY_BYTES {
            return Err("delivery queue registry reached its storage bound".into());
        }
        self.directory
            .atomic_write(
                OsStr::new(REGISTRY_FILE),
                "delivery-queue-registry",
                &encoded,
                PublishMode::Replace,
            )
            .and_then(|publication| publication.require_durable())
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
        if project != project.to_ascii_lowercase() {
            return Err("queue project identity must be canonical lowercase text".into());
        }
        if !is_sha256_digest(submission_digest) {
            return Err("queue submission digest must be a canonical sha256 digest".into());
        }
        self.locked(budget, cancel, clock, |registry| {
            let Some(existing) = registry.queues.get(queue_id) else {
                return Ok(None);
            };
            if existing.project == project && existing.submission_digest == submission_digest {
                return Ok(Some(existing.clone()));
            }
            Err(format!(
                "delivery queue id `{queue_id}` already names different input"
            ))
        })
    }

    /// Verify that a launch marker and replay request name an already reserved
    /// item in this project and execution directory. Raw queue metadata cannot
    /// enable the queue plan by itself.
    pub(super) fn authorise_launch(
        &self,
        project: &str,
        execution_directory: &str,
        request: &Value,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<AuthorisedQueueLaunch, String> {
        let marker: QueueItemMarker = serde_json::from_value(
            request
                .get("queue_item")
                .cloned()
                .ok_or("queue launch marker is missing")?,
        )
        .map_err(|_| "queue launch marker is invalid".to_owned())?;
        let mut replay_request = request.clone();
        let object = replay_request
            .as_object_mut()
            .ok_or("queue launch request must be an object")?;
        object.remove("queue_item");
        for field in ["wait_seconds", "explain", "detached"] {
            object.remove(field);
        }
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get(&marker.queue_id)
                .ok_or_else(|| "queue launch has no persisted owner".to_owned())?;
            if queue.project != project.to_ascii_lowercase()
                || marker.identity.project != queue.project
                || marker.identity.tracker != queue.tracker
                || marker.queue_id.trim().is_empty()
                || !is_sha256_digest(&marker.launch_digest)
            {
                return Err("queue launch does not belong to this project".into());
            }
            let item = queue
                .items
                .iter()
                .find(|item| item.identity == marker.identity && item.graph_id == marker.graph_id)
                .ok_or_else(|| "queue launch item identity or graph id changed".to_owned())?;
            let expected_digest = launch_digest(
                &item.launch_request,
                &item.execution_directory,
                &item.adoption,
            )?;
            if !matches!(
                item.state,
                QueueItemState::Launching | QueueItemState::Active
            ) || item.launch_digest != marker.launch_digest
                || item.execution_directory != execution_directory
                || item.launch_request != replay_request
                || expected_digest != item.launch_digest
            {
                return Err("queue launch does not match its persisted item intent".into());
            }
            let snapshot_item = queue
                .snapshot
                .items
                .get(item.snapshot_index)
                .filter(|snapshot| snapshot.identity == item.identity)
                .ok_or_else(|| "queue snapshot identity is corrupt".to_owned())?;
            Ok(AuthorisedQueueLaunch {
                queue: queue.clone(),
                item: item.clone(),
                snapshot_item: snapshot_item.clone(),
            })
        })
    }

    /// Revalidate an authenticated launch while its graph journal lock is
    /// held. A deterministic-refusal observer may have released the same row
    /// back to Pending after the optimistic authorization above.
    #[allow(
        clippy::too_many_arguments,
        reason = "launch identity and journal custody are checked together"
    )]
    pub(super) fn revalidate_launch(
        &self,
        authorised: &AuthorisedQueueLaunch,
        project: &str,
        execution_directory: &str,
        checkpoint_exists: bool,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get(&authorised.queue.queue_id)
                .ok_or_else(|| {
                    "queue launch owner disappeared before graph admission".to_owned()
                })?;
            if queue.project != project.to_ascii_lowercase()
                || queue.project != authorised.queue.project
                || queue.tracker != authorised.queue.tracker
                || queue.submission_digest != authorised.queue.submission_digest
            {
                return Err("queue launch owner changed before graph admission".into());
            }
            let owners = registry
                .queues
                .values()
                .flat_map(|current| current.items.iter())
                .filter(|item| item.graph_id == authorised.item.graph_id)
                .count();
            if owners != 1 {
                return Err("queue graph no longer has one durable launch owner".into());
            }
            let item = queue
                .items
                .iter()
                .find(|item| {
                    item.identity == authorised.item.identity
                        && item.graph_id == authorised.item.graph_id
                })
                .ok_or_else(|| "queue launch item disappeared before graph admission".to_owned())?;
            let state_allowed = launch_state_allowed(item.state, checkpoint_exists);
            let snapshot_item = queue
                .snapshot
                .items
                .get(item.snapshot_index)
                .filter(|snapshot| snapshot.identity == item.identity);
            if !state_allowed
                || item.snapshot_index != authorised.item.snapshot_index
                || item.launch_request != authorised.item.launch_request
                || item.execution_directory != execution_directory
                || item.execution_directory != authorised.item.execution_directory
                || item.adoption != authorised.item.adoption
                || item.launch_digest != authorised.item.launch_digest
                || snapshot_item != Some(&authorised.snapshot_item)
            {
                return Err("queue launch intent changed before graph admission".into());
            }
            Ok(())
        })
    }

    /// Return the single persisted adoption intent owned by this graph and
    /// tracker identity. The runtime owner calls this again under registry
    /// custody before it validates the forge and worktree state.
    pub(super) fn adoption_for(
        &self,
        graph_id: &str,
        identity: &QueueIdentity,
        operation_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Value, String> {
        self.locked(budget, cancel, clock, |registry| {
            let matches: Vec<_> = registry
                .queues
                .values()
                .flat_map(|queue| {
                    queue.items.iter().filter_map(move |item| {
                        (item.graph_id == graph_id
                            && &item.identity == identity
                            && matches!(
                                item.state,
                                QueueItemState::Launching
                                    | QueueItemState::Active
                                    | QueueItemState::Resuming
                            ))
                        .then_some((queue, item))
                    })
                })
                .collect();
            let [(queue, item)] = matches.as_slice() else {
                return Err("adoption intent does not have exactly one active queue owner".into());
            };
            let value = item
                .adoption
                .as_ref()
                .ok_or("queue item has no persisted adoption intent")?;
            let snapshot_item = queue
                .snapshot
                .items
                .get(item.snapshot_index)
                .filter(|snapshot| snapshot.identity == item.identity)
                .ok_or_else(|| "adoption item is absent from its persisted snapshot".to_owned())?;
            if value["operation_id"].as_str() != Some(operation_id)
                || value["project"].as_str() != Some(identity.project.as_str())
                || value["tracker"].as_str() != Some(identity.tracker.as_str())
                || value["item"].as_str() != Some(identity.item.as_str())
                || value["issue_identity"].as_str() != Some(identity.item.as_str())
                || value["schema_version"].as_u64() != Some(1)
                || &snapshot_item.identity != identity
                || snapshot_item.data["id"].as_str() != Some(identity.item.as_str())
                || snapshot_item.data["number"].as_u64() != value["issue_number"].as_u64()
                || !snapshot_item.data["repository"]
                    .as_str()
                    .zip(value["repository"].as_str())
                    .is_some_and(|(snapshot_repo, adoption_repo)| {
                        snapshot_repo.eq_ignore_ascii_case(adoption_repo)
                    })
            {
                return Err("adoption intent does not match its persisted tracker identity".into());
            }
            Ok(value.clone())
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
        self.locked(budget, cancel, clock, |registry| {
            if let Some(existing) = registry.queues.get(&submission.queue_id) {
                if existing.project == submission.project
                    && existing.submission_digest == submission.submission_digest
                {
                    return Ok(QueueAdmission::Admitted {
                        queue: Box::new(existing.clone()),
                        replayed: true,
                    });
                }
                return Err(format!(
                    "delivery queue id `{}` already names different input",
                    submission.queue_id
                ));
            }
            sanitise_submission(&mut submission)?;
            validate_submission(&submission)?;
            let items: Vec<QueueItem> = submission
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    Ok(QueueItem {
                        identity: item.identity.clone(),
                        graph_id: item.graph_id.clone(),
                        snapshot_index: index,
                        launch_request: item.launch_request.clone(),
                        execution_directory: item.execution_directory.clone(),
                        adoption: item.adoption.clone(),
                        launch_digest: launch_digest(
                            &item.launch_request,
                            &item.execution_directory,
                            &item.adoption,
                        )?,
                        state: QueueItemState::Pending,
                        terminal_outcome: None,
                        unavailable_reason: None,
                        overflow_reason: None,
                    })
                })
                .collect::<Result<_, String>>()?;
            let queue = QueueRecord {
                queue_id: submission.queue_id.clone(),
                submission_digest: submission.submission_digest.clone(),
                project: submission.project.clone(),
                tracker: submission.tracker.clone(),
                snapshot: submission.snapshot.clone(),
                knobs: submission.knobs.clone(),
                cap_wait: submission.cap_wait.clone(),
                admitted_at: submission.admitted_at,
                warning_at: submission.warning_at,
                overflow_at: submission.overflow_at,
                wait_warning: None,
                wait_warning_emitted: false,
                overflow_reason: None,
                items,
            };
            for item in &queue.items {
                if registry.queues.values().any(|existing| {
                    existing.project == queue.project
                        && existing
                            .items
                            .iter()
                            .any(|prior| prior.graph_id == item.graph_id)
                }) {
                    return Err(format!(
                        "queue item graph id `{}` is already reserved",
                        item.graph_id
                    ));
                }
            }
            let selected: BTreeSet<_> = queue
                .items
                .iter()
                .map(|item| item.identity.clone())
                .collect();
            let owners: Vec<_> = registry
                .queues
                .values()
                .flat_map(|current| {
                    current
                        .items
                        .iter()
                        .filter(|item| {
                            selected.contains(&item.identity)
                                && item.state != QueueItemState::Terminal
                        })
                        .map(move |item| ExistingOwner {
                            identity: item.identity.clone(),
                            queue_id: current.queue_id.clone(),
                            graph_id: item.graph_id.clone(),
                            state: item.state,
                            unavailable_reason: item.unavailable_reason.clone(),
                        })
                })
                .collect();
            if !owners.is_empty() {
                return Ok(QueueAdmission::Owned(owners));
            }
            registry
                .queues
                .insert(queue.queue_id.clone(), queue.clone());
            Ok(QueueAdmission::Admitted {
                queue: Box::new(queue),
                replayed: false,
            })
        })
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
        self.claim_runnable_for(Some(queue_id), &BTreeSet::new(), now, budget, cancel, clock)
    }

    /// Claim ordinary pending items and only the parked graphs explicitly found
    /// runnable by the scheduler. Parked rows remain durable owners until this
    /// lock atomically moves them to `Resuming`.
    pub(super) fn claim_queue_with_parked(
        &self,
        queue_id: &str,
        runnable_parked: &BTreeSet<QueueIdentity>,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueSchedule, String> {
        self.claim_runnable_for(Some(queue_id), runnable_parked, now, budget, cancel, clock)
    }

    /// Run the ordinary queue selection but persist and return only the named
    /// item's claim. Earlier runnable items still occupy their place in the
    /// selection, so a reader cannot jump the queue.
    pub(super) fn claim_item_with_parked(
        &self,
        (queue_id, item_id): (&str, &str),
        runnable_parked: &BTreeSet<QueueIdentity>,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueSchedule, String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get_mut(queue_id)
                .ok_or_else(|| format!("unknown queue `{queue_id}`"))?;
            Ok(claim_one_item(
                queue_id,
                queue,
                item_id,
                runnable_parked,
                now,
            ))
        })
    }

    fn claim_runnable_for(
        &self,
        only_queue: Option<&str>,
        runnable_parked: &BTreeSet<QueueIdentity>,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<QueueSchedule, String> {
        self.locked(budget, cancel, clock, |registry| {
            let mut schedule = QueueSchedule::default();
            for (queue_id, queue) in &mut registry.queues {
                if only_queue.is_some_and(|only| only != queue_id) {
                    continue;
                }
                claim_runnable_items(queue_id, queue, runnable_parked, now, &mut schedule);
            }
            Ok(schedule)
        })
    }

    /// Reserve a slot before applying a direct control to an already parked
    /// graph. The caller verifies the persisted execution identity first.
    pub(super) fn reserve_parked_for_control(
        &self,
        (queue_id, identity): (&str, &QueueIdentity),
        graph_id: &str,
        now: WallTime,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<bool, String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get_mut(queue_id)
                .ok_or_else(|| format!("unknown queue `{queue_id}`"))?;
            let active = queue
                .items
                .iter()
                .filter(|item| consumes_parallel_slot(item.state))
                .count();
            let max_parallel = queue.knobs.max_parallel.value;
            let overflow_allowed = active >= max_parallel
                && now >= queue.overflow_at
                && queue.wait_warning_emitted
                && queue.overflow_reason.is_some();
            let overflow_reason = queue.overflow_reason.clone();
            let Some(item) = queue
                .items
                .iter_mut()
                .find(|item| &item.identity == identity && item.graph_id == graph_id)
            else {
                return Err("queue item identity or graph id changed".into());
            };
            match item.state {
                QueueItemState::Launching | QueueItemState::Active | QueueItemState::Resuming => {
                    Ok(true)
                }
                QueueItemState::Parked => {
                    if active >= max_parallel && !overflow_allowed {
                        return Ok(false);
                    }
                    item.state = QueueItemState::Resuming;
                    if active >= max_parallel {
                        item.overflow_reason = overflow_reason;
                    }
                    Ok(true)
                }
                QueueItemState::Pending | QueueItemState::Terminal => Ok(false),
            }
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
        self.update_item(
            (queue_id, identity),
            graph_id,
            budget,
            cancel,
            clock,
            |item| {
                if item.state == QueueItemState::Terminal {
                    return Err("terminal queue item cannot become active".into());
                }
                if !matches!(
                    item.state,
                    QueueItemState::Launching | QueueItemState::Active | QueueItemState::Resuming
                ) {
                    return Err("queue item must be admitted before launch acceptance".into());
                }
                item.state = QueueItemState::Active;
                item.unavailable_reason = None;
                Ok(())
            },
        )
    }

    /// Retain a deterministic refusal made before graph persistence so tick does not replay it forever.
    pub(super) fn mark_launch_unavailable(
        &self,
        (queue_id, identity): (&str, &QueueIdentity),
        graph_id: &str,
        reason: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        let reason = sanitise_persisted_text(reason)?;
        self.update_item(
            (queue_id, identity),
            graph_id,
            budget,
            cancel,
            clock,
            |item| {
                if !matches!(
                    item.state,
                    QueueItemState::Pending | QueueItemState::Launching
                ) {
                    return Err("only an unaccepted queue item can become unavailable".into());
                }
                item.state = QueueItemState::Pending;
                item.unavailable_reason = Some(reason);
                Ok(())
            },
        )
    }

    /// Release the parallel slot while retaining durable queue ownership.
    pub(super) fn mark_parked(
        &self,
        queue_id: &str,
        identity: &QueueIdentity,
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        self.update_item(
            (queue_id, identity),
            graph_id,
            budget,
            cancel,
            clock,
            |item| {
                if item.state == QueueItemState::Parked {
                    return Ok(());
                }
                if !matches!(
                    item.state,
                    QueueItemState::Launching | QueueItemState::Active | QueueItemState::Resuming
                ) {
                    return Err("only an admitted queue item can become parked".into());
                }
                item.state = QueueItemState::Parked;
                Ok(())
            },
        )
    }

    /// Explicit queue replay retries items held by a deterministic pre-effect refusal.
    pub(super) fn clear_unavailable(
        &self,
        queue_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get_mut(queue_id)
                .ok_or_else(|| format!("unknown queue `{queue_id}`"))?;
            for item in &mut queue.items {
                if item.state == QueueItemState::Pending {
                    item.unavailable_reason = None;
                }
            }
            Ok(())
        })
    }

    /// Acknowledge warning delivery before the advisory cap can overflow.
    pub(super) fn acknowledge_wait_warning(
        &self,
        queue_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get_mut(queue_id)
                .ok_or_else(|| format!("unknown queue `{queue_id}`"))?;
            if queue.wait_warning.is_some() {
                queue.wait_warning_emitted = true;
            }
            Ok(())
        })
    }

    /// Terminal settlement must come from a conclusive graph observation.
    pub(super) fn mark_terminal(
        &self,
        (queue_id, identity): (&str, &QueueIdentity),
        graph_id: &str,
        outcome: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<(), String> {
        if outcome.trim().is_empty() {
            return Err("terminal outcome must be non-empty".into());
        }
        self.update_item(
            (queue_id, identity),
            graph_id,
            budget,
            cancel,
            clock,
            |item| {
                if item.state == QueueItemState::Pending {
                    return Err("pending queue item has no graph to settle".into());
                }
                if item.state == QueueItemState::Terminal {
                    if item.terminal_outcome.as_deref() == Some(outcome) {
                        return Ok(());
                    }
                    return Err("terminal queue outcome is already recorded".into());
                }
                item.state = QueueItemState::Terminal;
                item.terminal_outcome = Some(outcome.to_owned());
                Ok(())
            },
        )
    }

    pub(super) fn queues(
        &self,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
    ) -> Result<Vec<QueueRecord>, String> {
        self.locked(budget, cancel, clock, |registry| {
            Ok(registry.queues.values().cloned().collect())
        })
    }

    fn update_item(
        &self,
        (queue_id, identity): (&str, &QueueIdentity),
        graph_id: &str,
        budget: WaitBudget,
        cancel: &CancelToken,
        clock: &dyn Clock,
        update: impl FnOnce(&mut QueueItem) -> Result<(), String>,
    ) -> Result<(), String> {
        self.locked(budget, cancel, clock, |registry| {
            let queue = registry
                .queues
                .get_mut(queue_id)
                .ok_or_else(|| format!("unknown queue `{queue_id}`"))?;
            let item = queue
                .items
                .iter_mut()
                .find(|item| &item.identity == identity && item.graph_id == graph_id)
                .ok_or_else(|| "queue item identity or graph id changed".to_owned())?;
            update(item)
        })
    }
}

fn consumes_parallel_slot(state: QueueItemState) -> bool {
    matches!(
        state,
        QueueItemState::Launching | QueueItemState::Active | QueueItemState::Resuming
    )
}

fn launch_state_allowed(state: QueueItemState, checkpoint_exists: bool) -> bool {
    match state {
        QueueItemState::Launching => true,
        QueueItemState::Active => checkpoint_exists,
        QueueItemState::Pending
        | QueueItemState::Parked
        | QueueItemState::Resuming
        | QueueItemState::Terminal => false,
    }
}

fn claim_one_item(
    queue_id: &str,
    queue: &mut QueueRecord,
    item_id: &str,
    runnable_parked: &BTreeSet<QueueIdentity>,
    now: WallTime,
) -> QueueSchedule {
    let before = queue.items.clone();
    let mut schedule = QueueSchedule::default();
    claim_runnable_items(queue_id, queue, runnable_parked, now, &mut schedule);
    for (current, previous) in queue.items.iter_mut().zip(before) {
        if current.identity.item != item_id {
            *current = previous;
        }
    }
    schedule
        .launch
        .retain(|(_, item)| item.identity.item == item_id);
    schedule
}

fn claim_runnable_items(
    queue_id: &str,
    queue: &mut QueueRecord,
    runnable_parked: &BTreeSet<QueueIdentity>,
    now: WallTime,
    schedule: &mut QueueSchedule,
) {
    let max_parallel = queue.knobs.max_parallel.value;
    let active = queue
        .items
        .iter()
        .filter(|item| consumes_parallel_slot(item.state))
        .count();
    let runnable_count = queue
        .items
        .iter()
        .filter(|item| {
            (item.state == QueueItemState::Pending && item.unavailable_reason.is_none())
                || (item.state == QueueItemState::Parked
                    && runnable_parked.contains(&item.identity))
        })
        .count();
    schedule
        .waiting
        .extend(queue.items.iter().filter_map(|item| {
            item.unavailable_reason.as_ref().map(|reason| {
                format!(
                    "queue {} item {} is unavailable: {reason}",
                    queue.queue_id, item.identity.item
                )
            })
        }));
    if runnable_count > 0 && active >= max_parallel {
        if now >= queue.warning_at && queue.wait_warning.is_none() {
            let warning = format!(
                "queue {} is waiting at max_parallel={} from {} ({}); cap wait={} seconds from {} ({}) before advisory overflow at {}",
                queue.queue_id,
                max_parallel,
                queue.knobs.max_parallel.source.layer.as_str(),
                queue
                    .knobs
                    .max_parallel
                    .source
                    .origin
                    .as_deref()
                    .unwrap_or("no origin recorded"),
                queue.cap_wait.value,
                queue.cap_wait.source.layer.as_str(),
                queue
                    .cap_wait
                    .source
                    .origin
                    .as_deref()
                    .unwrap_or("no origin recorded"),
                queue.overflow_at,
            );
            queue.wait_warning = Some(warning);
        }
        let warning_pending = now >= queue.warning_at && !queue.wait_warning_emitted;
        if warning_pending && let Some(warning) = &queue.wait_warning {
            schedule.warnings.push(warning.clone());
        }
        if now < queue.overflow_at {
            schedule.waiting.push(format!(
                "queue {} has {runnable_count} runnable items waiting for active capacity {}/{}",
                queue.queue_id, active, max_parallel
            ));
            schedule.launch.extend(
                queue
                    .items
                    .iter()
                    .filter(|item| {
                        matches!(
                            item.state,
                            QueueItemState::Launching | QueueItemState::Resuming
                        )
                    })
                    .cloned()
                    .map(|item| (queue_id.to_owned(), item)),
            );
            return;
        }
        if warning_pending {
            schedule.waiting.push(format!(
                "queue {} cap warning is awaiting durable acknowledgement before advisory overflow",
                queue.queue_id
            ));
            schedule.launch.extend(
                queue
                    .items
                    .iter()
                    .filter(|item| {
                        matches!(
                            item.state,
                            QueueItemState::Launching | QueueItemState::Resuming
                        )
                    })
                    .cloned()
                    .map(|item| (queue_id.to_owned(), item)),
            );
            return;
        }
        if queue.overflow_reason.is_none() {
            queue.overflow_reason = Some(format!(
                "max_parallel={} from {} ({}) exceeded after cap wait={} seconds from {} ({}); advisory overflow began at {}",
                max_parallel,
                queue.knobs.max_parallel.source.layer.as_str(),
                queue
                    .knobs
                    .max_parallel
                    .source
                    .origin
                    .as_deref()
                    .unwrap_or("no origin recorded"),
                queue.cap_wait.value,
                queue.cap_wait.source.layer.as_str(),
                queue
                    .cap_wait
                    .source
                    .origin
                    .as_deref()
                    .unwrap_or("no origin recorded"),
                queue.overflow_at,
            ));
        }
    }
    let capacity = max_parallel.saturating_sub(active);
    let overflow = active >= max_parallel;
    let overflow_reason = queue.overflow_reason.clone();
    let mut claimed = 0;
    for item in &mut queue.items {
        let next_state = match item.state {
            QueueItemState::Pending if item.unavailable_reason.is_none() => {
                Some(QueueItemState::Launching)
            }
            QueueItemState::Parked if runnable_parked.contains(&item.identity) => {
                Some(QueueItemState::Resuming)
            }
            _ => None,
        };
        let Some(next_state) = next_state else {
            continue;
        };
        if !overflow && claimed >= capacity {
            break;
        }
        item.state = next_state;
        if overflow {
            item.overflow_reason = overflow_reason.clone();
        }
        claimed += 1;
    }
    schedule.launch.extend(
        queue
            .items
            .iter()
            .filter(|item| {
                matches!(
                    item.state,
                    QueueItemState::Launching | QueueItemState::Resuming
                )
            })
            .cloned()
            .map(|item| (queue_id.to_owned(), item)),
    );
}

fn validate_submission(submission: &QueueSubmission) -> Result<(), String> {
    if submission.queue_id.trim().is_empty() || submission.queue_id.contains('\0') {
        return Err("queue id must be non-empty text".into());
    }
    submission
        .knobs
        .validate()
        .map_err(|error| format!("invalid sealed queue knobs: {error}"))?;
    if submission.knobs.max_parallel.value == 0 {
        return Err("queue max_parallel must be positive".into());
    }
    if !(1..=MAX_QUEUE_CAP_WAIT_SECONDS).contains(&submission.cap_wait.value) {
        return Err("queue cap wait must be 1..=14400 seconds".into());
    }
    if !is_sha256_digest(&submission.submission_digest) {
        return Err("queue submission digest must be a canonical sha256 digest".into());
    }
    if submission.admitted_at > submission.warning_at
        || submission.warning_at >= submission.overflow_at
    {
        return Err("queue deadlines must satisfy admitted_at <= warning_at < overflow_at".into());
    }
    let deadlines = bounded_wait_deadlines(submission.admitted_at, &submission.cap_wait)?;
    if deadlines != (submission.warning_at, submission.overflow_at) {
        return Err("queue deadlines do not match the sourced cap-wait policy".into());
    }
    if submission.project.trim().is_empty() || submission.tracker.trim().is_empty() {
        return Err("queue project and tracker must be non-empty".into());
    }
    if submission.project != submission.project.to_ascii_lowercase()
        || submission.tracker != submission.tracker.to_ascii_lowercase()
    {
        return Err(
            "queue project and tracker identities must be canonical lowercase values".into(),
        );
    }
    if submission.items.is_empty() || submission.items.len() != submission.snapshot.items.len() {
        return Err("queue snapshot and item graph count must match and be non-empty".into());
    }
    if submission.snapshot.selector.trim().is_empty()
        || submission.snapshot.source.trim().is_empty()
    {
        return Err("queue tracker snapshot must record its selector and source".into());
    }
    let mut identities = BTreeSet::new();
    let mut graph_ids = BTreeSet::new();
    for (index, (item, snapshot)) in submission
        .items
        .iter()
        .zip(&submission.snapshot.items)
        .enumerate()
    {
        if item.identity != snapshot.identity {
            return Err(format!(
                "queue snapshot identity at position {index} does not match its item graph"
            ));
        }
        if item.identity.project != submission.project.to_ascii_lowercase()
            || item.identity.tracker != submission.tracker.to_ascii_lowercase()
            || [
                item.identity.project.as_str(),
                item.identity.tracker.as_str(),
                item.identity.item.as_str(),
            ]
            .iter()
            .any(|part| part.trim().is_empty() || part.contains('\0'))
        {
            return Err("queue item identity does not match its project and tracker".into());
        }
        if !identities.insert(item.identity.clone()) {
            return Err("queue submission contains a duplicate tracker identity".into());
        }
        if item.graph_id.trim().is_empty()
            || item.graph_id.contains('\0')
            || !graph_ids.insert(&item.graph_id)
        {
            return Err("queue item graph ids must be non-empty and unique".into());
        }
        let canonical_graph_id =
            provenant_fabric_runtime::graph::template::fold_graph_name(&item.graph_id)
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

fn launch_digest(
    request: &Value,
    execution_directory: &str,
    adoption: &Option<Value>,
) -> Result<String, String> {
    let bytes = serde_json::to_vec(&(request, execution_directory, adoption))
        .map_err(|_| "queue replay identity could not be encoded".to_owned())?;
    Ok(provenant_records::CanonicalV1::file_digest(&bytes).to_string())
}

fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

fn sanitise_submission(submission: &mut QueueSubmission) -> Result<(), String> {
    submission.snapshot.selector = sanitise_persisted_text(&submission.snapshot.selector)?;
    submission.snapshot.source = sanitise_persisted_text(&submission.snapshot.source)?;
    for item in &mut submission.items {
        item.launch_request = sanitise_persisted_value(&item.launch_request)?;
        item.adoption = item
            .adoption
            .as_ref()
            .map(sanitise_persisted_value)
            .transpose()?;
    }
    for item in &mut submission.snapshot.items {
        item.data = sanitise_persisted_value(&item.data)?;
    }
    Ok(())
}

fn sanitise_persisted_text(value: &str) -> Result<String, String> {
    let safe = provenant_redact::publication_text(value.as_bytes(), [])
        .map_err(|_| "queue input could not be sanitised for durable storage".to_owned())?;
    String::from_utf8(safe).map_err(|_| "queue input sanitisation produced invalid text".to_owned())
}

fn sanitise_persisted_value(value: &Value) -> Result<Value, String> {
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
                    if matches!(
                        folded.as_str(),
                        "token"
                            | "access_token"
                            | "refresh_token"
                            | "password"
                            | "secret"
                            | "credentials"
                            | "authorization"
                            | "api_key"
                            | "private_key"
                    ) {
                        return Err(format!(
                            "queue launch request contains forbidden secret field `{name}`"
                        ));
                    }
                    if matches!(folded.as_str(), "wait_seconds" | "explain" | "detached") {
                        return Err(format!(
                            "queue launch request contains ephemeral field `{name}`"
                        ));
                    }
                    visit(child)?;
                }
            }
            Value::Array(values) => {
                for child in values {
                    visit(child)?;
                }
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn identity(item: &str) -> QueueIdentity {
        QueueIdentity::new("project", "github", item).unwrap()
    }

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn item(identity: QueueIdentity, index: usize, state: QueueItemState) -> QueueItem {
        QueueItem {
            graph_id: format!("graph-{index}"),
            identity,
            snapshot_index: index,
            launch_request: json!({"op":"run", "template":"deliver"}),
            execution_directory: "/repo".into(),
            adoption: None,
            launch_digest: format!("sha256:{}", "a".repeat(64)),
            state,
            terminal_outcome: None,
            unavailable_reason: None,
            overflow_reason: None,
        }
    }

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn queue(items: Vec<QueueItem>) -> QueueRecord {
        let admitted_at = WallTime::from_micros(0).unwrap();
        let mut knobs = QueueKnobs::default();
        knobs.max_parallel.value = 1;
        let source = provenant_records::Source {
            layer: provenant_records::Layer::Default,
            origin: None,
        };
        QueueRecord {
            queue_id: "queue-1".into(),
            submission_digest: format!("sha256:{}", "b".repeat(64)),
            project: "project".into(),
            tracker: "github".into(),
            snapshot: QueueSnapshot {
                selector: "issues".into(),
                source: "fixture".into(),
                resolved_at: admitted_at,
                items: items
                    .iter()
                    .map(|item| QueueSnapshotItem {
                        identity: item.identity.clone(),
                        data: json!({"id":item.identity.item.as_str()}),
                    })
                    .collect(),
            },
            knobs,
            cap_wait: Sourced {
                value: 10,
                source: source.clone(),
            },
            admitted_at,
            warning_at: WallTime::from_micros(5_000_000).unwrap(),
            overflow_at: WallTime::from_micros(10_000_000).unwrap(),
            wait_warning: None,
            wait_warning_emitted: false,
            overflow_reason: None,
            items,
        }
    }

    #[test]
    fn selecting_one_item_preserves_order_and_leaves_other_claims_untouched() {
        let mut queue = queue(vec![
            item(identity("first"), 0, QueueItemState::Pending),
            item(identity("second"), 1, QueueItemState::Pending),
        ]);
        let now = WallTime::from_micros(0).unwrap();
        let skipped = claim_one_item("queue-1", &mut queue, "second", &BTreeSet::new(), now);
        assert!(skipped.launch.is_empty());
        assert!(
            queue
                .items
                .iter()
                .all(|item| item.state == QueueItemState::Pending)
        );
        let selected = claim_one_item("queue-1", &mut queue, "first", &BTreeSet::new(), now);
        assert_eq!(selected.launch.len(), 1);
        assert_eq!(selected.launch[0].1.identity.item, "first");
        assert_eq!(queue.items[0].state, QueueItemState::Launching);
        assert_eq!(queue.items[1].state, QueueItemState::Pending);
    }

    #[test]
    fn unanswered_parked_item_releases_slot_for_pending_admission() {
        let parked = item(identity("parked"), 0, QueueItemState::Parked);
        let parked_identity = parked.identity.clone();
        let mut queue = queue(vec![
            parked,
            item(identity("pending"), 1, QueueItemState::Pending),
            item(identity("next"), 2, QueueItemState::Pending),
        ]);
        let now = WallTime::from_micros(0).unwrap();
        let mut schedule = QueueSchedule::default();

        claim_runnable_items("queue-1", &mut queue, &BTreeSet::new(), now, &mut schedule);

        assert_eq!(queue.items[0].state, QueueItemState::Parked);
        assert_eq!(queue.items[1].state, QueueItemState::Launching);
        assert_eq!(queue.items[2].state, QueueItemState::Pending);

        queue.items[1].state = QueueItemState::Terminal;
        queue.items[1].terminal_outcome = Some("complete".into());
        let mut wake = QueueSchedule::default();
        claim_runnable_items(
            "queue-1",
            &mut queue,
            &BTreeSet::from([parked_identity.clone()]),
            now,
            &mut wake,
        );

        assert_eq!(queue.items[0].state, QueueItemState::Resuming);
        assert_eq!(queue.items[2].state, QueueItemState::Pending);
        assert!(wake.launch.iter().any(|(queue_id, item)| {
            queue_id == "queue-1"
                && item.identity == parked_identity
                && item.state == QueueItemState::Resuming
        }));

        let encoded = serde_json::to_vec(&queue).unwrap();
        let mut restored: QueueRecord = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(restored.items[0].state, QueueItemState::Resuming);
        let mut blocked = QueueSchedule::default();
        claim_runnable_items(
            "queue-1",
            &mut restored,
            &BTreeSet::from([parked_identity]),
            now,
            &mut blocked,
        );
        assert_eq!(restored.items[2].state, QueueItemState::Pending);
    }

    #[test]
    fn queue_order_reacquires_one_runnable_parked_item_across_replay() {
        let parked = item(identity("parked"), 0, QueueItemState::Parked);
        let parked_identity = parked.identity.clone();
        let mut queue = queue(vec![
            parked,
            item(identity("pending"), 1, QueueItemState::Pending),
        ]);
        let now = WallTime::from_micros(0).unwrap();
        let mut first = QueueSchedule::default();

        claim_runnable_items(
            "queue-1",
            &mut queue,
            &BTreeSet::from([parked_identity.clone()]),
            now,
            &mut first,
        );

        assert_eq!(queue.items[0].state, QueueItemState::Resuming);
        assert_eq!(queue.items[1].state, QueueItemState::Pending);
        assert_eq!(first.launch.len(), 1);
        assert_eq!(first.launch[0].1.identity, parked_identity);

        // A restarted scheduler observes the durable reservation and leaves
        // unrelated pending work untouched while it reconciles this graph.
        let encoded = serde_json::to_vec(&queue).unwrap();
        let mut restored: QueueRecord = serde_json::from_slice(&encoded).unwrap();
        let mut replay = QueueSchedule::default();
        claim_runnable_items(
            "queue-1",
            &mut restored,
            &BTreeSet::from([parked_identity.clone()]),
            now,
            &mut replay,
        );
        assert_eq!(restored.items[0].state, QueueItemState::Resuming);
        assert_eq!(restored.items[1].state, QueueItemState::Pending);
        assert_eq!(replay.launch.len(), 1);
        assert_eq!(replay.launch[0].1.identity, parked_identity);
    }

    #[test]
    fn earlier_pending_item_precedes_later_runnable_parked_item() {
        let parked = item(identity("parked"), 1, QueueItemState::Parked);
        let parked_identity = parked.identity.clone();
        let mut queue = queue(vec![
            item(identity("earlier"), 0, QueueItemState::Pending),
            parked,
            item(identity("later"), 2, QueueItemState::Pending),
        ]);
        let mut schedule = QueueSchedule::default();

        claim_runnable_items(
            "queue-1",
            &mut queue,
            &BTreeSet::from([parked_identity.clone()]),
            WallTime::from_micros(0).unwrap(),
            &mut schedule,
        );

        assert_eq!(queue.items[0].state, QueueItemState::Launching);
        assert_eq!(queue.items[1].state, QueueItemState::Parked);
        assert_eq!(queue.items[2].state, QueueItemState::Pending);
        assert_eq!(schedule.launch.len(), 1);
        assert_eq!(schedule.launch[0].1.identity.item, "earlier");
    }

    #[test]
    fn stale_launch_authorization_requires_launching_after_checkpoint_absence() {
        assert!(launch_state_allowed(QueueItemState::Launching, false));
        assert!(launch_state_allowed(QueueItemState::Launching, true));
        assert!(!launch_state_allowed(QueueItemState::Active, false));
        assert!(launch_state_allowed(QueueItemState::Active, true));
        for state in [
            QueueItemState::Pending,
            QueueItemState::Parked,
            QueueItemState::Resuming,
            QueueItemState::Terminal,
        ] {
            assert!(!launch_state_allowed(state, false));
            assert!(!launch_state_allowed(state, true));
        }
    }
}
