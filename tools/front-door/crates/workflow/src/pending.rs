//! Temporary shared types until their owning crates expose the accepted API.

// port: belongs in provenant-confinement
#[derive(Debug, thiserror::Error)]
pub enum OwnedFileError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    MultipleLinks(String),
    #[error("{message}")]
    Io {
        message: String,
        #[source]
        source: std::io::Error,
    },
}
