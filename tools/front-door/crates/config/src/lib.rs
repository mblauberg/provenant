//! Installation policy. These APIs are inert until the command cut-over.
#![forbid(unsafe_code)]

pub mod codex;
pub mod command;

pub mod front_door;
pub mod instance;
pub mod manifest;
mod pending;
pub mod workflows;

#[derive(Debug, thiserror::Error)]
pub enum Error {
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
    #[error("Codex config is invalid TOML: {0}")]
    Toml(#[from] toml_edit::TomlError),
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
