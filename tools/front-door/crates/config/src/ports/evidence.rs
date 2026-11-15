//! Durable, content-free evidence for declared command ports.
use super::{PortCommandDisposition, PortCommandExecution, PortSource, PortTable};
use provenant_os::{
    exec::{ExecOutcome, Termination},
    fs::{Directory, FileKind, LockGuard, LockMode, Publication, PublishMode},
};
use provenant_records::{CanonicalV1, FileDigest, Limits, PortCommandReceipt, PortReceiptLifetime};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    io::Cursor,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const RECEIPT_BYTES: u64 = 4096;
const RECEIPT_DEPTH: usize = 8;
const RECEIPT_MEMBERS: usize = 32;
const MAX_RECEIPTS: usize = 65_536;
const RECEIPTS_DIR: &str = "port-receipts";
const RECEIPT_LOCK: &str = ".port-receipts.lock";
const RECEIPT_PREFIX: &str = "port-command-";

#[derive(Debug, thiserror::Error)]
pub enum PortEvidenceError {
    #[error("unknown port name for command evidence")]
    PortName,
    #[error("invalid command evidence phase")]
    Phase,
    #[error("port receipt lifetime does not match its owner")]
    Lifetime,
    #[error("cannot read the system clock for command evidence")]
    Clock,
    #[error("cannot open host state for command evidence")]
    Open(#[source] provenant_os::fs::FsError),
    #[error("cannot resolve the host state directory for command evidence")]
    OpenState,
    #[error("cannot encode command evidence")]
    Encode(#[source] provenant_records::EncodeError),
    #[error("cannot read command evidence")]
    Read(#[source] provenant_os::fs::FsError),
    #[error("cannot decode command evidence")]
    Decode(#[source] provenant_records::DecodeError),
    #[error("cannot publish command evidence")]
    Publish(#[source] provenant_os::fs::FsError),
    #[error("cannot collect command evidence")]
    Collect(#[source] provenant_os::fs::FsError),
    #[error("port receipt publication or collection is busy")]
    Busy,
    #[error("command evidence changed before collection")]
    Changed,
}

/// Atomic publication can be visible even when the containing-directory sync
/// could not confirm durability. Callers must keep that state distinct from an
/// absent receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortCommandPublication {
    Durable,
    VisibleDurabilityUnconfirmed { cause: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortReceiptIssue {
    pub path: PathBuf,
    pub operation: String,
    pub cause: String,
}

#[derive(Clone, Debug)]
pub struct PortCommandReceiptEntry {
    pub receipt: PortCommandReceipt,
    pub bytes: u64,
    pub path: PathBuf,
    file_name: String,
}

impl PortCommandReceiptEntry {
    /// Bind a decoded receipt to its derived owner-local filename. Removal
    /// still re-reads and compares the complete receipt before unlinking.
    pub fn new(receipt: PortCommandReceipt, bytes: u64, path: PathBuf) -> Self {
        let file_name = receipt_name(&receipt.phase, receipt.request_key_digest);
        Self {
            receipt,
            bytes,
            path,
            file_name,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PortCommandReceiptInventory {
    pub entries: Vec<PortCommandReceiptEntry>,
    pub bytes: u64,
    pub complete: bool,
    pub issues: Vec<PortReceiptIssue>,
}

/// Exclusive, nonblocking owner lease spanning one set of receipt removals.
/// The root handle keeps the lock and all removals on the same directory inode.
pub struct PortCommandReceiptCollectionLock {
    state: PathBuf,
    receipt_root: Directory,
    _guard: LockGuard,
}

impl PortCommandReceiptInventory {
    fn issue(&mut self, path: PathBuf, operation: &str, cause: impl ToString) {
        self.complete = false;
        self.issues.push(PortReceiptIssue {
            path,
            operation: operation.into(),
            cause: cause.to_string(),
        });
    }
}

impl PortCommandExecution {
    /// Whether both captured streams and process completion are intact.
    pub fn capture_complete(&self) -> bool {
        self.outcome.as_ref().is_some_and(outcome_complete)
    }
}

/// Publish one content-free receipt for the latest execution of `port`/`key`.
/// The key is hashed into the filename; argv, environment and output bytes are
/// never persisted. A failed publication is returned to the port owner.
pub fn record_port_command(
    state: &Path,
    owner: &str,
    source: PortSource,
    phase: &str,
    key: &str,
    execution: &PortCommandExecution,
    lifetime: PortReceiptLifetime,
) -> Result<PortCommandPublication, PortEvidenceError> {
    let owner = PortTable::PORT_NAMES
        .iter()
        .copied()
        .find(|name| *name == owner)
        .ok_or(PortEvidenceError::PortName)?;
    validate_phase(phase)?;
    validate_lifetime(owner, &lifetime)?;
    let key_digest = CanonicalV1::file_digest(key.as_bytes());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| PortEvidenceError::Clock)?
        .as_secs();
    let receipt = receipt(owner, source, phase, key_digest, execution, lifetime, now);
    let name = receipt_name(phase, key_digest);
    let bytes = provenant_records::encode(&receipt).map_err(PortEvidenceError::Encode)?;
    let receipt_root = receipt_root(state, true)?.ok_or(PortEvidenceError::OpenState)?;
    let _publication = lock_receipt_root(&receipt_root)?;
    receipt_root
        .make_private()
        .map_err(PortEvidenceError::Open)?;
    receipt_root.sync().map_err(PortEvidenceError::Open)?;
    let directory =
        owner_directory_in(&receipt_root, owner, true)?.ok_or(PortEvidenceError::OpenState)?;
    let publication = directory
        .atomic_write(
            OsStr::new(&name),
            ".port-command-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(PortEvidenceError::Publish)?;
    Ok(match publication {
        Publication::Durable => PortCommandPublication::Durable,
        Publication::VisibleDurabilityUnconfirmed { sync_error } => {
            PortCommandPublication::VisibleDurabilityUnconfirmed {
                cause: sync_error.to_string(),
            }
        }
    })
}

/// Typed result from a matching command receipt. Admission decisions use the
/// declared exit code; capture completeness remains separate for response ports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortCommandReceiptProof {
    pub successful_exit: bool,
    pub capture_complete: bool,
}

/// Read the latest content-free receipt matching the frozen declaration and
/// request key. A missing or mismatched receipt is `None`.
pub fn port_command_receipt_proof(
    state: &Path,
    owner: &str,
    source: PortSource,
    phase: &str,
    key: &str,
    command_digest: FileDigest,
    success_codes: &[i32],
) -> Result<Option<PortCommandReceiptProof>, PortEvidenceError> {
    let owner = PortTable::PORT_NAMES
        .iter()
        .copied()
        .find(|name| *name == owner)
        .ok_or(PortEvidenceError::PortName)?;
    validate_phase(phase)?;
    let request_key_digest = CanonicalV1::file_digest(key.as_bytes());
    let name = receipt_name(phase, request_key_digest);
    let Some(directory) = owner_directory(state, owner, false)? else {
        return Ok(None);
    };
    let raw = match directory.read_stable(OsStr::new(&name), RECEIPT_BYTES) {
        Ok(raw) => raw,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(PortEvidenceError::Read(error)),
    };
    let receipt = decode_receipt(&raw).map_err(PortEvidenceError::Decode)?;
    if receipt.owner != owner
        || receipt.source != source
        || receipt.phase != phase
        || receipt.request_key_digest != request_key_digest
        || receipt.command_digest != command_digest
    {
        return Ok(None);
    }
    let successful_exit = !receipt.timed_out
        && !receipt.cancelled
        && receipt
            .exit_code
            .and_then(|code| i32::try_from(code).ok())
            .is_some_and(|code| success_codes.contains(&code));
    Ok(Some(PortCommandReceiptProof {
        successful_exit,
        capture_complete: receipt.capture_complete,
    }))
}

/// Inventory every known owner directory. Malformed entries are retained in
/// `issues` and make `complete` false, so GC can report a partial observation.
pub fn port_command_receipts(
    state: &Path,
) -> Result<PortCommandReceiptInventory, PortEvidenceError> {
    let state_dir = match Directory::open_anchor(state) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => {
            return Ok(PortCommandReceiptInventory {
                complete: true,
                ..Default::default()
            });
        }
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    let receipt_root = match state_dir.open_verified_directory(OsStr::new(RECEIPTS_DIR)) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => {
            return Ok(PortCommandReceiptInventory {
                complete: true,
                ..Default::default()
            });
        }
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    let mut inventory = PortCommandReceiptInventory {
        complete: true,
        ..Default::default()
    };
    if !private_owned_directory(&receipt_root) {
        inventory.issue(
            state.join(RECEIPTS_DIR),
            "inspect receipt root",
            "receipt directory is not private to the effective user",
        );
        return Ok(inventory);
    }
    let known = PortTable::PORT_NAMES;
    let mut visited = 0usize;
    for owner_entry in receipt_root.entries().map_err(PortEvidenceError::Read)? {
        let owner_entry = match owner_entry {
            Ok(entry) => entry,
            Err(error) => {
                inventory.issue(receipt_root.anchored_path(), "enumerate owners", error);
                continue;
            }
        };
        let owner = match owner_entry.to_str() {
            Some(owner) if known.contains(&owner) => owner,
            _ => {
                inventory.issue(
                    state.join(RECEIPTS_DIR).join(&owner_entry),
                    "inspect owner",
                    "unknown port receipt owner",
                );
                continue;
            }
        };
        let owner_dir = match receipt_root.open_verified_directory(&owner_entry) {
            Ok(directory) => directory,
            Err(error) => {
                inventory.issue(state.join(RECEIPTS_DIR).join(owner), "open owner", error);
                continue;
            }
        };
        if !private_owned_directory(&owner_dir) {
            inventory.issue(
                state.join(RECEIPTS_DIR).join(owner),
                "inspect owner directory",
                "owner directory is not private to the effective user",
            );
            continue;
        }
        for file_entry in match owner_dir.entries() {
            Ok(entries) => entries,
            Err(error) => {
                inventory.issue(
                    state.join(RECEIPTS_DIR).join(owner),
                    "enumerate receipts",
                    error,
                );
                continue;
            }
        } {
            let file_entry = match file_entry {
                Ok(entry) => entry,
                Err(error) => {
                    inventory.issue(
                        state.join(RECEIPTS_DIR).join(owner),
                        "enumerate receipts",
                        error,
                    );
                    continue;
                }
            };
            if file_entry.as_os_str() == OsStr::new(RECEIPT_LOCK) {
                continue;
            }
            visited += 1;
            if visited > MAX_RECEIPTS {
                inventory.issue(
                    state.join(RECEIPTS_DIR).join(owner),
                    "enumerate receipts",
                    format!("receipt count exceeds {MAX_RECEIPTS}"),
                );
                return Ok(inventory);
            }
            let path = state.join(RECEIPTS_DIR).join(owner).join(&file_entry);
            let file_name = match file_entry.to_str() {
                Some(name) if name.starts_with(RECEIPT_PREFIX) && name.ends_with(".json") => name,
                _ => {
                    inventory.issue(path, "inspect receipt", "unexpected owner entry");
                    continue;
                }
            };
            let metadata = match owner_dir.metadata_at(&file_entry, false) {
                Ok(metadata) => metadata,
                Err(error) => {
                    inventory.issue(path, "stat receipt", error);
                    continue;
                }
            };
            inventory.bytes = inventory.bytes.saturating_add(metadata.size);
            if metadata.kind != FileKind::Regular
                || metadata.links != 1
                || metadata.uid != provenant_os::fs::effective_uid()
                || metadata.mode & 0o077 != 0
            {
                inventory.issue(path, "read receipt", "entry is not a private regular file");
                continue;
            }
            if metadata.size > RECEIPT_BYTES {
                inventory.issue(path, "read receipt", "receipt exceeds its size limit");
                continue;
            }
            let raw = match owner_dir.read_stable(&file_entry, RECEIPT_BYTES) {
                Ok(raw) => raw,
                Err(error) => {
                    inventory.issue(path, "read receipt", error);
                    continue;
                }
            };
            let receipt = match decode_receipt(&raw) {
                Ok(receipt) => receipt,
                Err(error) => {
                    inventory.issue(path, "decode receipt", error);
                    continue;
                }
            };
            if receipt.owner != owner
                || validate_phase(&receipt.phase).is_err()
                || file_name != receipt_name(&receipt.phase, receipt.request_key_digest)
                || validate_lifetime(owner, &receipt.lifetime).is_err()
            {
                inventory.issue(
                    path,
                    "validate receipt",
                    "receipt identity does not match its path",
                );
                continue;
            }
            inventory
                .entries
                .push(PortCommandReceiptEntry::new(receipt, metadata.size, path));
        }
    }
    Ok(inventory)
}

/// Try to hold the publication owner lock for a complete collection pass.
/// A busy owner is a retryable skip; callers must not wait for publications.
pub fn try_lock_port_command_receipts(
    state: &Path,
) -> Result<PortCommandReceiptCollectionLock, PortEvidenceError> {
    let receipt_root = receipt_root(state, false)?.ok_or(PortEvidenceError::OpenState)?;
    let guard = lock_receipt_root(&receipt_root)?;
    Ok(PortCommandReceiptCollectionLock {
        state: state.to_path_buf(),
        receipt_root,
        _guard: guard,
    })
}

/// Remove one previously inventoried receipt after re-reading and verifying
/// its current bytes. The GC caller owns eligibility and holds the exclusive
/// publication owner lock for the entire ordered removal pass.
pub fn remove_port_command_receipt(
    state: &Path,
    lock: &PortCommandReceiptCollectionLock,
    entry: &PortCommandReceiptEntry,
) -> Result<u64, PortEvidenceError> {
    if lock.state != state {
        return Err(PortEvidenceError::Changed);
    }
    let owner = entry.receipt.owner.as_str();
    if entry.path != state.join(RECEIPTS_DIR).join(owner).join(&entry.file_name) {
        return Err(PortEvidenceError::Changed);
    }
    let Some(directory) = owner_directory_in(&lock.receipt_root, owner, false)? else {
        return Err(PortEvidenceError::Changed);
    };
    let name = OsStr::new(&entry.file_name);
    let metadata = directory
        .metadata_at(name, false)
        .map_err(PortEvidenceError::Collect)?;
    if metadata.kind != FileKind::Regular || metadata.links != 1 {
        return Err(PortEvidenceError::Changed);
    }
    let raw = directory
        .read_stable(name, RECEIPT_BYTES)
        .map_err(PortEvidenceError::Collect)?;
    let current = decode_receipt(&raw).map_err(PortEvidenceError::Decode)?;
    if current != entry.receipt || metadata.size != entry.bytes {
        return Err(PortEvidenceError::Changed);
    }
    directory.unlink(name).map_err(PortEvidenceError::Collect)?;
    directory.sync().map_err(PortEvidenceError::Collect)?;
    Ok(metadata.size)
}

fn lock_receipt_root(root: &Directory) -> Result<LockGuard, PortEvidenceError> {
    match root.try_lock(OsStr::new(RECEIPT_LOCK), LockMode::Exclusive) {
        Ok(guard) => Ok(guard),
        Err(provenant_os::fs::FsError::LockTimeout { .. }) => Err(PortEvidenceError::Busy),
        Err(error) => Err(PortEvidenceError::Open(error)),
    }
}

fn receipt_root(state: &Path, create: bool) -> Result<Option<Directory>, PortEvidenceError> {
    let root = match Directory::open_anchor(state) {
        Ok(directory) => directory,
        Err(error) if !create && error.is_not_found() => return Ok(None),
        Err(error) if create && error.is_not_found() => {
            let parent = state
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let leaf = state.file_name().ok_or(PortEvidenceError::Open(error))?;
            let parent = Directory::open_anchor(parent).map_err(PortEvidenceError::Open)?;
            match parent.mkdir(leaf, 0o700) {
                Ok(()) => parent.sync().map_err(PortEvidenceError::Open)?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(PortEvidenceError::Open(error)),
            }
            parent
                .open_verified_directory(leaf)
                .map_err(PortEvidenceError::Open)?
        }
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    let receipts = match root.open_verified_directory(OsStr::new(RECEIPTS_DIR)) {
        Ok(directory) => directory,
        Err(error) if !create && error.is_not_found() => return Ok(None),
        Err(error) if create && error.is_not_found() => {
            match root.mkdir(OsStr::new(RECEIPTS_DIR), 0o700) {
                Ok(()) => root.sync().map_err(PortEvidenceError::Open)?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(PortEvidenceError::Open(error)),
            }
            root.open_verified_directory(OsStr::new(RECEIPTS_DIR))
                .map_err(PortEvidenceError::Open)?
        }
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    Ok(Some(receipts))
}

fn owner_directory_in(
    receipts: &Directory,
    owner: &str,
    create: bool,
) -> Result<Option<Directory>, PortEvidenceError> {
    if !PortTable::PORT_NAMES.contains(&owner) {
        return Err(PortEvidenceError::PortName);
    }
    let owner_name = OsStr::new(owner);
    let owner_dir = match receipts.open_verified_directory(owner_name) {
        Ok(directory) => directory,
        Err(error) if !create && error.is_not_found() => return Ok(None),
        Err(error) if create && error.is_not_found() => {
            match receipts.mkdir(owner_name, 0o700) {
                Ok(()) => receipts.sync().map_err(PortEvidenceError::Open)?,
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(PortEvidenceError::Open(error)),
            }
            receipts
                .open_verified_directory(owner_name)
                .map_err(PortEvidenceError::Open)?
        }
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    if create {
        owner_dir.make_private().map_err(PortEvidenceError::Open)?;
        receipts.sync().map_err(PortEvidenceError::Open)?;
    }
    Ok(Some(owner_dir))
}

fn owner_directory(
    state: &Path,
    owner: &str,
    create: bool,
) -> Result<Option<Directory>, PortEvidenceError> {
    let Some(receipts) = receipt_root(state, create)? else {
        return Ok(None);
    };
    owner_directory_in(&receipts, owner, create)
}

fn private_owned_directory(directory: &Directory) -> bool {
    directory.metadata().is_ok_and(|metadata| {
        metadata.kind == FileKind::Directory
            && metadata.uid == provenant_os::fs::effective_uid()
            && metadata.mode & 0o077 == 0
    })
}

fn validate_phase(phase: &str) -> Result<(), PortEvidenceError> {
    if phase.is_empty()
        || phase.len() > 64
        || !phase
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(PortEvidenceError::Phase);
    }
    Ok(())
}

fn validate_lifetime(owner: &str, lifetime: &PortReceiptLifetime) -> Result<(), PortEvidenceError> {
    match (owner, lifetime) {
        ("admission", PortReceiptLifetime::AttemptTerminalAndRelease { .. }) => Ok(()),
        ("admission", PortReceiptLifetime::AgeDays { .. }) => Err(PortEvidenceError::Lifetime),
        (_, PortReceiptLifetime::AttemptTerminalAndRelease { .. }) => {
            Err(PortEvidenceError::Lifetime)
        }
        (_, PortReceiptLifetime::AgeDays { .. }) => Ok(()),
    }
}

fn receipt_name(phase: &str, request_key_digest: FileDigest) -> String {
    format!("{RECEIPT_PREFIX}{phase}-{request_key_digest}.json")
}

fn decode_receipt(raw: &[u8]) -> Result<PortCommandReceipt, provenant_records::DecodeError> {
    provenant_records::decode(
        Cursor::new(raw),
        Limits {
            max_bytes: RECEIPT_BYTES,
            max_depth: RECEIPT_DEPTH,
            max_members: RECEIPT_MEMBERS,
        },
    )
}

fn receipt(
    owner: &str,
    source: PortSource,
    phase: &str,
    request_key_digest: FileDigest,
    execution: &PortCommandExecution,
    lifetime: PortReceiptLifetime,
    recorded_at_unix_seconds: u64,
) -> PortCommandReceipt {
    let outcome = if execution.disposition == PortCommandDisposition::Succeeded
        && !execution.capture_complete()
    {
        PortCommandDisposition::Unknown
    } else {
        execution.disposition
    };
    let execution_outcome = execution.outcome.as_ref();
    PortCommandReceipt::new(
        owner.to_owned(),
        source,
        phase.to_owned(),
        request_key_digest,
        execution.command_digest,
        outcome,
        execution_outcome.and_then(|value| match value.termination {
            Termination::Exit(code) => Some(code),
            Termination::Signal(_) => None,
        }),
        execution_outcome.map(|value| value.elapsed_micros),
        execution_outcome.map(|value| CanonicalV1::file_digest(&value.stdout.bytes)),
        execution.capture_complete(),
        execution_outcome.is_some_and(|outcome| outcome.timed_out),
        execution_outcome.is_some_and(|outcome| outcome.cancelled),
        recorded_at_unix_seconds,
        lifetime,
    )
}

fn outcome_complete(outcome: &ExecOutcome) -> bool {
    let stream_complete = |stream: &provenant_os::exec::CapturedStream| {
        !stream.truncated
            && stream.sink_lost_bytes == 0
            && !stream.sink_failed
            && stream.oversized_frames == 0
            && stream.decoder_lost_frames == 0
            && !stream.read_failed
            && !stream.filter_failed
            && u64::try_from(stream.bytes.len()).ok() == Some(stream.total_bytes)
    };
    stream_complete(&outcome.stdout)
        && stream_complete(&outcome.stderr)
        && !outcome.timed_out
        && !outcome.drain_truncated
        && !outcome.overflow_stopped
        && !outcome.cancelled
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_os::exec::{CapturedStream, Termination};

    fn execution(bytes: &[u8], truncated: bool) -> PortCommandExecution {
        PortCommandExecution {
            disposition: PortCommandDisposition::Succeeded,
            command_digest: CanonicalV1::file_digest(b"declared argv and exits"),
            outcome: Some(ExecOutcome {
                termination: Termination::Exit(0),
                stdout: CapturedStream {
                    bytes: bytes.to_vec(),
                    total_bytes: bytes.len() as u64 + if truncated { 1 } else { 0 },
                    truncated,
                    ..CapturedStream::default()
                },
                stderr: CapturedStream::default(),
                timed_out: false,
                drain_truncated: false,
                overflow_stopped: false,
                cancelled: false,
                elapsed_micros: 17,
            }),
        }
    }

    fn age() -> PortReceiptLifetime {
        PortReceiptLifetime::AgeDays { days: 30 }
    }

    fn attempt() -> PortReceiptLifetime {
        PortReceiptLifetime::AttemptTerminalAndRelease {
            attempt_id: provenant_core::AttemptId::from_bytes([1; 16]),
            project_id: provenant_core::ProjectId::from_bytes([2; 16]),
            actor_id: provenant_core::ActorId::from_bytes([3; 16]),
            lease_epoch: 1,
        }
    }

    #[test]
    fn receipts_keep_typed_metadata_and_never_persist_command_or_output() {
        let complete = execution(b"private stdout payload", false);
        let value = serde_json::to_value(receipt(
            "tracker",
            PortSource::Project,
            "issue",
            CanonicalV1::file_digest(b"issue number 12"),
            &complete,
            age(),
            17,
        ))
        .unwrap();
        assert_eq!(value["owner"], "tracker");
        assert_eq!(value["backend"], "repository_command");
        assert_eq!(value["source"], "project");
        assert_eq!(value["phase"], "issue");
        assert_eq!(value["outcome"], "succeeded");
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["elapsed_micros"], 17);
        assert_eq!(value["recorded_at_unix_seconds"], 17);
        assert_eq!(value["lifetime"]["kind"], "age_days");
        assert!(value["schema_version"].as_u64().is_some());
        for forbidden in ["argv", "environment", "stdout", "stderr", "request_key"] {
            assert!(value.get(forbidden).is_none());
        }
        assert!(!value.to_string().contains("private stdout payload"));

        let truncated = execution(b"partial", true);
        assert!(!truncated.capture_complete());
        let value = serde_json::to_value(receipt(
            "tracker",
            PortSource::Project,
            "issue",
            CanonicalV1::file_digest(b"issue number 12"),
            &truncated,
            age(),
            18,
        ))
        .unwrap();
        assert_eq!(value["outcome"], "unknown");
        assert_eq!(value["capture_complete"], false);
    }

    #[test]
    fn success_proof_uses_declared_exit_even_when_capture_is_incomplete() {
        let state = tempfile::tempdir().unwrap();
        let partial = execution(b"partial", true);
        record_port_command(
            state.path(),
            "admission",
            PortSource::Project,
            "release",
            "attempt:epoch-1",
            &partial,
            attempt(),
        )
        .unwrap();
        assert!(!partial.capture_complete());
        assert!(
            port_command_receipt_proof(
                state.path(),
                "admission",
                PortSource::Project,
                "release",
                "attempt:epoch-1",
                partial.command_digest,
                &[0],
            )
            .unwrap()
            .is_some_and(|proof| proof.successful_exit && !proof.capture_complete)
        );
        assert!(
            port_command_receipt_proof(
                state.path(),
                "admission",
                PortSource::Host,
                "release",
                "attempt:epoch-1",
                partial.command_digest,
                &[0],
            )
            .unwrap()
            .is_none()
        );
        for (key, timed_out, cancelled) in [
            ("attempt:timeout", true, false),
            ("attempt:cancelled", false, true),
        ] {
            let mut interrupted = execution(b"", false);
            let outcome = interrupted.outcome.as_mut().expect("outcome present");
            outcome.timed_out = timed_out;
            outcome.cancelled = cancelled;
            record_port_command(
                state.path(),
                "admission",
                PortSource::Project,
                "release",
                key,
                &interrupted,
                attempt(),
            )
            .unwrap();
            assert!(
                port_command_receipt_proof(
                    state.path(),
                    "admission",
                    PortSource::Project,
                    "release",
                    key,
                    interrupted.command_digest,
                    &[0],
                )
                .unwrap()
                .is_some_and(|proof| !proof.successful_exit)
            );
        }
    }

    #[test]
    fn collection_lock_and_revalidation_preserve_refreshed_receipts() {
        let state = tempfile::tempdir().unwrap();
        record_port_command(
            state.path(),
            "tracker",
            PortSource::Project,
            "issue",
            "issue:1",
            &execution(b"initial", false),
            age(),
        )
        .unwrap();
        let mut stale_inventory = port_command_receipts(state.path()).unwrap();
        assert!(stale_inventory.complete);
        assert_eq!(stale_inventory.entries.len(), 1);
        let stale = stale_inventory.entries.pop().unwrap();

        record_port_command(
            state.path(),
            "tracker",
            PortSource::Project,
            "issue",
            "issue:1",
            &execution(b"refreshed", false),
            age(),
        )
        .unwrap();

        let lock = try_lock_port_command_receipts(state.path()).unwrap();
        let lock_inventory = port_command_receipts(state.path()).unwrap();
        assert!(lock_inventory.complete);
        assert_eq!(lock_inventory.entries.len(), 1);
        assert!(matches!(
            record_port_command(
                state.path(),
                "tracker",
                PortSource::Project,
                "issue",
                "issue:1",
                &execution(b"concurrent", false),
                age(),
            ),
            Err(PortEvidenceError::Busy)
        ));
        assert!(matches!(
            remove_port_command_receipt(state.path(), &lock, &stale),
            Err(PortEvidenceError::Changed)
        ));
        assert_eq!(
            lock_inventory.entries[0].receipt.stdout_digest,
            Some(CanonicalV1::file_digest(b"refreshed"))
        );
    }
}
