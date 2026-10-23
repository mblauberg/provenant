//! Installation policy. These APIs are inert until the command cut-over.
#![forbid(unsafe_code)]

pub mod codex;
pub mod command;

pub mod front_door;
pub mod instance;
pub mod managed_skills;
pub mod manifest;
mod pending;
mod filesystem;
pub mod workflows;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Path(#[from] provenant_core::paths::PathError),
    #[error("{message}")]
    Invalid { message: String },
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("provenant command collision={path}; {reason}")]
    Collision {
        path: std::path::PathBuf,
        reason: &'static str,
    },
    #[error("record codec integration is pending: {feature}")]
    PendingRecord { feature: &'static str },
    #[error("{context}: {path}: {source}")]
    Read {
        context: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{context}: {source}")]
    Json {
        context: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{context}: {source}")]
    CheckedJson {
        context: &'static str,
        path: std::path::PathBuf,
        #[source]
        source: provenant_records::checked_json::DecodeError,
    },
    #[error("Codex config is invalid TOML: {0}")]
    Toml(#[from] toml_edit::TomlError),
    #[error("Codex config symlink cannot be resolved: {path}: {source}")]
    CodexSymlink {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Codex config is not valid UTF-8: {source}")]
    CodexEncoding {
        path: std::path::PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("cannot use '{kind}' as a set element (unhashable type: '{kind}')")]
    CodexUnhashable { kind: &'static str },
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid {
        message: message.into(),
    }
}

fn io(path: &std::path::Path, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}

#[cfg(test)]
mod oracle;

fn resolve(path: &std::path::Path) -> Result<std::path::PathBuf> {
    Ok(provenant_core::paths::resolve(path)?)
}
