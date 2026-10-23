//! Installation policy. These APIs are inert until the command cut-over.
#![forbid(unsafe_code)]

pub mod codex;

mod pending;

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
    #[error("Codex config is invalid TOML: {0}")]
    Toml(#[from] toml_edit::TomlError),
}

pub type Result<T> = std::result::Result<T, Error>;

fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid { message: message.into() }
}

fn io(path: &std::path::Path, source: std::io::Error) -> Error {
    Error::Io { path: path.into(), source }
}

#[cfg(test)]
mod oracle;
