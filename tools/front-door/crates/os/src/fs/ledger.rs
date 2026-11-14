//! JSONL storage shared by telemetry and quota history. Appends use O_APPEND
//! and exactly one write per record. Rotation renames whole files; open
//! appenders retain their descriptor into the renamed tail.
use super::{Directory, FsError};
use std::{
    ffi::OsStr,
    io::{self, Write},
};

pub struct Ledger<'a> {
    directory: &'a Directory,
    current: &'a OsStr,
}

impl<'a> Ledger<'a> {
    pub fn new(directory: &'a Directory, current: &'a OsStr) -> Self {
        Self { directory, current }
    }

    /// `line` must contain one complete JSON record, including its newline.
    /// A leading newline may isolate a fragment left by a crashed writer.
    pub fn append(&self, line: &[u8]) -> Result<(), FsError> {
        if !line.ends_with(b"\n") {
            return Err(self.io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ledger record needs a newline",
            )));
        }
        let mut file = self.directory.open_append(self.current)?;
        let written = file.write(line).map_err(|error| self.io(error))?;
        if written != line.len() {
            // Never retry a short append: another writer may already follow it.
            return Err(self.io(io::Error::new(
                io::ErrorKind::WriteZero,
                "short ledger append",
            )));
        }
        file.sync()?;
        self.directory.sync()
    }

    /// Keep `tail` renamed predecessors, newest first. No age-based deletion.
    /// Call before append so new records normally enter the current file.
    pub fn rotate(&self, max_bytes: u64, incoming: u64, tail: &[&OsStr]) -> Result<bool, FsError> {
        let size = match self.directory.open_read(self.current) {
            Ok(file) => file.metadata()?.size,
            Err(error) if error.is_not_found() => return Ok(false),
            Err(error) => return Err(error),
        };
        if size == 0 || size.saturating_add(incoming) <= max_bytes || tail.is_empty() {
            return Ok(false);
        }
        for (from, to) in tail.iter().zip(tail.iter().skip(1)).rev() {
            self.rename(from, to)?;
        }
        self.rename(self.current, tail[0])?;
        self.directory.sync()?;
        Ok(true)
    }

    fn rename(&self, from: &OsStr, to: &OsStr) -> Result<(), FsError> {
        match self.directory.replace(from, self.directory, to) {
            Ok(()) => Ok(()),
            // Another rotator may have moved the source first.
            Err(error) if error.is_not_found() => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn io(&self, source: io::Error) -> FsError {
        FsError::ReadIo {
            path: self.current.into(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn concurrent_records_are_complete_and_parseable() {
        let root = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let path = root.path();
                scope.spawn(move || {
                    let directory = Directory::open_anchor(path).unwrap();
                    let ledger = Ledger::new(&directory, OsStr::new("events.jsonl"));
                    for row in 0..32 {
                        ledger
                            .append(format!("{{\"writer\":{writer},\"row\":{row}}}\n").as_bytes())
                            .unwrap();
                    }
                });
            }
        });
        let text = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        let rows: std::collections::BTreeSet<_> = text
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line).unwrap();
                (
                    row["writer"].as_u64().unwrap(),
                    row["row"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(text.lines().count(), 256);
        assert_eq!(rows.len(), 256);
    }

    #[test]
    fn rotation_keeps_the_configured_tail() {
        let root = tempfile::tempdir().unwrap();
        let directory = Directory::open_anchor(root.path()).unwrap();
        let current = OsStr::new("events.jsonl");
        let ledger = Ledger::new(&directory, current);
        let tail = [OsStr::new("events.jsonl.1"), OsStr::new("events.jsonl.2")];
        for row in 0..4 {
            let line = format!("{row}\n");
            ledger.rotate(2, line.len() as u64, &tail).unwrap();
            ledger.append(line.as_bytes()).unwrap();
        }
        for (name, expected) in [(current, "3\n"), (tail[0], "2\n"), (tail[1], "1\n")] {
            let mut text = String::new();
            directory
                .open_read(name)
                .unwrap()
                .read_to_string(&mut text)
                .unwrap();
            assert_eq!(text, expected);
        }
    }
}
