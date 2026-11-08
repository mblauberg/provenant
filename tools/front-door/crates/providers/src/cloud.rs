//! Provider-independent evidence returned by a cloud verification job.
//!
//! This module does not dispatch jobs or authenticate result channels. A caller
//! must obtain the envelope through a channel authorised for the job before
//! treating it as evidence.

use serde::{Deserialize, Serialize};

/// The remote job's report. `commit` is the full object ID of the checkout
/// that was tested, not a branch name or the commit requested in the prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudResult {
    pub job_id: String,
    pub nonce: String,
    pub commit: String,
    pub exit_status: i32,
    pub tests_run: Vec<String>,
    pub tests_failed: Vec<String>,
    pub log_location: String,
    pub tracked_files_changed: bool,
    pub observed_model: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelObservation {
    Matches,
    Differs,
    Unknown,
}

/// A result bound to the caller's expected commit. A successful exit alone is
/// insufficient when the job changed tracked files or reported failed tests.
#[derive(Debug, PartialEq, Eq)]
pub struct CheckedCloudResult {
    pub result: CloudResult,
    pub requested_model: String,
    pub model: ModelObservation,
    /// Whether the report names a log and avoids empty test names.
    pub evidence_complete: bool,
    pub passed: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CloudResultError {
    #[error("cloud result does not name the expected job")]
    JobMismatch,
    #[error("cloud result nonce does not match")]
    NonceMismatch,
    #[error("cloud result does not name the expected full commit object ID")]
    CommitMismatch,
}

fn full_oid(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl CloudResult {
    pub fn check(
        self,
        expected_job_id: &str,
        expected_nonce: &str,
        expected_commit: &str,
        requested_model: &str,
    ) -> Result<CheckedCloudResult, CloudResultError> {
        if expected_job_id.is_empty() || self.job_id != expected_job_id {
            return Err(CloudResultError::JobMismatch);
        }
        if expected_nonce.is_empty() || self.nonce != expected_nonce {
            return Err(CloudResultError::NonceMismatch);
        }
        // Git supports SHA-1 and SHA-256 object formats. Do not accept an
        // abbreviated ID, empty value, or a ref as evidence of an exact head.
        if !full_oid(expected_commit)
            || !full_oid(&self.commit)
            || !self.commit.eq_ignore_ascii_case(expected_commit)
        {
            return Err(CloudResultError::CommitMismatch);
        }
        let model = match self.observed_model.as_deref().filter(|s| !s.is_empty()) {
            Some(observed) if observed == requested_model => ModelObservation::Matches,
            Some(_) => ModelObservation::Differs,
            None => ModelObservation::Unknown,
        };
        let evidence_complete = self.tests_run.iter().all(|test| !test.trim().is_empty())
            && self.tests_failed.iter().all(|test| !test.trim().is_empty())
            && !self.log_location.trim().is_empty();
        let passed = evidence_complete
            && self.exit_status == 0
            && self.tests_failed.is_empty()
            && !self.tracked_files_changed;
        Ok(CheckedCloudResult {
            result: self,
            requested_model: requested_model.to_owned(),
            model,
            evidence_complete,
            passed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CloudResult {
        CloudResult {
            job_id: "job".into(),
            nonce: "nonce".into(),
            commit: "a".repeat(40),
            exit_status: 0,
            tests_run: vec!["targeted test".into()],
            tests_failed: vec![],
            log_location: "https://example.invalid/log".into(),
            tracked_files_changed: false,
            observed_model: Some("requested".into()),
        }
    }

    #[test]
    fn cloud_result_rejects_other_job_or_nonce() {
        let report = report();
        assert_eq!(
            report
                .clone()
                .check("other", "nonce", &"a".repeat(40), "requested"),
            Err(CloudResultError::JobMismatch)
        );
        assert_eq!(
            report.check("job", "other", &"a".repeat(40), "requested"),
            Err(CloudResultError::NonceMismatch)
        );
    }

    #[test]
    fn cloud_result_requires_exact_full_commit() {
        let result = report();
        assert!(
            result
                .clone()
                .check("job", "nonce", &"a".repeat(40), "requested")
                .unwrap()
                .passed
        );
        for expected in ["a".repeat(39), "b".repeat(40), "main".into()] {
            assert_eq!(
                result.clone().check("job", "nonce", &expected, "requested"),
                Err(CloudResultError::CommitMismatch)
            );
        }
        let mut sha256 = result;
        sha256.commit = "b".repeat(64);
        assert!(
            sha256
                .check("job", "nonce", &"b".repeat(64), "requested")
                .is_ok()
        );
    }

    #[test]
    fn cloud_result_missing_test_or_log_evidence_cannot_pass() {
        let mut result = report();
        result.tests_run = vec![" ".into()];
        let checked = result
            .clone()
            .check("job", "nonce", &"a".repeat(40), "requested")
            .unwrap();
        assert!(!checked.evidence_complete);
        assert!(!checked.passed);
        result.tests_run.clear();
        assert!(
            result
                .clone()
                .check("job", "nonce", &"a".repeat(40), "requested")
                .unwrap()
                .passed
        );
        result.log_location = " ".into();
        let checked = result
            .check("job", "nonce", &"a".repeat(40), "requested")
            .unwrap();
        assert!(!checked.evidence_complete);
        assert!(!checked.passed);
    }

    #[test]
    fn cloud_result_reports_failures_changes_and_model_observation() {
        let mut result = report();
        result.tracked_files_changed = true;
        result.observed_model = None;
        let checked = result
            .clone()
            .check("job", "nonce", &"a".repeat(40), "requested")
            .unwrap();
        assert!(!checked.passed);
        assert_eq!(checked.model, ModelObservation::Unknown);
        assert_eq!(checked.requested_model, "requested");
        result.tracked_files_changed = false;
        result.tests_failed.push("failed test".into());
        result.observed_model = Some("other".into());
        let checked = result
            .clone()
            .check("job", "nonce", &"a".repeat(40), "requested")
            .unwrap();
        assert!(!checked.passed);
        assert_eq!(checked.model, ModelObservation::Differs);
        result.tests_failed.clear();
        result.exit_status = 1;
        assert!(
            !result
                .check("job", "nonce", &"a".repeat(40), "requested")
                .unwrap()
                .passed
        );
    }
}

/// Provider-independent identity retained by the run owner after submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteJob {
    pub job_id: String,
    pub nonce: String,
    pub deadline_unix_seconds: u64,
    pub provider: String,
    pub target: String,
    pub environment: String,
    pub repository: String,
    pub branch: String,
    pub remote_id: String,
    pub commit: String,
    pub requested_model: String,
    pub requested_effort: Option<String>,
    pub state: RemoteState,
    /// Why the last submission attempt failed, kept so status can show it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submit_error: Option<String>,
    /// Set only when submission was classified a definite refusal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub submit_refused: bool,
    /// Codex transport chosen at submission; later config changes do not apply.
    #[serde(default, skip_serializing_if = "is_default_transport")]
    pub cloud_transport: provenant_config::CodexCloudTransport,
    /// Codex command chosen at submission.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_command: Option<String>,
}

fn is_default_transport(transport: &provenant_config::CodexCloudTransport) -> bool {
    *transport == provenant_config::CodexCloudTransport::default()
}

/// An actionable failure observed while the provider prepared the VM.
#[derive(Debug, PartialEq, Eq)]
pub struct EnvironmentDiagnostic<'a> {
    pub target: &'a str,
    pub environment: &'a str,
    pub detail: &'a str,
    pub remedy: &'static str,
}

const ENVIRONMENT_REMEDY: &str =
    "Create or repair the named provider environment and select the repository, then retry.";

impl RemoteJob {
    /// An unresolved submission settled as unknown, with no remote identity.
    pub fn is_unresolved_submission(&self) -> bool {
        matches!(self.state, RemoteState::Unknown { .. }) && self.remote_id.trim().is_empty()
    }
    pub fn environment_diagnostic(&self) -> Option<EnvironmentDiagnostic<'_>> {
        match &self.state {
            RemoteState::SetupFailed { detail } => Some(EnvironmentDiagnostic {
                target: &self.target,
                environment: &self.environment,
                detail,
                remedy: ENVIRONMENT_REMEDY,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemoteState {
    /// Submission was attempted or is about to be attempted; custody is unknown.
    Submitting,
    Running,
    Unknown {
        detail: String,
    },
    Finished {
        result: CloudResult,
    },
    /// The provider ended the turn without a verification result.
    Failed {
        detail: String,
    },
    /// The provider reported a terminal environment setup failure.
    SetupFailed {
        detail: String,
    },
    Cancelled,
    /// Tracking ended without confirming remote custody.
    Abandoned {
        reason: String,
    },
    CancellationUnconfirmed,
}

impl RemoteState {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Finished { .. }
                | Self::Failed { .. }
                | Self::SetupFailed { .. }
                | Self::Cancelled
                | Self::Abandoned { .. }
        )
    }
}

/// The task is supplied only at submission and is not stored in the job record.
/// The caller must apply its normal credential scan before constructing it.
#[derive(Clone, Copy)]
pub struct CloudSubmission<'a> {
    pub job: &'a RemoteJob,
    pub task: &'a str,
}

/// A provider owns transport and authentication of its observations.
pub trait CloudProvider {
    type Error;
    /// Check the target before a model turn when the provider supports it.
    fn check_environment(&mut self, request: CloudSubmission<'_>) -> Result<(), Self::Error>;
    fn submit(&mut self, request: CloudSubmission<'_>) -> Result<String, Self::Error>;
    /// Bounded text explaining a failed submission, saved with the job.
    fn failure_detail(&self, _error: &Self::Error) -> Option<String> {
        None
    }
    /// True only for a completed, definite refusal without a remote identity.
    fn submission_refused(&self, _error: &Self::Error) -> bool {
        false
    }
    /// Recover a submission only when the provider can identify it unambiguously.
    /// Record submission-time transport choices in the job before it is saved.
    fn stamp(&self, _job: &mut RemoteJob) {}
    fn recover(&mut self, _job: &RemoteJob) -> Result<Option<String>, Self::Error> {
        Ok(None)
    }
    /// Observe using the durable job context, including its repository and commit.
    fn observe(&mut self, job: &RemoteJob) -> Result<RemoteState, Self::Error>;
    /// Best-effort cleanup after an observation has been stored durably.
    fn after_save(&mut self, _job: &RemoteJob) {}
    /// True means the provider confirmed the job stopped.
    fn cancel(&mut self, job: &RemoteJob) -> Result<bool, Self::Error>;
}

/// The run owner supplies an attempt-scoped store whose save is durable.
pub trait CloudJobStore {
    type Error;
    /// Create the submission intent exactly once; an existing intent is not a retry.
    fn begin(&mut self, job: &RemoteJob) -> Result<(), Self::Error>;
    fn save(&mut self, job: &RemoteJob) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum CloudJobError<P, S> {
    Provider(P),
    Environment {
        target: String,
        environment: String,
        remedy: &'static str,
        error: P,
    },
    /// A failed submission can still have reached the provider. Do not retry.
    SubmitUnknown(P),
    /// Submission succeeded. Preserve this identity and do not blindly retry.
    SaveAfterSubmit {
        job: Box<RemoteJob>,
        error: S,
    },
    Store(S),
    /// The submission may have reached the provider but no ID was recorded.
    SubmissionUnknown,
    Result(CloudResultError),
}

/// Save the identity before returning and before the first observation.
pub fn submit<P: CloudProvider, S: CloudJobStore>(
    provider: &mut P,
    store: &mut S,
    mut job: RemoteJob,
    task: &str,
) -> Result<RemoteJob, CloudJobError<P::Error, S::Error>> {
    if !full_oid(&job.commit) {
        return Err(CloudJobError::Result(CloudResultError::CommitMismatch));
    }
    job.remote_id.clear();
    job.state = RemoteState::Submitting;
    provider
        .check_environment(CloudSubmission { job: &job, task })
        .map_err(|error| CloudJobError::Environment {
            target: job.target.clone(),
            environment: job.environment.clone(),
            remedy: ENVIRONMENT_REMEDY,
            error,
        })?;
    provider.stamp(&mut job);
    store.begin(&job).map_err(CloudJobError::Store)?;
    job.remote_id = match provider.submit(CloudSubmission { job: &job, task }) {
        Ok(id) => id,
        Err(error) => {
            if provider.submission_refused(&error) {
                let detail = provider
                    .failure_detail(&error)
                    .filter(|s| !s.trim().is_empty())
                    .map(|detail| provenant_confinement::redact::bounded_line(&detail, 600))
                    .unwrap_or_else(|| "submission unknown".into());
                job.submit_error = Some(detail.clone());
                job.submit_refused = true;
                job.state = RemoteState::Failed { detail };
                store.save(&job).map_err(CloudJobError::Store)?;
                return Ok(job);
            }
            let detail = provider
                .failure_detail(&error)
                .unwrap_or_else(|| "submission outcome unknown".into());
            job.submit_error = Some(provenant_confinement::redact::bounded_line(&detail, 600));
            store.save(&job).map_err(CloudJobError::Store)?;
            return Err(CloudJobError::SubmitUnknown(error));
        }
    };
    job.submit_error = None;
    if job.remote_id.trim().is_empty() {
        return Err(CloudJobError::SubmissionUnknown);
    }
    job.state = RemoteState::Running;
    store
        .save(&job)
        .map_err(|error| CloudJobError::SaveAfterSubmit {
            job: Box::new(job.clone()),
            error,
        })?;
    Ok(job)
}

/// Check terminal evidence before replacing the saved state.
pub fn reconcile<P: CloudProvider, S: CloudJobStore>(
    provider: &mut P,
    store: &mut S,
    job: &mut RemoteJob,
) -> Result<(), CloudJobError<P::Error, S::Error>> {
    if job.state.is_terminal() {
        return Ok(());
    }
    if job.state == RemoteState::Submitting || job.is_unresolved_submission() {
        let id = provider
            .recover(job)
            .map_err(CloudJobError::Provider)?
            .filter(|id| !id.trim().is_empty())
            .ok_or(CloudJobError::SubmissionUnknown)?;
        let mut recovered = job.clone();
        recovered.remote_id = id;
        recovered.state = RemoteState::Running;
        recovered.submit_error = None;
        store.save(&recovered).map_err(CloudJobError::Store)?;
        *job = recovered;
    }
    let mut state = provider.observe(job).map_err(CloudJobError::Provider)?;
    // Seeing a running job does not prove whether an earlier cancellation
    // request was delivered or will take effect later.
    if job.state == RemoteState::CancellationUnconfirmed
        && matches!(state, RemoteState::Running | RemoteState::Unknown { .. })
    {
        state = RemoteState::CancellationUnconfirmed;
    }
    if let RemoteState::Finished { result } = &state {
        result
            .clone()
            .check(&job.job_id, &job.nonce, &job.commit, &job.requested_model)
            .map_err(CloudJobError::Result)?;
    }
    let mut updated = job.clone();
    updated.state = state;
    store.save(&updated).map_err(CloudJobError::Store)?;
    *job = updated;
    provider.after_save(job);
    Ok(())
}

pub fn cancel<P: CloudProvider, S: CloudJobStore>(
    provider: &mut P,
    store: &mut S,
    job: &mut RemoteJob,
) -> Result<(), CloudJobError<P::Error, S::Error>> {
    if job.state.is_terminal() {
        return Ok(());
    }
    if job.state == RemoteState::Submitting || job.is_unresolved_submission() {
        return Err(CloudJobError::SubmissionUnknown);
    }
    // Persist the ambiguous outcome before contacting the provider. A crash or
    // lost response cannot otherwise distinguish an unattempted cancellation
    // from one that reached the remote service.
    let mut updated = job.clone();
    updated.state = RemoteState::CancellationUnconfirmed;
    store.save(&updated).map_err(CloudJobError::Store)?;
    *job = updated;
    let confirmed = provider.cancel(job).map_err(CloudJobError::Provider)?;
    if confirmed {
        let mut updated = job.clone();
        updated.state = RemoteState::Cancelled;
        store.save(&updated).map_err(CloudJobError::Store)?;
        *job = updated;
    }
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    struct FakeProvider {
        observed: RemoteState,
        confirmed: bool,
        submitted: usize,
    }
    impl CloudProvider for FakeProvider {
        type Error = ();
        fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
            Ok(())
        }
        fn submit(&mut self, request: CloudSubmission<'_>) -> Result<String, ()> {
            assert_eq!(request.job.commit, "a".repeat(40));
            assert_eq!(request.job.environment, "env");
            assert_eq!(request.job.repository, "owner/repo");
            assert_eq!(request.job.requested_model, "model");
            assert_eq!(request.job.requested_effort.as_deref(), Some("low"));
            assert_eq!(request.task, "run tests");
            self.submitted += 1;
            Ok("remote-1".into())
        }
        fn observe(&mut self, job: &RemoteJob) -> Result<RemoteState, ()> {
            assert_eq!(job.remote_id, "remote-1");
            assert_eq!(job.repository, "owner/repo");
            assert_eq!(job.commit, "a".repeat(40));
            Ok(self.observed.clone())
        }
        fn cancel(&mut self, job: &RemoteJob) -> Result<bool, ()> {
            assert_eq!(job.remote_id, "remote-1");
            Ok(self.confirmed)
        }
    }
    #[derive(Default)]
    struct FakeStore {
        saved: Option<RemoteJob>,
        fail: bool,
    }
    impl CloudJobStore for FakeStore {
        type Error = ();
        fn begin(&mut self, job: &RemoteJob) -> Result<(), ()> {
            if self.saved.is_some() {
                return Err(());
            }
            self.saved = Some(job.clone());
            Ok(())
        }
        fn save(&mut self, job: &RemoteJob) -> Result<(), ()> {
            if self.fail {
                Err(())
            } else {
                self.saved = Some(job.clone());
                Ok(())
            }
        }
    }
    fn job() -> RemoteJob {
        RemoteJob {
            job_id: "job".into(),
            nonce: "nonce".into(),
            deadline_unix_seconds: u64::MAX,
            provider: "fake".into(),
            target: "target".into(),
            environment: "env".into(),
            repository: "owner/repo".into(),
            branch: "main".into(),
            remote_id: String::new(),
            commit: "a".repeat(40),
            requested_model: "model".into(),
            requested_effort: Some("low".into()),
            state: RemoteState::Running,
            submit_error: None,
            submit_refused: false,
            cloud_transport: Default::default(),
            cloud_command: None,
        }
    }
    fn result(commit: &str) -> CloudResult {
        CloudResult {
            job_id: "job".into(),
            nonce: "nonce".into(),
            commit: commit.into(),
            exit_status: 0,
            tests_run: vec!["test".into()],
            tests_failed: vec![],
            log_location: "log".into(),
            tracked_files_changed: false,
            observed_model: None,
        }
    }

    #[test]
    fn cloud_definite_refusal_is_saved_as_failed() {
        struct Refusal(bool);
        impl CloudProvider for Refusal {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Ok(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                Err(())
            }
            fn failure_detail(&self, _: &()) -> Option<String> {
                Some("no environment".into())
            }
            fn submission_refused(&self, _: &()) -> bool {
                self.0
            }
            fn observe(&mut self, _: &RemoteJob) -> Result<RemoteState, ()> {
                panic!("unexpected observation")
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                panic!("unexpected cancellation")
            }
        }
        let mut store = FakeStore::default();
        let failed = submit(&mut Refusal(true), &mut store, job(), "run tests").unwrap();
        assert_eq!(
            failed.state,
            RemoteState::Failed {
                detail: "no environment".into()
            }
        );
        assert_eq!(store.saved, Some(failed));
        let mut store = FakeStore::default();
        assert!(matches!(
            submit(&mut Refusal(false), &mut store, job(), "run tests"),
            Err(CloudJobError::SubmitUnknown(()))
        ));
        let saved = store.saved.unwrap();
        assert_eq!(saved.state, RemoteState::Submitting);
        assert_eq!(saved.submit_error.as_deref(), Some("no environment"));
    }

    #[test]
    fn cloud_submit_error_never_keeps_a_credential_on_either_path() {
        struct Leaky(bool);
        impl CloudProvider for Leaky {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Ok(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                Err(())
            }
            fn failure_detail(&self, _: &()) -> Option<String> {
                Some(format!("denied for sk-{}", "a1b2c3d4".repeat(6)))
            }
            fn submission_refused(&self, _: &()) -> bool {
                self.0
            }
            fn observe(&mut self, _: &RemoteJob) -> Result<RemoteState, ()> {
                panic!("unexpected observation")
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                panic!("unexpected cancellation")
            }
        }
        for refused in [true, false] {
            let mut store = FakeStore::default();
            let _ = submit(&mut Leaky(refused), &mut store, job(), "run tests");
            let saved = store.saved.unwrap();
            let error = saved.submit_error.unwrap();
            assert!(error.starts_with("denied for"), "{error}");
            assert!(!error.contains("a1b2c3d4"), "{error}");
            if let RemoteState::Failed { detail } = &saved.state {
                assert!(refused && !detail.contains("a1b2c3d4"), "{detail}");
            }
        }
    }

    #[test]
    fn cloud_submit_records_identity_and_reconciles_after_reopen() {
        let mut provider = FakeProvider {
            observed: RemoteState::Finished {
                result: result(&"a".repeat(40)),
            },
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore::default();
        let submitted = submit(&mut provider, &mut store, job(), "run tests").unwrap();
        assert_eq!(store.saved.as_ref(), Some(&submitted));
        let mut reopened = store.saved.clone().unwrap();
        reconcile(&mut provider, &mut store, &mut reopened).unwrap();
        assert!(matches!(reopened.state, RemoteState::Finished { .. }));
        assert_eq!(store.saved.as_ref(), Some(&reopened));
        provider.observed = RemoteState::Running;
        reconcile(&mut provider, &mut store, &mut reopened).unwrap();
        cancel(&mut provider, &mut store, &mut reopened).unwrap();
        assert_eq!(store.saved, Some(reopened));
        assert_eq!(provider.submitted, 1);
    }
    #[test]
    fn cloud_rejects_wrong_commit_and_reports_unconfirmed_cancel() {
        let mut provider = FakeProvider {
            observed: RemoteState::Finished {
                result: result(&"b".repeat(40)),
            },
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore::default();
        let mut job = submit(&mut provider, &mut store, job(), "run tests").unwrap();
        assert!(matches!(
            reconcile(&mut provider, &mut store, &mut job),
            Err(CloudJobError::Result(CloudResultError::CommitMismatch))
        ));
        assert_eq!(store.saved.as_ref().unwrap().state, RemoteState::Running);
        cancel(&mut provider, &mut store, &mut job).unwrap();
        assert_eq!(job.state, RemoteState::CancellationUnconfirmed);
        provider.confirmed = true;
        cancel(&mut provider, &mut store, &mut job).unwrap();
        assert_eq!(job.state, RemoteState::Cancelled);
    }
    #[test]
    fn cloud_cancel_lost_response_retains_unconfirmed_state() {
        struct LostCancellation;
        impl CloudProvider for LostCancellation {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Ok(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                Ok("remote-1".into())
            }
            fn observe(&mut self, _: &RemoteJob) -> Result<RemoteState, ()> {
                Ok(RemoteState::Running)
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                Err(())
            }
        }
        let mut store = FakeStore::default();
        let mut job = submit(&mut LostCancellation, &mut store, job(), "run tests").unwrap();
        assert!(matches!(
            cancel(&mut LostCancellation, &mut store, &mut job),
            Err(CloudJobError::Provider(()))
        ));
        assert_eq!(job.state, RemoteState::CancellationUnconfirmed);
        assert_eq!(
            store.saved.unwrap().state,
            RemoteState::CancellationUnconfirmed
        );
    }
    #[test]
    fn cloud_reconcile_keeps_unconfirmed_cancel_until_terminal_observation() {
        let mut provider = FakeProvider {
            observed: RemoteState::Running,
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore::default();
        let mut job = submit(&mut provider, &mut store, job(), "run tests").unwrap();
        cancel(&mut provider, &mut store, &mut job).unwrap();
        let mut recovered = store.saved.clone().unwrap();
        reconcile(&mut provider, &mut store, &mut recovered).unwrap();
        assert_eq!(recovered.state, RemoteState::CancellationUnconfirmed);
        assert_eq!(store.saved.as_ref(), Some(&recovered));
        provider.observed = RemoteState::Unknown {
            detail: "deadline".into(),
        };
        reconcile(&mut provider, &mut store, &mut recovered).unwrap();
        assert_eq!(recovered.state, RemoteState::CancellationUnconfirmed);
        provider.observed = RemoteState::Finished {
            result: result(&"a".repeat(40)),
        };
        reconcile(&mut provider, &mut store, &mut recovered).unwrap();
        assert!(matches!(recovered.state, RemoteState::Finished { .. }));
        assert_eq!(store.saved, Some(recovered));
    }
    #[test]
    fn cloud_cancel_does_not_contact_provider_without_durable_intent() {
        struct CountingCancel(usize);
        impl CloudProvider for CountingCancel {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Ok(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                Ok("remote-1".into())
            }
            fn observe(&mut self, _: &RemoteJob) -> Result<RemoteState, ()> {
                Ok(RemoteState::Running)
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                self.0 += 1;
                Ok(true)
            }
        }
        let mut provider = CountingCancel(0);
        let mut store = FakeStore::default();
        let mut job = submit(&mut provider, &mut store, job(), "run tests").unwrap();
        store.fail = true;
        assert!(matches!(
            cancel(&mut provider, &mut store, &mut job),
            Err(CloudJobError::Store(()))
        ));
        assert_eq!(provider.0, 0);
        assert_eq!(job.state, RemoteState::Running);
    }
    #[test]
    fn cloud_environment_failure_names_target_and_never_submits() {
        struct Missing;
        impl CloudProvider for Missing {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Err(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                panic!("must not submit")
            }
            fn observe(&mut self, _: &RemoteJob) -> Result<RemoteState, ()> {
                panic!("must not observe")
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                panic!("must not cancel")
            }
        }
        let mut store = FakeStore::default();
        let error = submit(&mut Missing, &mut store, job(), "run tests").unwrap_err();
        assert!(
            matches!(error, CloudJobError::Environment { target, environment, remedy, .. } if target == "target" && environment == "env" && remedy.contains("Create or repair"))
        );
        assert!(store.saved.is_none());
    }

    #[test]
    fn cloud_setup_failure_after_submission_is_terminal_and_actionable() {
        let mut provider = FakeProvider {
            observed: RemoteState::SetupFailed {
                detail: "setup command failed".into(),
            },
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore::default();
        let submitted = submit(&mut provider, &mut store, job(), "run tests").unwrap();
        let mut recovered = store.saved.clone().unwrap();
        assert_eq!(submitted, recovered);
        reconcile(&mut provider, &mut store, &mut recovered).unwrap();
        let diagnostic = recovered.environment_diagnostic().unwrap();
        assert_eq!(diagnostic.target, "target");
        assert_eq!(diagnostic.environment, "env");
        assert_eq!(diagnostic.detail, "setup command failed");
        assert!(diagnostic.remedy.contains("repair"));
        provider.observed = RemoteState::Running;
        reconcile(&mut provider, &mut store, &mut recovered).unwrap();
        cancel(&mut provider, &mut store, &mut recovered).unwrap();
        assert_eq!(store.saved, Some(recovered));
    }

    #[test]
    fn cloud_recovery_saves_remote_identity_before_observation() {
        struct Recovery;
        impl CloudProvider for Recovery {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Ok(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                Err(())
            }
            fn recover(&mut self, job: &RemoteJob) -> Result<Option<String>, ()> {
                assert_eq!(job.nonce, "nonce");
                Ok(Some("recovered".into()))
            }
            fn observe(&mut self, job: &RemoteJob) -> Result<RemoteState, ()> {
                assert_eq!(job.remote_id, "recovered");
                Ok(RemoteState::Running)
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                Ok(false)
            }
        }
        let mut store = FakeStore::default();
        assert!(submit(&mut Recovery, &mut store, job(), "test").is_err());
        let mut recovered = store.saved.clone().unwrap();
        reconcile(&mut Recovery, &mut store, &mut recovered).unwrap();
        assert_eq!(store.saved.unwrap().remote_id, "recovered");
    }

    #[test]
    fn cloud_submit_failure_retains_unknown_intent() {
        struct LostResponse;
        impl CloudProvider for LostResponse {
            type Error = ();
            fn check_environment(&mut self, _: CloudSubmission<'_>) -> Result<(), ()> {
                Ok(())
            }
            fn submit(&mut self, _: CloudSubmission<'_>) -> Result<String, ()> {
                Err(())
            }
            fn observe(&mut self, _: &RemoteJob) -> Result<RemoteState, ()> {
                panic!("no remote id")
            }
            fn cancel(&mut self, _: &RemoteJob) -> Result<bool, ()> {
                panic!("no remote id")
            }
        }
        let mut store = FakeStore::default();
        assert!(matches!(
            submit(&mut LostResponse, &mut store, job(), "run tests"),
            Err(CloudJobError::SubmitUnknown(()))
        ));
        let mut recovered = store.saved.clone().unwrap();
        assert_eq!(recovered.state, RemoteState::Submitting);
        assert!(matches!(
            reconcile(&mut LostResponse, &mut store, &mut recovered),
            Err(CloudJobError::SubmissionUnknown)
        ));
        assert!(matches!(
            cancel(&mut LostResponse, &mut store, &mut recovered),
            Err(CloudJobError::SubmissionUnknown)
        ));
        assert!(matches!(
            submit(&mut LostResponse, &mut store, job(), "run tests"),
            Err(CloudJobError::Store(()))
        ));
    }

    #[test]
    fn cloud_submit_refuses_unpinned_commit_before_custody() {
        let mut provider = FakeProvider {
            observed: RemoteState::Running,
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore::default();
        let mut request = job();
        request.commit = "main".into();
        assert!(matches!(
            submit(&mut provider, &mut store, request, "run tests"),
            Err(CloudJobError::Result(CloudResultError::CommitMismatch))
        ));
        assert!(store.saved.is_none());
        assert_eq!(provider.submitted, 0);
    }

    #[test]
    fn cloud_submit_save_failure_preserves_remote_identity() {
        let mut provider = FakeProvider {
            observed: RemoteState::Running,
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore {
            saved: None,
            fail: true,
        };
        let error = submit(&mut provider, &mut store, job(), "run tests").unwrap_err();
        assert!(
            matches!(error, CloudJobError::SaveAfterSubmit { job, .. } if job.remote_id == "remote-1")
        );
        assert_eq!(provider.submitted, 1);
        let mut uncertain = store.saved.clone().unwrap();
        assert_eq!(uncertain.state, RemoteState::Submitting);
        assert!(matches!(
            reconcile(&mut provider, &mut store, &mut uncertain),
            Err(CloudJobError::SubmissionUnknown)
        ));
        assert!(matches!(
            submit(&mut provider, &mut store, job(), "run tests"),
            Err(CloudJobError::Store(()))
        ));
        assert_eq!(provider.submitted, 1);
    }
}
