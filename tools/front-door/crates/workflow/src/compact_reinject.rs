//! SessionStart(compact) state-file discovery and fail-open rendering.

use provenant_records::checked_json;
use regex::Regex;
use serde_json::Value;
use std::{
    io::{self, Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};
use thiserror::Error;

pub const NAME: &str = "STATE.md";
pub const MAX_CHARS: usize = 8000;
pub const CHUNK: usize = 4 << 20;
pub const WRITE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit"];

pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

#[derive(Debug, Error)]
pub enum CompactError {
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid transcript JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    NumberOutOfRange(#[from] checked_json::NumberOutOfRange),
    #[error("transcript field {field} has the wrong type")]
    InvalidField { field: &'static str },
}

/// The filesystem boundary used for transcript and state-file discovery.
pub trait CompactFileSystem {
    fn is_file(&self, path: &Path) -> bool;
    fn read(&self, path: &Path) -> io::Result<Vec<u8>>;
    fn open_read_seek(&self, path: &Path) -> io::Result<Box<dyn ReadSeek>>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct OsCompactFileSystem;

impl CompactFileSystem for OsCompactFileSystem {
    fn is_file(&self, path: &Path) -> bool {
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn open_read_seek(&self, path: &Path) -> io::Result<Box<dyn ReadSeek>> {
        Ok(Box::new(std::fs::File::open(path)?))
    }
}

pub struct ReverseLines {
    file: Box<dyn ReadSeek>,
    position: u64,
    tail: Vec<u8>,
    lines: Vec<Vec<u8>>,
    failed: bool,
}

impl ReverseLines {
    pub fn open(path: &Path, fs: &impl CompactFileSystem) -> io::Result<Self> {
        let mut file = fs.open_read_seek(path)?;
        let position = file.seek(SeekFrom::End(0))?;
        Ok(Self {
            file,
            position,
            tail: vec![],
            lines: vec![],
            failed: false,
        })
    }
}

impl Iterator for ReverseLines {
    type Item = io::Result<Vec<u8>>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.lines.pop() {
                return Some(Ok(line));
            }
            if self.failed {
                return None;
            }
            if self.position == 0 {
                return if self.tail.is_empty() {
                    None
                } else {
                    Some(Ok(std::mem::take(&mut self.tail)))
                };
            }
            let step = self.position.min(CHUNK as u64) as usize;
            self.position -= step as u64;
            let mut data = vec![0; step];
            if let Err(error) = self
                .file
                .seek(SeekFrom::Start(self.position))
                .and_then(|_| self.file.read_exact(&mut data))
            {
                self.failed = true;
                return Some(Err(error));
            }
            data.append(&mut self.tail);
            let mut pieces = data.split(|&byte| byte == b'\n');
            self.tail = pieces.next().unwrap_or_default().to_vec();
            self.lines = pieces.map(<[u8]>::to_vec).collect();
        }
    }
}

fn expanduser(candidate: &str, home: Option<&Path>) -> PathBuf {
    let environment_home = home
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from));
    if let Some(rest) = candidate.strip_prefix("~/") {
        if let Some(home) = environment_home.as_deref() {
            return home.join(rest);
        }
    } else if candidate == "~" {
        if let Some(home) = environment_home {
            return home;
        }
    }
    PathBuf::from(candidate)
}

fn normalise_absolute(path: &Path, cwd: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut result = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let _ = result.pop();
            }
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir | Component::Normal(_) => result.push(component.as_os_str()),
        }
    }
    result
}

/// Find an existing file. Relative names try each supplied directory and its
/// parents, matching the hook's repo-relative fallback behaviour.
pub fn resolve(
    candidate: &str,
    bases: &[PathBuf],
    home: Option<&Path>,
    cwd: &Path,
    fs: &impl CompactFileSystem,
) -> Option<PathBuf> {
    let expanded = expanduser(candidate, home);
    if expanded.is_absolute() {
        return fs.is_file(&expanded).then_some(expanded);
    }
    for base in bases {
        if base.as_os_str().is_empty() {
            continue;
        }
        let mut directory = base.clone();
        loop {
            let found = normalise_absolute(&directory.join(&expanded), cwd);
            if fs.is_file(&found) {
                return Some(found);
            }
            let parent = directory.parent();
            let Some(parent) = parent else { break };
            if parent == directory || parent.as_os_str().is_empty() {
                break;
            }
            directory = parent.to_path_buf();
        }
    }
    None
}

fn regex(pattern: &str) -> Option<Regex> {
    Regex::new(pattern).ok()
}

fn path_matches(command: &str) -> Vec<(usize, usize, String)> {
    let Some(path_pattern) = regex(
        r#"(?:^|[\s\"'=(:])((?:(?:~|\.{1,2}|[\w.-]+)?/(?:[^\s\"'`;|&<>()]*/)?)?STATE\.md)([^\w.-]|$)"#,
    ) else {
        return Vec::new();
    };
    // Python's lookbehind does not consume the boundary before each path.
    // This pattern includes that character because `regex` has no lookbehind,
    // so advance from each full-match start by one Unicode scalar to retain
    // overlapping candidates (for example the destination in `cp a/STATE.md
    // STATE.md`).
    let mut matches = Vec::new();
    let mut search_from = 0;
    while search_from < command.len() {
        let Some(captures) = path_pattern.captures_at(command, search_from) else {
            break;
        };
        let Some(full) = captures.get(0) else {
            break;
        };
        let Some(path) = captures.get(1) else {
            break;
        };
        matches.push((path.start(), path.end(), path.as_str().to_owned()));
        let Some(first) = command[full.start()..].chars().next() else {
            break;
        };
        search_from = full.start().saturating_add(first.len_utf8());
    }
    matches
}

fn is_written(command: &str, start: usize, end: usize) -> bool {
    let before = &command[..start];
    if regex(r#"(?:>>?|\btee(?:\s+-\w+)*)\s*[\"']?$"#)
        .is_some_and(|pattern| pattern.is_match(before))
    {
        return true;
    }
    let Some(separator) = regex(r"[;\n]|&&|\|\|?") else {
        return false;
    };
    let segment_start = separator
        .find_iter(&command[..start])
        .last()
        .map_or(0, |matched| matched.end());
    let rest_start = end;
    let rest_end = separator
        .find(&command[rest_start..])
        .map_or(command.len(), |matched| rest_start + matched.start());
    let command_segment = &command[segment_start..start];
    let rest = &command[rest_start..rest_end];
    let is_copy =
        regex(r"^\s*(?:cp|mv|install)\s").is_some_and(|pattern| pattern.is_match(command_segment));
    is_copy && rest.trim_matches([' ', '\"', '\'']).is_empty()
}

fn tool_ref_value(value: Option<&Value>) -> String {
    value.map_or_else(|| "None".to_owned(), provenant_core::py::python_str)
}

fn references(
    block: &Value,
    cwd_text: &str,
    home: Option<&Path>,
    process_cwd: &Path,
    fs: &impl CompactFileSystem,
) -> Result<Vec<(PathBuf, bool)>, CompactError> {
    let Some(block) = block.as_object() else {
        return Ok(Vec::new());
    };
    let tool = block.get("name");
    let params = match block
        .get("input")
        .filter(|value| provenant_core::py::python_truth(value))
    {
        None => None,
        Some(Value::Object(params)) => Some(params),
        Some(_) => return Err(CompactError::InvalidField { field: "input" }),
    };
    let cwd = PathBuf::from(cwd_text);
    let mut results = Vec::new();
    if tool
        .and_then(Value::as_str)
        .is_some_and(|tool| WRITE_TOOLS.contains(&tool) || tool == "Read")
    {
        if let Some(target) = params
            .and_then(|params| params.get("file_path"))
            .filter(|value| provenant_core::py::python_truth(value))
        {
            let Some(target) = target.as_str() else {
                return Err(CompactError::InvalidField { field: "file_path" });
            };
            if Path::new(target).file_name().and_then(|name| name.to_str()) == Some(NAME) {
                if let Some(found) = resolve(target, &[cwd], home, process_cwd, fs) {
                    let is_write = tool
                        .and_then(Value::as_str)
                        .is_some_and(|name| WRITE_TOOLS.contains(&name));
                    results.push((found, is_write));
                }
            }
        }
        return Ok(results);
    }
    if tool.and_then(Value::as_str) != Some("Bash") {
        return Ok(results);
    }
    let command = match params
        .and_then(|params| params.get("command"))
        .filter(|value| provenant_core::py::python_truth(value))
    {
        None => "",
        Some(Value::String(command)) => command,
        Some(_) => return Err(CompactError::InvalidField { field: "command" }),
    };
    if !command.contains(NAME) {
        return Ok(results);
    }

    let mut bases = vec![cwd.clone()];
    let mut directory = cwd;
    if let Some(cd_pattern) = regex(r#"\bcd\s+(\"[^\"]+\"|'[^']+'|[^\s;&|]+)"#) {
        for captures in cd_pattern.captures_iter(command) {
            let Some(value) = captures.get(1).map(|value| value.as_str()) else {
                continue;
            };
            let cd = value.trim_matches(['\"', '\'']);
            directory = directory.join(expanduser(cd, home));
            bases.insert(0, directory.clone());
        }
    }
    let in_place = regex(r#"sed\s+-i|\.write\(|\.write_text\(|open\([^)]*['"][wa]"#)
        .is_some_and(|pattern| pattern.is_match(command));
    let matches = path_matches(command);
    for (start, end, candidate) in matches.into_iter().rev() {
        if let Some(found) = resolve(&candidate, &bases, home, process_cwd, fs) {
            results.push((found, in_place || is_written(command, start, end)));
        }
    }
    Ok(results)
}

fn failed_ids(entry: &Value) -> Vec<String> {
    let Some(content) = entry
        .get("message")
        .and_then(Value::as_object)
        .and_then(|message| message.get("content"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| {
            block.as_object().is_some_and(|block| {
                block.get("type").and_then(Value::as_str) == Some("tool_result")
                    && block
                        .get("is_error")
                        .is_some_and(provenant_core::py::python_truth)
            })
        })
        .map(|block| tool_ref_value(block.as_object().and_then(|block| block.get("tool_use_id"))))
        .collect()
}

pub fn last_written_state(
    transcript: &Path,
    home: Option<&Path>,
    process_cwd: &Path,
    fs: &impl CompactFileSystem,
) -> Result<Option<PathBuf>, CompactError> {
    let mut lines = ReverseLines::open(transcript, fs).map_err(|source| CompactError::Io {
        path: transcript.to_path_buf(),
        source,
    })?;
    let mut failed = std::collections::HashSet::<String>::new();
    for raw in &mut lines {
        let raw = raw.map_err(|source| CompactError::Io {
            path: transcript.to_path_buf(),
            source,
        })?;
        let has_error = raw.windows(15).any(|window| window == b"\"is_error\":true")
            || raw
                .windows(16)
                .any(|window| window == b"\"is_error\": true");
        if !has_error
            && (!raw.windows(NAME.len()).any(|part| part == NAME.as_bytes())
                || !raw.windows(10).any(|part| part == b"\"tool_use\""))
        {
            continue;
        }
        let entry: Value = match checked_json::from_slice(&raw) {
            Ok(entry) => entry,
            Err(checked_json::DecodeError::Json(_)) => continue,
            Err(checked_json::DecodeError::NumberOutOfRange(error)) => {
                return Err(error.into());
            }
        };
        if !entry.is_object() {
            continue;
        }
        if has_error {
            failed.extend(failed_ids(&entry));
        }
        let Some(content) = entry
            .get("message")
            .and_then(Value::as_object)
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let cwd = entry.get("cwd").and_then(Value::as_str).unwrap_or_default();
        for block in content.iter().rev() {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let id = tool_ref_value(block.as_object().and_then(|block| block.get("id")));
            if failed.contains(&id) {
                continue;
            }
            for (path, is_write) in references(block, cwd, home, process_cwd, fs)? {
                if is_write {
                    return Ok(Some(path));
                }
            }
        }
    }
    Ok(None)
}

fn decode_python_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n")
}

pub fn render_text(path: &str, text: &str) -> String {
    let mut chars = text.chars();
    let mut retained: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.next().is_some() {
        retained.push_str(&format!("\n[... truncated; read the rest from {path}]"));
    }
    format!(
        "Context was just compacted. This session's state file is {path} (re-injected below). Its standing directions still bind; keep maintaining this file.\n\n{retained}"
    )
}

pub fn render(path: &Path, fs: &impl CompactFileSystem) -> Result<String, CompactError> {
    let bytes = fs.read(path).map_err(|source| CompactError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let text = decode_python_text(&bytes);
    Ok(render_text(&path.to_string_lossy(), &text))
}

/// Run the hook's JSON event path. Any error is swallowed to preserve its
/// fail-open contract; `None` means that the hook prints nothing.
pub fn run_hook(
    input: &str,
    home: Option<&Path>,
    process_cwd: &Path,
    fs: &impl CompactFileSystem,
) -> Option<String> {
    let value: Value = checked_json::from_str(input).ok()?;
    let transcript = value.get("transcript_path")?.as_str()?;
    let transcript = Path::new(transcript);
    if !fs.is_file(transcript) {
        return None;
    }
    let path = last_written_state(transcript, home, process_cwd, fs).ok()??;
    render(&path, fs).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn oracle(cases: Value) -> Option<Value> {
        crate::test_support::oracle_script("oracle_session.py", "compact_reinject", &cases)
    }

    #[test]
    fn reverse_lines_matches_oracle_across_chunk_and_empty_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join("transcript.jsonl");
        for bytes in [
            vec![],
            b"\n".to_vec(),
            b"first\nlast\n".to_vec(),
            b"first\nlast".to_vec(),
            [b"first\n".as_slice(), &vec![b'a'; CHUNK + 2], b"\nlast"].concat(),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            if let Some(expected) = oracle(json!([{"op":"reverse","path":path}])) {
                let lines = ReverseLines::open(&path, &OsCompactFileSystem)
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                let actual: Vec<_> = lines
                    .iter()
                    .map(|line| {
                        use sha2::{Digest, Sha256};
                        let digest = format!(
                            "sha256:{}",
                            Sha256::digest(line)
                                .iter()
                                .map(|byte| format!("{byte:02x}"))
                                .collect::<String>()
                        );
                        json!({"length":line.len(),"digest":digest})
                    })
                    .collect();
                assert_eq!(json!(actual), expected[0]);
            }
        }
    }

    #[test]
    fn hook_finds_last_successful_write_and_ignores_reads_and_failed_writes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let first = root.join("first").join(NAME);
        let last = root.join("last").join(NAME);
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::create_dir_all(last.parent().unwrap()).unwrap();
        std::fs::write(&first, "first body\n").unwrap();
        std::fs::write(&last, "last body\n").unwrap();
        let transcript = root.join("transcript.jsonl");
        let lines = [
            json!({"cwd":root,"message":{"content":[{"type":"tool_use","name":"Write","id":"good","input":{"file_path":first}}]}}),
            json!({"message":{"content":[{"type":"tool_result","tool_use_id":"good","is_error":false}]}}),
            json!({"cwd":root,"message":{"content":[{"type":"tool_use","name":"Edit","id":"bad","input":{"file_path":last}}]}}),
            json!({"message":{"content":[{"type":"tool_result","tool_use_id":"bad","is_error":true}]}}),
            json!({"cwd":root,"message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":first}}]}}),
        ];
        std::fs::write(
            &transcript,
            lines
                .iter()
                .map(Value::to_string)
                .collect::<Vec<_>>()
                .join("\n")
                + "\n",
        )
        .unwrap();
        let fs = OsCompactFileSystem;
        let input = json!({"transcript_path":transcript}).to_string();
        let actual = run_hook(&input, None, &root, &fs).unwrap();
        assert!(actual.contains("first body"));
        assert!(!actual.contains("last body"));
        if let Some(expected) = oracle(json!([{"op":"hook","input":input,"cwd":root}])) {
            assert_eq!(json!(actual), expected[0]);
        }
    }

    #[test]
    fn command_path_collection_matches_oracle_and_unicode_truncation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let state = root.join("sessions").join(NAME);
        std::fs::create_dir_all(state.parent().unwrap()).unwrap();
        std::fs::write(&state, "é🦀".repeat(4100)).unwrap();
        let transcript = root.join("transcript.jsonl");
        let entry = json!({"cwd":root,"message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cd sessions && printf x > STATE.md"}}]}});
        std::fs::write(&transcript, entry.to_string() + "\n").unwrap();
        let fs = OsCompactFileSystem;
        let input = json!({"transcript_path":transcript}).to_string();
        let actual = run_hook(&input, None, &root, &fs).unwrap();
        assert!(actual.contains("truncated; read the rest from"));
        if let Some(expected) = oracle(json!([{"op":"hook","input":input,"cwd":root}])) {
            assert_eq!(json!(actual), expected[0]);
        }
    }

    #[test]
    fn copy_destination_after_state_path_remains_a_write_candidate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source_dir = root.join("source");
        std::fs::create_dir_all(&source_dir).unwrap();
        std::fs::write(source_dir.join(NAME), "source state\n").unwrap();
        std::fs::write(root.join(NAME), "destination state\n").unwrap();
        let transcript = root.join("transcript.jsonl");
        let entry = json!({
            "cwd":root,
            "message":{"content":[{"type":"tool_use","name":"Bash","input":{"command":"cp source/STATE.md STATE.md"}}]}
        });
        std::fs::write(&transcript, entry.to_string() + "\n").unwrap();

        let fs = OsCompactFileSystem;
        let input = json!({"transcript_path":transcript}).to_string();
        let actual = run_hook(&input, None, &root, &fs).unwrap();
        assert!(actual.contains("destination state"));
        assert!(!actual.contains("source state"));
        if let Some(expected) = oracle(json!([{"op":"hook","input":input,"cwd":root}])) {
            assert_eq!(json!(actual), expected[0]);
        }
    }
}
