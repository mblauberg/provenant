//! Settlement snapshots of the shared handoff, independent of provider history.
use crate::{
    artifacts::{read_sealed, seal},
    *,
};
use provenant_os::fs::Directory;
use provenant_records::{
    AttemptFile,
    handoff::{HANDOFF_BYTES, Handoff},
    run::{PathScope, WriteSource},
};
use std::{
    ffi::OsStr,
    path::{Component, Path},
};

pub(crate) fn guidance() -> &'static str {
    "Keep the handoff current as work progresses, writing JSON atomically to the handoff.json path supplied below, outside the commit set. End your final answer with exactly one fenced handoff block, with nothing after it. Use schema_version 2 and status done, stopped_early or blocked; include note, remaining[], decisions[] and verify[] (verification performed and results). Use done only when the brief is finished. The JSON is at most 12288 bytes; note and each array entry are 1..4096 bytes, each array at most 64 strings. Example for unfinished work:\n```handoff\n{\"schema_version\":2,\"status\":\"stopped_early\",\"note\":\"what remains\",\"remaining\":[\"next step\"],\"decisions\":[],\"verify\":[]}\n```"
}

impl RunStore {
    /// Read the host's immutable settlement snapshot, never provider history.
    pub fn handoff(&self, record: &AttemptRecord) -> Result<Option<Handoff>> {
        let attempt = self.attempt_dir(record.run_id, record.attempt_id, false)?;
        read_snapshot(&attempt)
    }

    pub(crate) fn capture_handoff(
        &self,
        attempt: &Directory,
        record: &AttemptRecord,
        decoded: &Decoded,
    ) -> Result<Option<Handoff>> {
        if let Some(handoff) = read_snapshot(attempt)? {
            return Ok(Some(handoff));
        }
        let interrupted = !decoded.complete
            || matches!(
                decoded.outcome,
                Outcome::QuotaLimited
                    | Outcome::ModelUnavailable
                    | Outcome::Failed
                    | Outcome::TimedOut
                    | Outcome::Lost
            );
        let handoff = match Handoff::from_result(&decoded.result) {
            Ok(Some(handoff)) => Some(handoff),
            // The provider may die partway through its final fence. A complete
            // malformed answer remains ambiguous; an interrupted one can use
            // the last valid, atomically written progress snapshot.
            _ if interrupted => self.progress_handoff(attempt, record).ok().flatten(),
            _ => None,
        };
        // Provider-writable progress is output, not previously scanned input.
        // Sanitise every JSON field, including forward fields, before keeping
        // or disclosing the snapshot to a successor account or provider.
        let handoff = handoff.and_then(|handoff| {
            let raw = serde_json::to_vec(&handoff).ok()?;
            let safe = provenant_confinement::redact::known_shapes(&raw).ok()?;
            Handoff::from_json(&safe).ok()
        });
        if let Some(handoff) = &handoff {
            seal(
                attempt,
                AttemptFile::Handoff.name(),
                &serde_json::to_vec(handoff)
                    .map_err(|_| RunError::Validation("handoff encoding"))?,
                HANDOFF_BYTES as u64,
            )?;
        }
        Ok(handoff)
    }

    fn progress_handoff(
        &self,
        attempt: &Directory,
        record: &AttemptRecord,
    ) -> Result<Option<Handoff>> {
        let expected = self
            .context
            .state_root
            .as_path()
            .join("runs")
            .join(record.run_id.to_string())
            .join("attempts")
            .join(record.attempt_id.to_string())
            .join("provider-state");
        let mut grants = record
            .write_grants
            .iter()
            .filter(|grant| grant.source == WriteSource::PrivateState);
        let Some(grant) = grants.next() else {
            return Ok(None);
        };
        if grants.next().is_some() || grant.scope != PathScope::Subtree {
            return Ok(None);
        }
        // The applied grant pins the launched candidate. Never scan other
        // candidates, follow paths from model text or open provider credentials.
        let granted = grant.path.to_path_buf()?;
        let relative = match granted.strip_prefix(&expected) {
            Ok(relative) => relative,
            Err(_) => return Ok(None),
        };
        let mut parts = relative.components();
        let Some(Component::Normal(candidate)) = parts.next() else {
            return Ok(None);
        };
        if parts.next().is_some() {
            return Ok(None);
        }
        let state = attempt.open_verified_directory(OsStr::new("provider-state"))?;
        let candidate = state.open_verified_directory(candidate)?;
        let Some(bytes) = read_sealed(&candidate, "handoff.json", HANDOFF_BYTES as u64)? else {
            return Ok(None);
        };
        Ok(Handoff::from_json(&bytes).ok())
    }
}

fn read_snapshot(attempt: &Directory) -> Result<Option<Handoff>> {
    read_sealed(attempt, AttemptFile::Handoff.name(), HANDOFF_BYTES as u64)?
        .map(|bytes| Handoff::from_json(&bytes).map_err(RunError::Validation))
        .transpose()
}

pub fn handoff_progress_guidance(path: &Path) -> String {
    format!(
        "Progress handoff JSON: {}. Update this file atomically throughout the task and repeat its current value in the final handoff fence. It is lane state, never a repository file or a commit.\n",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{call, clock, context, exit, input, publication, seal_observation};
    use provenant_records::AbsolutePath;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    fn snapshot(status: &str) -> Vec<u8> {
        serde_json::json!({"schema_version":2,"status":status,"note":"Parser committed; renderer remains.","remaining":["Finish renderer"],"decisions":["Keep the parser API"],"verify":["Parser tests passed"]}).to_string().into_bytes()
    }
    fn decoded() -> Decoded {
        Decoded {
            outcome: Outcome::QuotaLimited,
            result: String::new(),
            complete: false,
            observed_route: None,
            usage: Default::default(),
            session: None,
            reason: None,
            partial_signals: vec![],
        }
    }
    fn grant(record: &mut AttemptRecord, candidate: &Path) -> TestResult {
        record.write_grants = vec![WriteGrantRecord {
            extra: Default::default(),
            source: WriteSource::PrivateState,
            path: AbsolutePath::new(candidate)?,
            scope: PathScope::Subtree,
        }];
        Ok(())
    }

    #[test]
    fn provider_death_retains_progress_and_supplies_the_fresh_retry_brief() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempts[0].attempt_id;
        let mut old = store.record(id, &call)?;
        let attempt = store.attempt_dir(old.run_id, id, false)?;
        let root = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&root, OsStr::new("candidate-1"), true)?;
        let secret_shape = "sk-abcdefghijklmnopqrstuvwx";
        let mut progress: serde_json::Value = serde_json::from_slice(&snapshot("stopped_early"))?;
        progress["future"] = serde_json::json!({"token": secret_shape});
        seal(
            &candidate,
            "handoff.json",
            &serde_json::to_vec(&progress)?,
            HANDOFF_BYTES as u64,
        )?;
        let candidate_path = store.context.state_root.as_path().join(format!(
            "runs/{}/attempts/{id}/provider-state/candidate-1",
            old.run_id
        ));
        grant(&mut old, &candidate_path)?;
        let mut interrupted = decoded();
        interrupted.result = "```handoff\n{\"schema_version\":2,".into();
        let handoff = store
            .capture_handoff(&attempt, &old, &interrupted)?
            .ok_or("missing snapshot")?;
        assert_eq!(handoff.remaining, ["Finish renderer"]);
        assert!(!handoff.fenced().contains(secret_shape));
        assert!(handoff.fenced().contains("[REDACTED]"));
        // Progress survives temporary cleanup and provider history expiry.
        crate::artifacts::remove_entry_tree(&attempt, OsStr::new("provider-state"))?;
        assert_eq!(store.handoff(&old)?, Some(handoff.clone()));
        let retry = PolicyRetry {
            schema_version: 1,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Continue on the next eligible account.".into(),
            not_before_unix_seconds: None,
        };
        store.stage_policy_retry(&old, &retry, &call)?;
        store
            .db
            .execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
        let mut publication = publication();
        publication.outcome = Outcome::QuotaLimited;
        store.publish_terminal(id, 1, &publication, &exit(), &call)?;
        let next = store.pending_attempts(input.run_id, &call)?.remove(0);
        let prompt =
            String::from_utf8(store.artifact_bytes(next.run_id, &next.prompt, RECORD_LIMIT)?)?;
        assert!(prompt.contains("exact prompt") && prompt.contains(&handoff.fenced()));
        assert!(!prompt.contains(secret_shape));
        assert!(prompt.contains("Keep the parser API") && prompt.contains("Parser tests passed"));
        assert_eq!(next.authority_digest, old.authority_digest);
        assert!(next.applied_route.is_none());
        Ok(())
    }

    #[test]
    fn oversized_handoff_brief_does_not_block_settlement() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let mut input = input();
        input.attempts[0].prompt = "x".repeat(RECORD_LIMIT as usize - 64);
        input.request.tasks[0].prompt_digest =
            provenant_records::CanonicalV1::file_digest(input.attempts[0].prompt.as_bytes());
        store.create(&input, &call)?;
        seal_observation(&store, &input, &exit())?;
        let id = input.attempts[0].attempt_id;
        let old = store.record(id, &call)?;
        let attempt = store.attempt_dir(old.run_id, id, false)?;
        let mut result = decoded();
        result.result = Handoff::from_json(&snapshot("stopped_early"))?.fenced();
        store.capture_handoff(&attempt, &old, &result)?;
        let retry = PolicyRetry {
            schema_version: 1,
            route: old.requested_route.clone(),
            resume: false,
            max_attempts: 2,
            instruction: "Continue.".into(),
            not_before_unix_seconds: None,
        };
        assert!(!store.stage_policy_retry(&old, &retry, &call)?);
        assert!(store.staged_policy_retry(&old)?.is_none());
        store
            .db
            .execute("DELETE FROM admission WHERE attempt=?1", [id.to_string()])?;
        let mut publication = publication();
        publication.outcome = Outcome::QuotaLimited;
        let terminal = store.publish_terminal(id, 1, &publication, &exit(), &call)?;
        assert!(matches!(
            terminal.state,
            AttemptState::Terminal {
                outcome: Outcome::QuotaLimited,
                ..
            }
        ));
        assert!(store.pending_attempts(input.run_id, &call)?.is_empty());
        Ok(())
    }

    #[test]
    fn final_answer_precedes_progress_and_bad_progress_never_guesses_completion() -> TestResult {
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        let attempt = store.attempt_dir(record.run_id, record.attempt_id, false)?;
        let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
        let base = store.context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}/provider-state",
            record.run_id, record.attempt_id
        ));
        grant(&mut record, &base.join("candidate-0"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .is_none()
        );
        seal(
            &candidate,
            "handoff.json",
            &snapshot("stopped_early"),
            HANDOFF_BYTES as u64,
        )?;
        // Another candidate's progress is not the applied candidate's evidence.
        grant(&mut record, &base.join("candidate-1"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .is_none()
        );
        grant(&mut record, &base.join("candidate-0"))?;
        let mut result = decoded();
        result.outcome = Outcome::Succeeded;
        result.complete = true;
        result.result = "```handoff\n{}\n```".into();
        assert!(store.capture_handoff(&attempt, &record, &result)?.is_none());
        result.outcome = Outcome::Succeeded;
        result.complete = true;
        result.result = "Completed prose without a declaration".into();
        assert!(store.capture_handoff(&attempt, &record, &result)?.is_none());
        result.result = Handoff::from_json(&snapshot("done"))?.fenced();
        assert!(
            !store
                .capture_handoff(&attempt, &record, &result)?
                .ok_or("missing final")?
                .unfinished()
        );
        Ok(())
    }

    #[test]
    fn progress_cannot_follow_links_or_escape_the_applied_attempt() -> TestResult {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir()?;
        let clock = clock();
        let cancel = provenant_core::CancelToken::new();
        let call = call(&clock, &cancel);
        let store = RunStore::open(&context(temp.path())?, &call)?;
        let input = input();
        store.create(&input, &call)?;
        let mut record = store.record(input.attempts[0].attempt_id, &call)?;
        let attempt = store.attempt_dir(record.run_id, record.attempt_id, false)?;
        let state = crate::artifacts::subdir(&attempt, OsStr::new("provider-state"), true)?;
        let candidate = crate::artifacts::subdir(&state, OsStr::new("candidate-0"), true)?;
        let base = store.context.state_root.as_path().join(format!(
            "runs/{}/attempts/{}/provider-state",
            record.run_id, record.attempt_id
        ));
        grant(&mut record, &base.join("candidate-0"))?;
        let outside = temp.path().join("outside.json");
        std::fs::write(&outside, snapshot("stopped_early"))?;
        symlink(&outside, base.join("candidate-0/handoff.json"))?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .is_none()
        );
        candidate.unlink(OsStr::new("handoff.json"))?;
        seal(
            &candidate,
            "handoff.json",
            &vec![b'x'; HANDOFF_BYTES + 1],
            (HANDOFF_BYTES + 1) as u64,
        )?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .is_none()
        );
        grant(&mut record, temp.path())?;
        assert!(
            store
                .capture_handoff(&attempt, &record, &decoded())?
                .is_none()
        );
        Ok(())
    }
}
