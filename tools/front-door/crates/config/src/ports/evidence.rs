//! Durable, content-free evidence for declared command ports.
use super::{PortCommandDisposition, PortCommandExecution, PortSource, PortTable};
use provenant_os::{
    exec::{ExecOutcome, Termination},
    fs::{Directory, PublishMode},
};
use provenant_records::{CanonicalV1, FileDigest};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, path::Path};

#[derive(Debug, thiserror::Error)]
pub enum PortEvidenceError {
    #[error("unknown port name for command evidence")]
    PortName,
    #[error("invalid command evidence phase")]
    Phase,
    #[error("cannot open host state for command evidence")]
    Open(#[source] provenant_os::fs::FsError),
    #[error("cannot encode command evidence")]
    Encode(#[source] serde_json::Error),
    #[error("cannot read command evidence")]
    Read(#[source] provenant_os::fs::FsError),
    #[error("cannot decode command evidence")]
    Decode(#[source] serde_json::Error),
    #[error("cannot publish command evidence")]
    Publish(#[source] provenant_os::fs::FsError),
}

#[derive(Serialize)]
#[serde(transparent)]
struct PortName(&'static str);

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum CommandPortBackend {
    RepositoryCommand,
}

#[derive(Serialize)]
struct PortCommandReceipt {
    owner: PortName,
    backend: CommandPortBackend,
    source: PortSource,
    phase: String,
    request_key_digest: FileDigest,
    command_digest: FileDigest,
    outcome: PortCommandDisposition,
    exit_code: Option<i64>,
    elapsed_micros: Option<u64>,
    stdout_digest: Option<FileDigest>,
    capture_complete: bool,
    timed_out: bool,
    cancelled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PortCommandReceiptView {
    owner: String,
    backend: CommandPortBackend,
    source: PortSource,
    phase: String,
    request_key_digest: FileDigest,
    command_digest: FileDigest,
    outcome: PortCommandDisposition,
    exit_code: Option<i64>,
    elapsed_micros: Option<u64>,
    stdout_digest: Option<FileDigest>,
    capture_complete: bool,
    timed_out: bool,
    cancelled: bool,
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
) -> Result<(), PortEvidenceError> {
    let owner = PortTable::PORT_NAMES
        .iter()
        .copied()
        .find(|name| *name == owner)
        .map(PortName)
        .ok_or(PortEvidenceError::PortName)?;
    if phase.is_empty()
        || phase.len() > 64
        || !phase
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(PortEvidenceError::Phase);
    }
    let request_key_digest = CanonicalV1::file_digest(key.as_bytes());
    let receipt = receipt(owner, source, phase, request_key_digest, execution);
    let name = format!("port-command-{}-{phase}-{request_key_digest}.json", owner.0);
    let bytes = serde_json::to_vec(&receipt).map_err(PortEvidenceError::Encode)?;
    let directory = match Directory::open_anchor(state) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => {
            let parent = state
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            let name = state.file_name().ok_or(PortEvidenceError::Open(error))?;
            let parent = Directory::open_anchor(parent).map_err(PortEvidenceError::Open)?;
            match parent.mkdir(name, 0o700) {
                Ok(()) => {}
                Err(error) if error.is_exists() => {}
                Err(error) => return Err(PortEvidenceError::Open(error)),
            }
            parent
                .open_verified_directory(name)
                .map_err(PortEvidenceError::Open)?
        }
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    directory
        .atomic_write(
            OsStr::new(&name),
            ".port-command-",
            &bytes,
            PublishMode::Replace,
        )
        .map_err(PortEvidenceError::Publish)?;
    Ok(())
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
    if phase.is_empty()
        || phase.len() > 64
        || !phase
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(PortEvidenceError::Phase);
    }
    let request_key_digest = CanonicalV1::file_digest(key.as_bytes());
    let name = format!("port-command-{owner}-{phase}-{request_key_digest}.json");
    let directory = match Directory::open_anchor(state) {
        Ok(directory) => directory,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(PortEvidenceError::Open(error)),
    };
    let raw = match directory.read_bounded(OsStr::new(&name), 4096) {
        Ok(raw) => raw,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => return Err(PortEvidenceError::Read(error)),
    };
    let receipt: PortCommandReceiptView =
        serde_json::from_slice(&raw).map_err(PortEvidenceError::Decode)?;
    if receipt.owner != owner
        || !matches!(receipt.backend, CommandPortBackend::RepositoryCommand)
        || receipt.source != source
        || receipt.phase != phase
        || receipt.request_key_digest != request_key_digest
        || receipt.command_digest != command_digest
        || !matches!(
            receipt.outcome,
            PortCommandDisposition::Succeeded | PortCommandDisposition::Unknown
        )
    {
        return Ok(None);
    }
    let _evidence_metadata = (receipt.elapsed_micros, receipt.stdout_digest);
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

fn receipt(
    owner: PortName,
    source: PortSource,
    phase: &str,
    request_key_digest: FileDigest,
    execution: &PortCommandExecution,
) -> PortCommandReceipt {
    let outcome = if execution.disposition == PortCommandDisposition::Succeeded
        && !execution.capture_complete()
    {
        PortCommandDisposition::Unknown
    } else {
        execution.disposition
    };
    let execution_outcome = execution.outcome.as_ref();
    PortCommandReceipt {
        owner,
        backend: CommandPortBackend::RepositoryCommand,
        source,
        phase: phase.to_owned(),
        request_key_digest,
        command_digest: execution.command_digest,
        outcome,
        exit_code: execution_outcome.and_then(|value| match value.termination {
            Termination::Exit(code) => Some(code),
            Termination::Signal(_) => None,
        }),
        elapsed_micros: execution_outcome.map(|value| value.elapsed_micros),
        stdout_digest: execution_outcome.map(|value| CanonicalV1::file_digest(&value.stdout.bytes)),
        capture_complete: execution.capture_complete(),
        timed_out: execution_outcome.is_some_and(|outcome| outcome.timed_out),
        cancelled: execution_outcome.is_some_and(|outcome| outcome.cancelled),
    }
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

    #[test]
    fn receipts_keep_typed_metadata_and_never_persist_command_or_output() {
        let complete = execution(b"private stdout payload", false);
        let value = serde_json::to_value(receipt(
            PortName("tracker"),
            PortSource::Project,
            "issue",
            CanonicalV1::file_digest(b"issue number 12"),
            &complete,
        ))
        .unwrap();
        assert_eq!(value["owner"], "tracker");
        assert_eq!(value["backend"], "repository_command");
        assert_eq!(value["source"], "project");
        assert_eq!(value["phase"], "issue");
        assert_eq!(value["outcome"], "succeeded");
        assert_eq!(
            value["command_digest"],
            serde_json::json!(CanonicalV1::file_digest(b"declared argv and exits").to_string())
        );
        assert_eq!(value["exit_code"], 0);
        assert_eq!(value["elapsed_micros"], 17);
        assert_eq!(
            value["stdout_digest"],
            serde_json::json!(CanonicalV1::file_digest(b"private stdout payload").to_string())
        );
        assert!(value["capture_complete"].as_bool().unwrap_or(false));
        for forbidden in ["argv", "environment", "stdout", "stderr", "request_key"] {
            assert!(value.get(forbidden).is_none());
        }
        assert!(!value.to_string().contains("private stdout payload"));

        let truncated = execution(b"partial", true);
        assert!(!truncated.capture_complete());
        let value = serde_json::to_value(receipt(
            PortName("tracker"),
            PortSource::Project,
            "issue",
            CanonicalV1::file_digest(b"issue number 12"),
            &truncated,
        ))
        .unwrap();
        assert_eq!(value["outcome"], "unknown");
        assert_eq!(value["capture_complete"], false);
    }
    #[test]
    fn success_proof_uses_declared_exit_even_when_capture_is_incomplete() {
        let state = tempfile::tempdir().unwrap();
        let execution = execution(b"partial", true);
        record_port_command(
            state.path(),
            "admission",
            PortSource::Project,
            "release",
            "attempt:epoch-1",
            &execution,
        )
        .unwrap();
        assert!(!execution.capture_complete());
        assert!(
            port_command_receipt_proof(
                state.path(),
                "admission",
                PortSource::Project,
                "release",
                "attempt:epoch-1",
                execution.command_digest,
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
                execution.command_digest,
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
}
