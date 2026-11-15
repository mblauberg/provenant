//! Shared, bounded input for commands that accept a task brief.
use std::{
    fmt,
    io::{self, Read},
    path::PathBuf,
};

/// Largest brief accepted by a command that does not own a narrower limit.
pub const MAX_BRIEF_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BriefSource {
    Text(String),
    File(PathBuf),
    Stdin,
}

impl BriefSource {
    pub fn file(path: PathBuf) -> Self {
        if path.as_path() == std::path::Path::new("-") {
            Self::Stdin
        } else {
            Self::File(path)
        }
    }
}

#[derive(Debug)]
pub enum BriefError {
    MultipleSources,
    TooLarge { limit: usize },
    InvalidUtf8,
    Io(io::Error),
}

impl fmt::Display for BriefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MultipleSources => write!(f, "choose only one brief source"),
            Self::TooLarge { limit } => write!(f, "brief exceeds {limit} bytes"),
            Self::InvalidUtf8 => write!(f, "brief must be UTF-8"),
            Self::Io(error) => write!(f, "brief could not be read: {error}"),
        }
    }
}

impl std::error::Error for BriefError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Select one source without reading it. Callers resolve their target and
/// command-specific cardinality before passing stdin to `read_brief`.
pub fn select_brief_source(
    sources: impl IntoIterator<Item = Option<BriefSource>>,
) -> Result<Option<BriefSource>, BriefError> {
    let mut selected = None;
    for source in sources.into_iter().flatten() {
        if selected.is_some() {
            return Err(BriefError::MultipleSources);
        }
        selected = Some(source);
    }
    Ok(selected)
}

/// Read one selected brief, refusing input beyond the caller's byte limit.
#[allow(
    clippy::disallowed_methods,
    reason = "stdin is an explicit real-time boundary"
)]
pub fn read_brief(source: BriefSource, limit: usize) -> Result<String, BriefError> {
    let mut bytes = Vec::new();
    let read = match source {
        BriefSource::Text(text) => {
            if text.len() > limit {
                return Err(BriefError::TooLarge { limit });
            }
            return Ok(text);
        }
        BriefSource::File(path) => std::fs::File::open(path)
            .map_err(BriefError::Io)?
            .take(limit.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(BriefError::Io)?,
        BriefSource::Stdin => {
            let bytes = provenant_os::sys::hook_io::read_stdin(
                limit,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .map_err(|error| {
                if error.kind() == io::ErrorKind::FileTooLarge {
                    BriefError::TooLarge { limit }
                } else {
                    BriefError::Io(error)
                }
            })?;
            return String::from_utf8(bytes).map_err(|_| BriefError::InvalidUtf8);
        }
    };
    if read > limit {
        return Err(BriefError::TooLarge { limit });
    }
    String::from_utf8(bytes).map_err(|_| BriefError::InvalidUtf8)
}

#[cfg(test)]
mod tests {
    use super::{BriefError, BriefSource, read_brief, select_brief_source};
    use std::path::PathBuf;

    #[test]
    fn source_selection_refuses_ambiguous_inputs_without_reading_them() {
        let selected = select_brief_source([
            Some(BriefSource::Text("positional".into())),
            Some(BriefSource::Text("named".into())),
        ]);
        assert!(matches!(selected, Err(BriefError::MultipleSources)));
    }

    #[test]
    fn file_source_is_bounded_and_accepts_stdin_spelling() {
        let path = std::env::temp_dir().join(format!("provenant-brief-{}", std::process::id()));
        std::fs::write(&path, b"brief").unwrap();
        assert_eq!(
            read_brief(BriefSource::File(path.clone()), 5).unwrap(),
            "brief"
        );
        assert!(matches!(
            read_brief(BriefSource::File(path.clone()), 4),
            Err(BriefError::TooLarge { limit: 4 })
        ));
        let _ = std::fs::remove_file(path);
        assert_eq!(BriefSource::file(PathBuf::from("-")), BriefSource::Stdin);
    }
}
