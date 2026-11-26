//! Host-bound declaration journal: the lane's only handoff channel.
//!
//! One journal per attempt, appended under an exclusive lock. The lane sends a
//! declaration over its relay; the host binds the attempt from its own holder
//! context, validates the payload and seals the result as a canonical line.
use crate::*;
use provenant_core::AttemptId;
use provenant_os::fs::{Directory, FsError, LockGuard, LockMode};
use provenant_records::{
    CanonicalV1,
    declaration::{
        DECLARATION_SCHEMA, DeclarationCounts, DeclarationError, DeclarationEvidence,
        DeclarationInput, DeclarationLimit, DeclarationLine, DeclarationReceipt,
        MAX_DECLARATION_JOURNAL_BYTES, SealedDeclaration,
    },
    handoff::{Handoff, HandoffError},
};
use std::{collections::HashMap, ffi::OsStr, io::Write};

const JOURNAL: &str = "declarations.jsonl";
const LOCK: &str = "declarations.lock";

/// A call's original disposition, kept so a replay returns the same reply.
#[derive(Clone)]
enum Original {
    Accepted(DeclarationReceipt),
    Rejected(HandoffError),
}

struct JournalState {
    next_sequence: u64,
    closed: bool,
    seen: HashMap<String, (provenant_records::FileDigest, Original)>,
    accepted: Option<SealedDeclaration>,
    last_error: Option<HandoffError>,
}

impl JournalState {
    fn from_lines(lines: &[DeclarationLine]) -> Self {
        let mut state = Self {
            next_sequence: 1,
            closed: false,
            seen: HashMap::new(),
            accepted: None,
            last_error: None,
        };
        for line in lines {
            match line {
                DeclarationLine::Accepted(sealed) => {
                    state.next_sequence = state.next_sequence.max(sealed.sequence + 1);
                    state.seen.insert(
                        sealed.request_id.clone(),
                        (
                            sealed.digest,
                            Original::Accepted(DeclarationReceipt {
                                sequence: sealed.sequence,
                                request_id: sealed.request_id.clone(),
                                digest: sealed.digest,
                                // A replay refreshes this from the live journal
                                // size; only the original call seals it.
                                remaining_bytes: 0,
                            }),
                        ),
                    );
                    state.accepted = Some(sealed.as_ref().clone());
                }
                DeclarationLine::Rejected {
                    sequence,
                    request_id,
                    digest,
                    error,
                } => {
                    state.next_sequence = state.next_sequence.max(sequence + 1);
                    state.seen.insert(
                        request_id.clone(),
                        (*digest, Original::Rejected(error.clone())),
                    );
                    state.last_error = Some(error.clone());
                }
                DeclarationLine::Closed => state.closed = true,
            }
        }
        state
    }

    fn evidence(&self) -> DeclarationEvidence {
        if let Some(sealed) = &self.accepted {
            DeclarationEvidence::Accepted(Box::new(sealed.clone()))
        } else if let Some(error) = &self.last_error {
            DeclarationEvidence::Rejected {
                error: error.clone(),
            }
        } else {
            DeclarationEvidence::Missing
        }
    }

    /// Counts over every line, not just the one that settles the lane: a
    /// rejected submission followed by an accepted one reports both.
    fn counts(lines: &[DeclarationLine]) -> DeclarationCounts {
        let mut counts = DeclarationCounts::default();
        for line in lines {
            match line {
                DeclarationLine::Accepted(_) => counts.accepted += 1,
                DeclarationLine::Rejected { .. } => counts.rejected += 1,
                DeclarationLine::Closed => {}
            }
        }
        counts
    }
}

// A zero-budget flock interrupted by a signal is surfaced as LockTimeout.
// A few immediate probes distinguish that transient refusal without waiting on
// another owner or spending a declaration attempt, mirroring settlement.rs.
fn try_declaration_lock(attempt: &Directory) -> provenant_os::fs::Result<LockGuard> {
    let mut result = attempt.try_lock(OsStr::new(LOCK), LockMode::Exclusive);
    for _ in 0..2 {
        if !matches!(result, Err(FsError::LockTimeout { .. })) {
            break;
        }
        result = attempt.try_lock(OsStr::new(LOCK), LockMode::Exclusive);
    }
    result
}

fn read_lines(attempt: &Directory) -> Result<Vec<DeclarationLine>> {
    let bytes = match attempt.read_bounded(OsStr::new(JOURNAL), MAX_DECLARATION_JOURNAL_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.is_not_found() => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_slice(line)
                .map_err(|_| RunError::Validation("corrupt declaration journal line"))
        })
        .collect()
}

fn encoded_line(line: &DeclarationLine) -> Result<Vec<u8>> {
    let mut bytes = CanonicalV1::bytes(line)?.as_bytes().to_vec();
    bytes.push(b'\n');
    Ok(bytes)
}

/// Append one already-admitted line. The caller owns the configurable bound
/// check; the host's own `Closed` line is always small enough to append.
fn append_line(attempt: &Directory, line: &DeclarationLine) -> Result<()> {
    let bytes = encoded_line(line)?;
    let mut file = attempt.open_append(OsStr::new(JOURNAL))?;
    file.write_all(&bytes)?;
    file.sync()?;
    Ok(())
}

/// Refuse an append that would pass the configured bound, naming the setting
/// and its source in the error so the lane can fix it in turn.
fn admit(
    bytes: &[u8],
    used: u64,
    limit: &DeclarationLimit,
) -> std::result::Result<(), DeclarationError> {
    if used.saturating_add(bytes.len() as u64) > limit.journal_bytes {
        return Err(DeclarationError::Full {
            key: limit.key.clone(),
            setting_source: limit.source.clone(),
        });
    }
    Ok(())
}

fn io(error: impl std::fmt::Display) -> DeclarationError {
    DeclarationError::Io(error.to_string())
}

impl RunStore {
    /// Append one declaration for a host-bound attempt. The attempt is always
    /// taken from the caller's holder context; a payload never selects one.
    pub fn declare(
        &self,
        attempt: AttemptId,
        input: &DeclarationInput,
        limit: &DeclarationLimit,
    ) -> std::result::Result<DeclarationReceipt, DeclarationError> {
        let dir = self.attempt_dir(attempt, false).map_err(io)?;
        let _lock = try_declaration_lock(&dir).map_err(io)?;
        let lines = read_lines(&dir).map_err(io)?;
        let state = JournalState::from_lines(&lines);
        if state.closed {
            return Err(DeclarationError::Closed);
        }
        let canonical = CanonicalV1::bytes(&input.payload).map_err(io)?;
        let digest = CanonicalV1::file_digest(canonical.as_bytes());
        if let Some((seen_digest, original)) = state.seen.get(&input.request_id) {
            if *seen_digest != digest {
                return Err(DeclarationError::Conflict);
            }
            return match original {
                Original::Accepted(receipt) => {
                    let mut receipt = receipt.clone();
                    receipt.remaining_bytes = limit.journal_bytes.saturating_sub(lines_bytes(&dir));
                    Ok(receipt)
                }
                Original::Rejected(error) => Err(DeclarationError::Invalid(error.clone())),
            };
        }
        let payload_bytes = serde_json::to_vec(&input.payload).map_err(io)?;
        let sequence = state.next_sequence;
        let handoff = match Handoff::from_json(&payload_bytes) {
            Ok(handoff) => handoff,
            Err(error) => {
                let line = DeclarationLine::Rejected {
                    sequence,
                    request_id: input.request_id.clone(),
                    digest,
                    error: error.clone(),
                };
                let bytes = encoded_line(&line).map_err(io)?;
                admit(&bytes, lines_bytes(&dir), limit)?;
                append_line(&dir, &line).map_err(io)?;
                return Err(DeclarationError::Invalid(error));
            }
        };
        let line = DeclarationLine::Accepted(Box::new(SealedDeclaration {
            schema_version: DECLARATION_SCHEMA,
            sequence,
            request_id: input.request_id.clone(),
            digest,
            handoff,
        }));
        let bytes = encoded_line(&line).map_err(io)?;
        admit(&bytes, lines_bytes(&dir), limit)?;
        append_line(&dir, &line).map_err(io)?;
        Ok(DeclarationReceipt {
            sequence,
            request_id: input.request_id.clone(),
            digest,
            remaining_bytes: limit.journal_bytes.saturating_sub(lines_bytes(&dir)),
        })
    }

    /// Close an attempt's declaration channel once and report what it settled
    /// on. Idempotent: a re-drive reads the sealed `Closed` line and appends
    /// nothing.
    pub(crate) fn close_declarations(
        &self,
        attempt: &Directory,
    ) -> Result<(DeclarationEvidence, DeclarationCounts)> {
        let _lock = try_declaration_lock(attempt)?;
        let lines = read_lines(attempt)?;
        let state = JournalState::from_lines(&lines);
        if !state.closed {
            append_line(attempt, &DeclarationLine::Closed)?;
        }
        Ok((state.evidence(), JournalState::counts(&lines)))
    }
}

fn lines_bytes(attempt: &Directory) -> u64 {
    match attempt.metadata_at(OsStr::new(JOURNAL), false) {
        Ok(metadata) => metadata.size,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context};
    use provenant_core::{CancelToken, Clock as _, OperationId};
    use provenant_records::{handoff::HandoffStatus, run::CanonicalRequest};
    use serde_json::json;
    use std::result::Result as StdResult;

    type TestResult = StdResult<(), Box<dyn std::error::Error>>;

    fn payload() -> serde_json::Value {
        json!({"schema_version":2,"status":"done","note":"fixture","remaining":[],"decisions":[],"verify":[]})
    }

    fn proposal() -> CanonicalRequest {
        serde_json::from_value(json!({
            "schema_version":1,
            "task":{
                "context":{"graph_id":"decl","node_id":"node","attempt":1},
                "prompt_digest":provenant_records::CanonicalV1::file_digest(b"private"),
                "route":{"kind":"model","provider":"fixture","model":"fixture/model","effort":null},
                "authority_digest":provenant_core::Digest::from_bytes([5;32]),
                "provider_timeout_seconds":30,"delivery_id":null
            },
            "parent":null
        }))
        .expect("canonical request")
    }

    #[test]
    fn close_is_idempotent_and_reports_last_accepted() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let attempt = AttemptId::from_bytes([7; 16]);
        store.create(
            &PreparedRun {
                attempt_id: attempt,
                operation_id: OperationId::parse("declaration-close-fixture")?,
                created_at: clock.wall_now()?,
                request: proposal(),
                prompt: "private".into(),
            },
            &call,
        )?;
        let dir = store.attempt_dir(attempt, false)?;
        let limit = DeclarationLimit::shipped();
        store.declare(
            attempt,
            &DeclarationInput {
                request_id: "first".into(),
                payload: payload(),
            },
            &limit,
        )?;
        // A later rejected line never displaces an accepted one.
        assert!(matches!(
            store.declare(
                attempt,
                &DeclarationInput {
                    request_id: "second".into(),
                    payload: json!({"schema_version":1,"status":"done"}),
                },
                &limit,
            ),
            Err(DeclarationError::Invalid(_))
        ));
        let (evidence, counts) = store.close_declarations(&dir)?;
        let DeclarationEvidence::Accepted(sealed) = &evidence else {
            panic!("expected the accepted declaration, got {evidence:?}")
        };
        assert_eq!(sealed.request_id, "first");
        assert_eq!(sealed.handoff.status, HandoffStatus::Done);
        assert_eq!(counts.accepted, 1);
        assert_eq!(counts.rejected, 1);
        let again = store.close_declarations(&dir)?;
        assert_eq!(again.0, evidence);
        assert_eq!(again.1, counts);
        assert!(matches!(
            store.declare(
                attempt,
                &DeclarationInput {
                    request_id: "third".into(),
                    payload: payload(),
                },
                &limit,
            ),
            Err(DeclarationError::Closed)
        ));
        Ok(())
    }

    /// Every file under `root`, relative to it.
    fn files(
        root: &std::path::Path,
    ) -> StdResult<std::collections::BTreeSet<String>, std::io::Error> {
        let mut found = std::collections::BTreeSet::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if let Ok(relative) = path.strip_prefix(root) {
                    found.insert(relative.to_string_lossy().into_owned());
                }
            }
        }
        Ok(found)
    }

    // Spec: docs/specs/run-settlement.md, test 7 (a lane cannot write
    // settlement records).
    #[test]
    fn a_declaration_writes_only_the_bound_attempts_journal() -> TestResult {
        let temp = tempfile::tempdir()?;
        let context = context(temp.path())?;
        let clock = clock();
        let cancel = CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let bound = AttemptId::from_bytes([7; 16]);
        let other = AttemptId::from_bytes([8; 16]);
        for (attempt, operation) in [(bound, "declaration-bound"), (other, "declaration-other")] {
            store.create(
                &PreparedRun {
                    attempt_id: attempt,
                    operation_id: OperationId::parse(operation)?,
                    created_at: clock.wall_now()?,
                    request: proposal(),
                    prompt: "private".into(),
                },
                &call,
            )?;
        }
        let before = files(temp.path())?;
        // The payload names the other attempt and a settlement record; neither
        // selects anything, since the attempt comes from the holder binding.
        let mut hostile = payload();
        hostile["attempt_id"] = json!(other.to_string());
        hostile["settlement_evidence"] = json!({"outcome": "succeeded"});
        // Accepted or rejected, the line lands in the bound journal only.
        let _ = store.declare(
            bound,
            &DeclarationInput {
                request_id: "hostile".into(),
                payload: hostile,
            },
            &DeclarationLimit::shipped(),
        );
        let after = files(temp.path())?;
        let added: Vec<_> = after.difference(&before).cloned().collect();
        let bound_dir = format!("attempts/{bound}/");
        assert!(
            added.iter().all(|path| path.contains(&bound_dir)
                && (path.ends_with(JOURNAL) || path.ends_with(LOCK))),
            "a declaration wrote outside the bound journal: {added:?}"
        );
        assert!(added.iter().any(|path| path.ends_with(JOURNAL)));
        assert!(before.difference(&after).next().is_none());
        // `create` reserves a queued attempt's private directory on disk: a
        // stored row alone has no journal to close. This happens after the
        // snapshot, so it cannot hide a stray write from the declaration above.
        let other_dir = store.attempt_dir(other, true)?;
        let (evidence, counts) = store.close_declarations(&other_dir)?;
        assert_eq!(evidence, DeclarationEvidence::Missing);
        assert_eq!(counts.accepted, 0);
        Ok(())
    }
}
