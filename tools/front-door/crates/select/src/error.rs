//! Errors from the affected-crate selector.

use std::{io, path::PathBuf, time::SystemTime};

/// A selector failure. None of these are panics: the caller decides how to surface them.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The repository could not be queried.
    #[error("git {args} failed in {workspace}: {detail}")]
    Git {
        /// The workspace the command ran in.
        workspace: PathBuf,
        /// The git arguments, joined for display.
        args: String,
        /// Exit status or stderr detail.
        detail: String,
    },

    /// The git executable could not be spawned.
    #[error("could not spawn git in {workspace}: {source}")]
    GitSpawn {
        /// The workspace the command ran in.
        workspace: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },

    /// `cargo metadata` failed for one of the two revisions.
    #[error("cargo metadata failed for {revision}: {source}")]
    Metadata {
        /// `base` or `head`, for diagnostics.
        revision: &'static str,
        /// The guppy error.
        #[source]
        source: guppy::Error,
    },

    /// Changed paths were not valid UTF-8.
    #[error("git returned a non-UTF-8 path: {0}")]
    NonUtf8Path(String),

    /// A `global_paths` entry is not a valid glob.
    #[error("invalid global path glob {pattern:?}: {source}")]
    InvalidGlobalPath {
        /// The offending pattern.
        pattern: String,
        /// The glob error.
        #[source]
        source: globset::Error,
    },

    /// The project key cannot be used as a state file name.
    #[error("invalid project key {0:?}: expected one path component without separators")]
    InvalidProjectKey(String),

    /// A wall-clock value cannot be expressed as Unix milliseconds.
    #[error("injected wall time is out of range: {0:?}")]
    OutOfRangeTime(SystemTime),

    /// The state file could not be written atomically.
    #[error("could not write select state to {path}: {source}")]
    WriteState {
        /// The state file path.
        path: PathBuf,
        /// The underlying I/O error.
        #[source]
        source: io::Error,
    },
}
