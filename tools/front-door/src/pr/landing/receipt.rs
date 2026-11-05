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

/// Serialize pending-state recovery and publication across local land processes.
pub fn lock(state: &Path) -> Result<provenant_os::fs::LockGuard, LandingError> {
    let directory = state.join("landing");
    std::fs::create_dir_all(&directory).map_err(|e| failed("create landing state directory", e))?;
    let directory = provenant_os::fs::Directory::open_anchor(&directory)
        .map_err(|e| failed("open landing state directory", e))?;
    directory
        .try_lock(
            "publication.lock".as_ref(),
            provenant_os::fs::LockMode::Exclusive,
        )
        .map_err(|e| failed("acquire landing publication lock", e))
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

/// Record the overall run, including selections that never produced a candidate.
#[allow(clippy::disallowed_methods, reason = "unique receipt name")]
pub fn write_run_summary(
    state: &Path,
    report: &super::driver::Report,
    shadow: bool,
) -> Result<(), LandingError> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| failed("receipt clock", e))?
        .as_nanos();
    let name = format!(
        "run-{nanos:x}-{:x}-{:x}.json",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let value = serde_json::json!({"shadow": shadow, "outcomes": report.outcomes, "receipts": report.receipts, "warnings": report.warnings, "head_moves": report.head_moves, "local_follow_up": report.local_steps});
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| failed("encode run summary", e))?;
    atomic_write(&receipt_dir(state).join(name), &bytes)
}

pub fn pending_path(
    state: &Path,
    repository: &str,
    candidate: &Candidate,
) -> Result<PathBuf, LandingError> {
    Ok(pending_dir(state).join(format!(
        "{}-{}.json",
        component(repository)?,
        component(&candidate.commit)?
    )))
}

#[cfg(test)]
thread_local! { pub(super) static FAIL_SETTLE_WRITE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }

/// Persist the candidate and policy evidence before a publication can become uncertain.
pub fn record_pending(state: &Path, attempt: &PendingAttempt) -> Result<(), LandingError> {
    #[cfg(test)]
    if attempt.settled.is_some() && FAIL_SETTLE_WRITE.with(|flag| flag.replace(false)) {
        return Err(LandingError::Failed(
            "injected marker rewrite failure".into(),
        ));
    }
    let bytes = serde_json::to_vec_pretty(attempt).map_err(|e| failed("encode pending", e))?;
    atomic_write(
        &pending_path(state, &attempt.repository, &attempt.candidate)?,
        &bytes,
    )
}

/// Read pending publication attempts for a target before any retry or cleanup.
pub fn pending(
    state: &Path,
    repository: &str,
    target: &str,
) -> Result<Vec<PendingAttempt>, LandingError> {
    let entries = match std::fs::read_dir(pending_dir(state)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed("read pending directory", error)),
    };
    let mut found = Vec::new();
    let prefix = format!("{}-", component(repository)?);
    for entry in entries {
        let path = entry.map_err(|e| failed("read pending entry", e))?.path();
        if path.extension().is_none_or(|ext| ext != "json")
            || !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        {
            continue;
        }
        let bytes = std::fs::read(&path).map_err(|e| failed("read pending record", e))?;
        // A record that cannot be read is a durable-state problem: refuse rather than skip,
        // because skipping could hide an uncertain publication.
        let attempt: PendingAttempt = serde_json::from_slice(&bytes)
            .map_err(|e| failed(&format!("decode {}", path.display()), e))?;
        if attempt.repository == repository && attempt.target == target {
            found.push(attempt);
        }
    }
    found.sort_by(|a, b| a.candidate.commit.cmp(&b.candidate.commit));
    Ok(found)
}

/// Remove the durable pending marker only after its receipt is settled.
pub fn clear_pending(
    state: &Path,
    repository: &str,
    candidate: &Candidate,
) -> Result<(), LandingError> {
    let path = pending_path(state, repository, candidate)?;
    match std::fs::remove_file(&path) {
        Ok(()) => {
            let dir = std::fs::File::open(pending_dir(state))
                .map_err(|e| failed("open pending directory", e))?;
            dir.sync_all()
                .map_err(|e| failed("sync pending directory", e))
        }
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
            split: Vec::new(),
            approved_to_landed: BTreeMap::from([("head".to_string(), "landed".to_string())]),
            cost: CostEstimate::Unknown { reason: "x".into() },
            alarms: vec![],
            shadow,
            head_moves: BTreeMap::new(),
        }
    }
    pub(crate) fn attempt(target: &str, commit: &str) -> PendingAttempt {
        PendingAttempt {
            repository: "fake-repository".into(),
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
            head_moves: Default::default(),
            settled: None,
            dispatched: None,
        }
    }

    #[test]
    fn publication_lock_excludes_other_landings_until_released() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        let held = lock(dir.path())?;
        assert!(lock(dir.path()).is_err());
        drop(held);
        let _next = lock(dir.path())?;
        Ok(())
    }

    #[test]
    fn pending_survives_reload_and_is_scoped_to_target() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        assert!(pending(dir.path(), "fake-repository", "main")?.is_empty());
        record_pending(dir.path(), &attempt("main", "c1"))?;
        record_pending(dir.path(), &attempt("other", "c2"))?;
        let mut other_repository = attempt("main", "c1");
        other_repository.repository = "another-repository".into();
        record_pending(dir.path(), &other_repository)?;
        assert_eq!(
            pending(dir.path(), "another-repository", "main")?,
            vec![other_repository]
        );
        let found = pending(dir.path(), "fake-repository", "main")?;
        assert_eq!(found, vec![attempt("main", "c1")]);
        clear_pending(dir.path(), "fake-repository", &candidate("c1"))?;
        clear_pending(dir.path(), "fake-repository", &candidate("c1"))?;
        assert!(pending(dir.path(), "fake-repository", "main")?.is_empty());
        assert_eq!(pending(dir.path(), "fake-repository", "other")?.len(), 1);
        Ok(())
    }

    #[test]
    fn corrupt_pending_record_refuses_instead_of_hiding_an_attempt() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        record_pending(dir.path(), &attempt("main", "c1"))?;
        std::fs::write(
            pending_dir(dir.path()).join("fake-repository-c1.json"),
            b"{",
        )
        .map_err(|e| failed("write", e))?;
        assert!(pending(dir.path(), "fake-repository", "main").is_err());
        Ok(())
    }

    #[test]
    fn receipt_round_trips_with_shadow_flag_and_mapping() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        let mut real = receipt("c1", false);
        real.head_moves.insert(
            1,
            super::super::head_move::HeadMove::C {
                reviewed_head: "old".into(),
                reviewed_base: None,
                reason: "approval-time base unknown".into(),
            },
        );
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
