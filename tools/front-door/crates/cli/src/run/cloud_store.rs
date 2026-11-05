//! Attempt-scoped persistence for cloud job identities and observations.
//!
//! The caller supplies a trusted private attempt directory and holds its run
//! lock across each read/modify/write. This store does not discover jobs or
//! dispatch providers.
use provenant_os::fs::{Directory, FsError, PublishMode};
use provenant_providers::cloud::{CloudJobStore, RemoteJob, RemoteState};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, ffi::OsStr};

const FILE: &str = "cloud-job.json";
const LIMIT: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    File(#[from] FsError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(
        "cloud job record has an unsupported version, changed identity or conflicting terminal state"
    )]
    Identity,
}

#[derive(Serialize, Deserialize)]
struct Record {
    schema_version: u64,
    job: RemoteJob,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

/// A descriptor-bound store in an already-open attempt directory.
pub struct AttemptCloudStore {
    directory: Directory,
}

impl AttemptCloudStore {
    pub fn new(directory: Directory) -> Self {
        Self { directory }
    }

    fn record(&self) -> Result<Option<Record>, Error> {
        let bytes = match self.directory.read_stable(OsStr::new(FILE), LIMIT) {
            Ok(bytes) => bytes,
            Err(error) if error.is_not_found() => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let record: Record = serde_json::from_slice(&bytes)?;
        if record.schema_version != 2 {
            return Err(Error::Identity);
        }
        validate_result(&record.job)?;
        Ok(Some(record))
    }

    pub fn load(&self) -> Result<Option<RemoteJob>, Error> {
        Ok(self.record()?.map(|record| record.job))
    }
}

fn validate_result(job: &RemoteJob) -> Result<(), Error> {
    if let RemoteState::Finished { result } = &job.state {
        result
            .clone()
            .check(&job.commit, &job.requested_model)
            .map_err(|_| Error::Identity)?;
    }
    Ok(())
}

impl CloudJobStore for AttemptCloudStore {
    type Error = Error;

    fn save(&mut self, job: &RemoteJob) -> Result<(), Error> {
        validate_result(job)?;
        let previous = self.record()?;
        if let Some(record) = &previous {
            let old = &record.job;
            // Once terminal evidence is durable, a stale observer must not
            // replace it with a later sample or a different terminal result.
            if matches!(
                old.state,
                RemoteState::Finished { .. } | RemoteState::Cancelled
            ) && old.state != job.state
            {
                return Err(Error::Identity);
            }
            if (
                &old.provider,
                &old.target,
                &old.environment,
                &old.repository,
                &old.remote_id,
                &old.commit,
                &old.requested_model,
                &old.requested_effort,
            ) != (
                &job.provider,
                &job.target,
                &job.environment,
                &job.repository,
                &job.remote_id,
                &job.commit,
                &job.requested_model,
                &job.requested_effort,
            ) {
                return Err(Error::Identity);
            }
        }
        let mode = if previous.is_some() {
            PublishMode::Replace
        } else {
            PublishMode::NoReplace
        };
        let record = Record {
            schema_version: 2,
            job: job.clone(),
            extra: previous.map(|record| record.extra).unwrap_or_default(),
        };
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() as u64 > LIMIT {
            return Err(FsError::TooLarge { limit: LIMIT }.into());
        }
        self.directory
            .atomic_write(OsStr::new(FILE), ".cloud-job-", &bytes, mode)?
            .require_durable()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_providers::cloud::CloudResult;

    fn job() -> RemoteJob {
        RemoteJob {
            provider: "fake".into(),
            target: "test".into(),
            environment: "env".into(),
            repository: "owner/repo".into(),
            remote_id: "remote-1".into(),
            commit: "a".repeat(40),
            requested_model: "model".into(),
            requested_effort: Some("low".into()),
            state: RemoteState::Running,
        }
    }

    #[test]
    fn cloud_store_reopens_and_preserves_identity() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let mut store = AttemptCloudStore::new(Directory::open_anchor(temp.path())?);
        assert_eq!(store.load()?, None);
        let mut original = job();
        store.save(&original)?;
        drop(store);
        let mut reopened = AttemptCloudStore::new(Directory::open_anchor(temp.path())?);
        assert_eq!(reopened.load()?, Some(original.clone()));
        let mut other = original.clone();
        other.remote_id = "remote-2".into();
        assert!(matches!(reopened.save(&other), Err(Error::Identity)));
        assert_eq!(reopened.load()?, Some(original.clone()));
        let mut wrong_result = original.clone();
        wrong_result.state = RemoteState::Finished {
            result: CloudResult {
                commit: "b".repeat(40),
                exit_status: 0,
                tests_run: vec![],
                tests_failed: vec![],
                log_location: "log".into(),
                tracked_files_changed: false,
                observed_model: None,
            },
        };
        assert!(matches!(reopened.save(&wrong_result), Err(Error::Identity)));
        original.state = RemoteState::CancellationUnconfirmed;
        reopened.save(&original)?;
        assert_eq!(reopened.load()?, Some(original.clone()));
        original.state = RemoteState::Cancelled;
        reopened.save(&original)?;
        let mut stale = original.clone();
        stale.state = RemoteState::Running;
        assert!(matches!(reopened.save(&stale), Err(Error::Identity)));
        assert_eq!(reopened.load()?, Some(original));
        Ok(())
    }
    #[test]
    fn cloud_store_recovers_submitted_job_for_reconciliation()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_providers::cloud::{CloudProvider, reconcile, submit};
        struct Fake;
        impl CloudProvider for Fake {
            type Error = ();
            fn submit(
                &mut self,
                request: provenant_providers::cloud::CloudSubmission<'_>,
            ) -> Result<String, ()> {
                assert_eq!(request.task, "run tests");
                assert_eq!(request.job.repository, "owner/repo");
                Ok("remote-1".into())
            }
            fn observe(&mut self, id: &str) -> Result<RemoteState, ()> {
                assert_eq!(id, "remote-1");
                Ok(RemoteState::Finished {
                    result: CloudResult {
                        commit: "a".repeat(40),
                        exit_status: 0,
                        tests_run: vec!["test".into()],
                        tests_failed: vec![],
                        log_location: "log".into(),
                        tracked_files_changed: false,
                        observed_model: Some("model".into()),
                    },
                })
            }
            fn cancel(&mut self, _: &str) -> Result<bool, ()> {
                Ok(false)
            }
        }
        let temp = tempfile::tempdir()?;
        let mut store = AttemptCloudStore::new(Directory::open_anchor(temp.path())?);
        let mut request = job();
        request.remote_id.clear();
        let submitted =
            submit(&mut Fake, &mut store, request, "run tests").map_err(|e| format!("{e:?}"))?;
        drop(store);
        let mut store = AttemptCloudStore::new(Directory::open_anchor(temp.path())?);
        let mut recovered = store.load()?.ok_or("missing job")?;
        assert_eq!(recovered, submitted);
        reconcile(&mut Fake, &mut store, &mut recovered).map_err(|e| format!("{e:?}"))?;
        assert!(matches!(
            store.load()?.unwrap().state,
            RemoteState::Finished { .. }
        ));
        Ok(())
    }
}
