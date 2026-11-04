use super::{LandingError, model::*};
use std::path::{Path, PathBuf};

fn failed(context: &str, error: impl std::fmt::Display) -> LandingError {
    LandingError::Failed(format!("{context}: {error}"))
}

/// File-name component for a commit or target; refuses anything that could escape its directory.
fn component(value: &str) -> Result<String, LandingError> {
    if value.is_empty() {
        return Err(LandingError::Failed("empty receipt key".into()));
    }
    Ok(value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect())
}

fn pending_dir(state: &Path) -> PathBuf {
    state.join("landing").join("pending")
}

fn receipt_dir(state: &Path) -> PathBuf {
    state.join("landing").join("receipts")
}

/// Write `bytes` to `path` through a synced sibling file and a rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), LandingError> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| LandingError::Failed("receipt path has no parent".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| failed("create state directory", e))?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let staging = parent.join(format!(
        ".stage-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&staging, path));
    if let Err(error) = written {
        let _ = std::fs::remove_file(&staging);
        return Err(failed("publish record", error));
    }
    if let Ok(dir) = std::fs::File::open(parent) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Persist a receipt, including approved-to-landed mappings and shadow decisions.
///
/// `path` is the project state directory; the receipt lands under it, keyed by candidate
/// commit and shadow flag, so a shadow run never overwrites a real receipt.
pub fn write_receipt(path: &Path, receipt: &LandingReceipt) -> Result<(), LandingError> {
    let kind = if receipt.shadow { "shadow" } else { "landed" };
    let name = format!("{}.{kind}.json", component(&receipt.candidate)?);
    let bytes = serde_json::to_vec_pretty(receipt).map_err(|e| failed("encode receipt", e))?;
    atomic_write(&receipt_dir(path).join(name), &bytes)
}

/// Persist the candidate and policy evidence before a publication can become uncertain.
pub fn record_pending(state: &Path, attempt: &PendingAttempt) -> Result<(), LandingError> {
    let name = format!("{}.json", component(&attempt.candidate.commit)?);
    let bytes = serde_json::to_vec_pretty(attempt).map_err(|e| failed("encode pending", e))?;
    atomic_write(&pending_dir(state).join(name), &bytes)
}

/// Read pending publication attempts for a target before any retry or cleanup.
pub fn pending(state: &Path, target: &str) -> Result<Vec<PendingAttempt>, LandingError> {
    let entries = match std::fs::read_dir(pending_dir(state)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed("read pending directory", error)),
    };
    let mut found = Vec::new();
    for entry in entries {
        let path = entry.map_err(|e| failed("read pending entry", e))?.path();
        if path.extension().is_none_or(|ext| ext != "json") {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| failed("read pending record", e))?;
        // A record that cannot be read is a durable-state problem: refuse rather than skip,
        // because skipping could hide an uncertain publication.
        let attempt: PendingAttempt = serde_json::from_slice(&bytes)
            .map_err(|e| failed(&format!("decode {}", path.display()), e))?;
        if attempt.target == target {
            found.push(attempt);
        }
    }
    found.sort_by(|a, b| a.candidate.commit.cmp(&b.candidate.commit));
    Ok(found)
}

/// Remove the durable pending marker only after its receipt is settled.
pub fn clear_pending(state: &Path, candidate: &Candidate) -> Result<(), LandingError> {
    let path = pending_dir(state).join(format!("{}.json", component(&candidate.commit)?));
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(failed("clear pending record", error)),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::pr::landing::policy::LandPolicy;
    use provenant_cli::domains::{GateKind, LandOverride, LandSettings};
    use std::collections::BTreeMap;

    pub(crate) fn candidate(commit: &str) -> Candidate {
        Candidate {
            base: "base".into(),
            steps: vec![],
            commit: commit.into(),
            tree: "tree".into(),
            ref_name: format!("land/{commit}"),
        }
    }
    pub(crate) fn policy() -> LandPolicy {
        LandPolicy::resolve(
            &LandSettings::default(),
            &BTreeMap::new(),
            None,
            None,
            &LandOverride::default(),
        )
        .expect("default policy resolves")
    }
    pub(crate) fn definition() -> GateDefinition {
        GateDefinition {
            kind: GateKind::Checks,
            digest: "d".into(),
            checks: vec!["ci".into()],
            commands: vec![],
            issuer: None,
            executor: None,
            coverage: vec![],
            timeout_seconds: 60,
            pool: None,
            env_passthrough: Vec::new(),
        }
    }
    pub(crate) fn evidence(commit: &str) -> GateEvidence {
        GateEvidence {
            commit: commit.into(),
            tree: "tree".into(),
            definition_digest: "d".into(),
            checks: vec![],
            issuer: None,
            coverage: vec![],
            result: GateResult::Passed,
            pool: None,
            duration_seconds: None,
        }
    }
    pub(crate) fn receipt(commit: &str, shadow: bool) -> LandingReceipt {
        LandingReceipt {
            policy: policy(),
            base: "base".into(),
            candidate: commit.into(),
            tree: "tree".into(),
            candidate_ref: format!("land/{commit}"),
            candidate_steps: vec![],
            pull_requests: vec![],
            gate_definition: definition(),
            gate_evidence: evidence(commit),
            publish: PublishDecision {
                mode: PublishMode::FastForward,
                reason: "test".into(),
            },
            outcomes: BTreeMap::new(),
            approved_to_landed: BTreeMap::from([("head".to_string(), "landed".to_string())]),
            cost: CostEstimate::Unknown { reason: "x".into() },
            alarms: vec![],
            shadow,
        }
    }
    fn attempt(target: &str, commit: &str) -> PendingAttempt {
        PendingAttempt {
            target: target.into(),
            candidate: candidate(commit),
            policy: policy(),
            gate_definition: definition(),
            gate_evidence: evidence(commit),
            publish: PublishDecision {
                mode: PublishMode::RestOrdered,
                reason: "test".into(),
            },
            cost: CostEstimate::Known { micros: 1 },
        }
    }

    #[test]
    fn pending_survives_reload_and_is_scoped_to_target() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        assert!(pending(dir.path(), "main")?.is_empty());
        record_pending(dir.path(), &attempt("main", "c1"))?;
        record_pending(dir.path(), &attempt("other", "c2"))?;
        let found = pending(dir.path(), "main")?;
        assert_eq!(found, vec![attempt("main", "c1")]);
        clear_pending(dir.path(), &candidate("c1"))?;
        clear_pending(dir.path(), &candidate("c1"))?;
        assert!(pending(dir.path(), "main")?.is_empty());
        assert_eq!(pending(dir.path(), "other")?.len(), 1);
        Ok(())
    }

    #[test]
    fn corrupt_pending_record_refuses_instead_of_hiding_an_attempt() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        record_pending(dir.path(), &attempt("main", "c1"))?;
        std::fs::write(pending_dir(dir.path()).join("c1.json"), b"{")
            .map_err(|e| failed("write", e))?;
        assert!(pending(dir.path(), "main").is_err());
        Ok(())
    }

    #[test]
    fn receipt_round_trips_with_shadow_flag_and_mapping() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        let real = receipt("c1", false);
        let shadow = receipt("c1", true);
        write_receipt(dir.path(), &real)?;
        write_receipt(dir.path(), &shadow)?;
        let read = |name: &str| -> LandingReceipt {
            serde_json::from_slice(
                &std::fs::read(receipt_dir(dir.path()).join(name)).expect("receipt exists"),
            )
            .expect("receipt decodes")
        };
        assert_eq!(read("c1.landed.json"), real);
        let shadow_back = read("c1.shadow.json");
        assert!(shadow_back.shadow);
        assert_eq!(shadow_back.approved_to_landed["head"], "landed");
        Ok(())
    }

    #[test]
    fn keys_cannot_escape_the_state_directory() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        write_receipt(dir.path(), &receipt("../../evil", false))?;
        assert!(
            receipt_dir(dir.path())
                .join("______evil.landed.json")
                .exists()
        );
        assert!(write_receipt(dir.path(), &receipt("", false)).is_err());
        Ok(())
    }
}
