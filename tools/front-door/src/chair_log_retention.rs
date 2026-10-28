//! Age collection for every session, including sessions of abandoned projects.
use provenant_core::{ProjectId, StateRoot};
use provenant_os::fs::{Directory, FsError};
use provenant_workflow::{
    log::{LogStore, Retention},
    session::HostSessionId,
};
use serde::Serialize;
use std::{ffi::OsStr, path::PathBuf};

#[derive(Default, Serialize)]
pub(crate) struct Report {
    pub expired: u64,
    pub errors: Vec<Issue>,
}

#[derive(Serialize)]
pub(crate) struct Issue {
    pub path: PathBuf,
    pub reason: String,
}

pub(crate) fn collect(root: &StateRoot, retention: Retention, now: i64, dry_run: bool) -> Report {
    let mut report = Report::default();
    if let Err(error) = sweep(root, retention, now, dry_run, &mut report) {
        report.errors.push(Issue {
            path: root.as_path().join("workflow/projects"),
            reason: error.to_string(),
        });
    }
    report
}

fn sweep(
    root: &StateRoot,
    retention: Retention,
    now: i64,
    dry_run: bool,
    report: &mut Report,
) -> Result<(), FsError> {
    let mut projects = Directory::open_anchor(root.as_path())?;
    for component in ["workflow", "projects"] {
        projects = match projects.open_verified_directory(OsStr::new(component)) {
            Ok(dir) => dir,
            Err(error) if error.is_not_found() => return Ok(()),
            Err(error) => return Err(error),
        };
    }
    for name in projects.entries()? {
        let name = name?;
        let Some(project) = name.to_str().and_then(|s| ProjectId::parse(s).ok()) else {
            continue;
        };
        let project_path = root.as_path().join("workflow/projects").join(&name);
        let result = (|| -> Result<(), FsError> {
            let project_dir = projects.open_verified_directory(&name)?;
            let logs = match project_dir.open_verified_directory(OsStr::new("chair-logs")) {
                Ok(dir) => dir,
                Err(error) if error.is_not_found() => return Ok(()),
                Err(error) => return Err(error),
            };
            for session_name in logs.entries()? {
                let session_name = session_name?;
                let Some(session) = session_name
                    .to_str()
                    .and_then(|s| HostSessionId::parse(s).ok())
                else {
                    continue;
                };
                let path = project_path.join("chair-logs").join(&session_name);
                match LogStore::new(root.clone(), project, session).gc(retention, now, dry_run) {
                    Ok(count) => report.expired += count,
                    Err(error) => report.errors.push(Issue {
                        path,
                        reason: error.to_string(),
                    }),
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            report.errors.push(Issue {
                path: project_path,
                reason: error.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(unused_imports)] // The worktree fixture includes the library without the test harness.
mod tests {
    use super::*;
    use provenant_core::{OperationId, RootInputs};
    use provenant_os::fs::SystemPathResolver;
    use provenant_workflow::log::Append;

    #[test]
    fn sweep_previews_and_expires_idle_sessions_across_projects()
    -> Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let state = tmp.path().join("state");
        std::fs::create_dir(&state)?;
        let mut inputs = RootInputs::new(tmp.path().join("home"), tmp.path().join("checkout"));
        inputs.state_dir = Some(state.clone());
        let root = StateRoot::resolve(&inputs, &SystemPathResolver)?;
        let retention = Retention {
            max_age_seconds: 10,
            replay_horizon_seconds: 10,
            max_entries: 100,
            max_bytes: 65536,
        };
        for id in [1, 2] {
            let store = LogStore::new(
                root.clone(),
                ProjectId::from_bytes([id; 16]),
                HostSessionId::parse("idle")?,
            );
            store.append(
                Append {
                    operation_id: OperationId::parse("one")?,
                    issued_at: 1,
                    host: "host".into(),
                    kind: "update".into(),
                    markdown: "old".into(),
                    request_ids: vec![],
                    attempt_ids: vec![],
                    issue_ids: vec![],
                    pr_ids: vec![],
                    channel: "chat".into(),
                    correction_of: None,
                },
                retention,
                1,
            )?;
        }
        let preview = collect(&root, retention, 100, true);
        assert_eq!(preview.expired, 2);
        assert!(preview.errors.is_empty());
        let applied = collect(&root, retention, 100, false);
        assert_eq!(applied.expired, 2);
        assert!(applied.errors.is_empty());
        assert_eq!(collect(&root, retention, 100, false).expired, 0);
        Ok(())
    }
}
