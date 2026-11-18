//! Age collection for every session, including sessions of abandoned projects.
use provenant_core::{ProjectId, StateRoot, SweepControl};
use provenant_os::fs::{Directory, DirectoryPosition, FsError, Metadata};
use provenant_workflow::{
    log::{LogStore, Retention},
    session::HostSessionId,
};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize)]
pub(crate) struct Report {
    pub expired: u64,
    pub errors: Vec<Issue>,
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Issue {
    pub path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
struct DirectoryStamp {
    device: u64,
    inode: u64,
    modified_ns: i128,
    changed_ns: i128,
}

impl From<Metadata> for DirectoryStamp {
    fn from(metadata: Metadata) -> Self {
        Self {
            device: metadata.device,
            inode: metadata.inode,
            modified_ns: metadata.modified_ns,
            changed_ns: metadata.changed_ns,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Cursor {
    version: u8,
    phase: Phase,
    projects_offset: DirectoryPosition,
    projects_stamp: DirectoryStamp,
    project: Option<String>,
    sessions_offset: DirectoryPosition,
    sessions_stamp: Option<DirectoryStamp>,
    log_continuation: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Projects,
    Sessions,
}

pub(crate) fn collect(
    root: &StateRoot,
    retention: Retention,
    now: i64,
    dry_run: bool,
    control: &SweepControl<'_>,
) -> Report {
    let mut report = Report::default();
    if let Err(error) = sweep(root, retention, now, dry_run, control, &mut report) {
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
    control: &SweepControl<'_>,
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
    let projects_path = root.as_path().join("workflow/projects");
    let projects_stamp = DirectoryStamp::from(projects.metadata()?);
    let mut cursor = resume_cursor(control, &projects_path, &projects_stamp);
    if let Some(saved) = cursor.take_if(|saved| saved.phase == Phase::Sessions) {
        let Some(project_name) = saved.project.as_deref() else {
            return Ok(());
        };
        let Some(project) = ProjectId::parse(project_name).ok() else {
            return Ok(());
        };
        match process_sessions(
            root,
            project,
            project_name,
            saved.projects_offset,
            saved.sessions_offset,
            saved.sessions_stamp,
            saved.log_continuation,
            &projects_stamp,
            retention,
            now,
            dry_run,
            control,
            report,
        ) {
            Ok(Some((path, cursor))) => {
                stop(report, path, cursor);
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => report.errors.push(Issue {
                path: projects_path.join(project_name),
                reason: error.to_string(),
            }),
        }
        cursor = Some(Cursor {
            version: 1,
            phase: Phase::Projects,
            projects_offset: saved.projects_offset,
            projects_stamp: projects_stamp.clone(),
            project: None,
            sessions_offset: DirectoryPosition::default(),
            sessions_stamp: None,
            log_continuation: None,
        });
    }

    let offset = cursor
        .as_ref()
        .filter(|saved| saved.phase == Phase::Projects)
        .map_or_else(DirectoryPosition::default, |saved| saved.projects_offset);
    let mut entries = projects.entries()?;
    entries.seek(offset)?;
    loop {
        if !control.should_continue() {
            let offset = entries.position()?;
            stop(
                report,
                projects_path.clone(),
                Cursor {
                    version: 1,
                    phase: Phase::Projects,
                    projects_offset: offset,
                    projects_stamp: projects_stamp.clone(),
                    project: None,
                    sessions_offset: DirectoryPosition::default(),
                    sessions_stamp: None,
                    log_continuation: None,
                },
            );
            return Ok(());
        }
        let project_offset = entries.position()?;
        let Some(name) = entries.next() else { break };
        let name = match name {
            Ok(name) => name,
            Err(error) => {
                report.errors.push(Issue {
                    path: projects_path.clone(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let Some(project) = name.to_str().and_then(|s| ProjectId::parse(s).ok()) else {
            continue;
        };
        match process_sessions(
            root,
            project,
            &name.to_string_lossy(),
            entries.position()?,
            DirectoryPosition::default(),
            None,
            None,
            &projects_stamp,
            retention,
            now,
            dry_run,
            control,
            report,
        ) {
            Ok(Some((path, cursor))) => {
                let mut cursor = cursor;
                cursor.projects_offset = entries.position().unwrap_or(project_offset);
                stop(report, path, cursor);
                return Ok(());
            }
            Ok(None) => {}
            Err(error) => report.errors.push(Issue {
                path: projects_path.join(name),
                reason: error.to_string(),
            }),
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_sessions(
    root: &StateRoot,
    project: ProjectId,
    project_name: &str,
    projects_offset: DirectoryPosition,
    session_offset: DirectoryPosition,
    expected_sessions_stamp: Option<DirectoryStamp>,
    log_continuation: Option<String>,
    projects_stamp: &DirectoryStamp,
    retention: Retention,
    now: i64,
    dry_run: bool,
    control: &SweepControl<'_>,
    report: &mut Report,
) -> Result<Option<(PathBuf, Cursor)>, FsError> {
    let project_path = root.as_path().join("workflow/projects").join(project_name);
    let project_dir = match Directory::open_anchor(root.as_path())?
        .open_verified_directory(OsStr::new("workflow"))
        .and_then(|workflow| workflow.open_verified_directory(OsStr::new("projects")))
        .and_then(|projects| projects.open_verified_directory(OsStr::new(project_name)))
    {
        Ok(directory) => directory,
        Err(error) => {
            report.errors.push(Issue {
                path: project_path,
                reason: error.to_string(),
            });
            return Ok(None);
        }
    };
    let logs = match project_dir.open_verified_directory(OsStr::new("chair-logs")) {
        Ok(dir) => dir,
        Err(error) if error.is_not_found() => return Ok(None),
        Err(error) => {
            report.errors.push(Issue {
                path: project_path,
                reason: error.to_string(),
            });
            return Ok(None);
        }
    };
    let logs_path = root
        .as_path()
        .join("workflow/projects")
        .join(project_name)
        .join("chair-logs");
    let logs_stamp = DirectoryStamp::from(logs.metadata()?);
    let session_offset = if expected_sessions_stamp.as_ref() == Some(&logs_stamp) {
        session_offset
    } else {
        DirectoryPosition::default()
    };
    let mut sessions = match logs.entries() {
        Ok(entries) => entries,
        Err(error) => {
            report.errors.push(Issue {
                path: logs_path,
                reason: error.to_string(),
            });
            return Ok(None);
        }
    };
    sessions.seek(session_offset)?;
    let mut log_continuation = log_continuation;
    loop {
        if !control.should_continue() {
            let offset = sessions.position()?;
            return Ok(Some((
                logs_path,
                Cursor {
                    version: 1,
                    phase: Phase::Sessions,
                    projects_offset,
                    projects_stamp: projects_stamp.clone(),
                    project: Some(project_name.to_owned()),
                    sessions_offset: offset,
                    sessions_stamp: Some(logs_stamp.clone()),
                    log_continuation: log_continuation.clone(),
                },
            )));
        }
        let session_offset = sessions.position()?;
        let Some(session_name) = sessions.next() else {
            break;
        };
        let continuation = log_continuation.take();
        let session_name = match session_name {
            Ok(name) => name,
            Err(error) => {
                report.errors.push(Issue {
                    path: logs_path.clone(),
                    reason: error.to_string(),
                });
                continue;
            }
        };
        let Some(session) = session_name
            .to_str()
            .and_then(|s| HostSessionId::parse(s).ok())
        else {
            continue;
        };
        let session_path = logs_path.join(&session_name);
        let database_path = session_path.join("log.sqlite3");
        let log_control = SweepControl {
            start_at: Some(&database_path),
            detail: continuation.as_deref(),
            cancel: control.cancel.clone(),
            keep_going: control.keep_going,
        };
        match LogStore::new(root.clone(), project, session).gc(
            retention,
            now,
            dry_run,
            &log_control,
        ) {
            Ok(result) => {
                report.expired = report.expired.saturating_add(result.pruned);
                if result.interrupted {
                    return Ok(Some((
                        logs_path,
                        Cursor {
                            version: 1,
                            phase: Phase::Sessions,
                            projects_offset,
                            projects_stamp: projects_stamp.clone(),
                            project: Some(project_name.to_owned()),
                            sessions_offset: session_offset,
                            sessions_stamp: Some(logs_stamp),
                            log_continuation: result.continuation,
                        },
                    )));
                }
            }
            Err(error) => report.errors.push(Issue {
                path: logs_path.join(&session_name),
                reason: error.to_string(),
            }),
        }
    }
    Ok(None)
}

fn resume_cursor(
    control: &SweepControl<'_>,
    projects_path: &Path,
    projects_stamp: &DirectoryStamp,
) -> Option<Cursor> {
    let saved: Cursor = serde_json::from_str(control.detail?).ok()?;
    if saved.version != 1 || &saved.projects_stamp != projects_stamp {
        return None;
    }
    let start = control.start_at?;
    match saved.phase {
        Phase::Projects if start == projects_path => Some(saved),
        Phase::Sessions => {
            let project = saved.project.as_deref()?;
            let sessions_path = projects_path.join(project).join("chair-logs");
            (start == sessions_path.as_path()).then_some(saved)
        }
        _ => None,
    }
}

fn stop(report: &mut Report, path: PathBuf, cursor: Cursor) {
    report.interrupted_at = Some(path);
    report.continuation = serde_json::to_string(&cursor).ok();
}

#[cfg(test)]
#[allow(unused_imports)] // The worktree fixture includes the library without the test harness.
mod tests {
    use super::*;
    use provenant_core::{OperationId, RootInputs};
    use provenant_os::fs::SystemPathResolver;
    use provenant_workflow::log::Append;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn fixture() -> Result<(tempfile::TempDir, StateRoot, Retention), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let state = tmp.path().join("state");
        provenant_os::fixture::private_state_dir(&state)?;
        let mut inputs = RootInputs::new(tmp.path().join("home"), tmp.path().join("checkout"));
        inputs.state_dir = Some(state);
        let root = StateRoot::resolve(&inputs, &SystemPathResolver)?;
        let retention = Retention {
            max_age_seconds: 10,
            replay_horizon_seconds: 10,
            max_entries: 100,
            max_bytes: 65536,
        };
        for id in [1, 2, 3] {
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
        Ok((tmp, root, retention))
    }

    /// Spec: docs/specs/workflow.md#chair-logs
    #[test]
    fn sweep_previews_and_expires_idle_sessions_across_projects()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_tmp, root, retention) = fixture()?;
        let preview = collect(&root, retention, 100, true, &SweepControl::default());
        assert_eq!(preview.expired, 3);
        assert!(preview.errors.is_empty());
        let applied = collect(&root, retention, 100, false, &SweepControl::default());
        assert_eq!(applied.expired, 3);
        assert!(applied.errors.is_empty());
        assert_eq!(
            collect(&root, retention, 100, false, &SweepControl::default()).expired,
            0
        );
        Ok(())
    }

    /// Spec: docs/specs/workflow.md#chair-logs
    #[test]
    fn interrupted_project_scan_resumes_without_losing_unvisited_logs()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_tmp, root, retention) = fixture()?;
        let checks = Arc::new(AtomicUsize::new(0));
        let observed = checks.clone();
        let keep_going = move || observed.fetch_add(1, Ordering::Relaxed) < 3;
        let first = collect(
            &root,
            retention,
            100,
            false,
            &SweepControl {
                keep_going: Some(&keep_going),
                ..Default::default()
            },
        );
        let at = first
            .interrupted_at
            .expect("the first pass records its directory");
        let detail = first
            .continuation
            .expect("the first pass records its cookie");
        let second = collect(
            &root,
            retention,
            100,
            false,
            &SweepControl {
                start_at: Some(&at),
                detail: Some(&detail),
                ..Default::default()
            },
        );
        assert!(second.interrupted_at.is_none());
        assert_eq!(first.expired + second.expired, 3);
        assert!(second.errors.is_empty());
        Ok(())
    }

    /// Spec: docs/specs/workflow.md#chair-logs
    #[test]
    fn unreadable_project_is_reported_without_blocking_other_projects()
    -> Result<(), Box<dyn std::error::Error>> {
        let (_tmp, root, retention) = fixture()?;
        let blocked = root
            .as_path()
            .join("workflow/projects")
            .join(ProjectId::from_bytes([9; 16]).to_string());
        std::fs::write(blocked, b"not a project directory")?;
        let report = collect(&root, retention, 100, false, &SweepControl::default());
        assert_eq!(report.expired, 3);
        assert_eq!(report.errors.len(), 1);
        assert!(report.interrupted_at.is_none());
        Ok(())
    }
}
