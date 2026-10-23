//! Streaming redaction for bytes retained by supervisor capture.
//!
//! The filter has no EOF callback, so it masks a possible secret prefix as
//! soon as it sees one and carries only the small matcher state between calls.
//! A false-positive prefix is redacted too; no buffered tail is lost and no
//! later chunk can expose bytes that completed a secret from an earlier chunk.
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
    terminal: bool,
}

struct LiteralAutomaton {
    nodes: Vec<Node>,
    state: usize,
    redaction_run: bool,
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
            nodes[state].terminal = true;
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
                nodes[child].terminal |= nodes[failure].terminal;
                queue.push_back(child);
            }
        }

        Self {
            nodes,
            state: 0,
            redaction_run: false,
        }
    }

    fn filter(&mut self, bytes: &[u8], output: &mut Vec<u8>) {
        for byte in bytes {
            while self.state != 0 && !self.nodes[self.state].edges.contains_key(byte) {
                self.state = self.nodes[self.state].failure;
            }
            self.state = self.nodes[self.state].edges.get(byte).copied().unwrap_or(0);

            // Every non-root trie state is a suffix that could complete a
            // configured secret. Mask it immediately to make chunk boundaries
            // safe even though CaptureFilter has no finish callback.
            let possibly_secret = self.state != 0 || self.nodes[self.state].terminal;
            if possibly_secret {
                if !self.redaction_run {
                    output.extend_from_slice(REDACTED);
                }
                self.redaction_run = true;
            } else {
                output.push(*byte);
                self.redaction_run = false;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum TokenKind {
    Bearer,
    Generic,
    AwsAccessKey,
    UrlAuthority,
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
    (b"://", TokenKind::UrlAuthority),
];
const MAX_MARKER_LENGTH: usize = 11;

struct TokenScanner {
    active: Option<(TokenKind, bool)>,
    tail: Vec<u8>,
}

impl Default for TokenScanner {
    fn default() -> Self {
        Self {
            active: None,
            tail: Vec::with_capacity(MAX_MARKER_LENGTH - 1),
        }
    }
}

impl TokenScanner {
    fn filter(&mut self, bytes: &[u8], output: &mut Vec<u8>) {
        let tail_len = self.tail.len();
        let mut combined = Vec::with_capacity(tail_len + bytes.len());
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(bytes);

        for index in 0..=bytes.len() {
            let boundary = tail_len + index;
            if boundary > tail_len
                && self.active.is_none()
                && let Some(kind) = marker_ending_at(&combined, boundary)
            {
                self.active = Some((kind, false));
            }
            if index == bytes.len() {
                break;
            }

            let byte = bytes[index];
            if let Some((kind, emitted)) = self.active {
                if token_byte(kind, byte) {
                    if !emitted {
                        output.extend_from_slice(REDACTED);
                    }
                    self.active = Some((kind, true));
                    continue;
                }
                self.active = None;
            }
            output.push(byte);
        }

        let keep = (MAX_MARKER_LENGTH - 1).min(combined.len());
        self.tail.clear();
        self.tail
            .extend_from_slice(&combined[combined.len().saturating_sub(keep)..]);
    }
}

fn marker_ending_at(bytes: &[u8], end: usize) -> Option<TokenKind> {
    TOKEN_MARKERS.iter().find_map(|(marker, kind)| {
        let start = end.checked_sub(marker.len())?;
        let candidate = bytes.get(start..end)?;
        candidate
            .iter()
            .zip(marker.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
            .then_some(*kind)
    })
}

fn token_byte(kind: TokenKind, byte: u8) -> bool {
    match kind {
        TokenKind::Bearer | TokenKind::Generic => {
            byte.is_ascii_alphanumeric() || b"._~+/-=_".contains(&byte)
        }
        TokenKind::AwsAccessKey => byte.is_ascii_uppercase() || byte.is_ascii_digit(),
        TokenKind::UrlAuthority => !byte.is_ascii_whitespace() && !b"/?#\"'<>`".contains(&byte),
    }
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
        self.tokens.filter(&literals, &mut filtered);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined(redactor: &mut Redactor, chunks: &[&[u8]]) -> Vec<u8> {
        let mut output = Vec::new();
        for chunk in chunks {
            output.extend(redactor.filter(chunk).expect("bounded input"));
        }
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
    fn bearer_and_credential_url_values_split_across_chunks_are_hidden() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(
            &mut redactor,
            &[
                b"Authorization: Bear",
                b"er abc.def_123\nfetch https:",
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
