//! The format-2 break. Installing 2.0.0 reads nothing of format 1: the old
//! format is learned from the install receipt and from which store files exist,
//! never by opening a store. The operator acknowledges the reset, then the
//! dropped stores are deleted and every other path is left alone.
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The one code an install carries when the reset is not allowed to proceed.
pub(crate) const BLOCKED: &str = "STATE_FORMAT_BREAK";

/// Dropped store classes, relative to the state root: the run store and run
/// files (attempt tmp included), both Fabric databases (the project store with
/// mail and claims, and the host index with chair bindings), graph journals,
/// the work index and PR contributions. SQLite companions are matched by suffix.
const DROPPED: &[&str] = &[
    "run.sqlite3",
    "runs",
    "fabric/fabric.sqlite3",
    "fabric/host.sqlite3",
    "fabric/graphs",
    "work-index.sqlite3",
    "pr-contributions.sqlite3",
];
const COMPANIONS: &[&str] = &["-wal", "-shm", "-journal", ".lock"];
const KEPT: &str = "usage ledger, quota history, provider history, session checkpoints, chair log";

pub(crate) struct Plan {
    pub dropped: Vec<PathBuf>,
}

/// The format break, if this host has one: a receipt that names an older
/// format, or no receipt and format-1 store files.
pub(crate) fn plan(state: &Path, receipt_format: Option<u64>) -> Option<Plan> {
    let dropped: Vec<PathBuf> = DROPPED
        .iter()
        .flat_map(|name| {
            let base = state.join(name);
            let mut found = vec![base.clone()];
            found.extend(COMPANIONS.iter().map(|suffix| {
                let mut path = base.clone().into_os_string();
                path.push(suffix);
                PathBuf::from(path)
            }));
            found
        })
        .filter(|path| path.symlink_metadata().is_ok())
        .collect();
    let older = match receipt_format {
        Some(version) => version < provenant_records::STATE_FORMAT_VERSION,
        None => !dropped.is_empty(),
    };
    older.then_some(Plan { dropped })
}

/// A lane whose supervisor still holds its custody lock. Found by lock, so no
/// store is read.
pub(crate) fn live_lane(state: &Path) -> Option<PathBuf> {
    use provenant_os::fs::{Directory, LockMode};
    let entries = |path: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(path)
            .map(|dir| dir.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    };
    entries(&state.join("runs"))
        .into_iter()
        .flat_map(|run| entries(&run.join("attempts")))
        .find(|attempt| {
            Directory::open_anchor(attempt).is_ok_and(|directory| {
                directory
                    .try_lock_existing(OsStr::new("custody.lock"), LockMode::Exclusive)
                    .is_err_and(|error| !error.is_not_found())
            })
        })
}

/// What the operator is told before acknowledging.
pub(crate) fn describe(plan: &Plan, state: &Path, unlanded: &[String]) -> String {
    let mut text = String::from("State format 1 will be reset to format 2.\n");
    text.push_str("Dropped:\n");
    for path in &plan.dropped {
        text.push_str(&format!(
            "  {}\n",
            path.strip_prefix(state).unwrap_or(path).display()
        ));
    }
    if plan.dropped.is_empty() {
        text.push_str("  (nothing is present)\n");
    }
    text.push_str(&format!("Kept untouched: {KEPT}\n"));
    for line in unlanded {
        text.push_str(&format!("Unlanded commits survive in Git: {line}\n"));
    }
    text
}

pub(crate) fn remove(plan: &Plan) -> std::io::Result<()> {
    for path in &plan.dropped {
        let result = if path.symlink_metadata()?.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match result {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}
