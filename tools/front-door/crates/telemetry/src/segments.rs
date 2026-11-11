//! Dated JSON-lines segments: one file per UTC day in a state directory. The
//! usage ledger and the transcript ingest store share this naming, so a day
//! is the unit that is written, read and bounded.
use provenant_os::fs::{Directory, FsError};

pub(crate) const SUFFIX: &str = ".jsonl";

/// The segment name for the UTC day containing `at`.
pub(crate) fn name(at: jiff::Timestamp) -> String {
    format!("{}{SUFFIX}", at.strftime("%Y-%m-%d"))
}

/// Whether `name` is a dated segment file name.
pub(crate) fn is_segment(name: &str) -> bool {
    name.strip_suffix(SUFFIX)
        .is_some_and(|date| date.len() == 10 && date.parse::<jiff::civil::Date>().is_ok())
}

/// Segment names in a directory, oldest day first. Other entries are skipped.
pub(crate) fn list(directory: &Directory) -> Result<Vec<String>, FsError> {
    let mut names = Vec::new();
    for entry in directory.entries()? {
        if let Some(name) = entry?.to_str().filter(|name| is_segment(name)) {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}
