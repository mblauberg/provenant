//! Retention adapter for graph checkpoints and their derived files.
use provenant_config::ByteObservation;
use provenant_core::{CancelToken, Clock, ProjectId, StateRoot};
use provenant_os::fs::{Directory, FileKind};
use std::ffi::OsStr;

/// Measure the graph journal tree for registered projects without following
/// links. The pending index is the only child directory owned by this store.
pub(crate) fn measure(
    state: &StateRoot,
    projects: impl IntoIterator<Item = ProjectId>,
) -> Result<ByteObservation, String> {
    let root = Directory::open_anchor(state.as_path()).map_err(|error| error.to_string())?;
    let Some(fabric) = open_optional(&root, OsStr::new("fabric"))? else {
        return Ok(ByteObservation::Measured { bytes: 0 });
    };
    let Some(graphs) = open_optional(&fabric, OsStr::new("graphs"))? else {
        return Ok(ByteObservation::Measured { bytes: 0 });
    };
    let mut bytes = 0u64;
    for project in projects {
        let Some(directory) = open_optional(&graphs, OsStr::new(&project.to_string()))? else {
            continue;
        };
        for entry in directory.entries().map_err(|error| error.to_string())? {
            let name = entry.map_err(|error| error.to_string())?;
            let metadata = directory
                .metadata_at(&name, false)
                .map_err(|error| error.to_string())?;
            bytes = bytes.saturating_add(metadata.allocated);
            if metadata.kind == FileKind::Directory && name == OsStr::new("pending") {
                let pending = directory
                    .open_verified_directory(&name)
                    .map_err(|error| error.to_string())?;
                for marker in pending.entries().map_err(|error| error.to_string())? {
                    let marker = marker.map_err(|error| error.to_string())?;
                    bytes = bytes.saturating_add(
                        pending
                            .metadata_at(&marker, false)
                            .map_err(|error| error.to_string())?
                            .allocated,
                    );
                }
            }
        }
    }
    Ok(ByteObservation::Measured { bytes })
}

/// Collect aged terminal checkpoints through the graph journal's resumable
/// collector. Busy graph writers are skipped and can be handled by a later GC.
pub(crate) fn collect(
    state: &StateRoot,
    projects: impl IntoIterator<Item = ProjectId>,
    retention_days: u64,
    dry_run: bool,
    clock: &dyn Clock,
) -> Result<usize, String> {
    if dry_run {
        return Ok(0);
    }
    let before_ns = i128::from(
        clock
            .wall_now()
            .map_err(|error| error.to_string())?
            .as_micros(),
    ) * 1_000
        - i128::from(retention_days) * 86_400 * 1_000_000_000;
    let cancel = CancelToken::new();
    let mut removed = 0usize;
    for project in projects {
        provenant_fabric_runtime::graph::FileJournal::rebuild_pending(state, project)
            .map_err(|error| error.to_string())?;
        removed += provenant_fabric_runtime::graph::FileJournal::collect_terminal(
            state, project, before_ns, &cancel, clock,
        )
        .map_err(|error| error.to_string())?;
    }
    Ok(removed)
}

fn open_optional(parent: &Directory, name: &OsStr) -> Result<Option<Directory>, String> {
    match parent.open_verified_directory(name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.is_not_found() => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}
