//! Retention adapter for graph checkpoints and their derived files.
use provenant_config::ByteObservation;
use provenant_core::{Clock, ProjectId, StateRoot, SweepControl};
use provenant_os::fs::{Directory, DirectoryPosition};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(crate) struct MeasureReport {
    pub observation: ByteObservation,
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
}

impl Default for MeasureReport {
    fn default() -> Self {
        Self {
            observation: ByteObservation::Unknown,
            interrupted_at: None,
            continuation: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct CollectionReport {
    pub removed: usize,
    pub interrupted_at: Option<PathBuf>,
    pub continuation: Option<String>,
}

/// Measure graph journals and the pending index without following links.
/// Directory traversal checks the sweep budget before each entry and resumes
/// from an opaque native directory cookie.
pub(crate) fn measure(
    state: &StateRoot,
    projects: impl IntoIterator<Item = ProjectId>,
    control: &SweepControl<'_>,
) -> Result<MeasureReport, String> {
    let projects = projects.into_iter().collect::<Vec<_>>();
    let resume = MeasureCursor::parse(control.detail);
    let graph_root = state.as_path().join("fabric").join("graphs");
    let resume_project = control.start_at.and_then(|candidate| {
        projects
            .iter()
            .copied()
            .find(|project| candidate.starts_with(graph_root.join(project.to_string())))
    });
    if control.start_at.is_some() && resume_project.is_none() {
        return measure(
            state,
            projects,
            &SweepControl {
                start_at: None,
                detail: None,
                cancel: control.cancel.clone(),
                keep_going: control.keep_going,
            },
        );
    }

    let root = Directory::open_anchor(state.as_path()).map_err(|error| error.to_string())?;
    let Some(fabric) = open_optional(&root, OsStr::new("fabric"))? else {
        return Ok(MeasureReport {
            observation: ByteObservation::Measured { bytes: 0 },
            ..Default::default()
        });
    };
    let Some(graphs) = open_optional(&fabric, OsStr::new("graphs"))? else {
        return Ok(MeasureReport {
            observation: ByteObservation::Measured { bytes: 0 },
            ..Default::default()
        });
    };

    let mut report = MeasureReport::default();
    let mut bytes = resume.as_ref().map_or(0, |cursor| cursor.bytes);
    let mut active = resume_project.is_none();
    for project in projects.iter().copied() {
        let project_path = graphs.anchored_path().join(project.to_string());
        let directory = match open_optional(&graphs, OsStr::new(&project.to_string()))? {
            Some(directory) => directory,
            None => continue,
        };
        let is_resume_project = resume_project == Some(project);
        if !active && !is_resume_project {
            continue;
        }
        active = true;
        let resume_pending = is_resume_project
            && resume
                .as_ref()
                .is_some_and(|cursor| cursor.phase == "pending");
        let pending = if resume_pending {
            open_optional(&directory, OsStr::new("pending"))?
        } else {
            let cursor = if is_resume_project {
                resume.as_ref().filter(|cursor| cursor.phase == "graph")
            } else {
                None
            };
            match walk_directory(
                &directory,
                &project_path,
                "graph",
                cursor,
                control.start_at,
                bytes,
                control,
            )? {
                Walk::Complete(total) => {
                    bytes = total;
                    None
                }
                Walk::Interrupted(total, path, cursor) => {
                    stop_measure(&mut report, total, path, cursor);
                    return Ok(report);
                }
                Walk::Stale => {
                    return measure(
                        state,
                        projects,
                        &SweepControl {
                            start_at: None,
                            detail: None,
                            cancel: control.cancel.clone(),
                            keep_going: control.keep_going,
                        },
                    );
                }
            }
        };

        let pending = match pending {
            Some(directory) => Some(directory),
            None => open_optional(&directory, OsStr::new("pending"))?,
        };
        if let Some(pending) = pending {
            let cursor = if is_resume_project {
                resume.as_ref().filter(|cursor| cursor.phase == "pending")
            } else {
                None
            };
            let path = pending.anchored_path();
            match walk_directory(
                &pending,
                &path,
                "pending",
                cursor,
                control.start_at,
                bytes,
                control,
            )? {
                Walk::Complete(total) => bytes = total,
                Walk::Interrupted(total, path, cursor) => {
                    stop_measure(&mut report, total, path, cursor);
                    return Ok(report);
                }
                Walk::Stale => {
                    return measure(
                        state,
                        projects,
                        &SweepControl {
                            start_at: None,
                            detail: None,
                            cancel: control.cancel.clone(),
                            keep_going: control.keep_going,
                        },
                    );
                }
            }
        }
    }
    if !active && resume_project.is_some() {
        return measure(
            state,
            projects,
            &SweepControl {
                start_at: None,
                detail: None,
                cancel: control.cancel.clone(),
                keep_going: control.keep_going,
            },
        );
    }
    report.observation = ByteObservation::Measured { bytes };
    Ok(report)
}

enum Walk {
    Complete(u64),
    Interrupted(u64, PathBuf, MeasureCursor),
    Stale,
}

fn walk_directory(
    directory: &Directory,
    display_root: &Path,
    phase: &str,
    cursor: Option<&MeasureCursor>,
    start_at: Option<&Path>,
    mut bytes: u64,
    control: &SweepControl<'_>,
) -> Result<Walk, String> {
    if cursor.is_some_and(|cursor| !cursor.matches(directory, phase)) {
        return Ok(Walk::Stale);
    }
    let initial_metadata = directory.metadata().map_err(|error| error.to_string())?;
    let mut entries = directory.entries().map_err(|error| error.to_string())?;
    if let Some(cursor) = cursor
        && entries.seek(cursor.cookie).is_err()
    {
        return Ok(Walk::Stale);
    }
    let mut target = cursor.and(start_at);
    loop {
        let before = entries.position().map_err(|error| error.to_string())?;
        let Some(item) = entries.next() else { break };
        let item = item.map_err(|error| error.to_string())?;
        let path = display_root.join(&item);
        if let Some(expected) = target {
            if expected != path {
                if !control.should_continue() {
                    return Ok(Walk::Interrupted(
                        bytes,
                        path,
                        MeasureCursor::new(phase, directory, before, bytes),
                    ));
                }
                continue;
            }
            target = None;
        }
        if !control.should_continue() {
            return Ok(Walk::Interrupted(
                bytes,
                path,
                MeasureCursor::new(phase, directory, before, bytes),
            ));
        }
        bytes = bytes.saturating_add(
            directory
                .metadata_at(&item, false)
                .map_err(|error| error.to_string())?
                .allocated,
        );
    }
    if target.is_some() {
        return Ok(Walk::Stale);
    }
    let final_metadata = directory.metadata().map_err(|error| error.to_string())?;
    if initial_metadata.device != final_metadata.device
        || initial_metadata.inode != final_metadata.inode
        || initial_metadata.modified_ns != final_metadata.modified_ns
    {
        return Ok(Walk::Stale);
    }
    Ok(Walk::Complete(bytes))
}

/// Collect aged terminal checkpoints through the graph journal owner.
pub(crate) fn collect(
    state: &StateRoot,
    projects: impl IntoIterator<Item = ProjectId>,
    retention_days: u64,
    dry_run: bool,
    clock: &dyn Clock,
    control: &SweepControl<'_>,
) -> Result<CollectionReport, String> {
    let mut report = CollectionReport::default();
    if dry_run {
        return Ok(report);
    }
    let projects = projects.into_iter().collect::<Vec<_>>();
    let before_ns = i128::from(
        clock
            .wall_now()
            .map_err(|error| error.to_string())?
            .as_micros(),
    ) * 1_000
        - i128::from(retention_days) * 86_400 * 1_000_000_000;
    let graph_root = state.as_path().join("fabric").join("graphs");
    let resume_project = control.start_at.and_then(|candidate| {
        projects
            .iter()
            .copied()
            .find(|project| candidate.starts_with(graph_root.join(project.to_string())))
    });
    let mut active = resume_project.is_none();
    // The root path and project set are already owned by the sweep driver.
    // The journal collector receives the shared cancellation token; it returns
    // at each project's boundary so the driver can persist the exact item.
    for project in projects {
        if !active && resume_project != Some(project) {
            continue;
        }
        active = true;
        if !control.should_continue() {
            report.interrupted_at = Some(graph_root.join(project.to_string()));
            report.continuation = Some("graph-collect-v1|project".into());
            return Ok(report);
        }
        let project_path = graph_root.join(project.to_string());
        let project_control = SweepControl {
            start_at: control
                .start_at
                .filter(|candidate| candidate.starts_with(&project_path)),
            detail: control
                .start_at
                .filter(|candidate| candidate.starts_with(&project_path))
                .and(control.detail),
            cancel: control.cancel.clone(),
            keep_going: control.keep_going,
        };
        let collected = provenant_fabric_runtime::graph::FileJournal::collect_terminal(
            state,
            project,
            before_ns,
            &project_control,
            clock,
        )
        .map_err(|error| error.to_string())?;
        report.removed = report.removed.saturating_add(collected.removed);
        if collected.interrupted_at.is_some() {
            report.interrupted_at = collected.interrupted_at;
            report.continuation = collected.continuation;
            return Ok(report);
        }
    }
    Ok(report)
}

fn stop_measure(report: &mut MeasureReport, bytes: u64, path: PathBuf, cursor: MeasureCursor) {
    report.observation = ByteObservation::Partial { bytes };
    report.interrupted_at = Some(path);
    report.continuation = Some(cursor.encode());
}

struct MeasureCursor {
    phase: String,
    device: u64,
    inode: u64,
    modified_ns: i128,
    cookie: DirectoryPosition,
    bytes: u64,
}

impl MeasureCursor {
    fn new(phase: &str, directory: &Directory, cookie: DirectoryPosition, bytes: u64) -> Self {
        let metadata = directory.metadata().ok();
        Self {
            phase: phase.into(),
            device: metadata.map_or(0, |metadata| metadata.device),
            inode: metadata.map_or(0, |metadata| metadata.inode),
            modified_ns: metadata.map_or(0, |metadata| metadata.modified_ns),
            cookie,
            bytes,
        }
    }
    fn matches(&self, directory: &Directory, phase: &str) -> bool {
        self.phase == phase
            && directory.metadata().is_ok_and(|metadata| {
                metadata.device == self.device
                    && metadata.inode == self.inode
                    && metadata.modified_ns == self.modified_ns
            })
    }
    fn encode(&self) -> String {
        format!(
            "graph-measure-v1|{}|{}|{}|{}|{}|{}|{}",
            self.phase,
            self.device,
            self.inode,
            self.modified_ns,
            self.cookie.block,
            self.cookie.offset,
            self.bytes
        )
    }
    fn parse(value: Option<&str>) -> Option<Self> {
        let mut fields = value?.split('|');
        if fields.next()? != "graph-measure-v1" {
            return None;
        }
        Some(Self {
            phase: fields.next()?.into(),
            device: fields.next()?.parse().ok()?,
            inode: fields.next()?.parse().ok()?,
            modified_ns: fields.next()?.parse().ok()?,
            cookie: DirectoryPosition {
                block: fields.next()?.parse().ok()?,
                offset: fields.next()?.parse().ok()?,
            },
            bytes: fields.next()?.parse().ok()?,
        })
    }
}

fn open_optional(parent: &Directory, name: &OsStr) -> Result<Option<Directory>, String> {
    match parent.open_verified_directory(name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
#[allow(unused_imports)] // The worktree driver includes the library without a test harness.
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn measurement_walk_resumes_at_the_unread_entry() {
        let temporary = tempfile::tempdir().unwrap();
        for (name, contents) in [
            ("a", b"first".as_slice()),
            ("b", b"second"),
            ("c", b"third"),
        ] {
            std::fs::write(temporary.path().join(name), contents).unwrap();
        }
        let directory = Directory::open_anchor(temporary.path()).unwrap();
        let expected = directory
            .entries()
            .unwrap()
            .map(|entry| {
                directory
                    .metadata_at(&entry.unwrap(), false)
                    .unwrap()
                    .allocated
            })
            .sum::<u64>();
        let checks = Cell::new(0usize);
        let keep_going = || {
            let count = checks.get() + 1;
            checks.set(count);
            count <= 1
        };
        let stopped = walk_directory(
            &directory,
            temporary.path(),
            "graph",
            None,
            None,
            0,
            &SweepControl {
                keep_going: Some(&keep_going),
                ..Default::default()
            },
        )
        .unwrap();
        let Walk::Interrupted(partial, candidate, cursor) = stopped else {
            panic!("walk did not yield")
        };
        assert!(partial > 0);
        let detail = cursor.encode();
        let cursor = MeasureCursor::parse(Some(&detail)).unwrap();
        let resumed = walk_directory(
            &directory,
            temporary.path(),
            "graph",
            Some(&cursor),
            Some(&candidate),
            partial,
            &SweepControl::default(),
        )
        .unwrap();
        assert!(matches!(resumed, Walk::Complete(bytes) if bytes == expected));
    }
}
