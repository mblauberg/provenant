//! Streaming, bounded evidence from a result artifact.
use sha2::{Digest as _, Sha256};

const HEAD_BYTES: usize = 4096;
const TAIL_BYTES: usize = 16384;
const VERDICT_BYTES: usize = 1024;

/// A streaming accumulator. Storage is bounded by the windows, handoff limits
/// and already validated required verdict, irrespective of artifact length.
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
    handoff: crate::handoff::HandoffCapture,
    dispositions: DispositionCapture,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedResult {
    /// Final declaration parsed with fence context from the complete stream.
    pub handoff: Result<Option<crate::handoff::Handoff>, &'static str>,
    /// Exactly one well-formed verdict, on the final non-empty line of the
    /// complete stream, ignoring a closed trailing `handoff` fence. Only lines
    /// that start with `VERDICT:` (after emphasis or list markers; not
    /// blockquotes) count as declarations. Independent of the legacy required-line contract.
    pub verdict: Option<Box<str>>,
    pub verdict_invalid: bool,
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
}

impl ResultCapture {
    pub const HEAD_BYTES: usize = HEAD_BYTES;
    /// Maximum bytes retained in the trailing result window.
    pub const TAIL_BYTES: usize = TAIL_BYTES;

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
            handoff: crate::handoff::HandoffCapture::default(),
            dispositions: DispositionCapture::default(),
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.verdict.push(chunk);
        self.handoff.push(chunk);
        self.dispositions.push(chunk);
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
        let (verdict, verdict_invalid, declarations) = self.verdict.finish();
        CapturedResult {
            handoff: self.handoff.finish(),
            verdict,
            verdict_invalid,
            declarations,
            digest,
            excerpt,
            tail,
            truncated,
            source_truncated: false,
            tail_truncated,
            matched_required: self.matched_required,
            disposition_proof: self.dispositions.finish(),
        }
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
/// every byte, even outside the excerpt windows. Repeated or malformed
/// declarations cannot be hidden by a later valid verdict. A closed trailing
/// `handoff` fence after the verdict is ignored, since every lane must end
/// with one; any other later text clears the verdict.
#[derive(Default)]
struct VerdictCapture {
    line: Vec<u8>,
    overflow: bool,
    in_handoff: bool,
    /// Declaration scan of the current line, over every byte so clipping
    /// cannot hide it: past the marker prefix, after digits of a possible
    /// ordered-list marker, not a declaration (blockquote or other text), and
    /// the first bytes after the prefix.
    past_prefix: bool,
    digits: bool,
    no_decl: bool,
    head: Vec<u8>,
    markers: u8,
    final_verdict: Option<Box<str>>,
}

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
            if self.head.len() < 8 {
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
                self.head.push(byte);
            }
        }
    }

    fn end_line(&mut self) {
        let text = std::str::from_utf8(&self.line).ok().map(str::trim);
        if self.in_handoff {
            self.in_handoff = self.overflow || text != Some("```");
        } else {
            if !self.no_decl && self.head.eq_ignore_ascii_case(b"verdict:") {
                self.markers = self.markers.saturating_add(1);
            }
            if self.final_verdict.is_some() && !self.overflow && text == Some("```handoff") {
                self.in_handoff = true;
            } else if self.overflow || text != Some("") {
                self.final_verdict = text
                    .filter(|line| {
                        !self.overflow
                            && self.markers == 1
                            && line.strip_prefix("VERDICT: ").is_some_and(|token| {
                                !token.is_empty()
                                    && token.bytes().all(|byte| {
                                        byte.is_ascii_uppercase()
                                            || byte.is_ascii_digit()
                                            || matches!(byte, b'_' | b'-')
                                    })
                            })
                    })
                    .map(Box::<str>::from);
            }
        }
        self.line.clear();
        self.overflow = false;
        self.past_prefix = false;
        self.digits = false;
        self.no_decl = false;
        self.head.clear();
    }

    fn finish(mut self) -> (Option<Box<str>>, bool, u8) {
        self.end_line();
        // An unterminated handoff fence is not a trailing handoff block.
        let verdict = self
            .final_verdict
            .filter(|_| self.markers == 1 && !self.in_handoff);
        let invalid = self.markers > 0 && verdict.is_none();
        (verdict, invalid, self.markers)
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
    fn verdict_survives_trailing_handoff_but_not_prose() {
        let block = "```handoff\n{\"note\":\"verdict: x\"}\n```\n";
        let run = |text: &str| {
            let mut capture = ResultCapture::new(None);
            for chunk in text.as_bytes().chunks(5) {
                capture.push(chunk);
            }
            capture.finish()
        };
        let ok = run(&format!("review\nVERDICT: PASS\n\n{block}"));
        assert_eq!(ok.verdict.as_deref(), Some("VERDICT: PASS"));
        assert!(!ok.verdict_invalid);
        let prose = run(&format!("VERDICT: PASS\nthanks\n{block}"));
        assert!(prose.verdict.is_none());
        let prose_after = run(&format!("VERDICT: PASS\n{block}done\n"));
        assert!(prose_after.verdict.is_none());
        let twice = run(&format!("VERDICT: PASS\nVERDICT: FAIL\n{block}"));
        assert!(twice.verdict.is_none() && twice.verdict_invalid);
        let long = " ".repeat(1100);
        for (text, valid) in [
            (format!("{long}VERDICT: FAIL\nVERDICT: PASS\n"), false),
            ("> VERDICT: FAIL\nVERDICT: PASS\n".to_owned(), true),
            ("1. VERDICT: FAIL\nVERDICT: PASS\n".to_owned(), false),
            ("2) VERDICT: FAIL\nVERDICT: PASS\n".to_owned(), false),
            (
                format!("VERDICT: PASS\n```handoff\n{{}}\n```{long}junk\n"),
                false,
            ),
        ] {
            assert_eq!(run(&text).verdict.is_some(), valid, "{text:?}");
        }
        let mid = run("I will say the verdict: soon.\nVERDICT: PASS\n");
        assert_eq!(mid.verdict.as_deref(), Some("VERDICT: PASS"));
    }

    #[test]
    fn handoff_fence_context_survives_chunks_and_clipped_windows() {
        let handoff = serde_json::json!({"schema_version":2,"status":"stopped_early","note":"Continue","remaining":[],"decisions":[],"verify":[]});
        let block = format!("```handoff\n{handoff}\n```");
        for fence in ["````", "~~~"] {
            let example = format!("{fence}markdown\n{}\n{block}\n{fence}", "x".repeat(28_000));
            for (text, expected) in [
                (example.clone(), false),
                (format!("{example}\n{block}"), true),
            ] {
                for size in [1, 7, 8192] {
                    let mut capture = ResultCapture::new(None);
                    for chunk in text.as_bytes().chunks(size) {
                        capture.push(chunk);
                    }
                    let result = capture.finish();
                    assert!(result.tail_truncated);
                    let parsed = result.handoff.unwrap();
                    assert_eq!(parsed.is_some(), expected);
                    if let Some(parsed) = parsed {
                        assert_eq!(parsed.note, "Continue");
                    }
                }
            }
        }
        // A previous declaration outside the tail still makes the final one ambiguous.
        let mut capture = ResultCapture::new(None);
        capture.push(format!("{block}\n{}\n{block}", "x".repeat(28_000)).as_bytes());
        assert!(capture.finish().handoff.is_err());
    }

    #[test]
    fn verdict_evidence_is_unique_final_and_independent_of_chunk_boundaries() {
        for (text, expected, invalid) in [
            (
                "Findings first.\nVERDICT: PASS\n\n".to_owned(),
                Some("VERDICT: PASS"),
                false,
            ),
            ("VERDICT: FAIL".to_owned(), Some("VERDICT: FAIL"), false),
            ("No verdict".to_owned(), None, false),
            ("VERDICT: PASS\nmore text".to_owned(), None, true),
            ("verdict: PASS\nVERDICT: PASS".to_owned(), None, true),
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
        assert_eq!(result.excerpt.len(), ResultCapture::HEAD_BYTES);
        assert_eq!(result.tail.len(), ResultCapture::TAIL_BYTES);
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
