//! GC policy for content-free port command receipts.
use provenant_config::{
    ByteObservation, PortCommandReceiptCollectionLock, PortCommandReceiptEntry,
    PortReceiptIssue, PortReceiptLifetime,
};
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, ProjectId, Roots};
use provenant_os::clock::SystemClock;
use provenant_records::CanonicalV1;
use provenant_run::{ExecutionContext, Request, RunStore};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Serialize)]
pub(crate) struct Report {
    pub observation: ByteObservation,
    pub errors: Vec<PortReceiptIssue>,
    pub removed_receipts: usize,
    pub bytes_freed: u64,
    pub busy: bool,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            observation: ByteObservation::Unknown,
            errors: Vec::new(),
            removed_receipts: 0,
            bytes_freed: 0,
            busy: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct AttemptScope {
    attempt: AttemptId,
    project: ProjectId,
    actor: ActorId,
    lease_epoch: u64,
}


pub(crate) fn collect(roots: &Roots, dry_run: bool) -> Report {
    let mut report = Report::default();
    let inventory = match provenant_config::port_command_receipts(roots.state.as_path()) {
        Ok(inventory) => inventory,
        Err(error) => {
            report.observation = ByteObservation::Unknown;
            report.errors.push(issue(
                roots.state.as_path(),
                "inventory port receipts",
                error,
            ));
            return report;
        }
    };
    let observation = if inventory.complete {
        ByteObservation::Measured {
            bytes: inventory.bytes,
        }
    } else {
        ByteObservation::Partial {
            bytes: inventory.bytes,
        }
    };
    report.errors.extend(inventory.issues);

    let now = SystemClock
        .wall_now()
        .ok()
        .and_then(|time| u64::try_from(time.as_micros()).ok())
        .map(|micros| micros / 1_000_000);
    if now.is_none() {
        report.errors.push(issue(
            roots.state.as_path(),
            "read clock for port receipt retention",
            "wall clock is unavailable or precedes the Unix epoch",
        ));
    }
    let mut proof_cache = HashMap::<AttemptScope, Result<bool, String>>::new();
    let mut eligible = Vec::new();
    for (index, entry) in inventory.entries.iter().enumerate() {
        let remove = match &entry.receipt.lifetime {
            PortReceiptLifetime::AgeDays { days } => now.is_some_and(|now| {
                now >= entry
                    .receipt
                    .recorded_at_unix_seconds
                    .saturating_add(u64::from(*days).saturating_mul(86_400))
            }),
            PortReceiptLifetime::AttemptTerminalAndRelease {
                attempt_id,
                project_id,
                actor_id,
                lease_epoch,
            } => {
                let scope = AttemptScope {
                    attempt: *attempt_id,
                    project: *project_id,
                    actor: *actor_id,
                    lease_epoch: *lease_epoch,
                };
                let proof = proof_cache.entry(scope).or_insert_with(|| {
                    attempt_release_proven(roots, scope, &inventory.entries, inventory.complete)
                });
                match proof {
                    Ok(proven) => *proven,
                    Err(cause) => {
                        report.errors.push(PortReceiptIssue {
                            path: entry.path.clone(),
                            operation: "prove admission release".into(),
                            cause: cause.clone(),
                        });
                        false
                    }
                }
            }
        };
        if remove {
            eligible.push(index);
        }
    }

    let mut ordinary = Vec::new();
    let mut release_proofs = Vec::new();
    for index in eligible {
        let receipt = &inventory.entries[index].receipt;
        if receipt.owner == "admission" && receipt.phase == "release" {
            release_proofs.push(index);
        } else {
            ordinary.push(index);
        }
    }
    if !dry_run && (!ordinary.is_empty() || !release_proofs.is_empty()) {
        let lock = match provenant_config::try_lock_port_command_receipts(roots.state.as_path()) {
            Ok(lock) => lock,
            Err(provenant_config::PortEvidenceError::Busy) => {
                report.busy = true;
                report.observation = observation;
                return report;
            }
            Err(error) => {
                report.errors.push(issue(
                    roots.state.as_path(),
                    "lock port receipt collection",
                    error,
                ));
                report.observation = observation;
                return report;
            }
        };
        remove_in_retry_order(&inventory.entries, ordinary, release_proofs, |index| {
            remove_receipt(
                roots.state.as_path(),
                &lock,
                &inventory.entries[index],
                &mut report,
            )
        });
    }
    report.observation = observation;
    report
}

fn attempt_release_proven(
    roots: &Roots,
    scope: AttemptScope,
    entries: &[PortCommandReceiptEntry],
    inventory_complete: bool,
) -> Result<bool, String> {
    if !inventory_complete {
        return Ok(false);
    }
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: scope.project,
        actor_id: scope.actor,
        read_prompts: false,
    };
    let clock = SystemClock;
    let cancel = CancelToken::new();
    let call = Request {
        clock: &clock,
        cancel: &cancel,
    };
    let Some(store) =
        RunStore::open_disk_read_only(&context, &call).map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let record = match store.record(scope.attempt, &call) {
        Ok(record) => record,
        Err(provenant_run::RunError::NotFound) => return Ok(false),
        Err(error) => return Err(error.to_string()),
    };
    if record.attempt_id != scope.attempt
        || record.project_id != scope.project
        || record.verified_owner != Some(scope.actor)
        || !matches!(
            record.state,
            provenant_records::run::AttemptState::Terminal { .. }
        )
    {
        return Ok(false);
    }
    let Some(binding) = &record.repository_admission_binding else {
        return Ok(false);
    };
    if binding.lease_epoch != scope.lease_epoch {
        return Ok(false);
    }
    let key = format!("{}:{}", scope.attempt, scope.lease_epoch);
    let request_digest = CanonicalV1::file_digest(key.as_bytes());
    Ok(entries.iter().any(|entry| {
        let PortReceiptLifetime::AttemptTerminalAndRelease {
            attempt_id,
            project_id,
            actor_id,
            lease_epoch,
        } = &entry.receipt.lifetime
        else {
            return false;
        };
        *attempt_id == scope.attempt
            && *project_id == scope.project
            && *actor_id == scope.actor
            && *lease_epoch == scope.lease_epoch
            && entry.receipt.owner == "admission"
            && entry.receipt.phase == "release"
            && entry.receipt.source == binding.source
            && entry.receipt.command_digest == binding.command_digest
            && entry.receipt.request_key_digest == request_digest
            && !entry.receipt.timed_out
            && !entry.receipt.cancelled
            && entry.receipt.exit_code.is_some_and(|code| {
                i32::try_from(code)
                    .ok()
                    .is_some_and(|code| binding.command.exit_codes.success.contains(&code))
            })
    }))
}

fn issue(path: &Path, operation: &str, cause: impl ToString) -> PortReceiptIssue {
    PortReceiptIssue {
        path: PathBuf::from(path),
        operation: operation.into(),
        cause: cause.to_string(),
    }
}

fn remove_receipt(
    state: &Path,
    lock: &PortCommandReceiptCollectionLock,
    entry: &PortCommandReceiptEntry,
    report: &mut Report,
) -> bool {
    match provenant_config::remove_port_command_receipt(state, lock, entry) {
        Ok(bytes) => {
            report.removed_receipts += 1;
            report.bytes_freed = report.bytes_freed.saturating_add(bytes);
            true
        }
        Err(error) => {
            report
                .errors
                .push(issue(&entry.path, "collect port receipt", error));
            false
        }
    }
}

fn remove_in_retry_order(
    entries: &[PortCommandReceiptEntry],
    ordinary: Vec<usize>,
    release_proofs: Vec<usize>,
    mut remove: impl FnMut(usize) -> bool,
) {
    let mut failed_attempts = std::collections::HashSet::new();
    for index in ordinary {
        if !remove(index)
            && let Some(scope) = attempt_scope(&entries[index])
        {
            failed_attempts.insert(scope);
        }
    }
    for index in release_proofs {
        if let Some(scope) = attempt_scope(&entries[index])
            && !failed_attempts.contains(&scope)
        {
            remove(index);
        }
    }
}

fn attempt_scope(entry: &PortCommandReceiptEntry) -> Option<AttemptScope> {
    match &entry.receipt.lifetime {
        PortReceiptLifetime::AttemptTerminalAndRelease {
            attempt_id,
            project_id,
            actor_id,
            lease_epoch,
        } => Some(AttemptScope {
            attempt: *attempt_id,
            project: *project_id,
            actor: *actor_id,
            lease_epoch: *lease_epoch,
        }),
        PortReceiptLifetime::AgeDays { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{CanonicalV1, PortCommandDisposition, PortReceiptLifetime, PortSource};

    fn admission_entry(phase: &str) -> PortCommandReceiptEntry {
        let attempt = AttemptId::from_bytes([1; 16]);
        let project = ProjectId::from_bytes([2; 16]);
        let actor = ActorId::from_bytes([3; 16]);
        PortCommandReceiptEntry::new(
            provenant_records::PortCommandReceipt::new(
                "admission".into(),
                PortSource::Project,
                phase.into(),
                CanonicalV1::file_digest(b"attempt:1"),
                CanonicalV1::file_digest(b"command"),
                PortCommandDisposition::Succeeded,
                Some(0),
                Some(1),
                None,
                true,
                false,
                false,
                1,
                PortReceiptLifetime::AttemptTerminalAndRelease {
                    attempt_id: attempt,
                    project_id: project,
                    actor_id: actor,
                    lease_epoch: 1,
                },
            ),
            1,
            PathBuf::from("receipt"),
        )
    }

    #[test]
    fn failed_sibling_collection_keeps_admission_release_proof_for_retry() {
        let entries = [admission_entry("acquire"), admission_entry("release")];
        let mut attempted = Vec::new();
        remove_in_retry_order(&entries, vec![0], vec![1], |index| {
            attempted.push(index);
            false
        });
        assert_eq!(attempted.first(), Some(&0));
        assert!(!attempted.contains(&1));
    }
}
