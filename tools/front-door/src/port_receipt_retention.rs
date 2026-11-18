//! GC policy for content-free port command receipts.
use provenant_config::{
    ByteObservation, PortCommandReceiptCollectionLock, PortCommandReceiptEntry, PortReceiptIssue,
    PortReceiptLifetime,
};
use provenant_core::{ActorId, AttemptId, CancelToken, Clock, ProjectId, Roots, SweepControl};
use provenant_records::{FileDigest, PortSource};
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
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            observation: ByteObservation::Unknown,
            errors: Vec::new(),
            removed_receipts: 0,
            bytes_freed: 0,
            busy: false,
            interrupted_at: None,
            continuation: None,
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

#[derive(Clone)]
struct AdmissionProof {
    source: PortSource,
    command_digest: FileDigest,
    request_key: String,
    success_codes: Vec<i32>,
}

pub(crate) fn collect(
    roots: &Roots,
    dry_run: bool,
    clock: &dyn Clock,
    control: &SweepControl<'_>,
) -> Report {
    let mut report = Report::default();
    let (phase, detail) = decode_cursor(control.detail);
    let scan_control = SweepControl {
        start_at: control.start_at,
        detail,
        cancel: control.cancel.clone(),
        keep_going: control.keep_going,
    };
    let inventory =
        match provenant_config::port_command_receipts(roots.state.as_path(), &scan_control) {
            Ok(inventory) => inventory,
            Err(error) => {
                report.errors.push(issue(
                    roots.state.as_path(),
                    "inventory port receipts",
                    error,
                ));
                return report;
            }
        };
    report.observation = observation(&inventory);
    report.errors.extend(inventory.issues.iter().cloned());
    if inventory.interrupted_at.is_some() && !control.should_continue() {
        if let Some(candidate) = inventory.interrupted_at.clone() {
            interrupt(
                &mut report,
                phase,
                candidate,
                inventory.continuation.as_deref(),
            );
        }
        return report;
    }

    let now = if phase == RetentionPhase::Ordinary {
        match clock
            .wall_now()
            .ok()
            .and_then(|time| u64::try_from(time.as_micros()).ok())
        {
            Some(micros) => Some(micros / 1_000_000),
            None => {
                report.errors.push(issue(
                    roots.state.as_path(),
                    "read clock for port receipt retention",
                    "wall clock is unavailable or precedes the Unix epoch",
                ));
                None
            }
        }
    } else {
        None
    };
    let mut proofs = HashMap::<AttemptScope, Result<Option<AdmissionProof>, String>>::new();
    let mut eligible = Vec::new();
    for (index, entry) in inventory.entries.iter().enumerate() {
        if !control.should_continue() {
            interrupt(
                &mut report,
                phase,
                entry.path.clone(),
                entry.cursor.as_deref(),
            );
            return report;
        }
        match phase {
            RetentionPhase::Ordinary if is_release_proof(entry) => continue,
            RetentionPhase::Release if !is_release_proof(entry) => continue,
            _ => {}
        }
        let remove = match (&entry.receipt.lifetime, phase) {
            (PortReceiptLifetime::AgeDays { days }, RetentionPhase::Ordinary) => {
                now.is_some_and(|now| {
                    now >= entry
                        .receipt
                        .recorded_at_unix_seconds
                        .saturating_add(u64::from(*days).saturating_mul(86_400))
                })
            }
            (PortReceiptLifetime::AttemptTerminalAndRelease { .. }, _) => {
                let Some(scope) = attempt_scope(entry) else {
                    continue;
                };
                let proof = proofs
                    .entry(scope)
                    .or_insert_with(|| attempt_release_proof(roots, scope, clock, &control.cancel));
                match proof {
                    Ok(Some(_)) if phase == RetentionPhase::Ordinary => true,
                    Ok(Some(proof)) if phase == RetentionPhase::Release => {
                        match acquisition_receipt_exists(roots.state.as_path(), proof) {
                            Ok(exists) => !exists,
                            Err(cause) => {
                                report.errors.push(issue(
                                    &entry.path,
                                    "inspect admission acquisition",
                                    cause,
                                ));
                                false
                            }
                        }
                    }
                    Ok(_) => false,
                    Err(cause) => {
                        report
                            .errors
                            .push(issue(&entry.path, "prove admission release", cause));
                        false
                    }
                }
            }
            _ => false,
        };
        if remove {
            eligible.push(index);
        }
    }

    if !dry_run && !eligible.is_empty() {
        for index in eligible {
            let entry = &inventory.entries[index];
            if !control.should_continue() {
                interrupt(
                    &mut report,
                    phase,
                    entry.path.clone(),
                    entry.cursor.as_deref(),
                );
                return report;
            }
            let lock = match provenant_config::try_lock_port_command_receipts(roots.state.as_path())
            {
                Ok(lock) => lock,
                Err(provenant_config::PortEvidenceError::Busy) => {
                    report.busy = true;
                    interrupt(
                        &mut report,
                        phase,
                        entry.path.clone(),
                        entry.cursor.as_deref(),
                    );
                    return report;
                }
                Err(error) => {
                    report.errors.push(issue(
                        roots.state.as_path(),
                        "lock port receipt collection",
                        error,
                    ));
                    return report;
                }
            };
            remove_receipt(roots.state.as_path(), &lock, entry, &mut report);
            drop(lock);
        }
    }

    if let Some(candidate) = inventory.interrupted_at {
        interrupt(
            &mut report,
            phase,
            candidate,
            inventory.continuation.as_deref(),
        );
        return report;
    }
    if dry_run || phase == RetentionPhase::Release {
        return report;
    }

    // Release proofs retire only after the ordinary pass has had a chance to
    // remove every matching acquisition receipt. A resumed release pass keeps
    // its own owner cursor and can therefore finish independently.
    let release_control = SweepControl {
        start_at: None,
        detail: None,
        cancel: control.cancel.clone(),
        keep_going: control.keep_going,
    };
    let release_inventory =
        match provenant_config::port_command_receipts(roots.state.as_path(), &release_control) {
            Ok(inventory) => inventory,
            Err(error) => {
                report.errors.push(issue(
                    roots.state.as_path(),
                    "inventory admission release receipts",
                    error,
                ));
                return report;
            }
        };
    report
        .errors
        .extend(release_inventory.issues.iter().cloned());
    if !release_inventory.complete {
        let bytes = match report.observation {
            ByteObservation::Measured { bytes } | ByteObservation::Partial { bytes } => bytes,
            ByteObservation::Unknown => release_inventory.bytes,
        };
        report.observation = ByteObservation::Partial { bytes };
    }
    let mut release_proofs = HashMap::<AttemptScope, Result<Option<AdmissionProof>, String>>::new();
    let mut release_eligible = Vec::new();
    for (index, entry) in release_inventory.entries.iter().enumerate() {
        if !control.should_continue() {
            interrupt(
                &mut report,
                RetentionPhase::Release,
                entry.path.clone(),
                entry.cursor.as_deref(),
            );
            return report;
        }
        if !is_release_proof(entry) {
            continue;
        }
        let Some(scope) = attempt_scope(entry) else {
            continue;
        };
        let proof = release_proofs
            .entry(scope)
            .or_insert_with(|| attempt_release_proof(roots, scope, clock, &control.cancel));
        match proof {
            Ok(Some(proof)) => match acquisition_receipt_exists(roots.state.as_path(), proof) {
                Ok(false) => release_eligible.push(index),
                Ok(true) => {}
                Err(cause) => {
                    report
                        .errors
                        .push(issue(&entry.path, "inspect admission acquisition", cause))
                }
            },
            Ok(None) => {}
            Err(cause) => report
                .errors
                .push(issue(&entry.path, "prove admission release", cause)),
        }
    }
    if !dry_run && !release_eligible.is_empty() {
        for index in release_eligible {
            let entry = &release_inventory.entries[index];
            if !control.should_continue() {
                interrupt(
                    &mut report,
                    RetentionPhase::Release,
                    entry.path.clone(),
                    entry.cursor.as_deref(),
                );
                return report;
            }
            let lock = match provenant_config::try_lock_port_command_receipts(roots.state.as_path())
            {
                Ok(lock) => lock,
                Err(provenant_config::PortEvidenceError::Busy) => {
                    report.busy = true;
                    interrupt(
                        &mut report,
                        RetentionPhase::Release,
                        entry.path.clone(),
                        entry.cursor.as_deref(),
                    );
                    return report;
                }
                Err(error) => {
                    report.errors.push(issue(
                        roots.state.as_path(),
                        "lock port receipt collection",
                        error,
                    ));
                    return report;
                }
            };
            remove_receipt(roots.state.as_path(), &lock, entry, &mut report);
            drop(lock);
        }
    }
    if let Some(candidate) = release_inventory.interrupted_at {
        interrupt(
            &mut report,
            RetentionPhase::Release,
            candidate,
            release_inventory.continuation.as_deref(),
        );
    }
    report
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RetentionPhase {
    Ordinary,
    Release,
}

impl RetentionPhase {
    fn name(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Release => "release",
        }
    }
}

fn decode_cursor(value: Option<&str>) -> (RetentionPhase, Option<&str>) {
    let Some(value) = value.and_then(|value| value.strip_prefix("port-retention-v1|")) else {
        return (RetentionPhase::Ordinary, value);
    };
    match value.split_once('|') {
        Some(("release", detail)) => (
            RetentionPhase::Release,
            (!detail.is_empty()).then_some(detail),
        ),
        Some(("ordinary", detail)) => (
            RetentionPhase::Ordinary,
            (!detail.is_empty()).then_some(detail),
        ),
        _ => (RetentionPhase::Ordinary, None),
    }
}

fn interrupt(report: &mut Report, phase: RetentionPhase, path: PathBuf, detail: Option<&str>) {
    report.interrupted_at = Some(path);
    report.continuation = Some(format!(
        "port-retention-v1|{}|{}",
        phase.name(),
        detail.unwrap_or_default()
    ));
}

fn observation(inventory: &provenant_config::PortCommandReceiptInventory) -> ByteObservation {
    if inventory.complete {
        ByteObservation::Measured {
            bytes: inventory.bytes,
        }
    } else {
        ByteObservation::Partial {
            bytes: inventory.bytes,
        }
    }
}

fn is_release_proof(entry: &PortCommandReceiptEntry) -> bool {
    entry.receipt.owner == "admission"
        && entry.receipt.phase == "release"
        && matches!(
            &entry.receipt.lifetime,
            PortReceiptLifetime::AttemptTerminalAndRelease { .. }
        )
}

fn attempt_release_proof(
    roots: &Roots,
    scope: AttemptScope,
    clock: &dyn Clock,
    cancel: &CancelToken,
) -> Result<Option<AdmissionProof>, String> {
    let context = ExecutionContext {
        state_root: roots.state.clone(),
        project_id: scope.project,
        actor_id: scope.actor,
        read_prompts: false,
    };
    let call = Request { clock, cancel };
    let Some(store) =
        RunStore::open_disk_read_only(&context, &call).map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let record = match store.record(scope.attempt, &call) {
        Ok(record) => record,
        Err(provenant_run::RunError::NotFound) => return Ok(None),
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
        return Ok(None);
    }
    let Some(binding) = record.repository_admission_binding.as_ref() else {
        return Ok(None);
    };
    if binding.lease_epoch != scope.lease_epoch {
        return Ok(None);
    }
    let request_key = format!("{}:{}", scope.attempt, scope.lease_epoch);
    let proof = provenant_config::port_command_receipt_proof(
        roots.state.as_path(),
        "admission",
        binding.source,
        "release",
        &request_key,
        binding.command_digest,
        &binding.command.exit_codes.success,
    )
    .map_err(|error| error.to_string())?;
    if !proof.is_some_and(|proof| proof.successful_exit && proof.capture_complete) {
        return Ok(None);
    }
    Ok(Some(AdmissionProof {
        source: binding.source,
        command_digest: binding.command_digest,
        request_key,
        success_codes: binding.command.exit_codes.success.clone(),
    }))
}

fn acquisition_receipt_exists(state: &Path, proof: &AdmissionProof) -> Result<bool, String> {
    Ok(provenant_config::port_command_receipt_proof(
        state,
        "admission",
        proof.source,
        "acquire",
        &proof.request_key,
        proof.command_digest,
        &proof.success_codes,
    )
    .map_err(|error| error.to_string())?
    .is_some())
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
) {
    match provenant_config::remove_port_command_receipt(state, lock, entry) {
        Ok(bytes) => {
            report.removed_receipts += 1;
            report.bytes_freed = report.bytes_freed.saturating_add(bytes);
        }
        Err(error) => report
            .errors
            .push(issue(&entry.path, "collect port receipt", error)),
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
