//! Persisted path layout for the build store, queue, ledger and slot cache.

use std::path::{Path, PathBuf};

/// `<state>/build` root.
pub fn build_root(state: &Path) -> PathBuf {
    state.join("build")
}

/// `<state>/build/store` root.
pub fn store_root(state: &Path) -> PathBuf {
    build_root(state).join("store")
}

/// `<state>/build/store/<project_key>.git` bare repository.
pub fn store_path(state: &Path, project_key: &str) -> PathBuf {
    store_root(state).join(format!("{project_key}.git"))
}

/// `<state>/build/store/<project_key>.lock`.
pub fn store_lock(state: &Path, project_key: &str) -> PathBuf {
    store_root(state).join(format!("{project_key}.lock"))
}

/// `<state>/build/queue/build` root.
pub fn queue_root(state: &Path) -> PathBuf {
    build_root(state).join("queue/build")
}

/// Queue ticket path.
pub fn queue_ticket(state: &Path, ticket_id: &str) -> PathBuf {
    queue_root(state).join(format!("{ticket_id}.json"))
}

/// Canonical host ledger path.
pub fn jobs_path(state: &Path) -> PathBuf {
    build_root(state).join("jobs.jsonl")
}

/// Canonical ledger flock path.
pub fn jobs_lock(state: &Path) -> PathBuf {
    build_root(state).join("jobs.lock")
}

/// Maintenance state path.
pub fn maintenance_path(state: &Path) -> PathBuf {
    build_root(state).join("maintenance.json")
}

/// Host slot-cache root under `~/Library/Caches/provenant-slots`.
pub fn slot_cache_root(home: &Path) -> PathBuf {
    home.join("Library/Caches/provenant-slots")
}

/// A project's private slot-cache root.
pub fn project_slots(home: &Path, project_key: &str) -> PathBuf {
    slot_cache_root(home).join(project_key)
}

/// Slot root for admission index `index`.
pub fn slot_root(home: &Path, project_key: &str, index: u64) -> PathBuf {
    project_slots(home, project_key).join(index.to_string())
}

/// Slot clone path.
pub fn slot_repo(home: &Path, project_key: &str, index: u64) -> PathBuf {
    slot_root(home, project_key, index).join("repo")
}

/// Slot Cargo target directory.
pub fn slot_target(home: &Path, project_key: &str, index: u64) -> PathBuf {
    slot_root(home, project_key, index).join("target")
}

/// Slot temporary directory.
pub fn slot_tmp(home: &Path, project_key: &str, index: u64) -> PathBuf {
    slot_root(home, project_key, index).join("tmp")
}

/// Slot record path.
pub fn slot_record(home: &Path, project_key: &str, index: u64) -> PathBuf {
    slot_root(home, project_key, index).join("slot.json")
}

/// Running-job record path.
pub fn slot_job(home: &Path, project_key: &str, index: u64) -> PathBuf {
    slot_root(home, project_key, index).join("job.json")
}

/// Slot flock path.
pub fn slot_lock(home: &Path, project_key: &str, index: u64) -> PathBuf {
    slot_root(home, project_key, index).join("slot.lock")
}
