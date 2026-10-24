//! Host-only, bounded identity projection from the selected attempt's Codex home.
use crate::ProviderError;
use provenant_supervisor::fs::Directory;
use serde::Deserialize;
use std::{
    ffi::OsStr,
    io::{BufRead, BufReader, Read},
    path::Path,
};

#[derive(Deserialize)]
#[serde(tag = "type")]
enum Record {
    #[serde(rename = "session_meta")]
    Session { payload: Session },
    #[serde(rename = "turn_context")]
    Turn { payload: Turn },
    #[serde(other)]
    Other,
}
#[derive(Deserialize)]
struct Session {
    id: String,
}
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Turn {
    pub model: String,
    pub effort: Option<String>,
}

/// Never reads prompts/messages into a retained record. The only returned fields
/// are model and effort, bound to the exec stream's provider-written thread id.
pub fn observe(home: &Path, thread: &str) -> Result<Turn, ProviderError> {
    if thread.is_empty()
        || thread.len() > 64
        || !thread
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(ProviderError::UnknownIdentity);
    }
    let sessions = Directory::open_anchor(home)
        .and_then(|d| d.open_verified_directory(OsStr::new("sessions")))
        .map_err(|_| ProviderError::UnknownIdentity)?;
    let suffix = format!("-{thread}.jsonl");
    let mut visited = 0;
    let mut found = None;
    let mut matched = false;
    for year in sessions
        .entries()
        .map_err(|_| ProviderError::UnknownIdentity)?
    {
        let year = year.map_err(|_| ProviderError::UnknownIdentity)?;
        visit(&mut visited)?;
        if !date_component(&year, 4) {
            continue;
        }
        let year = sessions
            .open_verified_directory(&year)
            .map_err(|_| ProviderError::UnknownIdentity)?;
        for month in year.entries().map_err(|_| ProviderError::UnknownIdentity)? {
            let month = month.map_err(|_| ProviderError::UnknownIdentity)?;
            visit(&mut visited)?;
            if !date_component(&month, 2) {
                continue;
            }
            let month = year
                .open_verified_directory(&month)
                .map_err(|_| ProviderError::UnknownIdentity)?;
            for day in month
                .entries()
                .map_err(|_| ProviderError::UnknownIdentity)?
            {
                let day = day.map_err(|_| ProviderError::UnknownIdentity)?;
                visit(&mut visited)?;
                if !date_component(&day, 2) {
                    continue;
                }
                let day = month
                    .open_verified_directory(&day)
                    .map_err(|_| ProviderError::UnknownIdentity)?;
                for name in day.entries().map_err(|_| ProviderError::UnknownIdentity)? {
                    visit(&mut visited)?;
                    let name = name.map_err(|_| ProviderError::UnknownIdentity)?;
                    if !name
                        .to_str()
                        .is_some_and(|s| s.starts_with("rollout-") && s.ends_with(&suffix))
                    {
                        continue;
                    }
                    if matched {
                        return Err(ProviderError::IdentityMismatch);
                    }
                    matched = true;
                    let file = day
                        .open_read(&name)
                        .map_err(|_| ProviderError::UnknownIdentity)?;
                    if file.metadata().map_err(|_| ProviderError::Malformed)?.size
                        > 32 * 1024 * 1024
                    {
                        return Err(ProviderError::Overflow);
                    }
                    let mut reader = BufReader::new(file.take(32 * 1024 * 1024 + 1));
                    let mut line = Vec::new();
                    let mut session = false;
                    let mut turn = None;
                    loop {
                        line.clear();
                        let count = (&mut reader)
                            .take(1024 * 1024 + 1)
                            .read_until(b'\n', &mut line)
                            .map_err(|_| ProviderError::Malformed)?;
                        if count == 0 {
                            break;
                        }
                        if count > 1024 * 1024 {
                            return Err(ProviderError::Overflow);
                        }
                        match serde_json::from_slice::<Record>(&line)
                            .map_err(|_| ProviderError::Malformed)?
                        {
                            Record::Session { payload } if payload.id == thread && !session => {
                                session = true
                            }
                            Record::Session { .. } => return Err(ProviderError::IdentityMismatch),
                            Record::Turn { payload } => {
                                if payload.model.is_empty()
                                    || turn.as_ref().is_some_and(|old| old != &payload)
                                {
                                    return Err(ProviderError::IdentityMismatch);
                                }
                                turn = Some(payload);
                            }
                            Record::Other => {}
                        }
                    }
                    if !session {
                        return Err(ProviderError::UnknownIdentity);
                    }
                    found = turn;
                }
            }
        }
    }
    found.ok_or(ProviderError::UnknownIdentity)
}
fn date_component(value: &OsStr, len: usize) -> bool {
    value
        .to_str()
        .is_some_and(|v| v.len() == len && v.bytes().all(|b| b.is_ascii_digit()))
}

fn visit(count: &mut usize) -> Result<(), ProviderError> {
    *count += 1;
    if *count > 4096 {
        Err(ProviderError::Overflow)
    } else {
        Ok(())
    }
}
