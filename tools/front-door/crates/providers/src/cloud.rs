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
