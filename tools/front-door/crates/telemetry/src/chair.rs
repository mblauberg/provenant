//! Chair token usage, read on demand from a session's own Claude Code
//! transcript. The SessionStart hook only learns which account a session runs
//! under; tokens exist once the session has run, so the report reads them.
//! Only assistant message IDs, models and usage counts are kept.
use crate::usage::ChairSession;
use provenant_records::Usage;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

/// Largest transcript read for one session.
const MAX_TRANSCRIPT_BYTES: u64 = 1 << 30;
/// Bytes read across every session of one report.
const MAX_TOTAL_BYTES: u64 = 2 << 30;
/// A longer line is a tool result or attachment, never a usage record.
const MAX_LINE_BYTES: usize = 8 << 20;
const MAX_PROJECT_DIRS: usize = 10_000;
const MAX_SUBAGENTS: usize = 2_000;
const MAX_MESSAGES: usize = 1_000_000;

/// One model's total within one chair session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChairModelUsage {
    pub model: String,
    pub usage: Usage,
}

/// Why a chair session has no token counts. Never silent: the report counts
/// these sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unread {
    NoConfigDir,
    TranscriptNotFound,
    TranscriptUnreadable,
    TranscriptTooLarge,
    ReadBudgetExhausted,
}

#[derive(Deserialize)]
struct Record {
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<Message>,
}
#[derive(Deserialize)]
struct Message {
    id: Option<String>,
    model: Option<String>,
    usage: Option<Raw>,
}
#[derive(Clone, Default, Deserialize)]
struct Raw {
    input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_creation: Option<Split>,
    output_tokens: Option<u64>,
}
#[derive(Clone, Default, Deserialize)]
struct Split {
    ephemeral_1h_input_tokens: Option<u64>,
}
impl Raw {
    /// A message repeats as it streams; the largest count of each field is its
    /// final one.
    fn merge(&mut self, other: Self) {
        let max = |a: Option<u64>, b: Option<u64>| a.max(b);
        self.input_tokens = max(self.input_tokens, other.input_tokens);
        self.cache_read_input_tokens =
            max(self.cache_read_input_tokens, other.cache_read_input_tokens);
        self.cache_creation_input_tokens = max(
            self.cache_creation_input_tokens,
            other.cache_creation_input_tokens,
        );
        self.output_tokens = max(self.output_tokens, other.output_tokens);
        let one_hour = |raw: &Self| raw.cache_creation.as_ref()?.ephemeral_1h_input_tokens;
        let merged = max(one_hour(self), one_hour(&other));
        self.cache_creation = merged.map(|n| Split {
            ephemeral_1h_input_tokens: Some(n),
        });
    }
}

#[derive(Default)]
struct Tally {
    messages: BTreeMap<String, (String, Raw)>,
}
impl Tally {
    fn add_line(&mut self, line: &[u8]) -> Result<(), Unread> {
        // Most lines are prompts and tool results; only an assistant record
        // carries a `usage` object.
        if !line.windows(7).any(|window| window == b"\"usage\"") {
            return Ok(());
        }
        let Ok(record) = serde_json::from_slice::<Record>(line) else {
            return Ok(());
        };
        if record.kind.as_deref() != Some("assistant") {
            return Ok(());
        }
        let Some(Message {
            id: Some(id),
            model: Some(model),
            usage: Some(usage),
        }) = record.message
        else {
            return Ok(());
        };
        if id.is_empty() || model.is_empty() || model == "<synthetic>" {
            return Ok(());
        }
        if !self.messages.contains_key(&id) && self.messages.len() >= MAX_MESSAGES {
            return Err(Unread::TranscriptTooLarge);
        }
        match self.messages.entry(id) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert((model, usage));
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if entry.get().0 == model {
                    entry.get_mut().1.merge(usage);
                }
            }
        }
        Ok(())
    }
    fn finish(self) -> Vec<ChairModelUsage> {
        let mut by_model = BTreeMap::<String, (Usage, bool)>::new();
        for (model, raw) in self.messages.into_values() {
            let (usage, split) = by_model.entry(model).or_default();
            let add = |slot: &mut Option<u64>, n: u64| {
                *slot = Some(slot.unwrap_or(0).saturating_add(n));
            };
            add(&mut usage.uncached_input, raw.input_tokens.unwrap_or(0));
            add(&mut usage.cache_read, raw.cache_read_input_tokens.unwrap_or(0));
            add(
                &mut usage.cache_write,
                raw.cache_creation_input_tokens.unwrap_or(0),
            );
            add(
                &mut usage.output_excluding_reasoning,
                raw.output_tokens.unwrap_or(0),
            );
            if let Some(one_hour) = raw
                .cache_creation
                .and_then(|split| split.ephemeral_1h_input_tokens)
            {
                add(&mut usage.cache_write_1h, one_hour);
                *split = true;
            }
        }
        by_model
            .into_iter()
            .map(|(model, (mut usage, split))| {
                if !split {
                    usage.cache_write_1h = None;
                }
                ChairModelUsage { model, usage }
            })
            .collect()
    }
}

fn valid_session(session: &str) -> bool {
    (1..=64).contains(&session.len())
        && session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn is_plain(path: &Path, directory: bool) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| {
        if directory {
            meta.is_dir()
        } else {
            meta.is_file()
        }
    })
}

/// The session's transcript and its sub-agent transcripts. Links are never
/// followed.
fn locate(config: &Path, session: &str) -> Result<Vec<PathBuf>, Unread> {
    let projects = config.join("projects");
    let entries = std::fs::read_dir(&projects).map_err(|_| Unread::TranscriptNotFound)?;
    let mut files = Vec::new();
    for (seen, entry) in entries.enumerate() {
        if seen >= MAX_PROJECT_DIRS {
            return Err(Unread::TranscriptTooLarge);
        }
        let directory = entry.map_err(|_| Unread::TranscriptUnreadable)?.path();
        if !is_plain(&directory, true) {
            continue;
        }
        let main = directory.join(format!("{session}.jsonl"));
        if !is_plain(&main, false) {
            continue;
        }
        files.push(main);
        let subagents = directory.join(session).join("subagents");
        if is_plain(&subagents, true)
            && let Ok(entries) = std::fs::read_dir(&subagents)
        {
            for entry in entries.flatten().take(MAX_SUBAGENTS) {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("agent-")
                    && name.ends_with(".jsonl")
                    && is_plain(&entry.path(), false)
                {
                    files.push(entry.path());
                }
            }
        }
        break;
    }
    if files.is_empty() {
        Err(Unread::TranscriptNotFound)
    } else {
        Ok(files)
    }
}

fn read_file(path: &Path, tally: &mut Tally, budget: &mut u64) -> Result<(), Unread> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| Unread::TranscriptUnreadable)?;
    let meta = file.metadata().map_err(|_| Unread::TranscriptUnreadable)?;
    if !meta.is_file() {
        return Err(Unread::TranscriptUnreadable);
    }
    if meta.len() > MAX_TRANSCRIPT_BYTES {
        return Err(Unread::TranscriptTooLarge);
    }
    if meta.len() > *budget {
        return Err(Unread::ReadBudgetExhausted);
    }
    // The file can grow while read; bound the bytes taken, not only its size.
    let allowance = meta.len().saturating_add(1 << 20).min(*budget);
    let mut reader = BufReader::with_capacity(1 << 20, file.take(allowance));
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = (&mut reader)
            .take(MAX_LINE_BYTES as u64 + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| Unread::TranscriptUnreadable)?;
        if read == 0 {
            break;
        }
        *budget = budget.saturating_sub(read as u64);
        if line.len() > MAX_LINE_BYTES && line.last() != Some(&b'\n') {
            // Drain the rest of an oversized line without keeping it.
            let mut sink = Vec::new();
            loop {
                sink.clear();
                let drained = (&mut reader)
                    .take(1 << 20)
                    .read_until(b'\n', &mut sink)
                    .map_err(|_| Unread::TranscriptUnreadable)?;
                *budget = budget.saturating_sub(drained as u64);
                if drained == 0 || sink.last() == Some(&b'\n') {
                    break;
                }
            }
            continue;
        }
        tally.add_line(&line)?;
    }
    Ok(())
}

/// Token counts of one session, summed over its transcript and its sub-agents'.
fn session_usage(config: &Path, session: &str, budget: &mut u64) -> Result<Vec<ChairModelUsage>, Unread> {
    if !valid_session(session) {
        return Err(Unread::TranscriptNotFound);
    }
    let mut tally = Tally::default();
    for file in locate(config, session)? {
        read_file(&file, &mut tally, budget)?;
    }
    Ok(tally.finish())
}

/// Fill each session's token counts. `config_dir` names the Claude config
/// directory a session ran under, or `None` when that is unknown. A session
/// that cannot be read keeps its reason instead of being dropped.
pub fn attach_usage(sessions: &mut [ChairSession], config_dir: impl Fn(&ChairSession) -> Option<PathBuf>) {
    let mut budget = MAX_TOTAL_BYTES;
    let mut done = BTreeMap::<(String, String, Option<PathBuf>), Result<Vec<ChairModelUsage>, Unread>>::new();
    for chair in sessions.iter_mut() {
        let config = config_dir(chair);
        let key = (chair.account.clone(), chair.session.clone(), config.clone());
        let result = done.entry(key).or_insert_with(|| match &config {
            Some(config) => session_usage(config, &chair.session, &mut budget),
            None => Err(Unread::NoConfigDir),
        });
        match result {
            Ok(models) => {
                chair.usage.clone_from(models);
                chair.unread = None;
            }
            Err(reason) => {
                chair.usage.clear();
                chair.unread = Some(*reason);
            }
        }
    }
}

#[cfg(test)]
#[path = "chair/tests.rs"]
mod tests;
