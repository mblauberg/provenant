//! Streaming, bounded evidence from a result artifact.
use sha2::{Digest as _, Sha256};

const HEAD_BYTES: usize = 4096;
const TAIL_BYTES: usize = 16384;

/// A streaming accumulator. Storage is bounded by the window sizes and the
/// already validated required verdict, irrespective of artifact length.
pub struct ResultCapture {
    hash: Sha256,
    head: Vec<u8>,
    tail: Vec<u8>,
    bytes: u64,
    required: Option<String>,
    matched_required: bool,
    matched_bytes: usize,
    line_started: bool,
    line_invalid: bool,
    utf8: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedResult {
    pub digest: String,
    pub excerpt: String,
    pub tail: String,
    pub truncated: bool,
    pub source_truncated: bool,
    pub tail_truncated: bool,
    pub matched_required: bool,
}

impl ResultCapture {
    pub fn new(required_verdict: Option<&str>) -> Self {
        Self {
            hash: Sha256::new(),
            head: Vec::new(),
            tail: Vec::new(),
            bytes: 0,
            required: required_verdict.map(|value| value.trim().to_owned()),
            matched_required: false,
            matched_bytes: 0,
            line_started: false,
            line_invalid: false,
            utf8: Vec::with_capacity(4),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.hash.update(chunk);
        self.bytes = self.bytes.saturating_add(chunk.len() as u64);
        let take = HEAD_BYTES.saturating_sub(self.head.len()).min(chunk.len());
        self.head.extend_from_slice(&chunk[..take]);
        if chunk.len() >= TAIL_BYTES {
            self.tail.clear();
            self.tail
                .extend_from_slice(&chunk[chunk.len() - TAIL_BYTES..]);
        } else {
            let discard = self
                .tail
                .len()
                .saturating_add(chunk.len())
                .saturating_sub(TAIL_BYTES);
            self.tail.drain(..discard);
            self.tail.extend_from_slice(chunk);
        }
        if self.required.is_some() && !self.matched_required {
            for &byte in chunk {
                self.utf8.push(byte);
                loop {
                    match std::str::from_utf8(&self.utf8) {
                        Ok(text) => {
                            let ch = text.chars().next();
                            self.utf8.clear();
                            if let Some(ch) = ch {
                                self.character(ch);
                            }
                            break;
                        }
                        Err(error) if error.error_len().is_none() && self.utf8.len() < 4 => break,
                        Err(_) => {
                            self.line_invalid = true;
                            self.utf8.remove(0);
                            if self.utf8.is_empty() {
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    fn character(&mut self, ch: char) {
        if ch == '\n' {
            self.end_line();
            return;
        }
        let Some(required) = self.required.as_deref() else {
            return;
        };
        if !self.line_started && ch.is_whitespace() {
            return;
        }
        self.line_started = true;
        if self.matched_bytes == required.len() {
            if !ch.is_whitespace() {
                self.line_invalid = true;
            }
        } else if required[self.matched_bytes..].starts_with(ch) {
            self.matched_bytes += ch.len_utf8();
        } else {
            self.line_invalid = true;
        }
    }

    fn end_line(&mut self) {
        if !self.line_invalid
            && self.line_started
            && self
                .required
                .as_ref()
                .is_some_and(|required| self.matched_bytes == required.len())
        {
            self.matched_required = true;
        }
        self.matched_bytes = 0;
        self.line_started = false;
        self.line_invalid = false;
    }

    pub fn finish(mut self) -> CapturedResult {
        if !self.utf8.is_empty() {
            self.line_invalid = true;
        }
        self.end_line();
        let digest = format!("sha256:{}", provenant_core::hex(&self.hash.finalize()));
        // Window boundaries can bisect a character. Lossy decoding also keeps
        // malformed artifact bytes from becoming unbounded allocations.
        let excerpt = window(&self.head, HEAD_BYTES, false);
        let tail = window(&self.tail, TAIL_BYTES, true);
        let tail_truncated = self.bytes > TAIL_BYTES as u64
            || self.tail.first().is_some_and(|byte| byte & 0xc0 == 0x80)
            || String::from_utf8_lossy(&self.tail).len() > TAIL_BYTES;
        let truncated = self.bytes > HEAD_BYTES as u64
            || String::from_utf8_lossy(&self.head).len() > HEAD_BYTES;
        CapturedResult {
            digest,
            excerpt,
            tail,
            truncated,
            source_truncated: false,
            tail_truncated,
            matched_required: self.matched_required,
        }
    }
}

fn window(bytes: &[u8], cap: usize, tail: bool) -> String {
    let mut start = 0;
    if tail {
        while start < bytes.len() && bytes[start] & 0xc0 == 0x80 {
            start += 1;
        }
    }
    let text = String::from_utf8_lossy(&bytes[start..]);
    if tail {
        let mut start = text.len().saturating_sub(cap);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        text[start..].to_owned()
    } else {
        let mut end = text.len().min(cap);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_all_bytes_and_keeps_bounded_windows() {
        let data = vec![b'x'; 100_000];
        let mut capture = ResultCapture::new(None);
        for chunk in data.chunks(137) {
            capture.push(chunk);
        }
        let result = capture.finish();
        assert_eq!(
            result.digest,
            format!("sha256:{}", provenant_core::hex(&Sha256::digest(&data)))
        );
        assert_eq!(result.excerpt.len(), HEAD_BYTES);
        assert_eq!(result.tail.len(), TAIL_BYTES);
        assert!(result.truncated);
    }

    #[test]
    fn unicode_boundaries_and_verdict_in_middle() {
        let data = format!(
            "{}é\n\u{2003}VERDICT: PASS\u{2003}\n{}",
            "a".repeat(4095),
            "z".repeat(20000)
        );
        let mut capture = ResultCapture::new(Some("VERDICT: PASS"));
        for chunk in data.as_bytes().chunks(2) {
            capture.push(chunk);
        }
        let result = capture.finish();
        assert!(result.matched_required);
        assert!(result.excerpt.len() <= HEAD_BYTES);
        assert!(result.tail.len() <= TAIL_BYTES);
        assert!(result.truncated);
    }

    #[test]
    fn rejects_embedded_or_incomplete_verdicts() {
        for data in [
            "x VERDICT: PASS\n",
            "VERDICT: PASS x\n",
            "VERDICT: PAS",
            "VERDICT: PASS\u{fffd}\n",
        ] {
            let mut capture = ResultCapture::new(Some("VERDICT: PASS"));
            capture.push(data.as_bytes());
            assert!(!capture.finish().matched_required);
        }
    }

    #[test]
    fn malformed_utf8_remains_bounded_and_tail_keeps_end() {
        let mut capture = ResultCapture::new(None);
        capture.push(&vec![0xff; 20000]);
        capture.push(b"the end");
        let result = capture.finish();
        assert!(result.excerpt.len() <= HEAD_BYTES);
        assert!(result.tail.len() <= TAIL_BYTES);
        assert!(result.tail.ends_with("the end"));
        assert!(result.tail_truncated);
    }
}
