//! Streaming, bounded evidence from a result artifact.
use sha2::{Digest as _, Sha256};

const HEAD_BYTES: usize = 4096;
const TAIL_BYTES: usize = 16384;
const VERDICT_BYTES: usize = 1024;

/// A streaming accumulator. Display windows and verdict proof are bounded;
/// structured findings are transient parsing input from the sealed artifact.
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
    verdict: VerdictCapture,
    dispositions: DispositionCapture,
    findings: FindingsCapture,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedResult {
    /// The settled attempt's sealed handoff. Capture never parses it from text;
    /// the caller that holds the sealed snapshot sets it.
    pub handoff: Result<Option<crate::handoff::Handoff>, &'static str>,
    /// Exactly one well-formed verdict among the lines outside fenced blocks
    /// that start with `VERDICT:` (after emphasis or list markers; not
    /// blockquotes), wherever it sits. Independent of the required-line contract.
    pub verdict: Option<Box<str>>,
    /// Declarations were seen but are not exactly one well-formed verdict.
    pub verdict_invalid: bool,
    /// Why there is no verdict, naming what was seen; `None` when there is one.
    pub verdict_cause: Option<String>,
    /// Saturating count of verdict declarations across the complete stream.
    pub declarations: u8,
    pub digest: String,
    pub excerpt: String,
    pub tail: String,
    pub truncated: bool,
    pub source_truncated: bool,
    pub tail_truncated: bool,
    pub matched_required: bool,
    /// Bounded stream proof for explicit per-finding disposition lines.
    pub disposition_proof: DispositionProof,
    /// The last complete findings block. Transient parsing input only; graph
    /// checkpoints keep a summary and the sealed result artifact reference.
    pub findings: Option<String>,
}

impl ResultCapture {
    pub const HEAD_BYTES: usize = HEAD_BYTES;

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
            verdict: VerdictCapture::default(),
            dispositions: DispositionCapture::default(),
            findings: FindingsCapture::default(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.verdict.push(chunk);
        self.dispositions.push(chunk);
        self.findings.push(chunk);
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
        let (verdict, verdict_invalid, declarations, verdict_cause) = self.verdict.finish();
        CapturedResult {
            handoff: Ok(None),
            verdict,
            verdict_invalid,
            verdict_cause,
            declarations,
            digest,
            excerpt,
            tail,
            truncated,
            source_truncated: false,
            tail_truncated,
            matched_required: self.matched_required,
            disposition_proof: self.dispositions.finish(),
            findings: self.findings.finish(),
        }
    }
}

/// Preserve structured findings independently of the display windows. The
/// artifact is the durable copy; this transient buffer lives only while its
/// complete JSON is parsed, never in the graph's checkpoint envelope.
#[derive(Default)]
struct FindingsCapture {
    line: Vec<u8>,
    body: Option<Vec<u8>>,
    complete: Option<String>,
}

impl FindingsCapture {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.end_line();
            } else if self.body.is_some() || self.line.len() < 32 {
                self.line.push(byte);
            }
        }
    }

    fn end_line(&mut self) {
        let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
        if let Some(body) = &mut self.body {
            if line.starts_with(b"```") && line[3..].iter().all(u8::is_ascii_whitespace) {
                self.complete = self
                    .body
                    .take()
                    .and_then(|bytes| String::from_utf8(bytes).ok());
            } else {
                body.extend_from_slice(&self.line);
                body.push(b'\n');
            }
        } else if line == b"```findings" {
            self.body = Some(Vec::new());
            // A later incomplete block must not silently reuse an earlier one.
            self.complete = None;
        }
        self.line.clear();
    }

    fn finish(mut self) -> Option<String> {
        if !self.line.is_empty() {
            self.end_line();
        }
        self.complete
    }
}

/// Bounded evidence for the identities and dispositions in a complete result.
/// Only SHA-256 hashes are retained, never the finding text or reasons.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispositionProof {
    invalid: bool,
    by_reference: Option<bool>,
    entries: std::collections::BTreeMap<[u8; 32], [u8; 32]>,
}

impl DispositionProof {
    fn identity<'a>(&self, item: &'a (String, String)) -> &'a str {
        if self.by_reference == Some(true) {
            &item.1
        } else {
            &item.0
        }
    }

    pub fn matches(&self, identities: &[(String, String)], allowed: &[String]) -> bool {
        if self.invalid || self.entries.len() != identities.len() {
            return false;
        }
        let allowed: std::collections::BTreeSet<[u8; 32]> = allowed
            .iter()
            .map(|value| Sha256::digest(value.as_bytes()).into())
            .collect();
        let mut seen = std::collections::BTreeSet::new();
        identities.iter().all(|item| {
            let key: [u8; 32] = Sha256::digest(self.identity(item).as_bytes()).into();
            seen.insert(key)
                && self
                    .entries
                    .get(&key)
                    .is_some_and(|status| allowed.contains(status))
        })
    }

    /// A typed entry must agree with its corresponding proof, if it is an
    /// original finding. New findings are checked by the typed-output rules.
    pub fn agrees(&self, id: &str, disposition: &str, identities: &[(String, String)]) -> bool {
        let mut matches = identities.iter().filter(|item| item.0 == id);
        let Some(item) = matches.next() else {
            return true;
        };
        if matches.next().is_some() {
            return false;
        }
        let key: [u8; 32] = Sha256::digest(self.identity(item).as_bytes()).into();
        let status: [u8; 32] = Sha256::digest(disposition.as_bytes()).into();
        self.entries.get(&key) == Some(&status)
    }
}

const DISPOSITION_PREFIX: &[u8] = b"DISPOSITION ";
// Three 1024-byte fields can expand sixfold when encoded as JSON.
const DISPOSITION_LINE_BYTES: usize = 20_000;
// The graph admits at most 512 nodes and each task exports at most 32 findings.
const DISPOSITION_RECORDS: usize = 512 * 32;

#[derive(Default)]
struct DispositionCapture {
    line: Vec<u8>,
    overflow: bool,
    proof: DispositionProof,
}

impl DispositionCapture {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.end_line();
            } else if self.line.len() < DISPOSITION_LINE_BYTES {
                self.line.push(byte);
            } else {
                self.overflow = true;
            }
        }
    }

    fn end_line(&mut self) {
        let line = std::mem::take(&mut self.line);
        let overflow = std::mem::take(&mut self.overflow);
        let Some(json) = line.strip_prefix(DISPOSITION_PREFIX) else {
            return;
        };
        if overflow {
            self.proof.invalid = true;
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(json) else {
            self.proof.invalid = true;
            return;
        };
        let Some((disposition, reason)) = value
            .get("disposition")
            .and_then(serde_json::Value::as_str)
            .zip(value.get("reason").and_then(serde_json::Value::as_str))
        else {
            self.proof.invalid = true;
            return;
        };
        let (key, by_reference) = match (value.get("id"), value.get("member"), value.get("index")) {
            (Some(id), None, None) => match id.as_str().filter(|id| id.len() <= 1024) {
                Some(id) => (id.to_owned(), false),
                None => {
                    self.proof.invalid = true;
                    return;
                }
            },
            (None, Some(member), Some(index)) => match member
                .as_str()
                .filter(|member| !member.is_empty() && member.len() <= 64)
                .zip(index.as_u64())
            {
                Some((member, index)) => (format!("{member}:{index}"), true),
                None => {
                    self.proof.invalid = true;
                    return;
                }
            },
            _ => {
                self.proof.invalid = true;
                return;
            }
        };
        if self
            .proof
            .by_reference
            .is_some_and(|mode| mode != by_reference)
            || disposition.len() > 1024
            || reason.len() > 1024
            || reason.trim().is_empty()
        {
            self.proof.invalid = true;
            return;
        }
        self.proof.by_reference = Some(by_reference);
        let identity: [u8; 32] = Sha256::digest(key.as_bytes()).into();
        let status: [u8; 32] = Sha256::digest(disposition.as_bytes()).into();
        if self.proof.entries.len() >= DISPOSITION_RECORDS
            || self.proof.entries.insert(identity, status).is_some()
        {
            self.proof.invalid = true;
        }
    }

    fn finish(mut self) -> DispositionProof {
        if !self.line.is_empty() || self.overflow {
            self.end_line();
        }
        self.proof
    }
}

/// Retain one bounded line and count standalone verdict declarations across
/// every byte, even outside the excerpt windows. One rule: the result holds
/// exactly one `VERDICT:` line outside fenced blocks. Fenced blocks (findings,
/// handoff, quoted examples) and prose that merely mentions a verdict never
/// count, and position does not matter. Two declarations, or one malformed
/// declaration, are rejected with a cause that names what was seen.
#[derive(Default)]
struct VerdictCapture {
    line: Vec<u8>,
    overflow: bool,
    lines: usize,
    /// Open fence: its character, run length and line number.
    fence: Option<(u8, usize, usize)>,
    /// Declaration scan of the current line, over every byte so clipping
    /// cannot hide it: past the marker prefix, after digits of a possible
    /// ordered-list marker, not a declaration (blockquote or other text), the
    /// first bytes after the prefix, and where the declaration starts.
    past_prefix: bool,
    digits: bool,
    no_decl: bool,
    head: Vec<u8>,
    start: usize,
    declarations: u8,
    /// The first two declarations as written, for the cause.
    seen: Vec<String>,
    /// The latest declaration, if well formed.
    verdict: Option<Box<str>>,
}

const DECLARATION: &[u8] = b"VERDICT:";

impl VerdictCapture {
    fn push(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            if byte == b'\n' {
                self.end_line();
            } else {
                self.scan(byte);
                if self.line.len() < VERDICT_BYTES {
                    self.line.push(byte);
                } else {
                    self.overflow = true;
                }
            }
        }
    }

    fn scan(&mut self, byte: u8) {
        if self.no_decl {
            return;
        }
        if self.past_prefix {
            if self.head.len() < DECLARATION.len() {
                self.head.push(byte);
            }
            return;
        }
        match byte {
            b' ' | b'\t' | b'*' | b'_' | b'-' | b'+' | b'#' if !self.digits => {}
            b'0'..=b'9' => self.digits = true,
            b'.' | b')' if self.digits => self.digits = false,
            b'>' => self.no_decl = true,
            _ if self.digits => self.no_decl = true,
            _ => {
                self.past_prefix = true;
                self.start = self.line.len();
                self.head.push(byte);
            }
        }
    }

    fn end_line(&mut self) {
        self.lines += 1;
        let text = std::str::from_utf8(&self.line)
            .ok()
            .map(str::trim)
            .filter(|_| !self.overflow);
        let run = text.and_then(fence_run);
        match self.fence {
            Some((ch, len, _)) => {
                // A closing fence repeats the opener's character at least as
                // often and carries no other text.
                if let (Some(text), Some((c, n))) = (text, run)
                    && c == ch
                    && n >= len
                    && text.bytes().all(|byte| byte == ch)
                {
                    self.fence = None;
                }
            }
            None => {
                if let (Some(text), Some((ch, len))) = (text, run) {
                    // A backtick fence's info string cannot hold a backtick,
                    // which makes `inline` code on its own line prose.
                    if ch != b'`' || !text[len..].contains('`') {
                        self.fence = Some((ch, len, self.lines));
                    }
                } else if !self.no_decl && self.head == DECLARATION {
                    self.declare();
                }
            }
        }
        self.line.clear();
        self.overflow = false;
        self.past_prefix = false;
        self.digits = false;
        self.no_decl = false;
        self.head.clear();
    }

    fn declare(&mut self) {
        self.declarations = self.declarations.saturating_add(1);
        let written = if self.overflow {
            None
        } else {
            std::str::from_utf8(&self.line)
                .ok()
                .and_then(|line| line.get(self.start..))
                .map(str::trim)
        };
        let token = written.and_then(|text| {
            let token = text[DECLARATION.len()..]
                .trim_matches(|c: char| c.is_whitespace() || matches!(c, '*' | '_' | '`'));
            (!token.is_empty()
                && token.bytes().all(|byte| {
                    byte.is_ascii_uppercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'_' | b'-')
                }))
            .then_some(token)
        });
        self.verdict = token.map(|token| format!("VERDICT: {token}").into());
        if self.seen.len() < 2 {
            self.seen.push(match written {
                Some(text) => quoted(text),
                None => "a VERDICT: line that is over-long or not UTF-8".to_owned(),
            });
        }
    }

    fn finish(mut self) -> (Option<Box<str>>, bool, u8, Option<String>) {
        if !self.line.is_empty() {
            self.end_line();
        }
        let verdict = self.verdict.take().filter(|_| self.declarations == 1);
        let invalid = self.declarations > 0 && verdict.is_none();
        let cause = verdict.is_none().then(|| match (self.declarations, &self.fence) {
            (0, Some((_, _, line))) => format!(
                "no VERDICT: line outside fenced blocks; the fence opened at line {line} is never closed, so everything after it was read as fenced"
            ),
            (0, None) => "no VERDICT: line outside fenced blocks".to_owned(),
            (1, _) => format!(
                "the VERDICT: line {} is not VERDICT: PASS, FAIL or INCONCLUSIVE",
                self.seen[0]
            ),
            (count, _) => format!(
                "{count} VERDICT: lines outside fenced blocks, {} and {}{}; exactly one is allowed",
                self.seen[0],
                self.seen[1],
                if count > 2 { " and more" } else { "" }
            ),
        });
        (verdict, invalid, self.declarations, cause)
    }
}

/// The character and length of a fence opener or closer: three or more
/// backticks or tildes at the start of a trimmed line.
fn fence_run(text: &str) -> Option<(u8, usize)> {
    let ch = *text
        .as_bytes()
        .first()
        .filter(|&&b| matches!(b, b'`' | b'~'))?;
    let len = text.bytes().take_while(|&byte| byte == ch).count();
    (len >= 3).then_some((ch, len))
}

fn quoted(text: &str) -> String {
    let clipped: String = text.chars().take(80).collect();
    if clipped.len() < text.len() {
        format!("`{clipped}...`")
    } else {
        format!("`{clipped}`")
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

    fn run(text: &str) -> CapturedResult {
        let mut capture = ResultCapture::new(None);
        for chunk in text.as_bytes().chunks(5) {
            capture.push(chunk);
        }
        capture.finish()
    }

    const HANDOFF: &str = "```handoff\n{\"note\":\"VERDICT: FAIL\"}\n```\n";
    const FINDINGS: &str = "```findings\n{\"findings\":[]}\n```\n";

    /// The shape that settled as invalid in #3193: prose, a findings block,
    /// one verdict line, then a handoff block.
    #[test]
    fn prose_findings_one_verdict_handoff_is_one_verdict() {
        for line in [
            "VERDICT: PASS",
            "VERDICT: PASS   ",
            "**VERDICT: PASS**",
            "- VERDICT: PASS",
            "VERDICT:PASS",
        ] {
            let text = format!("Reviewed the diff.\n\n{FINDINGS}{line}\n\n{HANDOFF}");
            let captured = run(&text);
            assert_eq!(captured.verdict.as_deref(), Some("VERDICT: PASS"), "{line}");
            assert!(!captured.verdict_invalid && captured.verdict_cause.is_none());
            assert_eq!(captured.declarations, 1);
        }
    }

    #[test]
    fn two_verdict_lines_outside_fences_fail_naming_both() {
        let captured = run(&format!(
            "VERDICT: PASS\nmore\n{FINDINGS}VERDICT: FAIL\n{HANDOFF}"
        ));
        assert!(captured.verdict.is_none() && captured.verdict_invalid);
        assert_eq!(captured.declarations, 2);
        let cause = captured.verdict_cause.unwrap();
        assert!(
            cause.contains("`VERDICT: PASS`") && cause.contains("`VERDICT: FAIL`"),
            "{cause}"
        );
        let malformed = run("VERDICT: MAYBE soon\n");
        assert!(malformed.verdict.is_none() && malformed.verdict_invalid);
        assert!(malformed.verdict_cause.unwrap().contains("MAYBE soon"));
    }

    #[test]
    fn verdict_words_in_fences_and_prose_are_ignored() {
        for text in [
            format!("Verdict: looks fine.\nThe verdict: PASS is likely.\nVERDICT: PASS\n{HANDOFF}"),
            "```text\nVERDICT: FAIL\n```\nVERDICT: PASS\n".to_owned(),
            "~~~\nVERDICT: FAIL\n~~~\nVERDICT: PASS\n".to_owned(),
            "````\n```\nVERDICT: FAIL\n```\n````\nVERDICT: PASS\n".to_owned(),
            "VERDICT: PASS\n```\nVERDICT: FAIL\n".to_owned(),
            "> VERDICT: FAIL\nsee VERDICT: FAIL\nVERDICT: PASS\nmore findings\n".to_owned(),
        ] {
            let captured = run(&text);
            assert_eq!(
                captured.verdict.as_deref(),
                Some("VERDICT: PASS"),
                "{text:?}"
            );
            assert!(!captured.verdict_invalid, "{text:?}");
        }
        let swallowed = run("```findings\n{}\nVERDICT: PASS\n");
        assert!(swallowed.verdict.is_none() && !swallowed.verdict_invalid);
        assert!(swallowed.verdict_cause.unwrap().contains("never closed"));
        assert!(
            run("nothing here\n")
                .verdict_cause
                .unwrap()
                .contains("no VERDICT")
        );
    }

    #[test]
    fn overlong_declarations_still_count() {
        let long = " ".repeat(1100);
        for (text, valid) in [
            (format!("{long}VERDICT: FAIL\nVERDICT: PASS\n"), false),
            ("> VERDICT: FAIL\nVERDICT: PASS\n".to_owned(), true),
            ("1. VERDICT: FAIL\nVERDICT: PASS\n".to_owned(), false),
            ("2) VERDICT: FAIL\nVERDICT: PASS\n".to_owned(), false),
        ] {
            assert_eq!(run(&text).verdict.is_some(), valid, "{text:?}");
        }
    }

    #[test]
    fn verdict_evidence_is_unique_and_independent_of_chunk_boundaries() {
        for (text, expected, invalid) in [
            (
                "Findings first.\nVERDICT: PASS\n\n".to_owned(),
                Some("VERDICT: PASS"),
                false,
            ),
            ("VERDICT: FAIL".to_owned(), Some("VERDICT: FAIL"), false),
            ("No verdict".to_owned(), None, false),
            (
                "VERDICT: PASS\nmore text".to_owned(),
                Some("VERDICT: PASS"),
                false,
            ),
            ("VERDICT: PASS\nVERDICT: PASS".to_owned(), None, true),
            (
                "Quoted: VERDICT: PASS\nVERDICT: PASS".to_owned(),
                Some("VERDICT: PASS"),
                false,
            ),
            (
                format!(
                    "{}\nVERDICT: FAIL\n{}\nVERDICT: PASS",
                    "x".repeat(5000),
                    "y".repeat(20000)
                ),
                None,
                true,
            ),
        ] {
            for size in [1, 7, 4096] {
                let mut capture = ResultCapture::new(Some("VERDICT: PASS"));
                for chunk in text.as_bytes().chunks(size) {
                    capture.push(chunk);
                }
                let captured = capture.finish();
                assert_eq!(captured.verdict.as_deref(), expected);
                assert_eq!(captured.verdict_invalid, invalid);
            }
        }
    }

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
    #[test]
    fn disposition_proof_covers_complete_stream_and_rejects_gaps() {
        let ids: Vec<String> = (0..40).map(|i| format!("lens:F{i}")).collect();
        let identities: Vec<_> = ids
            .iter()
            .enumerate()
            .map(|(i, id)| (id.clone(), format!("lens:{i}")))
            .collect();
        let mut text = "x".repeat(25_000) + "\n";
        for id in &ids {
            text.push_str(&format!(
                "DISPOSITION {}\n",
                serde_json::json!({"id":id,"disposition":"rejected","reason":"evidence"})
            ));
        }
        let mut capture = ResultCapture::new(None);
        for chunk in text.as_bytes().chunks(7) {
            capture.push(chunk);
        }
        let proof = capture.finish().disposition_proof;
        assert!(proof.matches(&identities, &["rejected".into()]));
        assert!(!proof.matches(&identities[..39], &["rejected".into()]));
        let mut substituted = identities.clone();
        substituted[0].0 = "other:F0".into();
        assert!(!proof.matches(&substituted, &["rejected".into()]));
        let mut duplicated = identities.clone();
        duplicated[0] = duplicated[1].clone();
        assert!(!proof.matches(&duplicated, &["rejected".into()]));
        assert!(!proof.matches(&identities, &["accepted".into()]));
        assert!(proof.agrees("lens:F0", "rejected", &identities));
        assert!(!proof.agrees("lens:F0", "accepted", &identities));
        assert!(proof.agrees("new:F0", "accepted", &identities));
        let mut references = ResultCapture::new(None);
        for i in 0..40 {
            references.push(format!("DISPOSITION {}\n", serde_json::json!({"member":"lens","index":i,"disposition":"rejected","reason":"evidence"})).as_bytes());
        }
        let compact = references.finish().disposition_proof;
        assert!(compact.matches(&identities, &["rejected".into()]));
        assert!(compact.agrees("lens:F0", "rejected", &identities));
        assert!(!compact.agrees("lens:F0", "accepted", &identities));
        let mut bad = ResultCapture::new(None);
        bad.push(b"DISPOSITION {\"id\":\"x\",\"disposition\":\"rejected\",\"reason\":\" \"}\n");
        assert!(
            !bad.finish()
                .disposition_proof
                .matches(&[("x".into(), "lens:0".into())], &["rejected".into()])
        );
    }
}
