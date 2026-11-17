//! Streaming redaction for bytes retained by supervisor capture.
//!
//! Exact literal prefixes and incomplete token markers stay buffered across
//! chunks. The capture filter's EOF hook releases any unmatched tail.
//! `key=value` credential assignments are scanned byte by byte: keys stream
//! through unchanged and only the value is withheld, so no replay is needed.
use provenant_os::exec::{CaptureFilter, FRAME_LIMIT, FilterError};
use provenant_os::fs::{Directory, FileHandle, FileKind, Metadata};
use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;

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
            matched: false,
        }
    }

    fn filter(&mut self, bytes: &[u8], output: &mut Vec<(u8, bool)>) {
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

    fn finish(&mut self, output: &mut Vec<(u8, bool)>) {
        while !self.pending.is_empty() {
            self.emit_front(output);
        }
        self.state = 0;
    }

    fn emit_front(&mut self, output: &mut Vec<(u8, bool)>) {
        let Some((byte, masked)) = self.pending.pop_front() else {
            return;
        };
        output.push((byte, masked));
    }
}

#[derive(Clone, Copy)]
enum TokenKind {
    Bearer,
    Generic,
    AwsAccessKey,
}

const TOKEN_MARKERS: &[(&[u8], TokenKind)] = &[
    (b"Bearer \0", TokenKind::Bearer),
    (b"ghp_\0", TokenKind::Generic),
    (b"gho_\0", TokenKind::Generic),
    (b"ghu_\0", TokenKind::Generic),
    (b"ghs_\0", TokenKind::Generic),
    (b"ghr_\0", TokenKind::Generic),
    (b"github_pat_\0", TokenKind::Generic),
    (b"sk-\0", TokenKind::Generic),
    (b"AKIA\0", TokenKind::AwsAccessKey),
];
const MAX_MARKER_LENGTH: usize = 11;

fn marker_pattern(marker: &[u8]) -> &[u8] {
    marker.strip_suffix(&[0]).unwrap_or(marker)
}

struct ActiveToken {
    kind: TokenKind,
    // Withheld until the body length determines whether the marker belongs to
    // a credential. Generic/AWS markers are masked with a matching body.
    // Candidate body bytes are delayed only until the minimum credential
    // length is known. The bytes remain in source form, including JSON escapes.
    marker: Vec<u8>,
    body: Vec<u8>,
    body_length: usize,
    emitted: bool,
}

#[derive(Default)]
enum Escape {
    #[default]
    None,
    Slash,
    Unicode(u16, u8),
}

struct TokenScanner {
    active: Option<ActiveToken>,
    // A possible marker or a partial Unicode token character at a chunk edge.
    tail: Vec<u8>,
    boundary: bool,
    escape: Escape,
    // URL userinfo cannot be decided until the authority delimiter arrives.
    // This remains bounded by FRAME_LIMIT, as in the original scanner.
    url_authority: Option<Vec<u8>>,
    matched: bool,
    replacements: Vec<(usize, usize)>,
}

impl Default for TokenScanner {
    fn default() -> Self {
        Self {
            active: None,
            tail: Vec::with_capacity(MAX_MARKER_LENGTH - 1),
            boundary: true,
            escape: Escape::None,
            url_authority: None,
            matched: false,
            replacements: Vec::new(),
        }
    }
}

impl TokenScanner {
    /// Return decisions for source bytes in order. Each byte appears once;
    /// `true` marks the credential body or URL userinfo for masking.
    fn filter(&mut self, bytes: &[u8], output: &mut Vec<(u8, bool)>) -> Result<(), FilterError> {
        let mut combined = std::mem::take(&mut self.tail);
        combined.extend_from_slice(bytes);
        self.process(&combined, false, output)
    }

    fn finish(&mut self, output: &mut Vec<(u8, bool)>) -> Result<(), FilterError> {
        let tail = std::mem::take(&mut self.tail);
        self.process(&tail, true, output)?;
        if let Some(authority) = self.url_authority.take() {
            self.finish_authority(authority, output);
        }
        self.boundary = true;
        self.escape = Escape::None;
        self.active = None;
        Ok(())
    }

    fn process(
        &mut self,
        input: &[u8],
        eof: bool,
        output: &mut Vec<(u8, bool)>,
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
                    self.observe(input[index]);
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
                let remaining = &input[index..];
                if byte == b'\\'
                    && !eof
                    && remaining.len() < 6
                    && (remaining.len() == 1
                        || (remaining[1] == b'u'
                            && remaining[2..].iter().all(u8::is_ascii_hexdigit)))
                {
                    self.tail.extend_from_slice(remaining);
                    return Ok(());
                }
                let escaped = if remaining.len() >= 6 && remaining.starts_with(b"\\u") {
                    std::str::from_utf8(&remaining[2..6])
                        .ok()
                        .and_then(|digits| u16::from_str_radix(digits, 16).ok())
                        .filter(|value| *value <= 127 && token_byte(kind, *value as u8))
                        .map(|_| 6)
                } else {
                    None
                };
                if token_byte(kind, byte) || escaped.is_some() {
                    let width = escaped.unwrap_or(1);
                    let became_secret = if let Some(active) = self.active.as_mut() {
                        if active.emitted {
                            output.extend(remaining[..width].iter().copied().map(|b| (b, true)));
                            false
                        } else {
                            active.body.extend_from_slice(&remaining[..width]);
                            active.body_length += 1;
                            if active.body_length >= minimum_token_length(kind) {
                                if matches!(kind, TokenKind::Bearer) {
                                    if active.marker != b"Bearer " {
                                        self.replacements.push((output.len(), active.marker.len()));
                                    }
                                    output.extend(active.marker.drain(..).map(|b| (b, false)));
                                } else {
                                    output.extend(active.marker.drain(..).map(|b| (b, true)));
                                }
                                output.extend(active.body.drain(..).map(|b| (b, true)));
                                active.emitted = true;
                                true
                            } else {
                                false
                            }
                        }
                    } else {
                        false
                    };
                    if became_secret {
                        self.matched = true;
                    }
                    for byte in &remaining[..width] {
                        self.observe(*byte);
                    }
                    index += width;
                    continue;
                }
                self.finish_active(output);
                continue;
            }

            if input[index..].starts_with(b"://") {
                output.extend(b"://".iter().copied().map(|b| (b, false)));
                for byte in b"://" {
                    self.observe(*byte);
                }
                self.url_authority = Some(Vec::new());
                index += 3;
                continue;
            }
            if !eof && b"://".starts_with(&input[index..]) {
                self.tail.extend_from_slice(&input[index..]);
                return Ok(());
            }

            // Only an escaped opening backslash disables a Unicode marker.
            // Raw ASCII markers still match immediately after a backslash.
            if self.boundary && !(input[index] == b'\\' && matches!(self.escape, Escape::Slash)) {
                if let Some((length, _kind)) = token_marker_at(&input[index..]) {
                    let marker = &input[index..index + length];
                    let marker = marker.to_vec();
                    for byte in &marker {
                        self.observe(*byte);
                    }
                    self.active = Some(ActiveToken {
                        kind: _kind,
                        marker,
                        body: Vec::with_capacity(minimum_token_length(_kind)),
                        body_length: 0,
                        emitted: false,
                    });
                    index += length;
                    continue;
                }
                if !eof && token_marker_prefix(&input[index..]) {
                    self.tail.extend_from_slice(&input[index..]);
                    return Ok(());
                }
            }

            let byte = input[index];
            output.push((byte, false));
            self.observe(byte);
            index += 1;
        }
        if eof {
            self.finish_active(output);
        }
        Ok(())
    }

    fn observe(&mut self, byte: u8) {
        match std::mem::take(&mut self.escape) {
            Escape::None if byte == b'\\' => {
                self.boundary = true;
                self.escape = Escape::Slash;
            }
            Escape::Slash if byte == b'u' => {
                self.boundary = false;
                self.escape = Escape::Unicode(0, 0);
            }
            Escape::Slash if b"bfnrt\\\"/".contains(&byte) => self.boundary = true,
            Escape::Unicode(value, count) if byte.is_ascii_hexdigit() => {
                let digit = (byte as char).to_digit(16).unwrap_or_default() as u16;
                let value = value * 16 + digit;
                if count == 3 {
                    self.boundary = value > 127 || !word_byte(value as u8);
                } else {
                    self.boundary = false;
                    self.escape = Escape::Unicode(value, count + 1);
                }
            }
            _ => self.boundary = !word_byte(byte),
        }
    }

    fn finish_active(&mut self, output: &mut Vec<(u8, bool)>) {
        if let Some(mut active) = self.active.take()
            && !active.emitted
        {
            output.extend(active.marker.drain(..).map(|byte| (byte, false)));
            output.extend(active.body.drain(..).map(|byte| (byte, false)));
        }
    }

    fn finish_authority(&mut self, authority: Vec<u8>, output: &mut Vec<(u8, bool)>) {
        if let Some(userinfo_end) = authority.iter().rposition(|byte| *byte == b'@') {
            let userinfo = &authority[..userinfo_end];
            if userinfo.contains(&b':') {
                output.extend(
                    authority[..userinfo_end]
                        .iter()
                        .copied()
                        .map(|byte| (byte, true)),
                );
                output.extend(
                    authority[userinfo_end..]
                        .iter()
                        .copied()
                        .map(|byte| (byte, false)),
                );
                self.matched = true;
                return;
            }
        }
        output.extend(authority.into_iter().map(|byte| (byte, false)));
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

enum MarkerMatch {
    Complete(usize),
    Partial,
    Mismatch,
}

fn marker_match(bytes: &[u8], marker: &[u8]) -> MarkerMatch {
    let mut index = 0;
    for expected in marker {
        if index == bytes.len() {
            return MarkerMatch::Partial;
        }
        if bytes[index] == b'\\' {
            let remaining = &bytes[index..];
            let encoded = format!("\\u{:04x}", expected);
            let width = remaining.len().min(6);
            if remaining[..width].get(1).is_some_and(|byte| *byte != b'u')
                || !remaining[..width].eq_ignore_ascii_case(&encoded.as_bytes()[..width])
            {
                return MarkerMatch::Mismatch;
            }
            if width < 6 {
                return MarkerMatch::Partial;
            }
            index += 6;
        } else if bytes[index] == *expected {
            index += 1;
        } else {
            return MarkerMatch::Mismatch;
        }
    }
    MarkerMatch::Complete(index)
}

fn token_marker_at(bytes: &[u8]) -> Option<(usize, TokenKind)> {
    TOKEN_MARKERS.iter().find_map(|(marker, kind)| {
        match marker_match(bytes, marker_pattern(marker)) {
            MarkerMatch::Complete(length) => Some((length, *kind)),
            _ => None,
        }
    })
}

fn token_marker_prefix(bytes: &[u8]) -> bool {
    TOKEN_MARKERS.iter().any(|(marker, _)| {
        matches!(
            marker_match(bytes, marker_pattern(marker)),
            MarkerMatch::Partial
        )
    })
}

fn word_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn url_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || b"/?#\\\"'<>`\\\\".contains(&byte)
}

/// Credential keys, matched case-insensitively. A run of identifier bytes
/// matches a key when it ends with the key and the byte before the key is a
/// separator (or the run starts there): `db_password` and `x-api-key` match,
/// while `compass` and `oauth` do not. `client_secret` matches through `secret`.
/// The flag marks `Authorization`, whose scheme word (`Basic`, `Bearer`) stays.
const ASSIGNMENT_KEYS: &[(&[u8], bool)] = &[
    (b"password", false),
    (b"passwd", false),
    (b"pass", false),
    (b"token", false),
    (b"secret", false),
    (b"api_key", false),
    (b"api-key", false),
    (b"apikey", false),
    (b"access_key", false),
    (b"access-key", false),
    (b"auth", false),
    (b"authorization", true),
];
/// The longest key is 13 bytes; the run keeps enough bytes before it to judge
/// the separator that precedes it.
const ASSIGNMENT_RUN: usize = 32;
/// Scheme words longer than this are not `Basic` or `Bearer`.
const SCHEME_WORD: usize = 16;

/// Where the assignment scanner is. Quoted values remain masked until a
/// definite closing delimiter or the raw line end.
#[derive(Default)]
enum Assignment {
    #[default]
    Scan,
    /// After a credential key: quotes, escapes and whitespace may precede the separator.
    Gap { scheme: bool },
    /// After the separator: quotes, escapes and whitespace may precede the value.
    Lead {
        scheme: bool,
        quote: Option<u8>,
        quote_slashes: usize,
        backslashes: usize,
    },
    /// Letters after `Authorization:`, held until the scheme is known.
    Scheme { word: Vec<u8> },
    /// Inside a value, which is replaced by one `[REDACTED]`. `line` values end
    /// only at a newline, so an unknown `Authorization` scheme hides its whole line.
    Value {
        quote: Option<u8>,
        quote_slashes: usize,
        backslashes: usize,
        pending_slashes: usize,
        line: bool,
    },
}

// Redacts the values of credential keys. Quoted values end at their closing
// quote; a JSON-escaped value (`\"…\"`) ends at the `\"`.
#[derive(Default)]
struct AssignmentScanner {
    state: Assignment,
    run: Vec<u8>,
    after_backslash: bool,
    matched: bool,
    insertions: Vec<usize>,
}

impl AssignmentScanner {
    fn filter(&mut self, bytes: &[u8], output: &mut DecisionQueue) {
        for byte in bytes {
            self.step(*byte, output);
        }
    }

    fn finish(&mut self, output: &mut DecisionQueue) {
        match std::mem::take(&mut self.state) {
            Assignment::Scheme { word } => {
                // A held scheme word never reached whitespace, so it is the credential.
                self.mask_bytes(&word, output);
            }
            Assignment::Value {
                quote: Some(_),
                pending_slashes,
                ..
            } => self.mask_slashes(pending_slashes, output),
            _ => {}
        }
        self.run.clear();
        self.after_backslash = false;
    }

    fn keep(&mut self, byte: u8, masked: bool, output: &mut DecisionQueue) {
        output.push((byte, masked));
        self.matched |= masked;
    }

    fn mask_bytes(&mut self, bytes: &[u8], output: &mut DecisionQueue) {
        for byte in bytes {
            self.keep(*byte, true, output);
        }
    }

    fn mask_slashes(&mut self, count: usize, output: &mut DecisionQueue) {
        for _ in 0..count {
            self.keep(b'\\', true, output);
        }
    }

    fn keep_slashes(&mut self, count: usize, masked: bool, output: &mut DecisionQueue) {
        for _ in 0..count {
            self.keep(b'\\', masked, output);
        }
    }

    // `std::mem::take` leaves `Scan`, so a branch that sets no state returns to scanning.
    fn step(&mut self, byte: u8, output: &mut DecisionQueue) {
        loop {
            match std::mem::take(&mut self.state) {
                Assignment::Scan => {
                    let escaped = std::mem::take(&mut self.after_backslash);
                    if escaped && byte.is_ascii_alphanumeric() {
                        // The letter of a JSON escape such as `\\n` is not part of a key.
                        self.keep(byte, false, output);
                        return;
                    }
                    if byte.is_ascii_alphanumeric() || b"_-.".contains(&byte) {
                        self.run.push(byte.to_ascii_lowercase());
                        if self.run.len() > ASSIGNMENT_RUN {
                            self.run.remove(0);
                        }
                        self.keep(byte, false, output);
                        return;
                    }
                    let key = self.key_ending_run();
                    self.run.clear();
                    if let Some(scheme) = key {
                        self.state = Assignment::Gap { scheme };
                        continue;
                    }
                    self.after_backslash = byte == b'\\' && !escaped;
                    self.keep(byte, false, output);
                    return;
                }
                Assignment::Gap { scheme } => {
                    if byte == b'=' || byte == b':' {
                        self.keep(byte, false, output);
                        self.state = Assignment::Lead {
                            scheme,
                            quote: None,
                            quote_slashes: 0,
                            backslashes: 0,
                        };
                        return;
                    }
                    if byte.is_ascii_whitespace() || b"\"'\\".contains(&byte) {
                        self.keep(byte, false, output);
                        self.state = Assignment::Gap { scheme };
                        return;
                    }
                    // Not an assignment after all, so the byte is rescanned.
                    continue;
                }
                Assignment::Lead {
                    scheme,
                    quote,
                    quote_slashes,
                    backslashes,
                } => {
                    if quote.is_none() && (byte.is_ascii_whitespace() || byte == b'\\') {
                        self.keep(byte, false, output);
                        self.state = Assignment::Lead {
                            scheme,
                            quote,
                            quote_slashes,
                            backslashes: if byte == b'\\' {
                                backslashes.saturating_add(1)
                            } else {
                                0
                            },
                        };
                        return;
                    }
                    if quote.is_none() && (byte == b'"' || byte == b'\'') {
                        self.keep(byte, false, output);
                        self.state = Assignment::Lead {
                            scheme,
                            quote: Some(byte),
                            quote_slashes: backslashes,
                            backslashes: 0,
                        };
                        return;
                    }
                    if quote.is_none() && is_delimiter(byte) {
                        continue;
                    }
                    if scheme && quote.is_none() && byte.is_ascii_alphabetic() {
                        self.state = Assignment::Scheme { word: vec![byte] };
                        return;
                    }
                    self.state = Assignment::Value {
                        quote,
                        quote_slashes,
                        backslashes: 0,
                        pending_slashes: 0,
                        line: scheme,
                    };
                    continue;
                }
                Assignment::Scheme { mut word } => {
                    if byte.is_ascii_alphabetic() && word.len() < SCHEME_WORD {
                        word.push(byte);
                        self.state = Assignment::Scheme { word };
                        return;
                    }
                    let known =
                        word.eq_ignore_ascii_case(b"basic") || word.eq_ignore_ascii_case(b"bearer");
                    if known && byte.is_ascii_whitespace() {
                        for source_byte in &word {
                            self.keep(*source_byte, false, output);
                        }
                        self.keep(byte, false, output);
                        self.state = Assignment::Lead {
                            scheme: false,
                            quote: None,
                            quote_slashes: 0,
                            backslashes: 0,
                        };
                        return;
                    }
                    self.mask_bytes(&word, output);
                    self.state = Assignment::Value {
                        quote: None,
                        quote_slashes: 0,
                        backslashes: 0,
                        pending_slashes: 0,
                        line: true,
                    };
                    continue;
                }
                Assignment::Value {
                    quote,
                    quote_slashes,
                    backslashes,
                    pending_slashes,
                    line,
                } => match quote {
                    Some(quote) => {
                        if byte == b'\n' || byte == b'\r' {
                            self.mask_slashes(pending_slashes, output);
                            continue;
                        }
                        if byte == quote && quote_close(backslashes, quote_slashes) {
                            // Keep only the source slashes needed to encode this close.
                            // Older slashes from the same run were emitted as masked.
                            if pending_slashes > 0 {
                                // The old scanner masks the first slash as a value
                                // byte before recognising the closing delimiter.
                                self.insertions.push(output.total);
                                self.matched = true;
                            }
                            self.keep_slashes(pending_slashes, false, output);
                            self.keep(byte, false, output);
                            return;
                        }
                        if byte == b'\\' {
                            let next_count = backslashes.saturating_add(1);
                            if pending_slashes < quote_slashes {
                                self.state = Assignment::Value {
                                    quote: Some(quote),
                                    quote_slashes,
                                    backslashes: next_count,
                                    pending_slashes: pending_slashes.saturating_add(1),
                                    line,
                                };
                            } else {
                                self.keep(byte, true, output);
                                self.state = Assignment::Value {
                                    quote: Some(quote),
                                    quote_slashes,
                                    backslashes: next_count,
                                    pending_slashes,
                                    line,
                                };
                            }
                            return;
                        }
                        self.mask_slashes(pending_slashes, output);
                        self.keep(byte, true, output);
                        self.state = Assignment::Value {
                            quote: Some(quote),
                            quote_slashes,
                            backslashes: 0,
                            pending_slashes: 0,
                            line,
                        };
                        return;
                    }
                    None => {
                        let ends = if line {
                            byte == b'\n' || byte == b'\r'
                        } else {
                            is_delimiter(byte)
                        };
                        if ends {
                            continue;
                        }
                        self.keep(byte, true, output);
                        self.state = Assignment::Value {
                            quote: None,
                            quote_slashes: 0,
                            backslashes: 0,
                            pending_slashes: 0,
                            line,
                        };
                        return;
                    }
                },
            }
        }
    }

    /// The scheme flag of the key that the run ends with, if any.
    fn key_ending_run(&self) -> Option<bool> {
        ASSIGNMENT_KEYS.iter().find_map(|(key, scheme)| {
            let start = self.run.len().checked_sub(key.len())?;
            let bounded = start == 0 || !self.run[start - 1].is_ascii_alphanumeric();
            (bounded && self.run.ends_with(key)).then_some(*scheme)
        })
    }
}

// The delimiter has its own source slashes; each value-level backslash takes
// one encoding layer. An even number leaves the delimiter unescaped.
fn closing_quote(backslashes: usize, quote_slashes: usize, layer: usize) -> bool {
    backslashes % layer == quote_slashes && (backslashes / layer).is_multiple_of(2)
}

// The opener's slashes give the shallowest layer that can represent it, but
// the value may be escaped any number of layers deeper (an apostrophe, which
// JSON never escapes, reveals nothing at all). A quote is a definite close
// only when every power-of-two layer from the opener's up to the backslash run
// agrees; a fixed depth cap would release a value escaped one layer deeper.
fn quote_close(backslashes: usize, quote_slashes: usize) -> bool {
    let mut layer = quote_slashes.saturating_add(1).next_power_of_two();
    loop {
        if !closing_quote(backslashes, quote_slashes, layer) {
            return false;
        }
        layer = layer.saturating_mul(2);
        if layer > backslashes {
            return true;
        }
    }
}

/// Bytes that end an unquoted value.
fn is_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || b"\"'\\&,;<>)]}`".contains(&byte)
}

// Run-length encoding keeps a withheld closing backslash run compact even when
// the opener is arbitrarily long. Each run covers source bytes, not output.
#[derive(Default)]
struct DecisionQueue {
    runs: VecDeque<DecisionRun>,
    total: usize,
}
struct DecisionRun {
    byte: u8,
    masked: bool,
    count: usize,
}
impl DecisionQueue {
    fn extend(&mut self, decisions: Vec<(u8, bool)>) {
        for decision in decisions {
            self.push(decision);
        }
    }
    fn push(&mut self, (byte, masked): (u8, bool)) {
        self.total += 1;
        if let Some(last) = self.runs.back_mut()
            && last.byte == byte
            && last.masked == masked
        {
            last.count += 1;
        } else {
            self.runs.push_back(DecisionRun {
                byte,
                masked,
                count: 1,
            });
        }
    }
    fn consume(&mut self, count: usize) {
        let first = self.runs.front_mut().expect("nonempty");
        first.count -= count;
        if first.count == 0 {
            self.runs.pop_front();
        }
    }
}
fn append_frame_bytes(frames: &mut Vec<Vec<u8>>, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        if frames.last().is_none_or(|frame| frame.len() == FRAME_LIMIT) {
            frames.push(Vec::new());
        }
        let frame = frames.last_mut().expect("frame exists");
        let count = bytes.len().min(FRAME_LIMIT - frame.len());
        frame.extend_from_slice(&bytes[..count]);
        bytes = &bytes[count..];
    }
}
fn append_frame_repeat(frames: &mut Vec<Vec<u8>>, byte: u8, mut count: usize) {
    while count > 0 {
        if frames.last().is_none_or(|frame| frame.len() == FRAME_LIMIT) {
            frames.push(Vec::new());
        }
        let frame = frames.last_mut().expect("frame exists");
        let take = count.min(FRAME_LIMIT - frame.len());
        frame.resize(frame.len() + take, byte);
        count -= take;
    }
}

/// Stateful filter used directly by supervisor capture. Values are never
/// serialized or printed; malformed bounds make construction fail closed.
pub struct Redactor {
    literals: LiteralAutomaton,
    tokens: TokenScanner,
    assignments: AssignmentScanner,
    failed: bool,
    decisions: [DecisionQueue; 3],
    redaction_run: bool,
    emitted_source: usize,
    token_source: usize,
    replacements: VecDeque<(usize, usize)>,
    insertions: VecDeque<usize>,
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
            assignments: AssignmentScanner::default(),
            failed: false,
            decisions: std::array::from_fn(|_| DecisionQueue::default()),
            redaction_run: false,
            emitted_source: 0,
            token_source: 0,
            replacements: VecDeque::new(),
            insertions: VecDeque::new(),
        })
    }

    pub fn has_redaction(&self) -> bool {
        self.literals.matched || self.tokens.matched || self.assignments.matched
    }

    fn filter_frames(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FilterError> {
        if self.failed {
            return Err(FilterError);
        }
        let mut frames = Vec::new();
        for chunk in bytes.chunks(FRAME_LIMIT) {
            let mut literals = Vec::new();
            let mut tokens = Vec::new();
            self.literals.filter(chunk, &mut literals);
            self.tokens.filter(chunk, &mut tokens).map_err(|_| {
                self.failed = true;
                FilterError
            })?;
            self.assignments.filter(chunk, &mut self.decisions[2]);
            self.insertions
                .extend(self.assignments.insertions.drain(..));
            self.decisions[0].extend(literals);
            self.queue_tokens(tokens);
            self.emit_decided(&mut frames)?;
        }
        Ok(frames)
    }

    fn finish_frames(&mut self) -> Result<Vec<Vec<u8>>, FilterError> {
        if self.failed {
            return Err(FilterError);
        }
        let mut literals = Vec::new();
        let mut tokens = Vec::new();
        self.literals.finish(&mut literals);
        self.tokens.finish(&mut tokens).map_err(|_| {
            self.failed = true;
            FilterError
        })?;
        self.assignments.finish(&mut self.decisions[2]);
        self.insertions
            .extend(self.assignments.insertions.drain(..));
        self.decisions[0].extend(literals);
        self.queue_tokens(tokens);
        let mut frames = Vec::new();
        self.emit_decided(&mut frames)?;
        if self.decisions.iter().any(|queue| !queue.runs.is_empty()) {
            self.failed = true;
            return Err(FilterError);
        }
        Ok(frames)
    }

    fn queue_tokens(&mut self, tokens: Vec<(u8, bool)>) {
        for (start, len) in self.tokens.replacements.drain(..) {
            self.replacements
                .push_back((self.token_source + start, len));
        }
        self.token_source += tokens.len();
        self.decisions[1].extend(tokens);
    }

    fn emit_decided(&mut self, frames: &mut Vec<Vec<u8>>) -> Result<(), FilterError> {
        while self.decisions.iter().all(|queue| !queue.runs.is_empty()) {
            if self.insertions.front() == Some(&self.emitted_source) {
                self.insertions.pop_front();
                if !self.redaction_run {
                    append_frame_bytes(frames, REDACTED);
                }
                self.redaction_run = true;
            }
            if let Some(&(start, len)) = self.replacements.front()
                && start == self.emitted_source
            {
                // A replacement is safe only if all detectors have decided the
                // entire marker and none masks any of its original bytes.
                if self
                    .decisions
                    .iter()
                    .any(|queue| queue.runs.iter().map(|run| run.count).sum::<usize>() < len)
                {
                    break;
                }
                let masked = self.decisions.iter().any(|queue| {
                    let mut left = len;
                    queue.runs.iter().any(|run| {
                        if left == 0 {
                            return false;
                        }
                        let used = left.min(run.count);
                        left -= used;
                        run.masked
                    })
                });
                self.replacements.pop_front();
                if !masked {
                    for queue in &mut self.decisions {
                        let mut left = len;
                        while left > 0 {
                            let count =
                                left.min(queue.runs.front().expect("marker decisions").count);
                            queue.consume(count);
                            left -= count;
                        }
                    }
                    append_frame_bytes(frames, b"Bearer ");
                    self.redaction_run = false;
                    self.emitted_source += len;
                    continue;
                }
            }
            let first = self.decisions[0].runs.front().expect("nonempty");
            let byte = first.byte;
            let mut count = self
                .decisions
                .iter()
                .map(|queue| queue.runs.front().expect("nonempty").count)
                .min()
                .expect("three queues");
            if let Some(&(start, _)) = self.replacements.front() {
                count = count.min(start - self.emitted_source);
            }
            if let Some(&start) = self.insertions.front() {
                count = count.min(start - self.emitted_source);
            }
            let masked = self
                .decisions
                .iter()
                .any(|queue| queue.runs.front().expect("nonempty").masked);
            if !self
                .decisions
                .iter()
                .all(|queue| queue.runs.front().expect("nonempty").byte == byte)
            {
                self.failed = true;
                return Err(FilterError);
            }
            if masked {
                if !self.redaction_run {
                    append_frame_bytes(frames, REDACTED);
                }
            } else {
                append_frame_repeat(frames, byte, count);
            }
            self.redaction_run = masked;
            self.emitted_source += count;
            for queue in &mut self.decisions {
                queue.consume(count);
            }
        }
        Ok(())
    }
}

/// Filter one bounded in-memory message or artifact using the same known
/// credential shapes as capture. Filter before paging to preserve token context.
pub fn known_shapes(bytes: &[u8]) -> Result<Vec<u8>, FilterError> {
    let mut redactor = Redactor::new([]).map_err(|_| FilterError)?;
    let mut safe = Vec::new();
    for chunk in bytes.chunks(8192) {
        safe.extend(redactor.filter(chunk)?.into_iter().flatten());
    }
    safe.extend(redactor.finish()?.into_iter().flatten());
    Ok(safe)
}

const SESSION_CHUNK: usize = 32 * 1024;
const SESSION_DEPTH: u8 = 8;
const SESSION_ENTRIES: usize = 100_000;
const SESSION_CANDIDATES: usize = 64;
const SESSION_STAGING: &str = ".session-redact-";
static SESSION_STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Refusals seen during one scrub. A refusal is recorded and the scrub goes on
/// to the remaining entries, so one bad file never leaves another unscrubbed.
#[derive(Default)]
struct Scrub {
    rewritten: usize,
    failed: bool,
    text_only: bool,
}

impl Scrub {
    /// Keep a value that was read, or record the refusal and move on.
    fn keep<T>(&mut self, result: Result<T, FilterError>) -> Option<T> {
        result.inspect_err(|_| self.failed = true).ok()
    }

    fn file(&mut self, dir: &Directory, name: &OsStr) {
        match scrub_file(dir, name, self.text_only) {
            Ok(rewritten) => self.rewritten += usize::from(rewritten),
            Err(_) => self.failed = true,
        }
    }

    fn finish(self) -> Result<usize, FilterError> {
        if self.failed {
            Err(FilterError)
        } else {
            Ok(self.rewritten)
        }
    }
}

/// Post-process provider-owned session text under an attempt's private state.
///
/// Codex writes rollouts and history files and Claude writes project sessions
/// there, so a credential a lane printed can survive in a transcript even when
/// captures and the result are filtered. `.jsonl` transcripts in the known
/// session layouts are rewritten in place. Claude's `projects` roots also hold
/// large tool output as regular `.txt` and `.json` files, so those are scrubbed
/// too. Cursor chats and OpenCode data text are scrubbed best effort with the
/// same traversal bounds; failures there do not fail settlement. SQLite files
/// (including journals) and other binary stores remain unscrubbed.
/// Credentials, cache trees and links are never touched. A
/// file without a known shape is only read and left as it is.
///
/// Every entry is visited even when another one is refused: a link, a hard
/// link, a file that changes while it is read, a line over the frame bound, a
/// tree deeper than the bound, or more candidates than the bound. Each refusal
/// is recorded and the call returns an error once the walk is done. Candidates
/// past the bound are not descended into. Returns the number of rewritten files
/// when every entry was scrubbed.
pub fn scrub_session_history(root: &Path) -> Result<usize, FilterError> {
    let state = match Directory::open_anchor(root) {
        Ok(state) => state,
        Err(error) if error.is_not_found() => return Ok(0),
        Err(_) => return Err(FilterError),
    };
    let mut scrub = Scrub::default();
    scrub_layout(&state, &mut scrub);
    let Some(entries) = scrub.keep(state.entries().map_err(|_| FilterError)) else {
        return scrub.finish();
    };
    let mut candidates = 0;
    for candidate in entries {
        let Some(candidate) = scrub.keep(candidate.map_err(|_| FilterError)) else {
            break;
        };
        let is_candidate = candidate.to_str().is_some_and(|name| {
            name.strip_prefix("candidate-")
                .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
        });
        if !is_candidate {
            continue;
        }
        candidates += 1;
        if candidates > SESSION_CANDIDATES {
            scrub.failed = true;
            break;
        }
        let candidate = scrub.keep(
            state
                .open_verified_directory(&candidate)
                .map_err(|_| FilterError),
        );
        if let Some(candidate) = candidate {
            scrub_layout(&candidate, &mut scrub);
        }
    }
    scrub.finish()
}

fn scrub_layout(dir: &Directory, scrub: &mut Scrub) {
    for name in ["sessions", "archived_sessions"] {
        if let Some(Some(sessions)) = scrub.keep(subdirectory(dir, name)) {
            scrub_tree(&sessions, 0, is_jsonl, scrub);
        }
    }
    // These formerly host-backed stores are best effort: an unreadable or
    // over-bound store must not turn a successful settlement into a failure.
    let mut stores = Scrub {
        text_only: true,
        ..Scrub::default()
    };
    for (parent, child) in [(".cursor", "chats"), ("data", "opencode")] {
        if let Some(Some(parent)) = stores.keep(subdirectory(dir, parent))
            && let Some(Some(tree)) = stores.keep(subdirectory(&parent, child))
        {
            scrub_tree(&tree, 0, is_store_candidate, &mut stores);
        }
    }
    scrub.rewritten += stores.rewritten;
    scrub_files(dir, scrub);
    if let Some(Some(config)) = scrub.keep(subdirectory(dir, "claude-config")) {
        if let Some(Some(projects)) = scrub.keep(subdirectory(&config, "projects")) {
            scrub_tree(&projects, 0, is_projects_text, scrub);
        }
        scrub_files(&config, scrub);
    }
}

fn scrub_files(dir: &Directory, scrub: &mut Scrub) {
    for name in ["history.jsonl", "session_index.jsonl"] {
        if let Some(Some(file)) = scrub.keep(regular_file(dir, name)) {
            scrub.file(dir, &file);
        }
    }
}

fn scrub_tree(dir: &Directory, depth: u8, wanted: fn(&OsStr) -> bool, scrub: &mut Scrub) {
    if depth > SESSION_DEPTH {
        scrub.failed = true;
        return;
    }
    let Some(entries) = scrub.keep(dir.entries().map_err(|_| FilterError)) else {
        return;
    };
    for (index, entry) in entries.enumerate() {
        if index >= SESSION_ENTRIES {
            scrub.failed = true;
            return;
        }
        let Some(entry) = scrub.keep(entry.map_err(|_| FilterError)) else {
            return;
        };
        let Some(metadata) = scrub.keep(dir.metadata_at(&entry, false).map_err(|_| FilterError))
        else {
            continue;
        };
        match metadata.kind {
            FileKind::Directory => {
                let child =
                    scrub.keep(dir.open_verified_directory(&entry).map_err(|_| FilterError));
                if let Some(child) = child {
                    scrub_tree(&child, depth + 1, wanted, scrub);
                }
            }
            FileKind::Regular if wanted(&entry) => scrub.file(dir, &entry),
            _ => {}
        }
    }
}

fn subdirectory(dir: &Directory, name: &str) -> Result<Option<Directory>, FilterError> {
    match dir.metadata_at(OsStr::new(name), false) {
        Ok(metadata) if metadata.kind == FileKind::Directory => dir
            .open_verified_directory(OsStr::new(name))
            .map(Some)
            .map_err(|_| FilterError),
        Ok(_) => Ok(None),
        Err(error) if error.is_not_found() => Ok(None),
        Err(_) => Err(FilterError),
    }
}

fn regular_file(dir: &Directory, name: &str) -> Result<Option<std::ffi::OsString>, FilterError> {
    match dir.metadata_at(OsStr::new(name), false) {
        Ok(metadata) if metadata.kind == FileKind::Regular => Ok(Some(OsStr::new(name).into())),
        Ok(_) => Ok(None),
        Err(error) if error.is_not_found() => Ok(None),
        Err(_) => Err(FilterError),
    }
}

fn is_jsonl(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| name.ends_with(".jsonl"))
}

/// Claude keeps large tool output as plain text beside its transcripts.
fn is_projects_text(name: &OsStr) -> bool {
    is_jsonl(name)
        || name
            .to_str()
            .is_some_and(|name| name.ends_with(".txt") || name.ends_with(".json"))
}

// SQLite and its sidecars remain binary stores even if a fixture or empty
// journal happens to contain only text. Other files are checked as streamed UTF-8.
fn is_store_candidate(name: &OsStr) -> bool {
    !name.to_str().is_some_and(|name| {
        name == "auth.json"
            || name.starts_with(SESSION_STAGING)
            || [".sqlite", ".sqlite3", ".db"].iter().any(|extension| {
                name.rsplit_once(extension)
                    .is_some_and(|(_, tail)| matches!(tail, "" | "-wal" | "-shm" | "-journal"))
            })
    })
}

/// Rewrite one transcript through the streaming filter, replacing the entry
/// only when a shape matched. A clean file is read once and never staged,
/// copied or synced. A file that changes while it is read fails closed; a link
/// or hard link is refused rather than followed.
fn scrub_file(dir: &Directory, name: &OsStr, text_only: bool) -> Result<bool, FilterError> {
    let before = dir.metadata_at(name, false).map_err(|_| FilterError)?;
    if before.kind != FileKind::Regular || before.links != 1 {
        return Err(FilterError);
    }
    if !filter_pass(dir, name, &before, None, text_only)? {
        return Ok(false);
    }
    let staging = staging_name();
    let staging = OsStr::new(&staging);
    let mut target = dir.create_new(staging).map_err(|_| FilterError)?;
    let staged =
        filter_pass(dir, name, &before, Some(&mut target), text_only).and_then(|matched| {
            target.sync().map_err(|_| FilterError)?;
            Ok(matched)
        });
    if !matches!(staged, Ok(true)) {
        let _ = dir.unlink(staging);
        return staged;
    }
    if dir.replace(staging, dir, name).is_err() {
        let _ = dir.unlink(staging);
        return Err(FilterError);
    }
    dir.sync().map_err(|_| FilterError)?;
    Ok(true)
}

/// Stream one transcript through a fresh redactor, writing the filtered bytes
/// to `staged` when one is given. Returns whether any shape matched. The file
/// must match `before` when it is opened and must not change while it is read.
fn filter_pass(
    dir: &Directory,
    name: &OsStr,
    before: &Metadata,
    mut staged: Option<&mut FileHandle>,
    text_only: bool,
) -> Result<bool, FilterError> {
    let mut source = dir.open_read(name).map_err(|_| FilterError)?;
    let opened = source.metadata().map_err(|_| FilterError)?;
    if !before.unchanged(&opened) {
        return Err(FilterError);
    }
    let mut redactor = Redactor::new([]).map_err(|_| FilterError)?;
    let mut utf8_tail = Vec::new();
    let mut buffer = [0_u8; SESSION_CHUNK];
    loop {
        let read = source.read(&mut buffer).map_err(|_| FilterError)?;
        if read == 0 {
            break;
        }
        if text_only {
            let bytes = &buffer[..read];
            if bytes.contains(&0) {
                return Ok(false);
            }
            utf8_tail.extend_from_slice(bytes);
            match std::str::from_utf8(&utf8_tail) {
                Ok(_) => utf8_tail.clear(),
                Err(error) if error.error_len().is_none() => {
                    let valid = error.valid_up_to();
                    utf8_tail.drain(..valid);
                }
                Err(_) => return Ok(false),
            }
        }
        for frame in redactor.filter(&buffer[..read])? {
            write_staged(staged.as_deref_mut(), &frame)?;
        }
    }
    if !utf8_tail.is_empty() {
        return Ok(false);
    }
    for frame in redactor.finish()? {
        write_staged(staged.as_deref_mut(), &frame)?;
    }
    let after = source.metadata().map_err(|_| FilterError)?;
    if !opened.unchanged(&after) {
        return Err(FilterError);
    }
    Ok(redactor.has_redaction())
}

fn write_staged(staged: Option<&mut FileHandle>, bytes: &[u8]) -> Result<(), FilterError> {
    match staged {
        Some(target) => target.write_all(bytes).map_err(|_| FilterError),
        None => Ok(()),
    }
}

fn staging_name() -> String {
    format!(
        "{SESSION_STAGING}{}-{}",
        std::process::id(),
        SESSION_STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// Redact delivery diagnostics, including every URL or scp-style remote
/// username. Those fields can contain credentials even when no password is
/// present. The common filter handles the other credential shapes first.
pub fn delivery_diagnostics(detail: &str) -> String {
    let filtered = match known_shapes(detail.as_bytes()) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(FilterError) => return "[REDACTED]".into(),
    };
    let mut result = String::with_capacity(filtered.len());
    let mut rest = filtered.as_str();
    while let Some(scheme_end) = rest.find("://") {
        let authority_start = scheme_end + 3;
        result.push_str(&rest[..authority_start]);
        rest = &rest[authority_start..];
        let token_end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>' | '(' | ')'))
            .unwrap_or(rest.len());
        let token = &rest[..token_end];
        if let Some(at) = token.rfind('@') {
            result.push_str("[REDACTED]");
            result.push_str(&token[at..]);
            rest = &rest[token_end..];
        } else {
            // A malformed or truncated authority may have lost its '@'.
            result.push_str("[REDACTED]");
            let authority_end = token.find(['/', '?', '#']).unwrap_or(token.len());
            rest = &rest[authority_end..];
        }
    }
    result.push_str(rest);
    let mut safe = String::with_capacity(result.len());
    for token in result.split_inclusive(|c: char| {
        c.is_whitespace() || matches!(c, '\'' | '"' | '<' | '>' | '(' | ')')
    }) {
        if let Some(at) = token.rfind('@')
            && token[at + 1..].contains(':')
            && !token.contains("://")
        {
            safe.push_str("[REDACTED]");
            safe.push_str(&token[at..]);
        } else {
            safe.push_str(token);
        }
    }
    safe
}

/// A one-line excerpt of lane or server text, redacted with the known
/// credential shapes before it is bounded, so a cut never leaves part of a
/// secret that no longer matches a shape. Text that cannot be filtered is
/// replaced, never passed through.
pub fn bounded_line(text: &str, max_chars: usize) -> String {
    match known_shapes(text.as_bytes()) {
        Ok(safe) => provenant_core::bounded_line(&String::from_utf8_lossy(&safe), max_chars),
        Err(FilterError) => {
            provenant_core::bounded_line("excerpt could not be sanitised", max_chars)
        }
    }
}

impl CaptureFilter for Redactor {
    fn filter(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FilterError> {
        self.filter_frames(bytes)
    }

    fn finish(&mut self) -> Result<Vec<Vec<u8>>, FilterError> {
        self.finish_frames()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn joined(redactor: &mut Redactor, chunks: &[&[u8]]) -> Vec<u8> {
        let mut output = Vec::new();
        for chunk in chunks {
            output.extend(
                redactor
                    .filter(chunk)
                    .expect("bounded input")
                    .into_iter()
                    .flatten(),
            );
        }
        output.extend(
            redactor
                .finish()
                .expect("bounded finish")
                .into_iter()
                .flatten(),
        );
        output
    }

    #[test]
    fn literal_redaction_is_chunk_invariant_and_preserves_unmatched_text() {
        let cases = [
            (
                vec![b"token-123".to_vec()],
                b"git push --force origin main\nstdout: token-123 suffix".as_slice(),
                b"git push --force origin main\nstdout: [REDACTED] suffix".as_slice(),
            ),
            (
                vec![b"token-123".to_vec()],
                b"rm -rf ../state\nstderr: token-123 suffix".as_slice(),
                b"rm -rf ../state\nstderr: [REDACTED] suffix".as_slice(),
            ),
            (
                vec![b"abcde".to_vec(), b"cd".to_vec()],
                b"abcde".as_slice(),
                REDACTED,
            ),
            (
                vec![b"secret-value".to_vec()],
                b"ordinary secretx and done".as_slice(),
                b"ordinary secretx and done".as_slice(),
            ),
            (
                vec![b"secret-value".to_vec()],
                b"ordinary secret-".as_slice(),
                b"ordinary secret-".as_slice(),
            ),
        ];

        for (secrets, input, expected) in cases {
            for split in 0..=input.len() {
                let mut redactor = Redactor::new(secrets.clone()).expect("valid secrets");
                assert_eq!(
                    joined(&mut redactor, &[&input[..split], &input[split..]]),
                    expected,
                    "split {split} for {input:?}"
                );
            }
        }

        // Retain the original multi-chunk paths as well as every single split.
        let mut redactor = Redactor::new([b"token-123".to_vec()]).expect("valid secret");
        assert_eq!(
            joined(
                &mut redactor,
                &[
                    b"git push --force origin main\nstdout: ",
                    b"tok",
                    b"en-123 suffix",
                ]
            ),
            b"git push --force origin main\nstdout: [REDACTED] suffix"
        );

        let mut redactor = Redactor::new([b"token-123".to_vec()]).expect("valid secret");
        assert_eq!(
            joined(
                &mut redactor,
                &[b"rm -rf ../state\nstderr: ", b"tok", b"en-123 suffix",]
            ),
            b"rm -rf ../state\nstderr: [REDACTED] suffix"
        );

        let mut redactor =
            Redactor::new([b"abcde".to_vec(), b"cd".to_vec()]).expect("valid secrets");
        assert_eq!(joined(&mut redactor, &[b"ab", b"cd", b"e"]), REDACTED);
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
    fn token_shapes_are_redacted_or_preserved_across_chunk_boundaries()
    -> Result<(), Box<dyn std::error::Error>> {
        token_markers_split_across_chunks_redact_only_at_word_boundaries();
        json_escaped_boundaries_redact_all_token_shapes_across_chunks();
        raw_backslashes_before_tokens_do_not_disable_redaction_across_chunks();
        unicode_escapes_inside_tokens_are_redacted_across_chunks();
        bearer_and_credential_url_values_split_across_chunks_are_hidden();
        token_markers_redact_at_minimum_body_lengths_across_chunks();
        token_split_across_the_session_chunk_boundary_is_redacted()?;
        Ok(())
    }

    fn token_markers_split_across_chunks_redact_only_at_word_boundaries() {
        let mut redactor = Redactor::new([]).expect("empty secret set");
        let output = joined(
            &mut redactor,
            &[b"xsk-embedded sk", b"-abcdefghijklmnopqrst"],
        );
        assert_eq!(output, b"xsk-embedded [REDACTED]");
    }

    fn json_escaped_boundaries_redact_all_token_shapes_across_chunks() {
        for (marker, kind) in TOKEN_MARKERS {
            let marker = marker_pattern(marker);
            let body = match kind {
                TokenKind::AwsAccessKey => "0123456789ABCDEF",
                _ => "abcdefghijklmnopqrst",
            };
            for prefix in [
                " ", "\n", r"\n", r"\t", r"\r", r#"\""#, r"\u0020", r"\u000a",
            ] {
                let input = format!("{prefix}{}{body}!", String::from_utf8_lossy(marker));
                let expected = if matches!(kind, TokenKind::Bearer) {
                    format!("{prefix}Bearer [REDACTED]!")
                } else {
                    format!("{prefix}[REDACTED]!")
                };
                for split in 0..=input.len() {
                    let mut redactor = Redactor::new([]).expect("empty secret set");
                    assert_eq!(
                        joined(
                            &mut redactor,
                            &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                        ),
                        expected.as_bytes(),
                        "prefix {prefix:?}, marker {marker:?}, split {split}"
                    );
                }
            }
            for prefix in ["x", r"\u0061", r"\\n"] {
                let input = format!("{prefix}{}{body}!", String::from_utf8_lossy(marker));
                let mut redactor = Redactor::new([]).expect("empty secret set");
                assert_eq!(joined(&mut redactor, &[input.as_bytes()]), input.as_bytes());
            }
        }
    }

    fn raw_backslashes_before_tokens_do_not_disable_redaction_across_chunks() {
        for (marker, kind) in TOKEN_MARKERS {
            let marker = marker_pattern(marker);
            let bodies: &[&str] = match kind {
                TokenKind::AwsAccessKey => &["0123456789ABCDEF", r"01234567\u00389ABCDEF"],
                _ => &["abcdefghijklmnopqrst", r"abcdefghij\u006blmnopqrst"],
            };
            for body in bodies {
                for prefix in [r"\", r"\\", r"\\\"] {
                    let input = format!("{prefix}{}{body}!", String::from_utf8_lossy(marker));
                    let expected = if matches!(kind, TokenKind::Bearer) {
                        format!("{prefix}Bearer [REDACTED]!")
                    } else {
                        format!("{prefix}[REDACTED]!")
                    };
                    for split in 0..=input.len() {
                        let mut redactor = Redactor::new([]).expect("empty secret set");
                        assert_eq!(
                            joined(
                                &mut redactor,
                                &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                            ),
                            expected.as_bytes(),
                            "prefix {prefix:?}, marker {marker:?}, body {body:?}, split {split}"
                        );
                    }
                    let chunks: Vec<&[u8]> = input.as_bytes().chunks(1).collect();
                    let mut redactor = Redactor::new([]).expect("empty secret set");
                    assert_eq!(joined(&mut redactor, &chunks), expected.as_bytes());
                }
            }
        }
    }

    fn unicode_escapes_inside_tokens_are_redacted_across_chunks() {
        for (marker, kind) in TOKEN_MARKERS {
            let marker = marker_pattern(marker);
            let body = match kind {
                TokenKind::AwsAccessKey => r"01234567\u00389ABCDEF",
                _ => r"abcdefghij\u006blmnopqrst",
            };
            let input = format!("{}{}!", String::from_utf8_lossy(marker), body);
            let expected = if matches!(kind, TokenKind::Bearer) {
                "Bearer [REDACTED]!"
            } else {
                "[REDACTED]!"
            };
            for split in 0..=input.len() {
                let mut redactor = Redactor::new([]).expect("empty secret set");
                assert_eq!(
                    joined(
                        &mut redactor,
                        &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                    ),
                    expected.as_bytes(),
                    "marker {marker:?}, split {split}"
                );
            }
            let first = marker[0];
            for prefix in ["", r"\", r"\\"] {
                let input = format!(
                    "{prefix}\\u{first:04x}{}{}!",
                    String::from_utf8_lossy(&marker[1..]),
                    body
                );
                let expected = if prefix == r"\" {
                    // The Unicode marker's opening backslash is itself escaped.
                    input.clone()
                } else {
                    format!("{prefix}{expected}")
                };
                for split in 0..=input.len() {
                    let mut redactor = Redactor::new([]).expect("empty secret set");
                    assert_eq!(
                        joined(
                            &mut redactor,
                            &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                        ),
                        expected.as_bytes(),
                        "prefix {prefix:?}, escaped marker {marker:?}, split {split}"
                    );
                }
                let chunks: Vec<&[u8]> = input.as_bytes().chunks(1).collect();
                let mut redactor = Redactor::new([]).expect("empty secret set");
                assert_eq!(joined(&mut redactor, &chunks), expected.as_bytes());
            }
        }
        for input in [r"sk-short\u0061!", r"sk-short\u0021!", r"sk-short\u00xz!"] {
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(joined(&mut redactor, &[input.as_bytes()]), input.as_bytes());
        }
    }

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
    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    fn credential_assignments_are_redacted_across_chunk_boundaries() {
        let cases: &[(&str, &str)] = &[
            ("password=hunter2 next", "password=[REDACTED] next"),
            ("PASSWORD: hunter2\n", "PASSWORD: [REDACTED]\n"),
            ("passwd = 'x y' end", "passwd = '[REDACTED]' end"),
            ("pass:hunter2,", "pass:[REDACTED],"),
            (
                "db_password=\"correct horse\"",
                "db_password=\"[REDACTED]\"",
            ),
            ("token=abc123&next=1", "token=[REDACTED]&next=1"),
            ("x-auth-token=abc", "x-auth-token=[REDACTED]"),
            ("secret: \"a\\\"b\"}", "secret: \"[REDACTED]\"}"),
            (
                "secret: \"'LEAK_SENTINEL\" end",
                "secret: \"[REDACTED]\" end",
            ),
            ("client_secret=xyz", "client_secret=[REDACTED]"),
            ("api_key=sk_live_123", "api_key=[REDACTED]"),
            ("api-key: x123 done", "api-key: [REDACTED] done"),
            ("apikey=abc", "apikey=[REDACTED]"),
            ("access_key=AKX", "access_key=[REDACTED]"),
            ("auth=abc", "auth=[REDACTED]"),
            ("msg\\npassword=hunter2", "msg\\npassword=[REDACTED]"),
            (
                r#"{\"password\":\"hunter2\"}"#,
                r#"{\"password\":\"[REDACTED]\"}"#,
            ),
            (
                r#"{"content":"{\"password\":\"abc\\\"LEAK_SENTINEL\"}"}"#,
                r#"{"content":"{\"password\":\"[REDACTED]\"}"}"#,
            ),
            (
                r#"{\"password\":\"abc\\\\\\\"LEAK_SENTINEL\"}"#,
                r#"{\"password\":\"[REDACTED]\"}"#,
            ),
            (
                r#"{\"password\":\"abc\\\\\"} end"#,
                r#"{\"password\":\"[REDACTED]"#,
            ),
            (
                r#"\"{\\\"password\\\":\\\"abc\\\\\\\"LEAK_SENTINEL\\\"}\""#,
                r#"\"{\\\"password\\\":\\\"[REDACTED]\\\"}\""#,
            ),
            (
                "Authorization: Basic dXNlcjpwYXNz\n",
                "Authorization: Basic [REDACTED]\n",
            ),
            (
                "authorization: bearer abc.def\n",
                "authorization: bearer [REDACTED]\n",
            ),
            (
                "Authorization: Digest abc def\n",
                "Authorization: [REDACTED]\n",
            ),
            ("Authorization: Basic", "Authorization: [REDACTED]"),
        ];
        for (input, expected) in cases {
            assert_eq!(
                known_shapes(input.as_bytes()).expect("bounded input"),
                expected.as_bytes()
            );
            for split in 0..=input.len() {
                let mut redactor = Redactor::new([]).expect("empty secret set");
                assert_eq!(
                    joined(
                        &mut redactor,
                        &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                    ),
                    expected.as_bytes(),
                    "{input:?} split {split}"
                );
            }
            let chunks: Vec<&[u8]> = input.as_bytes().chunks(1).collect();
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(
                joined(&mut redactor, &chunks),
                expected.as_bytes(),
                "{input:?} byte by byte"
            );
            assert!(redactor.has_redaction(), "{input:?}");
        }

        // Encode the whole log repeatedly so the expected source span follows
        // the same encoding as the credential, including its closing delimiter.
        for quote in ['\'', '"'] {
            for value in [
                format!("abc\\{quote}LEAK_SENTINEL"),
                "LEAK_SENTINEL\\\\".into(),
            ] {
                let mut input = format!("password={quote}{value}{quote} end");
                let ambiguous_close = value.ends_with("\\\\");
                let mut expected = if ambiguous_close {
                    format!("password={quote}[REDACTED]")
                } else {
                    format!("password={quote}[REDACTED]{quote} end")
                };
                for depth in 0..=2 {
                    for split in 0..=input.len() {
                        let mut redactor = Redactor::new([]).expect("empty secret set");
                        assert_eq!(
                            joined(
                                &mut redactor,
                                &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                            ),
                            expected.as_bytes(),
                            "depth {depth}, split {split}, {input:?}"
                        );
                    }
                    let mut redactor = Redactor::new([]).expect("empty secret set");
                    assert_eq!(
                        joined(
                            &mut redactor,
                            &input.as_bytes().chunks(1).collect::<Vec<_>>()
                        ),
                        expected.as_bytes(),
                        "depth {depth}, bytewise, {input:?}"
                    );
                    input = serde_json::to_string(&input).expect("encode input");
                    expected = serde_json::to_string(&expected).expect("encode expected");
                    if ambiguous_close {
                        expected.pop();
                    }
                }
            }
        }
    }

    #[test]
    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    fn quoted_credentials_are_line_bounded_and_hide_leading_bytes() {
        let cases = [
            (
                r#"{"msg": "escaped \" quote", "password": "val\\"} end"#,
                r#"{"msg": "escaped \" quote", "password": "[REDACTED]"#,
            ),
            (
                r#"Today is a 6" pipe and password="val\\" end"#,
                r#"Today is a 6" pipe and password="[REDACTED]"#,
            ),
            (
                r#""{\"a\": \"hello \\\" world\", \"password\": \"val\\\\\"} end""#,
                r#""{\"a\": \"hello \\\" world\", \"password\": \"[REDACTED]"#,
            ),
            (r#"password="\\" end"#, r#"password="[REDACTED]"#),
            (r#"password="  " end"#, r#"password="[REDACTED]" end"#),
            (r#"password="\secret""#, r#"password="[REDACTED]""#),
            (r#"password=" secret""#, r#"password="[REDACTED]""#),
            (
                "\"password=\\\"\\\\\\\" end\"\nfollowing line",
                "\"password=\\\"[REDACTED]\nfollowing line",
            ),
            (
                r#"{"log":"password='abc\\'LEAK_SENTINEL' end"}"#,
                r#"{"log":"password='[REDACTED]' end"}"#,
            ),
            (
                "password=\" LEAK_SENTINEL\nfollowing line",
                "password=\"[REDACTED]\nfollowing line",
            ),
            (
                "password=\"\\LEAK_SENTINEL\r\nfollowing line",
                "password=\"[REDACTED]\r\nfollowing line",
            ),
            (
                "password=\"abc\\\"LEAK_SENTINEL\nfollowing line",
                "password=\"[REDACTED]\nfollowing line",
            ),
            (
                "password=\"safe\" token=LEAK_SENTINEL\nfollowing line",
                "password=\"[REDACTED]\" token=[REDACTED]\nfollowing line",
            ),
            (
                "password=\"safe\" same line\nfollowing line",
                "password=\"[REDACTED]\" same line\nfollowing line",
            ),
            (
                r#"password="abc\\" token=LEAK_SENTINEL"#,
                r#"password="[REDACTED]"#,
            ),
        ];
        for (input, expected) in cases {
            let bytes = input.as_bytes();
            for split in 0..=bytes.len() {
                let mut redactor = Redactor::new([]).expect("empty secret set");
                assert_eq!(
                    joined(&mut redactor, &[&bytes[..split], &bytes[split..]]),
                    expected.as_bytes(),
                    "{input:?}, split {split}"
                );
                assert!(redactor.has_redaction(), "{input:?}, split {split}");
            }
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(
                joined(&mut redactor, &bytes.chunks(1).collect::<Vec<_>>()),
                expected.as_bytes(),
                "{input:?}, bytewise"
            );
        }
    }

    #[test]
    fn ordinary_prose_with_credential_words_is_preserved() {
        let input = "token budget is 5; auth failed; oauth=1; compass=north; max_tokens=4096; passwords=3; tokenizer: fast; bypass: on";
        for split in 0..=input.len() {
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(
                joined(
                    &mut redactor,
                    &[&input.as_bytes()[..split], &input.as_bytes()[split..]]
                ),
                input.as_bytes(),
                "split {split}"
            );
        }
        let mut redactor = Redactor::new([]).expect("empty secret set");
        assert_eq!(joined(&mut redactor, &[input.as_bytes()]), input.as_bytes());
        assert!(!redactor.has_redaction());
    }

    #[test]
    fn bounded_line_redacts_credential_assignments() {
        assert_eq!(
            bounded_line("unclassified; password=fixture-secret", 512),
            "unclassified; password=[REDACTED]"
        );
    }

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

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    fn checked_frames(redactor: &mut Redactor, chunks: &[&[u8]]) -> Vec<u8> {
        let mut output = Vec::new();
        for chunk in chunks {
            for frame in redactor.filter(chunk).expect("filter succeeds") {
                assert!(frame.len() <= FRAME_LIMIT);
                output.extend(frame);
            }
        }
        for frame in redactor.finish().expect("finish succeeds") {
            assert!(frame.len() <= FRAME_LIMIT);
            output.extend(frame);
        }
        output
    }

    #[test]
    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    fn overlapping_literal_does_not_change_assignment_boundaries() {
        let input = b"password=\"X\\\"LEAK_SENTINEL\" end\nnext";
        let expected = b"password=\"[REDACTED]\" end\nnext";
        for split in 0..=input.len() {
            let mut redactor = Redactor::new([b"X\\".to_vec()]).expect("secret");
            assert_eq!(
                checked_frames(&mut redactor, &[&input[..split], &input[split..]]),
                expected,
                "split {split}"
            );
        }
        for width in [1, 256, 8192, FRAME_LIMIT] {
            let mut redactor = Redactor::new([b"X\\".to_vec()]).expect("secret");
            assert_eq!(
                checked_frames(&mut redactor, &input.chunks(width).collect::<Vec<_>>()),
                expected,
                "width {width}"
            );
        }
        // An encoded Bearer prefix is canonicalised only when that cannot
        // undo a literal redaction within the original marker.
        let input = br"\u0042earer abcdefghijklmnopqrst!";
        for split in 0..=input.len() {
            let mut redactor = Redactor::new([br"\u0042".to_vec()]).expect("secret");
            assert_eq!(
                checked_frames(&mut redactor, &[&input[..split], &input[split..]]),
                b"[REDACTED]earer [REDACTED]!",
                "Bearer split {split}"
            );
        }
    }

    #[test]
    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    fn expansions_and_long_delimiters_are_chunk_invariant_and_framed() {
        let short = b"x x x tail";
        let short_expected = b"[REDACTED] [REDACTED] [REDACTED] tail";
        for split in 0..=short.len() {
            let mut redactor = Redactor::new([b"x".to_vec()]).expect("secret");
            assert_eq!(
                checked_frames(&mut redactor, &[&short[..split], &short[split..]]),
                short_expected
            );
        }
        let expanded = b"x ".repeat(FRAME_LIMIT / 4 + 1);
        let expected = b"[REDACTED] ".repeat(FRAME_LIMIT / 4 + 1);
        for width in [1, 256, 8192, FRAME_LIMIT] {
            let mut redactor = Redactor::new([b"x".to_vec()]).expect("secret");
            assert_eq!(
                checked_frames(&mut redactor, &expanded.chunks(width).collect::<Vec<_>>()),
                expected,
                "width {width}"
            );
        }
        let empty_escaped = br#"password=\"\" end"#;
        let empty_expected = br#"password=\"[REDACTED]\" end"#;
        for split in 0..=empty_escaped.len() {
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(
                checked_frames(
                    &mut redactor,
                    &[&empty_escaped[..split], &empty_escaped[split..]]
                ),
                empty_expected,
                "empty split {split}"
            );
        }
        let sample = b"password=\\\\\\\\\\\\\\\"hidden\\\\\\\\\\\\\\\" end\nnext";
        let sample_expected = b"password=\\\\\\\\\\\\\\\"[REDACTED]\\\\\\\\\\\\\\\" end\nnext";
        for split in 0..=sample.len() {
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(
                checked_frames(&mut redactor, &[&sample[..split], &sample[split..]]),
                sample_expected,
                "split {split}"
            );
        }
        let slashes = vec![b'\\'; FRAME_LIMIT - 1];
        let mut input = b"password=".to_vec();
        input.extend(&slashes);
        input.extend(b"\"hidden");
        input.extend(&slashes);
        input.extend(b"\" end\nnext");
        let mut expected = b"password=".to_vec();
        expected.extend(&slashes);
        expected.extend(b"\"[REDACTED]");
        expected.extend(&slashes);
        expected.extend(b"\" end\nnext");
        for width in [1, 256, 8192, FRAME_LIMIT] {
            let mut redactor = Redactor::new([]).expect("empty secret set");
            assert_eq!(
                checked_frames(&mut redactor, &input.chunks(width).collect::<Vec<_>>()),
                expected,
                "width {width}"
            );
        }
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

    #[test]
    fn settlement_scrub_removes_tokens_from_session_text_and_keeps_binaries()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let codex = root.join("candidate-0/sessions/2026/10/24");
        std::fs::create_dir_all(&codex)?;
        let rollout = codex.join("rollout-2026-10-24T00-00-00-thread.jsonl");
        std::fs::write(
            &rollout,
            format!(
                "{{\"type\":\"response_item\",\"payload\":{{\"content\":\"printed {token} and kept\"}}}}\n"
            ),
        )?;
        let claude = root.join("candidate-0/claude-config/projects/key");
        std::fs::create_dir_all(&claude)?;
        let session = claude.join("session.jsonl");
        std::fs::write(
            &session,
            format!("{{\"message\":{{\"content\":\"{token}\"}},\"keep\":\"claude\"}}\n"),
        )?;
        let history = root.join("candidate-0/history.jsonl");
        std::fs::write(&history, format!("{token}\n"))?;
        let database = root.join("candidate-0/thread_history_1.sqlite");
        std::fs::write(&database, token)?;

        assert_eq!(scrub_session_history(root)?, 3);
        for path in [&rollout, &session, &history] {
            let text = std::fs::read_to_string(path)?;
            assert!(!text.contains(token), "plaintext survived in {path:?}");
            assert!(text.contains("[REDACTED]"));
        }
        assert!(std::fs::read_to_string(&rollout)?.contains("and kept"));
        assert_eq!(std::fs::read(&database)?, token.as_bytes());
        serde_json::from_str::<serde_json::Value>(std::fs::read_to_string(&rollout)?.trim())?;
        Ok(())
    }

    #[test]
    fn session_scrub_reaches_relocated_text_and_preserves_binary_stores()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let candidate = root.join("candidate-0");
        let texts = [
            ".cursor/chats/project/session/messages.json",
            "data/opencode/log/session.log",
            "data/opencode/storage/message/session/message.json",
            "data/opencode/snapshot/project/HEAD",
            "data/opencode/shell/session.sh",
        ];
        for name in texts {
            let path = candidate.join(name);
            std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
            // Split a multibyte character across reads and retain ANSI text.
            let prefix = " ".repeat(SESSION_CHUNK - 1);
            std::fs::write(path, format!("{prefix}🦀 \x1b[32m {token}\x1b[0m\n"))?;
        }
        let binaries = [
            ".cursor/chats/project/session/store.db",
            ".cursor/chats/project/session/store.db-wal",
            "data/opencode/opencode.db",
            "data/opencode/opencode.db-shm",
            "data/opencode/snapshot/project/blob",
        ];
        for name in binaries {
            let bytes = if name.ends_with("blob") {
                format!("{token}{}\0binary", " ".repeat(SESSION_CHUNK)).into_bytes()
            } else {
                token.as_bytes().to_vec()
            };
            std::fs::write(candidate.join(name), bytes)?;
        }
        assert_eq!(scrub_session_history(root)?, texts.len());
        for name in texts {
            assert_eq!(
                std::fs::read_to_string(candidate.join(name))?,
                format!(
                    "{}🦀 \x1b[32m [REDACTED]\x1b[0m\n",
                    " ".repeat(SESSION_CHUNK - 1)
                )
            );
        }
        for name in binaries {
            assert!(std::fs::read(candidate.join(name))?.starts_with(token.as_bytes()));
        }
        Ok(())
    }

    #[test]
    fn session_scrub_skips_links_and_leaves_clean_transcripts_alone()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let outside = root.join("outside.jsonl");
        std::fs::write(&outside, token)?;
        let sessions = root.join("candidate-0/sessions");
        std::fs::create_dir_all(&sessions)?;
        std::os::unix::fs::symlink(&outside, sessions.join("linked.jsonl"))?;
        let clean = sessions.join("clean.jsonl");
        std::fs::write(&clean, b"{\"content\":\"ordinary\"}\n")?;
        let inode = std::fs::metadata(&clean)?.ino();

        assert_eq!(scrub_session_history(root)?, 0);
        assert_eq!(std::fs::read(&outside)?, token.as_bytes());
        assert_eq!(std::fs::read(&clean)?, b"{\"content\":\"ordinary\"}\n");
        // A clean transcript is never staged or replaced, so its inode survives.
        assert_eq!(std::fs::metadata(&clean)?.ino(), inode);
        Ok(())
    }

    #[test]
    fn session_scrub_keeps_going_after_an_overlong_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        // A `://` run longer than the frame bound cannot be filtered, so the file is refused.
        let sessions = root.join("candidate-0/sessions");
        std::fs::create_dir_all(&sessions)?;
        let overlong = format!("https://{}\n", "a".repeat(FRAME_LIMIT + 1));
        std::fs::write(sessions.join("overlong.jsonl"), overlong)?;
        // Layout order scrubs `sessions` before `claude-config`, so the good
        // file comes after the refused one.
        let good = root.join("candidate-0/claude-config/projects/key/session.jsonl");
        std::fs::create_dir_all(good.parent().ok_or("parent")?)?;
        std::fs::write(&good, format!("{{\"message\":\"{token}\"}}\n"))?;

        assert!(scrub_session_history(root).is_err());
        assert!(!std::fs::read_to_string(&good)?.contains(token));
        assert!(std::fs::read_to_string(&good)?.contains("[REDACTED]"));
        Ok(())
    }

    #[test]
    fn session_scrub_keeps_going_past_a_tree_over_the_depth_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let mut deep = root.join("candidate-0/sessions");
        for level in 0..=SESSION_DEPTH {
            deep.push(format!("d{level}"));
        }
        std::fs::create_dir_all(&deep)?;
        std::fs::write(deep.join("deep.jsonl"), format!("{token}\n"))?;
        let good = root.join("candidate-0/claude-config/projects/key/session.jsonl");
        std::fs::create_dir_all(good.parent().ok_or("parent")?)?;
        std::fs::write(&good, format!("{{\"message\":\"{token}\"}}\n"))?;

        assert!(scrub_session_history(root).is_err());
        assert!(!std::fs::read_to_string(&good)?.contains(token));
        Ok(())
    }

    #[test]
    fn session_scrub_refuses_more_candidates_than_its_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        for index in 0..=SESSION_CANDIDATES {
            std::fs::create_dir_all(root.join(format!("candidate-{index}")))?;
        }
        assert!(scrub_session_history(root).is_err());
        Ok(())
    }

    fn token_split_across_the_session_chunk_boundary_is_redacted()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        // The token starts ten bytes before the first read ends, so it spans two reads.
        let mut text = " ".repeat(SESSION_CHUNK - 10);
        text.push_str(token);
        text.push_str("\n{\"keep\":\"large\"}\n");
        assert!(text.len() > SESSION_CHUNK);
        let sessions = root.join("candidate-0/sessions");
        std::fs::create_dir_all(&sessions)?;
        let transcript = sessions.join("large.jsonl");
        std::fs::write(&transcript, &text)?;

        assert_eq!(scrub_session_history(root)?, 1);
        let rewritten = std::fs::read_to_string(&transcript)?;
        assert!(!rewritten.contains(token));
        assert!(rewritten.contains("[REDACTED]"));
        assert!(rewritten.ends_with("{\"keep\":\"large\"}\n"));
        Ok(())
    }

    #[test]
    fn session_scrub_reaches_tool_result_text_under_projects()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let root = temp.path();
        let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let results = root.join("candidate-0/claude-config/projects/key/session/tool-results");
        std::fs::create_dir_all(&results)?;
        let text = results.join("toolu_text.txt");
        std::fs::write(&text, format!("printed {token}\n"))?;
        let json = results.join("toolu_json.json");
        std::fs::write(&json, format!("{{\"stdout\":\"{token}\"}}\n"))?;
        let binary = results.join("toolu_blob.bin");
        std::fs::write(&binary, token)?;
        let outside = root.join("candidate-0/sessions/notes.txt");
        std::fs::create_dir_all(outside.parent().ok_or("parent")?)?;
        std::fs::write(&outside, token)?;

        assert_eq!(scrub_session_history(root)?, 2);
        for path in [&text, &json] {
            let body = std::fs::read_to_string(path)?;
            assert!(!body.contains(token), "plaintext survived in {path:?}");
            assert!(body.contains("[REDACTED]"));
        }
        assert_eq!(std::fs::read(&binary)?, token.as_bytes());
        assert_eq!(std::fs::read(&outside)?, token.as_bytes());
        Ok(())
    }
}
