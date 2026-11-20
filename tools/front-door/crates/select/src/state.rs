//! Durable run state, written atomically and treated as advisory.

use crate::error::Error;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

pub(super) const STATE_VERSION: u32 = 1;

/// The persisted counter and last full-run timestamp.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RunState {
    pub(super) version: u32,
    pub(super) runs_since_full: u64,
    pub(super) last_full_unix_millis: u64,
}

impl RunState {
    /// A fresh state for a project whose last full run is `now`.
    pub(super) fn fresh(now_unix_millis: u64) -> Self {
        Self {
            version: STATE_VERSION,
            runs_since_full: 0,
            last_full_unix_millis: now_unix_millis,
        }
    }

    fn is_current(&self) -> bool {
        self.version == STATE_VERSION
    }
}

/// Reads the state file. `Ok(None)` means "no usable prior run".
///
/// A missing or corrupt file is a warning, not a refusal: the caller treats it as
/// due for a full run and replaces it on the same call.
pub(super) fn load(path: &Path) -> Result<Option<RunState>, String> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(format!("could not read {}: {error}", path.display())),
    };
    let state: RunState = serde_json::from_slice(&bytes)
        .map_err(|error| format!("could not parse {}: {error}", path.display()))?;
    if !state.is_current() {
        return Err(format!(
            "unsupported state version {} in {}",
            state.version,
            path.display()
        ));
    }
    Ok(Some(state))
}

/// Writes `state` via a temporary file in the same directory and an atomic rename.
///
/// The temporary name is derived from the injected clock, the process id and a
/// counter, so concurrent selectors never share a name and no ambient clock is read.
pub(super) fn store(path: &Path, state: &RunState, now_unix_millis: u64) -> Result<(), Error> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| Error::WriteState {
        path: path.to_path_buf(),
        source,
    })?;

    let leaf = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "select.json".to_owned());
    let unique = NEXT.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(
        ".{leaf}.{}.{}.{}.tmp",
        std::process::id(),
        now_unix_millis,
        unique
    ));
    let bytes = serde_json::to_vec(state).map_err(|source| Error::WriteState {
        path: path.to_path_buf(),
        source: io::Error::other(source),
    })?;
    let result = fs::write(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(|source| Error::WriteState {
        path: path.to_path_buf(),
        source,
    })
}
