//! Bounded-memory transcript lines and state rendering. Tool-reference discovery
//! and the fail-open session hook remain in the Python owner.
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
};
pub const NAME: &str = "STATE.md";
pub const MAX_CHARS: usize = 8000;
pub const CHUNK: usize = 4 << 20;
pub const WRITE_TOOLS: &[&str] = &["Write", "Edit", "MultiEdit"];

pub struct ReverseLines {
    file: File,
    position: u64,
    tail: Vec<u8>,
    lines: Vec<Vec<u8>>,
    failed: bool,
}
impl ReverseLines {
    pub fn open(path: &Path) -> io::Result<Self> {
        let mut file = File::open(path)?;
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
            let mut pieces = data.split(|&b| b == b'\n');
            self.tail = pieces.next().unwrap_or_default().to_vec();
            self.lines = pieces.map(<[u8]>::to_vec).collect();
        }
    }
}

pub fn render(path: &Path) -> io::Result<String> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    Ok(render_text(&path.to_string_lossy(), &text))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn reverse_lines_matches_python_across_chunks_and_empty_ends() {
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
            let actual = ReverseLines::open(&path)
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            if let Some(expected) =
                crate::test_support::oracle("compact_reverse", &json!([{"path":path}]))
            {
                let actual: Vec<_> = actual
                    .iter()
                    .map(|line| {
                        use sha2::{Digest, Sha256};
                        let digest = format!(
                            "sha256:{}",
                            Sha256::digest(line)
                                .iter()
                                .map(|b| format!("{b:02x}"))
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
    fn state_truncation_counts_unicode_scalars_and_replaces_invalid_utf8() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().canonicalize().unwrap().join(NAME);
        for bytes in [
            b"line\r\nsecond\rthird".to_vec(),
            "é🦀".repeat(4001).into_bytes(),
            vec![255, 255, 0xe2, 0x82],
            vec![],
        ] {
            std::fs::write(&path, &bytes).unwrap();
            if let Some(expected) =
                crate::test_support::oracle("compact_render", &json!([{"path":path}]))
            {
                assert_eq!(json!(render(&path).unwrap()), expected[0]);
            }
        }
    }
}
