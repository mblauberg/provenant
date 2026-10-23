//! Streaming redaction for bytes retained by supervisor capture.
//!
//! Exact literal prefixes and incomplete token markers stay buffered across
//! chunks. The capture filter's EOF hook releases any unmatched tail.
use std::collections::{BTreeMap, VecDeque};
use thiserror::Error;

use provenant_supervisor::exec::{CaptureFilter, FRAME_LIMIT, FilterError};

const REDACTED: &[u8] = b"[REDACTED]";
const MAX_SECRETS: usize = 128;
const MAX_SECRET_BYTES: usize = 64 * 1024;
const MAX_SECRET_LENGTH: usize = 16 * 1024;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RedactorError {
    #[error("secret value cannot be empty")]
    EmptySecret,
    #[error("secret set has more than {MAX_SECRETS} values")]
    TooManySecrets,
    #[error("secret set exceeds its byte bound")]
    SecretSetTooLarge,
    #[error("a secret value exceeds its byte bound")]
    SecretTooLong,
}

#[derive(Default)]
struct Node {
    edges: BTreeMap<u8, usize>,
    failure: usize,
    output_length: usize,
}

struct LiteralAutomaton {
    nodes: Vec<Node>,
    state: usize,
    pending: VecDeque<(u8, bool)>,
    max_secret_length: usize,
    redaction_run: bool,
    matched: bool,
}

impl LiteralAutomaton {
    fn new(secrets: &[Vec<u8>]) -> Self {
        let mut nodes = vec![Node::default()];
        for secret in secrets {
            let mut state = 0;
            for byte in secret {
                let next = if let Some(next) = nodes[state].edges.get(byte) {
                    *next
                } else {
                    let next = nodes.len();
                    nodes.push(Node::default());
                    nodes[state].edges.insert(*byte, next);
                    next
                };
                state = next;
            }
            nodes[state].output_length = secret.len();
        }

        let mut queue = VecDeque::new();
        for child in nodes[0].edges.values().copied() {
            queue.push_back(child);
        }
        while let Some(state) = queue.pop_front() {
            let edges: Vec<(u8, usize)> = nodes[state]
                .edges
                .iter()
                .map(|(byte, child)| (*byte, *child))
                .collect();
            for (byte, child) in edges {
                let mut fallback = nodes[state].failure;
                while fallback != 0 && !nodes[fallback].edges.contains_key(&byte) {
                    fallback = nodes[fallback].failure;
                }
                let failure = nodes[fallback].edges.get(&byte).copied().unwrap_or(0);
                nodes[child].failure = failure;
                nodes[child].output_length =
                    nodes[child].output_length.max(nodes[failure].output_length);
                queue.push_back(child);
            }
        }

        Self {
            nodes,
            state: 0,
            pending: VecDeque::new(),
            max_secret_length: secrets.iter().map(Vec::len).max().unwrap_or(0),
            redaction_run: false,
            matched: false,
        }
    }

    fn filter(&mut self, bytes: &[u8], output: &mut Vec<u8>) {
        for byte in bytes {
            while self.state != 0 && !self.nodes[self.state].edges.contains_key(byte) {
                self.state = self.nodes[self.state].failure;
            }
            self.state = self.nodes[self.state].edges.get(byte).copied().unwrap_or(0);
            self.pending.push_back((*byte, false));

            let matched_length = self.nodes[self.state].output_length;
            if matched_length > 0 {
                for (_, masked) in self.pending.iter_mut().rev().take(matched_length) {
                    *masked = true;
                }
                self.matched = true;
            }
            let keep = self.max_secret_length.saturating_sub(1);
            while self.pending.len() > keep {
                self.emit_front(output);
            }
        }
    }

    fn finish(&mut self, output: &mut Vec<u8>) {
        while !self.pending.is_empty() {
            self.emit_front(output);
        }
        self.state = 0;
        self.redaction_run = false;
    }

    fn emit_front(&mut self, output: &mut Vec<u8>) {
        let Some((byte, masked)) = self.pending.pop_front() else {
            return;
        };
        if masked {
            if !self.redaction_run {
                output.extend_from_slice(REDACTED);
            }
            self.redaction_run = true;
        } else {
            output.push(byte);
            self.redaction_run = false;
        }
    }
}

#[derive(Clone, Copy)]
enum TokenKind {
    Bearer,
    Generic,
    AwsAccessKey,
}

const TOKEN_MARKERS: &[(&[u8], TokenKind)] = &[
    (b"Bearer ", TokenKind::Bearer),
    (b"ghp_", TokenKind::Generic),
    (b"gho_", TokenKind::Generic),
    (b"ghu_", TokenKind::Generic),
    (b"ghs_", TokenKind::Generic),
    (b"ghr_", TokenKind::Generic),
    (b"github_pat_", TokenKind::Generic),
    (b"sk-", TokenKind::Generic),
    (b"AKIA", TokenKind::AwsAccessKey),
];
const MAX_MARKER_LENGTH: usize = 11;

struct ActiveToken {
    marker: &'static [u8],
    kind: TokenKind,
    body: Vec<u8>,
    emitted: bool,
}

struct TokenScanner {
    active: Option<ActiveToken>,
    tail: Vec<u8>,
    last_byte: Option<u8>,
    url_authority: Option<Vec<u8>>,
    matched: bool,
}

impl Default for TokenScanner {
    fn default() -> Self {
        Self {
            active: None,
            tail: Vec::with_capacity(MAX_MARKER_LENGTH - 1),
            last_byte: None,
            url_authority: None,
            matched: false,
        }
    }
}

impl TokenScanner {
    fn filter(&mut self, bytes: &[u8], output: &mut Vec<u8>) -> Result<(), FilterError> {
        let mut combined = std::mem::take(&mut self.tail);
        combined.extend_from_slice(bytes);
        self.process(&combined, false, output)
    }

    fn finish(&mut self, output: &mut Vec<u8>) -> Result<(), FilterError> {
        let tail = std::mem::take(&mut self.tail);
        self.process(&tail, true, output)?;
        if let Some(authority) = self.url_authority.take() {
            self.finish_authority(authority, output);
        }
        self.last_byte = None;
        self.active = None;
        Ok(())
    }

    fn process(
        &mut self,
        input: &[u8],
        eof: bool,
        output: &mut Vec<u8>,
    ) -> Result<(), FilterError> {
        let mut index = 0;
        while index < input.len() {
            if self.url_authority.is_some() {
                let mut authority = self.url_authority.take().unwrap_or_default();
                while index < input.len() && !url_delimiter(input[index]) {
                    authority.push(input[index]);
                    if authority.len() > FRAME_LIMIT {
                        return Err(FilterError);
                    }
                    self.last_byte = Some(input[index]);
                    index += 1;
                }
                if index < input.len() {
                    self.finish_authority(authority, output);
                } else {
                    self.url_authority = Some(authority);
                    return Ok(());
                }
                continue;
            }

            if let Some(kind) = self.active.as_ref().map(|active| active.kind) {
                let byte = input[index];
                if token_byte(kind, byte) {
                    let mut became_secret = false;
                    if let Some(active) = self.active.as_mut()
                        && !active.emitted
                    {
                        active.body.push(byte);
                        if active.body.len() >= minimum_token_length(kind) {
                            active.body.clear();
                            active.emitted = true;
                            became_secret = true;
                        }
                    }
                    if became_secret {
                        if matches!(kind, TokenKind::Bearer) {
                            output.extend_from_slice(b"Bearer ");
                        }
                        output.extend_from_slice(REDACTED);
                        self.matched = true;
                    }
                    self.last_byte = Some(byte);
                    index += 1;
                    continue;
                }
                self.finish_active(output);
                continue;
            }

            if input[index..].starts_with(b"://") {
                output.extend_from_slice(b"://");
                self.last_byte = Some(b'/');
                self.url_authority = Some(Vec::new());
                index += 3;
                continue;
            }
            if !eof && b"://".starts_with(&input[index..]) {
                self.tail.extend_from_slice(&input[index..]);
                return Ok(());
            }

            let preceding = if index == 0 {
                self.last_byte
            } else {
                Some(input[index - 1])
            };
            let at_word_boundary = preceding.is_none_or(|byte| !word_byte(byte));
            if at_word_boundary {
                if let Some((marker, kind)) = token_marker_at(&input[index..]) {
                    self.last_byte = marker.last().copied();
                    self.active = Some(ActiveToken {
                        marker,
                        kind,
                        body: Vec::with_capacity(minimum_token_length(kind)),
                        emitted: false,
                    });
                    index += marker.len();
                    continue;
                }
                if !eof && token_marker_prefix(&input[index..]) {
                    self.tail.extend_from_slice(&input[index..]);
                    return Ok(());
                }
            }

            let byte = input[index];
            output.push(byte);
            self.last_byte = Some(byte);
            index += 1;
        }
        if eof {
            self.finish_active(output);
        }
        Ok(())
    }

    fn finish_active(&mut self, output: &mut Vec<u8>) {
        if let Some(active) = self.active.take()
            && !active.emitted
        {
            output.extend_from_slice(active.marker);
            output.extend_from_slice(&active.body);
        }
    }

    fn finish_authority(&mut self, authority: Vec<u8>, output: &mut Vec<u8>) {
        if let Some(userinfo_end) = authority.iter().rposition(|byte| *byte == b'@') {
            let userinfo = &authority[..userinfo_end];
            let has_password = userinfo.contains(&b':');
            if has_password {
                output.extend_from_slice(REDACTED);
                output.extend_from_slice(&authority[userinfo_end..]);
                self.matched = true;
            } else {
                output.extend_from_slice(&authority);
            }
        } else {
            output.extend_from_slice(&authority);
        }
        self.last_byte = authority.last().copied().or(self.last_byte);
    }
}

fn minimum_token_length(kind: TokenKind) -> usize {
    match kind {
        TokenKind::Bearer | TokenKind::Generic => 20,
        TokenKind::AwsAccessKey => 16,
    }
}

fn token_byte(kind: TokenKind, byte: u8) -> bool {
    match kind {
        TokenKind::Bearer | TokenKind::Generic => {
            byte.is_ascii_alphanumeric() || b"._~+/-=_".contains(&byte)
        }
        TokenKind::AwsAccessKey => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
    }
}

fn token_marker_at(bytes: &[u8]) -> Option<(&'static [u8], TokenKind)> {
    TOKEN_MARKERS
        .iter()
        .find_map(|(marker, kind)| bytes.starts_with(marker).then_some((*marker, *kind)))
}

fn token_marker_prefix(bytes: &[u8]) -> bool {
    TOKEN_MARKERS
        .iter()
        .any(|(marker, _)| bytes.len() < marker.len() && marker.starts_with(bytes))
}

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn url_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || b"/?#\"'<>`\\".contains(&byte)
}

/// Stateful filter used directly by supervisor capture. Values are never
/// serialized or printed; malformed bounds make construction fail closed.
pub struct Redactor {
    literals: LiteralAutomaton,
    tokens: TokenScanner,
    failed: bool,
}

impl Redactor {
    pub fn new(secrets: impl IntoIterator<Item = Vec<u8>>) -> Result<Self, RedactorError> {
        let mut secrets: Vec<Vec<u8>> = secrets.into_iter().collect();
        if secrets.len() > MAX_SECRETS {
            return Err(RedactorError::TooManySecrets);
        }
        if secrets.iter().any(Vec::is_empty) {
            return Err(RedactorError::EmptySecret);
        }
        if secrets
            .iter()
            .any(|secret| secret.len() > MAX_SECRET_LENGTH)
        {
            return Err(RedactorError::SecretTooLong);
        }
        let total = secrets
            .iter()
            .try_fold(0usize, |sum, secret| sum.checked_add(secret.len()));
        if total.is_none_or(|total| total > MAX_SECRET_BYTES) {
            return Err(RedactorError::SecretSetTooLarge);
        }
        secrets.sort();
        secrets.dedup();

        Ok(Self {
            literals: LiteralAutomaton::new(&secrets),
            tokens: TokenScanner::default(),
            failed: false,
        })
    }

    pub fn has_redaction(&self) -> bool {
        self.literals.matched || self.tokens.matched
    }

    fn filter_bounded(&mut self, bytes: &[u8]) -> Result<Vec<u8>, FilterError> {
        if self.failed || bytes.len() > FRAME_LIMIT {
            self.failed = true;
            return Err(FilterError);
        }
        let mut literals = Vec::with_capacity(bytes.len());
        self.literals.filter(bytes, &mut literals);
        if literals.len() > FRAME_LIMIT {
            self.failed = true;
            return Err(FilterError);
        }
        let mut filtered = Vec::with_capacity(literals.len());
        self.tokens.filter(&literals, &mut filtered).map_err(|_| {
            self.failed = true;
            FilterError
        })?;
        if filtered.len() > FRAME_LIMIT {
            self.failed = true;
            return Err(FilterError);
        }
        Ok(filtered)
    }

    fn finish_bounded(&mut self) -> Result<Vec<u8>, FilterError> {
        if self.failed {
            return Err(FilterError);
        }
        let mut literals = Vec::new();
        self.literals.finish(&mut literals);
        let mut filtered = Vec::new();
        self.tokens.filter(&literals, &mut filtered).map_err(|_| {
            self.failed = true;
            FilterError
        })?;
        self.tokens.finish(&mut filtered).map_err(|_| {
            self.failed = true;
            FilterError
        })?;
        if filtered.len() > FRAME_LIMIT {
            self.failed = true;
            return Err(FilterError);
        }
        Ok(filtered)
    }
}

impl CaptureFilter for Redactor {
    fn filter(&mut self, bytes: &[u8]) -> Result<Vec<u8>, FilterError> {
        self.filter_bounded(bytes)
    }

    fn finish(&mut self) -> Result<Vec<u8>, FilterError> {
        self.finish_bounded()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(redactor: &mut Redactor, chunks: &[&[u8]]) -> Vec<u8> {
        let mut output = Vec::new();
        for chunk in chunks {
            output.extend(redactor.filter(chunk).expect("bounded input"));
        }
        output.extend(redactor.finish().expect("bounded finish"));
        output
    }

    #[test]
    fn known_secret_split_across_chunks_is_never_emitted() {
        let mut redactor = Redactor::new([b"token-123".to_vec()]).expect("valid secret");
        let output = joined(&mut redactor, &[b"prefix tok", b"en-123 suffix"]);
        assert!(
            !output
                .windows(b"token-123".len())
                .any(|window| window == b"token-123")
        );
        assert!(output.starts_with(b"prefix "));
        assert!(output.ends_with(b" suffix"));
        assert!(
            output
                .windows(REDACTED.len())
                .any(|window| window == REDACTED)
        );
    }

    #[test]
    fn nested_literal_matches_mask_the_longer_secret_across_chunks() {
        let whole: [&[u8]; 1] = [b"abcde"];
        let split: [&[u8]; 3] = [b"ab", b"cd", b"e"];
        for chunks in [&whole[..], &split[..]] {
            let mut redactor =
                Redactor::new([b"abcde".to_vec(), b"cd".to_vec()]).expect("valid secrets");
            assert_eq!(joined(&mut redactor, chunks), REDACTED);
        }
    }

    #[test]
    fn incomplete_literal_prefix_is_preserved_after_mismatch_across_chunks() {
        let mut redactor = Redactor::new([b"secret-value".to_vec()]).expect("valid secret");
        let output = joined(&mut redactor, &[b"ordinary secr", b"etx and done"]);
        assert_eq!(output, b"ordinary secretx and done");
    }

    #[test]
    fn eof_flushes_unmatched_literal_tail() {
        let mut redactor = Redactor::new([b"secret-value".to_vec()]).expect("valid secret");
        let mut output = redactor.filter(b"ordinary secret-").expect("bounded input");
        output.extend(redactor.finish().expect("bounded finish"));
        assert_eq!(output, b"ordinary secret-");
    }

    #[test]
    fn ordinary_urls_and_embedded_token_markers_are_preserved() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(
            &mut redactor,
            &[
                b"https://example.invalid/path xsk-embedded-value and ",
                b"https://user:pass@example.invalid/path",
            ],
        );
        assert_eq!(
            output,
            b"https://example.invalid/path xsk-embedded-value and https://[REDACTED]@example.invalid/path"
        );
    }

    #[test]
    fn token_markers_split_across_chunks_redact_only_at_word_boundaries() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(
            &mut redactor,
            &[b"xsk-embedded sk", b"-abcdefghijklmnopqrst"],
        );
        assert_eq!(output, b"xsk-embedded [REDACTED]");
    }

    #[test]
    fn bearer_and_credential_url_values_split_across_chunks_are_hidden() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(
            &mut redactor,
            &[
                b"Bearer abc.def_123",
                b"456789XYZ\nfetch https:",
                b"//alice:password@example.invalid/path",
            ],
        );
        assert!(!output.windows(11).any(|window| window == b"abc.def_123"));
        assert!(
            !output
                .windows(b"alice:password@example.invalid".len())
                .any(|window| window == b"alice:password@example.invalid")
        );
        assert!(output.ends_with(b"/path"));
    }

    #[test]
    fn short_or_case_mismatched_tokens_and_passwordless_userinfo_are_preserved() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let expected = b"bearer values | Bearer values | Bearer abcdefghijklmnopqrs | sk- | sk-abcdefghijklmnopqrs | SK-abcdefghijklmnopqrstuvwxyz | GHP_abcdefghijklmnopqrst | GHO_abcdefghijklmnopqrst | GHU_abcdefghijklmnopqrst | GHS_abcdefghijklmnopqrst | GHR_abcdefghijklmnopqrst | GitHub_pat_abcdefghijklmnopqrst | AKIA123456789012345 | Akia1234567890123456 | ssh://git@host | https://user@example.invalid/path";
        let output = joined(
            &mut redactor,
            &[
                b"bearer values | Bearer values | Bearer abcdefghijklmnopqrs | sk",
                b"- | sk-abcdefghijklmnopqrs | SK-abcdefghijklmnopqrstuvwxyz | GHP_abcdefghijklmnopqrst | GHO_abcdefghijklmnopqrst | GHU_abcdefghijklmnopqrst | GHS_abcdefghijklmnopqrst | GHR_abcdefghijklmnopqrst | GitHub_pat_abcdefghijklmnopqrst | AKIA123456789012345 | Akia1234567890123456 | ssh://git@host | https://user@example.invalid/path",
            ],
        );
        assert_eq!(output, expected);
        assert!(!redactor.has_redaction());
    }

    #[test]
    fn empty_password_component_in_url_userinfo_is_redacted() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(&mut redactor, &[b"https://user:@example.invalid/path"]);
        assert_eq!(output, b"https://[REDACTED]@example.invalid/path");
        assert!(redactor.has_redaction());
    }

    #[test]
    fn token_markers_redact_at_minimum_body_lengths_across_chunks() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(
            &mut redactor,
            &[
                b"Bearer abcdefghij",
                b"klmnopqrst\nsk-abcdefghijkl",
                b"mnopqrst AKIA01234567",
                b"89ABCDEF!",
            ],
        );
        assert_eq!(output, b"Bearer [REDACTED]\n[REDACTED] [REDACTED]!");
        assert!(redactor.has_redaction());
    }

    #[test]
    fn an_expansion_over_the_capture_frame_bound_fails_closed() {
        let mut redactor = Redactor::new([b"x".to_vec()]).expect("valid secret");
        let input = b"x ".repeat(FRAME_LIMIT / 4 + 1);
        assert!(redactor.filter(&input).is_err());
        assert!(redactor.filter(b"future raw secret").is_err());
    }

    #[test]
    fn invalid_secret_sets_are_rejected_without_rendering_values() {
        assert_eq!(
            Redactor::new([Vec::new()]).err(),
            Some(RedactorError::EmptySecret)
        );
        assert_eq!(
            Redactor::new([vec![b'x'; MAX_SECRET_LENGTH + 1]]).err(),
            Some(RedactorError::SecretTooLong)
        );
    }
}
