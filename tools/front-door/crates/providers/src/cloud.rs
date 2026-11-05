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
    pub passed: bool,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum CloudResultError {
    #[error("cloud result does not name the expected full commit object ID")]
    CommitMismatch,
}

impl CloudResult {
    pub fn check(
        self,
        expected_commit: &str,
        requested_model: &str,
    ) -> Result<CheckedCloudResult, CloudResultError> {
        // Git supports SHA-1 and SHA-256 object formats. Do not accept an
        // abbreviated ID, empty value, or a ref as evidence of an exact head.
        let full_oid =
            |s: &str| matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit());
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
        let passed =
            self.exit_status == 0 && self.tests_failed.is_empty() && !self.tracked_files_changed;
        Ok(CheckedCloudResult {
            result: self,
            requested_model: requested_model.to_owned(),
            model,
            passed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> CloudResult {
        CloudResult {
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
    fn cloud_result_requires_exact_full_commit() {
        let result = report();
        assert!(
            result
                .clone()
                .check(&"a".repeat(40), "requested")
                .unwrap()
                .passed
        );
        for expected in ["a".repeat(39), "b".repeat(40), "main".into()] {
            assert_eq!(
                result.clone().check(&expected, "requested"),
                Err(CloudResultError::CommitMismatch)
            );
        }
        let mut sha256 = result;
        sha256.commit = "b".repeat(64);
        assert!(sha256.check(&"b".repeat(64), "requested").is_ok());
    }

    #[test]
    fn cloud_result_reports_failures_changes_and_model_observation() {
        let mut result = report();
        result.tracked_files_changed = true;
        result.observed_model = None;
        let checked = result.clone().check(&"a".repeat(40), "requested").unwrap();
        assert!(!checked.passed);
        assert_eq!(checked.model, ModelObservation::Unknown);
        assert_eq!(checked.requested_model, "requested");
        result.tracked_files_changed = false;
        result.tests_failed.push("failed test".into());
        result.observed_model = Some("other".into());
        let checked = result.clone().check(&"a".repeat(40), "requested").unwrap();
        assert!(!checked.passed);
        assert_eq!(checked.model, ModelObservation::Differs);
        result.tests_failed.clear();
        result.exit_status = 1;
        assert!(!result.check(&"a".repeat(40), "requested").unwrap().passed);
    }
}

/// Provider-independent identity retained by the run owner after submission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteJob {
    pub provider: String,
    pub target: String,
    pub environment: String,
    pub remote_id: String,
    pub commit: String,
    pub requested_model: String,
    pub state: RemoteState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RemoteState {
    Running,
    Finished { result: CloudResult },
    Cancelled,
    CancellationUnconfirmed,
}

/// A provider owns transport and authentication of its observations.
pub trait CloudProvider {
    type Error;
    fn submit(&mut self) -> Result<String, Self::Error>;
    fn observe(&mut self, remote_id: &str) -> Result<RemoteState, Self::Error>;
    /// True means the provider confirmed the job stopped.
    fn cancel(&mut self, remote_id: &str) -> Result<bool, Self::Error>;
}

/// The run owner supplies an attempt-scoped store whose save is durable.
pub trait CloudJobStore {
    type Error;
    fn save(&mut self, job: &RemoteJob) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum CloudJobError<P, S> {
    Provider(P),
    /// Submission succeeded. Preserve this identity and do not blindly retry.
    SaveAfterSubmit {
        job: RemoteJob,
        error: S,
    },
    Store(S),
    Result(CloudResultError),
}

/// Save the identity before returning and before the first observation.
pub fn submit<P: CloudProvider, S: CloudJobStore>(
    provider: &mut P,
    store: &mut S,
    mut job: RemoteJob,
) -> Result<RemoteJob, CloudJobError<P::Error, S::Error>> {
    job.remote_id = provider.submit().map_err(CloudJobError::Provider)?;
    job.state = RemoteState::Running;
    store
        .save(&job)
        .map_err(|error| CloudJobError::SaveAfterSubmit {
            job: job.clone(),
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
    if matches!(
        job.state,
        RemoteState::Finished { .. } | RemoteState::Cancelled
    ) {
        return Ok(());
    }
    let state = provider
        .observe(&job.remote_id)
        .map_err(CloudJobError::Provider)?;
    if let RemoteState::Finished { result } = &state {
        result
            .clone()
            .check(&job.commit, &job.requested_model)
            .map_err(CloudJobError::Result)?;
    }
    let mut updated = job.clone();
    updated.state = state;
    store.save(&updated).map_err(CloudJobError::Store)?;
    *job = updated;
    Ok(())
}

pub fn cancel<P: CloudProvider, S: CloudJobStore>(
    provider: &mut P,
    store: &mut S,
    job: &mut RemoteJob,
) -> Result<(), CloudJobError<P::Error, S::Error>> {
    if matches!(
        job.state,
        RemoteState::Finished { .. } | RemoteState::Cancelled
    ) {
        return Ok(());
    }
    let confirmed = provider
        .cancel(&job.remote_id)
        .map_err(CloudJobError::Provider)?;
    let mut updated = job.clone();
    updated.state = if confirmed {
        RemoteState::Cancelled
    } else {
        RemoteState::CancellationUnconfirmed
    };
    store.save(&updated).map_err(CloudJobError::Store)?;
    *job = updated;
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
        fn submit(&mut self) -> Result<String, ()> {
            self.submitted += 1;
            Ok("remote-1".into())
        }
        fn observe(&mut self, id: &str) -> Result<RemoteState, ()> {
            assert_eq!(id, "remote-1");
            Ok(self.observed.clone())
        }
        fn cancel(&mut self, id: &str) -> Result<bool, ()> {
            assert_eq!(id, "remote-1");
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
            provider: "fake".into(),
            target: "target".into(),
            environment: "env".into(),
            remote_id: String::new(),
            commit: "a".repeat(40),
            requested_model: "model".into(),
            state: RemoteState::Running,
        }
    }
    fn result(commit: &str) -> CloudResult {
        CloudResult {
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
    fn cloud_submit_records_identity_and_reconciles_after_reopen() {
        let mut provider = FakeProvider {
            observed: RemoteState::Finished {
                result: result(&"a".repeat(40)),
            },
            confirmed: false,
            submitted: 0,
        };
        let mut store = FakeStore::default();
        let submitted = submit(&mut provider, &mut store, job()).unwrap();
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
        let mut job = submit(&mut provider, &mut store, job()).unwrap();
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
        let error = submit(&mut provider, &mut store, job()).unwrap_err();
        assert!(
            matches!(error, CloudJobError::SaveAfterSubmit { job, .. } if job.remote_id == "remote-1")
        );
        assert_eq!(provider.submitted, 1);
    }
}
