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
        .map_err(|e| match e {
            provenant_os::fs::FsError::LockTimeout { .. } => LandingError::Busy,
            e => failed("acquire landing publication lock", e),
        })
}

fn receipt_dir(state: &Path) -> PathBuf {
    state.join("landing").join("receipts")
}

/// Write `bytes` to `path` through a synced sibling file and a rename.
pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), LandingError> {
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
    let path = receipt_path(path, &receipt.candidate, receipt.shadow)?;
    let bytes = serde_json::to_vec_pretty(receipt).map_err(|e| failed("encode receipt", e))?;
    atomic_write(&path, &bytes)
}

/// Resolve the contained path for a candidate receipt without exposing its filename format.
pub fn receipt_path(state: &Path, commit: &str, shadow: bool) -> Result<PathBuf, LandingError> {
    let kind = if shadow { "shadow" } else { "landed" };
    Ok(receipt_dir(state).join(format!("{}.{kind}.json", component(commit)?)))
}

/// Read a receipt through the bounded record decoder, preserving fields from newer minor
/// releases when the receipt is written again.
#[cfg(test)]
pub fn read_receipt(
    state: &Path,
    commit: &str,
    shadow: bool,
) -> Result<Option<LandingReceipt>, LandingError> {
    let path = receipt_path(state, commit, shadow)?;
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(failed("read receipt", error)),
    };
    let receipt: LandingReceipt =
        provenant_records::decode(file, provenant_records::Limits::default())
            .map_err(|error| failed("decode receipt", error))?;
    if receipt.candidate != commit || receipt.shadow != shadow {
        return Err(LandingError::Failed("receipt identity mismatch".into()));
    }
    Ok(Some(receipt))
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
    let value = serde_json::json!({"shadow": shadow, "outcomes": report.outcomes, "receipts": report.receipts, "warnings": report.warnings, "local_follow_up": report.local_steps});
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
        let attempt: PendingAttempt =
            provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())
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
    use crate::pr::landing::test_support::LandingFixture;
    #[allow(
        unused_imports,
        reason = "used only by #[test] functions, which the worktrees driver compiles out"
    )]
    use std::collections::BTreeMap;

    pub(crate) fn candidate(commit: &str) -> Candidate {
        LandingFixture::candidate(commit)
    }
    pub(crate) fn policy() -> crate::pr::landing::policy::LandPolicy {
        LandingFixture::policy()
    }
    pub(crate) fn definition() -> GateDefinition {
        LandingFixture::definition()
    }
    pub(crate) fn evidence(commit: &str) -> GateEvidence {
        LandingFixture::evidence(commit, GateResult::Passed, None)
    }
    pub(crate) fn receipt(commit: &str, shadow: bool) -> LandingReceipt {
        LandingFixture::receipt(commit, shadow)
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
                extra: Default::default(),
            },
            cost: CostEstimate::Known {
                micros: 1,
                extra: Default::default(),
            },
            settled: None,
            dispatched: None,
            started_at: None,
            extra: Default::default(),
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
        let attempt = attempt("main", "c1");
        let path = pending_path(dir.path(), &attempt.repository, &attempt.candidate)?;
        record_pending(dir.path(), &attempt)?;
        std::fs::write(path, b"{").map_err(|e| failed("write", e))?;
        assert!(pending(dir.path(), "fake-repository", "main").is_err());
        Ok(())
    }

    #[test]
    fn receipt_round_trips_with_shadow_flag_and_mapping() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        let real = receipt("c1", false);
        let shadow = receipt("c1", true);
        write_receipt(dir.path(), &real)?;
        write_receipt(dir.path(), &shadow)?;
        assert_eq!(read_receipt(dir.path(), "c1", false)?, Some(real));
        let shadow_back = read_receipt(dir.path(), "c1", true)?.expect("shadow receipt exists");
        assert!(shadow_back.shadow);
        assert_eq!(shadow_back.approved_to_landed["head"], "landed");
        assert!(read_receipt(dir.path(), "missing", false)?.is_none());
        Ok(())
    }

    #[test]
    fn receipt_keys_are_sanitised_and_empty_keys_refused() -> Result<(), LandingError> {
        let dir = tempfile::tempdir().map_err(|e| failed("tmp", e))?;
        let hostile = receipt("../../evil", false);
        let path = receipt_path(dir.path(), &hostile.candidate, hostile.shadow)?;
        assert!(path.starts_with(dir.path()));
        write_receipt(dir.path(), &hostile)?;
        assert_eq!(
            read_receipt(dir.path(), "../../evil", false)?,
            Some(hostile)
        );
        assert!(write_receipt(dir.path(), &receipt("", false)).is_err());
        Ok(())
    }

    fn round_trip_with_future_field<T>(record: &T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned,
    {
        let mut payload = serde_json::to_value(record)?;
        payload
            .as_object_mut()
            .expect("stored records are JSON objects")
            .insert(
                "future-from-newer-minor".into(),
                serde_json::json!({"enabled": true}),
            );
        let bytes = serde_json::to_vec(&payload)?;
        let decoded: T =
            provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())?;
        assert_eq!(serde_json::to_value(decoded)?, payload);
        Ok(())
    }

    fn bounded_round_trip<T>(record: &T) -> Result<(), Box<dyn std::error::Error>>
    where
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    {
        let bytes = serde_json::to_vec(record)?;
        let decoded: T =
            provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())?;
        assert_eq!(&decoded, record);
        Ok(())
    }

    // These enums live in other crates, so the local test table cannot be
    // generated beside their declarations. Exhaustive identity matches make
    // adding a variant a compile-time prompt to extend each case table.
    fn every_gate_kind(
        value: provenant_records::landing::GateKind,
    ) -> provenant_records::landing::GateKind {
        match value {
            provenant_records::landing::GateKind::Checks
            | provenant_records::landing::GateKind::Commands => value,
        }
    }

    fn gate_kinds() -> Vec<provenant_records::landing::GateKind> {
        [
            provenant_records::landing::GateKind::Checks,
            provenant_records::landing::GateKind::Commands,
        ]
        .into_iter()
        .map(every_gate_kind)
        .collect()
    }

    fn every_gate_result(value: GateResult) -> GateResult {
        match value {
            GateResult::Passed
            | GateResult::Failed
            | GateResult::Pending
            | GateResult::Missing
            | GateResult::Skipped
            | GateResult::Neutral => value,
        }
    }

    fn gate_results() -> Vec<GateResult> {
        [
            GateResult::Passed,
            GateResult::Failed,
            GateResult::Pending,
            GateResult::Missing,
            GateResult::Skipped,
            GateResult::Neutral,
        ]
        .into_iter()
        .map(every_gate_result)
        .collect()
    }

    fn every_approval_source(
        value: provenant_cli::domains::ApprovalSource,
    ) -> provenant_cli::domains::ApprovalSource {
        match value {
            provenant_cli::domains::ApprovalSource::ProvenantReview
            | provenant_cli::domains::ApprovalSource::Forge => value,
        }
    }

    fn approval_sources() -> Vec<provenant_cli::domains::ApprovalSource> {
        [
            provenant_cli::domains::ApprovalSource::ProvenantReview,
            provenant_cli::domains::ApprovalSource::Forge,
        ]
        .into_iter()
        .map(every_approval_source)
        .collect()
    }

    fn add_future_field(value: &mut serde_json::Value, field: &str) {
        value
            .as_object_mut()
            .expect("stored values are JSON objects")
            .insert(field.into(), serde_json::json!({"from": "newer minor"}));
    }

    #[test]
    // Spec: docs/specs/state.md#strict-bounded-records
    fn stored_shapes_preserve_newer_fields_through_the_bounded_decoder()
    -> Result<(), Box<dyn std::error::Error>> {
        round_trip_with_future_field(&receipt("c1", false))?;
        round_trip_with_future_field(&attempt("main", "c1"))?;
        for mode in PublishMode::all() {
            bounded_round_trip(&mode)?;
        }
        for outcome in Outcome::all() {
            bounded_round_trip(&outcome)?;
        }
        for estimate in CostEstimate::all() {
            round_trip_with_future_field(&estimate)?;
        }
        for outcome in SettledOutcome::all() {
            round_trip_with_future_field(&outcome)?;
        }
        for kind in gate_kinds() {
            bounded_round_trip(&kind)?;
        }
        for result in gate_results() {
            bounded_round_trip(&result)?;
        }
        for source in approval_sources() {
            bounded_round_trip(&source)?;
        }

        let mut attempt = attempt("main", "candidate");
        attempt.candidate = crate::pr::landing::test_support::LandingFixture::ordered_candidate();
        let pr = &mut attempt.candidate.steps[0].pr;
        pr.approval_evidence = vec![super::super::model::ApprovalEvidence {
            source: provenant_cli::domains::ApprovalSource::Forge,
            reference: "review".into(),
            reviewer: "reviewer".into(),
            head: pr.head.clone(),
            extra: Default::default(),
        }];
        pr.accepted = Some(super::super::model::Accepted {
            reason: "accepted for test fixture".into(),
            actor: "chair".into(),
            extra: Default::default(),
        });
        let policy = LandingFixture::policy();
        attempt.policy = policy;
        let mut record = LandingFixture::receipt(&attempt.candidate.commit, false);
        record.candidate_steps = attempt.candidate.steps.clone();
        record.pull_requests = attempt
            .candidate
            .steps
            .iter()
            .map(|step| step.pr.clone())
            .collect();
        record.outcomes = BTreeMap::from([(2, Outcome::Landed)]);
        record.alarms = vec![super::super::model::Alarm {
            code: "POST_CHECK".into(),
            message: "fixture alarm".into(),
            extra: Default::default(),
        }];
        record.approved_to_landed = BTreeMap::from([("head-2".into(), "landed-2".into())]);
        attempt.settled = Some(super::super::model::SettledAttempt {
            record,
            outcomes: BTreeMap::from([(
                2,
                super::super::model::SettledOutcome::Landed {
                    commit: "landed-2".into(),
                    extra: Default::default(),
                },
            )]),
            landed_commit: Some("landed-2".into()),
            refusal: None,
            receipt_pending: true,
            tree_pending: false,
            follow_pending: false,
            local_follow_pending: false,
            cleanup_pending: true,
            extra: Default::default(),
        });
        let mut payload = serde_json::to_value(&attempt)?;
        add_future_field(&mut payload, "future-attempt-field");
        add_future_field(&mut payload["candidate"], "future-candidate-field");
        add_future_field(&mut payload["candidate"]["steps"][0], "future-step-field");
        add_future_field(
            &mut payload["candidate"]["steps"][0]["pr"],
            "future-pr-field",
        );
        add_future_field(
            &mut payload["candidate"]["steps"][0]["pr"]["accepted"],
            "future-acceptance-field",
        );
        add_future_field(
            &mut payload["candidate"]["steps"][0]["pr"]["approval_evidence"][0],
            "future-approval-field",
        );
        // Closed gate and port declarations discard unknown fields; only the
        // records with explicit extension maps promise to preserve them.
        add_future_field(&mut payload["gate_evidence"], "future-evidence-field");
        add_future_field(&mut payload["policy"], "future-policy-field");
        let policy_fields = payload["policy"]["fields"]
            .as_object_mut()
            .expect("policy fields are a map");
        let sourced = policy_fields
            .values_mut()
            .next()
            .expect("resolved policy has sourced fields");
        sourced["source"]["rule"] = serde_json::json!({"index": 0, "target": "main"});
        add_future_field(sourced, "future-sourced-field");
        add_future_field(&mut sourced["source"], "future-source-field");
        add_future_field(&mut payload["publish"], "future-publish-field");
        add_future_field(&mut payload["cost"], "future-cost-field");
        add_future_field(&mut payload["settled"], "future-settlement-field");
        add_future_field(
            &mut payload["settled"]["record"],
            "future-settled-receipt-field",
        );
        add_future_field(
            &mut payload["settled"]["record"]["candidate_steps"][0],
            "future-settled-step-field",
        );
        add_future_field(
            &mut payload["settled"]["record"]["pull_requests"][0],
            "future-settled-pr-field",
        );
        add_future_field(
            &mut payload["settled"]["record"]["alarms"][0],
            "future-alarm-field",
        );
        add_future_field(
            &mut payload["settled"]["outcomes"]["2"],
            "future-settled-outcome-field",
        );
        let sourced = payload["policy"]["fields"]
            .as_object_mut()
            .expect("policy fields are a map")
            .values_mut()
            .next()
            .expect("resolved policy has sourced fields");
        add_future_field(&mut sourced["source"]["rule"], "future-target-rule-field");
        let bytes = serde_json::to_vec(&payload)?;
        let decoded: PendingAttempt =
            provenant_records::decode(bytes.as_slice(), provenant_records::Limits::default())?;
        assert_eq!(serde_json::to_value(decoded)?, payload);
        Ok(())
    }
}
