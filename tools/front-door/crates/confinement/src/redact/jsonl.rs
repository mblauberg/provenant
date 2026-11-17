//! JSON syntax owns the delimiters; the plain redactor sees decoded values only.
use super::source::{self, SourceMap};
use super::{
    ASSIGNMENT_KEYS, ASSIGNMENT_RUN, FrameEmitter, MAX_SECRET_LENGTH, REDACTED, Redactor,
    RedactorError,
};
use provenant_os::exec::{CaptureFilter, FRAME_LIMIT, FilterError, PROVIDER_LINE_LIMIT};

const STRING_PIECE: usize = 4096;
const MAX_DEPTH: usize = 128;

#[derive(Clone, Copy, PartialEq)]
enum Position {
    Root,
    Done,
    ArrayFirst,
    ArrayValue,
    ArrayComma,
    ObjectFirst,
    ObjectKey,
    ObjectColon,
    ObjectValue,
    ObjectComma,
}

struct Context {
    position: Position,
    sensitive: bool,
}

pub(super) fn credential_key(tail: &[u8]) -> bool {
    ASSIGNMENT_KEYS.iter().any(|(key, _)| {
        let Some(start) = tail.len().checked_sub(key.len()) else {
            return false;
        };
        (start == 0 || !tail[start - 1].is_ascii_alphanumeric()) && tail.ends_with(key)
    })
}

struct StringToken {
    key: bool,
    masked: bool,
    tail: Vec<u8>,
    decoded: Vec<u8>,
    escape: Vec<u8>,
    utf8: Vec<u8>,
    key_source: Vec<u8>,
    key_overflow: bool,
    source: SourceMap,
    redaction_run: bool,
}

struct Scalar {
    bytes: Vec<u8>,
    // JSON number grammar needs constant state even for arbitrarily long tokens.
    number: Number,
    streamed: bool,
    masked: bool,
}

#[derive(Clone, Copy)]
enum Number {
    Start,
    Sign,
    Zero,
    Integer,
    Dot,
    Fraction,
    Exponent,
    ExponentSign,
    ExponentDigits,
    Invalid,
}
impl Number {
    fn step(self, b: u8) -> Self {
        use Number::*;
        match (self, b) {
            (Start, b'-') => Sign,
            (Start | Sign, b'0') => Zero,
            (Start | Sign, b'1'..=b'9') | (Integer, b'0'..=b'9') => Integer,
            (Zero | Integer, b'.') => Dot,
            (Dot | Fraction, b'0'..=b'9') => Fraction,
            (Zero | Integer | Fraction, b'e' | b'E') => Exponent,
            (Exponent, b'+' | b'-') => ExponentSign,
            (Exponent | ExponentSign | ExponentDigits, b'0'..=b'9') => ExponentDigits,
            _ => Invalid,
        }
    }
    fn complete(self) -> bool {
        matches!(
            self,
            Self::Zero | Self::Integer | Self::Fraction | Self::ExponentDigits
        )
    }
}

/// A bounded streaming tokenizer. Sink frames, not total output per call, are
/// bounded by CaptureFilter. Values stream independently of provider line size.
/// Keys are scanned as decoded strings and masked as a whole when sensitive.
pub struct JsonlRedactor {
    secrets: Vec<Vec<u8>>,
    inner: Redactor,
    positions: Vec<Context>,
    string: Option<StringToken>,
    scalar: Option<Scalar>,
    started: bool,
    plain: bool,
    discard: bool,
    source_line_bytes: usize,
    provider_limit: bool,
    mask_value: bool,
    scalar_secret: bool,
    has_redaction: bool,
    invalid: bool,
    failed: bool,
}

impl JsonlRedactor {
    /// Maximum output of a source-capped provider line. Unchanged spans retain
    /// source encoding; masks coalesce. The worst per-byte growth is a one-byte
    /// scalar becoming a quoted marker. Replay uses this bound only after the
    /// provider filter has enforced the source cap and marked invalid frames.
    pub const OUTPUT_LINE_LIMIT: usize = PROVIDER_LINE_LIMIT * (REDACTED.len() + 2);

    pub fn new(secrets: impl IntoIterator<Item = Vec<u8>>) -> Result<Self, RedactorError> {
        let secrets: Vec<Vec<u8>> = secrets.into_iter().collect();
        if secrets
            .iter()
            .any(|secret| std::str::from_utf8(secret).is_err())
        {
            return Err(RedactorError::InvalidUtf8Secret);
        }
        let inner = Redactor::new(secrets.clone())?;
        // Long numeric scalars cannot be retracted after their prefix streams.
        // Conservatively mask them if a literal could occur in a JSON scalar.
        let scalar_secret = secrets
            .iter()
            .any(|s| s.iter().all(|b| b"-+0123456789.eE".contains(b)));
        Ok(Self {
            secrets,
            inner,
            positions: vec![Context {
                position: Position::Root,
                sensitive: false,
            }],
            string: None,
            scalar: None,
            started: false,
            plain: false,
            discard: false,
            source_line_bytes: 0,
            provider_limit: false,
            mask_value: false,
            scalar_secret,
            has_redaction: false,
            invalid: false,
            failed: false,
        })
    }

    /// Apply the provider protocol's source-line limit before redaction can shrink it.
    pub fn provider(secrets: impl IntoIterator<Item = Vec<u8>>) -> Result<Self, RedactorError> {
        let mut redactor = Self::new(secrets)?;
        redactor.provider_limit = true;
        Ok(redactor)
    }

    pub fn has_redaction(&self) -> bool {
        self.has_redaction || self.inner.has_redaction()
    }

    fn reset_inner(&mut self) -> Result<(), FilterError> {
        self.has_redaction |= self.inner.has_redaction();
        self.inner = Redactor::new(self.secrets.clone()).map_err(|_| FilterError)?;
        Ok(())
    }

    fn flush_string(
        &mut self,
        token: &mut StringToken,
        out: &mut FrameEmitter<'_>,
    ) -> Result<(), FilterError> {
        if token.key || token.masked {
            token.decoded.clear();
            return Ok(());
        }
        self.inner
            .filter_decisions(&token.decoded, &mut |_, count, masked| {
                token
                    .source
                    .decide(count, masked, &mut |byte, count, masked| {
                        source::emit(byte, count, masked, &mut token.redaction_run, out);
                        Ok(())
                    })
            })?;
        token.decoded.clear();
        Ok(())
    }

    fn finish_string(
        &mut self,
        token: &mut StringToken,
        out: &mut FrameEmitter<'_>,
    ) -> Result<(), FilterError> {
        self.flush_string(token, out)?;
        if token.key && token.key_overflow {
            self.inner.finish(&mut |bytes| out.push_bytes(bytes))?;
        } else if !token.key && !token.masked {
            self.inner.finish_decisions(&mut |_, count, masked| {
                token
                    .source
                    .decide(count, masked, &mut |byte, count, masked| {
                        source::emit(byte, count, masked, &mut token.redaction_run, out);
                        Ok(())
                    })
            })?;
            token.source.finish()?;
        } else {
            self.inner.finish(&mut |_| {})?;
        }
        Ok(())
    }

    pub(super) fn valid(&self) -> bool {
        !self.invalid
    }

    fn decoded(
        &mut self,
        token: &mut StringToken,
        bytes: &[u8],
        original: &[u8],
        out: &mut FrameEmitter<'_>,
    ) -> Result<(), FilterError> {
        if token.key {
            if !token.key_overflow {
                token.decoded.extend_from_slice(bytes);
            }
            for &b in bytes {
                if token.tail.len() == ASSIGNMENT_RUN {
                    token.tail.remove(0);
                }
                token.tail.push(b.to_ascii_lowercase());
            }
        } else if !token.masked {
            token.source.push(original, bytes.len())?;
            token.decoded.extend_from_slice(bytes);
            if token.decoded.len() >= STRING_PIECE {
                self.flush_string(token, out)?;
            }
        }
        Ok(())
    }

    fn malformed(
        &mut self,
        remainder: &[u8],
        sensitive: bool,
        out: &mut FrameEmitter<'_>,
    ) -> Result<(), FilterError> {
        // The remainder may contain escaped keys and literals. Mask it all,
        // including across chunks, rather than guessing how invalid JSON decodes.
        let _ = (remainder, sensitive);
        out.push_bytes(b"\0[REDACTED]");
        self.discard = true;
        self.invalid = true;
        self.has_redaction = true;
        Ok(())
    }

    fn plain_bytes(&mut self, bytes: &[u8], out: &mut FrameEmitter<'_>) -> Result<(), FilterError> {
        self.invalid = true;
        self.inner.filter(bytes, &mut |b| out.push_bytes(b))
    }

    fn string_byte(
        &mut self,
        b: u8,
        mut token: StringToken,
        out: &mut FrameEmitter<'_>,
    ) -> Result<(), FilterError> {
        if token.key && !(token.escape.is_empty() && b == b'"') {
            if !token.key_overflow && token.key_source.len() == MAX_SECRET_LENGTH {
                token.key_overflow = true;
                token.decoded.clear();
                self.inner
                    .filter(&token.key_source, &mut |bytes| out.push_bytes(bytes))?;
                token.key_source.clear();
            }
            if token.key_overflow {
                self.inner
                    .filter(&[b], &mut |bytes| out.push_bytes(bytes))?;
            } else {
                token.key_source.push(b);
            }
        }
        if token.escape.is_empty() && (b >= 0x80 || !token.utf8.is_empty()) {
            token.utf8.push(b);
            match std::str::from_utf8(&token.utf8) {
                Ok(_) => token.utf8.clear(),
                Err(e) if e.error_len().is_none() && token.utf8.len() < 4 => {}
                Err(_) => {
                    self.finish_string(&mut token, out)?;
                    self.reset_inner()?;
                    return self.malformed(&[b], token.masked || credential_key(&token.tail), out);
                }
            }
        }
        if !token.escape.is_empty() {
            token.escape.push(b);
            let e = &token.escape;
            let needed = if e.get(1) == Some(&b'u') {
                if e.len() < 6 {
                    6
                } else {
                    let high =
                        u16::from_str_radix(std::str::from_utf8(&e[2..6]).unwrap_or(""), 16).ok();
                    if high.is_some_and(|n| (0xd800..=0xdbff).contains(&n)) {
                        12
                    } else {
                        6
                    }
                }
            } else {
                2
            };
            if e.len() == needed {
                let mut quoted = vec![b'"'];
                quoted.extend_from_slice(e);
                quoted.push(b'"');
                match serde_json::from_slice::<String>(&quoted) {
                    Ok(decoded) => {
                        let original = token.escape.clone();
                        self.decoded(&mut token, decoded.as_bytes(), &original, out)?;
                        token.escape.clear();
                    }
                    Err(_) => {
                        self.finish_string(&mut token, out)?;
                        self.reset_inner()?;
                        return self.malformed(
                            &quoted[1..quoted.len() - 1],
                            token.masked || credential_key(&token.tail),
                            out,
                        );
                    }
                }
            }
        } else if b == b'\\' {
            token.escape.push(b);
        } else if b == b'"' {
            if token.key {
                self.mask_value = credential_key(&token.tail);
                if token.key_overflow {
                    self.inner.finish(&mut |bytes| out.push_bytes(bytes))?;
                } else {
                    self.inner.filter(&token.decoded, &mut |_| {})?;
                    self.inner.finish(&mut |_| {})?;
                    if self.inner.has_redaction() {
                        out.push_bytes(REDACTED);
                        self.has_redaction = true;
                    } else {
                        out.push_bytes(&token.key_source);
                    }
                }
                self.reset_inner()?;
                out.push_bytes(b"\"");
                self.positions.last_mut().ok_or(FilterError)?.position = Position::ObjectColon;
            } else {
                self.finish_string(&mut token, out)?;
                self.reset_inner()?;
                out.push_bytes(b"\"");
            }
            return Ok(());
        } else if b < 0x20 {
            self.finish_string(&mut token, out)?;
            self.reset_inner()?;
            return self.malformed(&[b], token.masked || credential_key(&token.tail), out);
        } else {
            self.decoded(&mut token, &[b], &[b], out)?;
        }
        self.string = Some(token);
        Ok(())
    }

    fn finish_scalar(
        &mut self,
        scalar: Scalar,
        out: &mut FrameEmitter<'_>,
    ) -> Result<(), FilterError> {
        let valid = scalar.number.complete()
            || (!scalar.streamed
                && matches!(scalar.bytes.as_slice(), b"true" | b"false" | b"null"));
        if !valid {
            if self.positions.len() == 1
                && self.positions[0].position == Position::Done
                && !scalar.streamed
            {
                self.plain = true;
                return self.plain_bytes(&scalar.bytes, out);
            }
            return self.malformed(&scalar.bytes, scalar.masked, out);
        }
        if !scalar.streamed {
            let masked = scalar.masked
                || self
                    .secrets
                    .iter()
                    .any(|s| scalar.bytes.windows(s.len()).any(|w| w == s));
            if masked {
                out.push_bytes(b"\"[REDACTED]\"");
                self.has_redaction = true;
            } else {
                out.push_bytes(&scalar.bytes);
            }
        }
        Ok(())
    }

    fn byte(&mut self, b: u8, out: &mut FrameEmitter<'_>) -> Result<(), FilterError> {
        if self.plain {
            return self.plain_bytes(&[b], out);
        }
        if let Some(token) = self.string.take() {
            return self.string_byte(b, token, out);
        }
        if let Some(mut scalar) = self.scalar.take() {
            if b.is_ascii_whitespace() || matches!(b, b',' | b']' | b'}') {
                self.finish_scalar(scalar, out)?;
                return self.byte(b, out);
            }
            scalar.number = scalar.number.step(b);
            let possible_literal = !scalar.streamed
                && [b"true".as_slice(), b"false", b"null"]
                    .iter()
                    .any(|literal| {
                        literal.starts_with(&scalar.bytes)
                            && literal.get(scalar.bytes.len()) == Some(&b)
                    });
            if matches!(scalar.number, Number::Invalid) && !possible_literal {
                // A complete root literal followed by an escape can conceal the
                // start of a credential key. Passing both to the plain scanner
                // joins them into one word and loses the key boundary.
                if self.positions.len() == 1
                    && matches!(scalar.bytes.as_slice(), b"true" | b"false" | b"null")
                    && b == b'\\'
                {
                    return self.malformed(&scalar.bytes, scalar.masked, out);
                }
                let mut remainder = scalar.bytes;
                remainder.push(b);
                if self.positions.len() == 1 && !scalar.streamed {
                    self.plain = true;
                    return self.plain_bytes(&remainder, out);
                }
                return self.malformed(&remainder, scalar.masked, out);
            }
            if scalar.bytes.len() == MAX_SECRET_LENGTH {
                scalar.streamed = true;
                scalar.masked |= self.scalar_secret;
                if scalar.masked {
                    out.push_bytes(b"\"[REDACTED]\"");
                    self.has_redaction = true;
                } else {
                    out.push_bytes(&scalar.bytes);
                }
                scalar.bytes.clear();
            }
            if scalar.streamed {
                if !scalar.masked {
                    out.push_bytes(&[b]);
                }
            } else {
                scalar.bytes.push(b);
            }
            self.scalar = Some(scalar);
            return Ok(());
        }
        if b.is_ascii_whitespace() {
            out.push_bytes(&[b]);
            return Ok(());
        }
        if !self.started {
            self.started = true;
            if !matches!(
                b,
                b'{' | b'[' | b'"' | b'-' | b'0'..=b'9' | b't' | b'f' | b'n'
            ) {
                self.plain = true;
                return self.plain_bytes(&[b], out);
            }
        }
        let context = self.positions.last().ok_or(FilterError)?;
        let pos = context.position;
        let inherited = context.sensitive;
        match (pos, b) {
            (Position::ArrayFirst | Position::ArrayComma, b']')
            | (Position::ObjectFirst | Position::ObjectComma, b'}') => {
                self.positions.pop();
                out.push_bytes(&[b]);
            }
            (Position::ArrayComma, b',') => {
                self.positions.last_mut().ok_or(FilterError)?.position = Position::ArrayValue;
                out.push_bytes(&[b]);
            }
            (Position::ObjectComma, b',') => {
                self.positions.last_mut().ok_or(FilterError)?.position = Position::ObjectKey;
                out.push_bytes(&[b]);
            }
            (Position::ObjectColon, b':') => {
                self.positions.last_mut().ok_or(FilterError)?.position = Position::ObjectValue;
                out.push_bytes(&[b]);
            }
            (Position::ObjectFirst | Position::ObjectKey, b'"') => {
                out.push_bytes(b"\"");
                self.string = Some(StringToken {
                    key: true,
                    masked: inherited,
                    tail: Vec::new(),
                    decoded: Vec::new(),
                    escape: Vec::new(),
                    utf8: Vec::new(),
                    key_source: Vec::new(),
                    key_overflow: false,
                    source: SourceMap::default(),
                    redaction_run: false,
                });
            }
            (
                Position::Root
                | Position::ArrayFirst
                | Position::ArrayValue
                | Position::ObjectValue,
                _,
            ) => {
                let masked = inherited || (pos == Position::ObjectValue && self.mask_value);
                self.mask_value = false;
                self.positions.last_mut().ok_or(FilterError)?.position = match pos {
                    Position::Root => Position::Done,
                    Position::ObjectValue => Position::ObjectComma,
                    _ => Position::ArrayComma,
                };
                match b {
                    b'{' | b'[' => {
                        if self.positions.len() > MAX_DEPTH {
                            return self.malformed(&[b], true, out);
                        }
                        self.positions.push(Context {
                            position: if b == b'{' {
                                Position::ObjectFirst
                            } else {
                                Position::ArrayFirst
                            },
                            sensitive: masked,
                        });
                        out.push_bytes(&[b]);
                    }
                    b'"' => {
                        out.push_bytes(b"\"");
                        if masked {
                            out.push_bytes(REDACTED);
                            self.has_redaction = true;
                        }
                        self.string = Some(StringToken {
                            key: false,
                            masked,
                            tail: Vec::new(),
                            decoded: Vec::new(),
                            escape: Vec::new(),
                            utf8: Vec::new(),
                            key_source: Vec::new(),
                            key_overflow: false,
                            source: SourceMap::default(),
                            redaction_run: false,
                        });
                    }
                    b'-' | b'0'..=b'9' | b't' | b'f' | b'n' => {
                        self.scalar = Some(Scalar {
                            bytes: vec![b],
                            number: Number::Start.step(b),
                            streamed: false,
                            masked,
                        })
                    }
                    _ => return self.malformed(&[b], masked, out),
                }
            }
            _ => return self.malformed(&[b], inherited || self.mask_value, out),
        }
        Ok(())
    }

    fn end_line(&mut self, out: &mut FrameEmitter<'_>) -> Result<(), FilterError> {
        if let Some(scalar) = self.scalar.take() {
            self.finish_scalar(scalar, out)?;
        }
        if self.started
            && (self.plain
                || self.string.is_some()
                || self.positions.len() != 1
                || self.positions[0].position != Position::Done)
        {
            self.invalid = true;
        }
        if let Some(mut token) = self.string.take() {
            self.finish_string(&mut token, out)?;
            // Keep an incomplete final string incomplete, even if masking hid it.
            out.push_bytes(b"\0");
        } else {
            self.inner.finish(&mut |b| out.push_bytes(b))?;
        }
        self.reset_inner()?;
        self.positions.clear();
        self.positions.push(Context {
            position: Position::Root,
            sensitive: false,
        });
        self.started = false;
        self.plain = false;
        self.discard = false;
        self.source_line_bytes = 0;
        self.mask_value = false;
        Ok(())
    }
}

impl JsonlRedactor {
    fn filter_inner(
        &mut self,
        bytes: &[u8],
        sink: &mut dyn FnMut(&[u8]),
    ) -> Result<(), FilterError> {
        if bytes.len() > FRAME_LIMIT {
            return Err(FilterError);
        }
        let mut out = FrameEmitter::new(sink);
        let mut remaining = bytes;
        while !remaining.is_empty() {
            if remaining[0] == b'\n' {
                remaining = &remaining[1..];
                self.end_line(&mut out)?;
                out.push_bytes(b"\n");
                continue;
            }
            let line_part = remaining
                .iter()
                .position(|b| *b == b'\n')
                .unwrap_or(remaining.len());
            let allowed = if self.provider_limit {
                PROVIDER_LINE_LIMIT.saturating_sub(self.source_line_bytes)
            } else {
                usize::MAX
            };
            if allowed == 0 && !self.discard {
                // Preserve the source-size failure in the retained, redacted
                // frame, so replay reports the correct line even if it shrank.
                self.malformed(&[], true, &mut out)?;
            }
            let n = if self.discard {
                line_part
            } else {
                line_part.min(allowed)
            };
            self.source_line_bytes = self.source_line_bytes.saturating_add(n);
            let part = &remaining[..n];
            remaining = &remaining[n..];
            if self.discard {
                continue;
            }
            let mut index = 0;
            while index < part.len() {
                if self.discard {
                    break;
                }
                if self.plain {
                    self.plain_bytes(&part[index..], &mut out)?;
                    break;
                }
                self.byte(part[index], &mut out)?;
                index += 1;
            }
        }
        out.flush();
        Ok(())
    }
    fn finish_inner(&mut self, sink: &mut dyn FnMut(&[u8])) -> Result<(), FilterError> {
        let mut out = FrameEmitter::new(sink);
        self.end_line(&mut out)?;
        out.flush();
        Ok(())
    }
}

impl CaptureFilter for JsonlRedactor {
    fn filter(&mut self, bytes: &[u8], sink: &mut dyn FnMut(&[u8])) -> Result<(), FilterError> {
        if self.failed {
            return Err(FilterError);
        }
        let result = self.filter_inner(bytes, sink);
        self.failed |= result.is_err();
        result
    }
    fn finish(&mut self, sink: &mut dyn FnMut(&[u8])) -> Result<(), FilterError> {
        if self.failed {
            return Err(FilterError);
        }
        let result = self.finish_inner(sink);
        self.failed |= result.is_err();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn source_map_exhaustion_is_sticky() {
        let mut filter = JsonlRedactor::new([]).unwrap();
        filter.filter(br#"{"v":""#, &mut |_| {}).unwrap();
        let token = filter.string.as_mut().unwrap();
        for i in 0..source::MAX_SOURCE_SPANS {
            token
                .source
                .push(if i % 2 == 0 { b"a" } else { b"b" }, 1)
                .unwrap();
        }
        assert!(filter.filter(b"x", &mut |_| {}).is_err());
        let mut output = Vec::new();
        assert!(
            filter
                .filter(b"tail", &mut |b| output.extend_from_slice(b))
                .is_err()
        );
        assert!(filter.finish(&mut |b| output.extend_from_slice(b)).is_err());
        assert!(output.is_empty());
    }

    fn random(state: &mut u64) -> usize {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state as usize
    }

    fn filtered(input: &[u8], seed: u64) -> (Vec<u8>, bool) {
        let mut filter =
            JsonlRedactor::new([b"literal-secret".to_vec(), b"12345".to_vec()]).unwrap();
        let mut result = Vec::new();
        let mut state = seed;
        let mut left = input;
        let mut sink = |frame: &[u8]| {
            assert!(frame.len() <= FRAME_LIMIT);
            result.extend_from_slice(frame);
        };
        while !left.is_empty() {
            let n = (random(&mut state) % 4096 + 1).min(left.len());
            filter.filter(&left[..n], &mut sink).unwrap();
            left = &left[n..];
        }
        filter.finish(&mut sink).unwrap();
        (result, filter.has_redaction())
    }

    fn same_structure(input: &Value, output: &Value) {
        match (input, output) {
            (Value::Object(a), Value::Object(b)) => {
                assert_eq!(a.keys().collect::<Vec<_>>(), b.keys().collect::<Vec<_>>());
                for (key, value) in a {
                    same_structure(value, &b[key]);
                }
            }
            (Value::Array(a), Value::Array(b)) => {
                assert_eq!(a.len(), b.len());
                for (a, b) in a.iter().zip(b) {
                    same_structure(a, b);
                }
            }
            (Value::String(_), Value::String(s)) => {
                assert!(!s.contains("literal-secret"));
                assert!(!s.contains("12345"));
                assert!(!s.contains("ghp_abcdefghijklmnopqrstuvwxyz"));
            }
            (Value::Number(n), Value::String(s)) if n.to_string().contains("12345") => {
                assert_eq!(s, "[REDACTED]")
            }
            _ => assert_eq!(input, output),
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn unmasked_string_spans_retain_original_source_escapes() {
        let input = br#"{ "keep":"\n\r\t\b\f\/\u000a\u0041\ud83d\ude03\\\"", "value":"left\t\u006citeral-secret\nright" }"#;
        let expected = br#"{ "keep":"\n\r\t\b\f\/\u000a\u0041\ud83d\ude03\\\"", "value":"left\t[REDACTED]\nright" }"#;
        for seed in 1..32 {
            let (output, changed) = filtered(input, seed);
            assert!(changed);
            assert_eq!(output, expected);
            assert!(serde_json::from_slice::<Value>(&output).is_ok());
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn generated_json_preserves_structure_and_hides_decoded_secrets_for_random_chunkings() {
        let mut state = 923_491;
        for _ in 0..48 {
            let size = random(&mut state) % 200;
            let value = json!({
                "password": "ghp_abcdefghijklmnopqrstuvwxyz",
                "payload": [format!("{} literal-secret ghp_abcdefghijklmnopqrstuvwxyz \" \\ 😃", "x".repeat(size)),
                    {"safe": "ordinary prose", "number": 123456, "empty": "", "truth": true, "nil": null}],
                "tail": [false, -9.25]
            });
            let input = serde_json::to_vec(&value).unwrap();
            let expected = filtered(&input, 1).0;
            for _ in 0..8 {
                let (output, changed) = filtered(&input, random(&mut state) as u64 | 1);
                assert!(changed);
                assert_eq!(output, expected);
                let parsed = serde_json::from_slice(&output).expect("valid JSON remains valid");
                same_structure(&value, &parsed);
            }
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn credential_keys_are_masked_as_valid_json() {
        let input = br#"{"\u0067hp_abcdefghijklmnopqrstuvwxyz":"safe","ordinary":"value"}"#;
        for seed in 1..9 {
            let (output, changed) = filtered(input, seed);
            assert!(changed);
            let parsed: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(parsed["[REDACTED]"], "safe");
            assert_eq!(parsed["ordinary"], "value");
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn escaped_literals_and_surrogates_are_decoded_before_redaction() {
        let input = br#"{"password":"ghp_abcdefghijklmnopqrstuvwxyz","value":"\u006citeral-\u0073ecret \ud83d\ude03 \u0067hp_abcdefghijklmnopqrstuvwxyz","number":123456,"truth":true}"#;
        let expected = filtered(input, 1).0;
        let parsed: Value = serde_json::from_slice(&expected).unwrap();
        assert_eq!(parsed["password"], "[REDACTED]");
        assert_eq!(parsed["value"], "[REDACTED] 😃 [REDACTED]");
        assert_eq!(parsed["number"], "[REDACTED]");
        for split in 0..input.len() {
            let mut filter =
                JsonlRedactor::new([b"literal-secret".to_vec(), b"12345".to_vec()]).unwrap();
            let mut output = Vec::new();
            let mut sink = |b: &[u8]| output.extend_from_slice(b);
            filter.filter(&input[..split], &mut sink).unwrap();
            filter.filter(&input[split..], &mut sink).unwrap();
            filter.finish(&mut sink).unwrap();
            assert_eq!(output, expected);
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn malformed_remainder_masks_escaped_secrets_across_chunks() {
        for input in [
            br#"{"ok":true,!"pass\u0077ord":"unshaped-value","x":"literal-\u0073ecret"}"#
                .as_slice(),
            br#"broken pass\u0077ord=unshaped-value literal-\u0073ecret"#,
            br#"\u0070assword=unshaped-value literal-\u0073ecret"#,
            br#"true\u0070assword=unshaped-value literal-\u0073ecret"#,
        ] {
            let expected = filtered(input, 1).0;
            for split in 0..=input.len() {
                let mut filter =
                    JsonlRedactor::new([b"literal-secret".to_vec(), b"12345".to_vec()]).unwrap();
                let mut output = Vec::new();
                let mut sink = |b: &[u8]| output.extend_from_slice(b);
                filter.filter(&input[..split], &mut sink).unwrap();
                filter.filter(&input[split..], &mut sink).unwrap();
                filter.finish(&mut sink).unwrap();
                assert_eq!(output, expected, "split {split}, input {input:?}");
            }
            for seed in 2..20 {
                let output = filtered(input, seed).0;
                assert_eq!(output, expected);
                assert!(
                    !output
                        .windows(b"unshaped-value".len())
                        .any(|w| w == b"unshaped-value")
                );
                assert!(!output.windows(b"literal-".len()).any(|w| w == b"literal-"));
                assert!(serde_json::from_slice::<Value>(&output).is_err());
            }
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn nesting_limit_excludes_root_and_masks_excess() {
        let valid = format!("{}0{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        let output = filtered(valid.as_bytes(), 1).0;
        assert_eq!(output, valid.as_bytes());
        let excess = format!(
            "{}\"unshaped-value\"{}\n{{\"ok\":true}}\n",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        let output = filtered(excess.as_bytes(), 1).0;
        let lines: Vec<_> = output.split(|b| *b == b'\n').collect();
        assert!(serde_json::from_slice::<Value>(lines[0]).is_err());
        assert!(
            !output
                .windows(b"unshaped-value".len())
                .any(|w| w == b"unshaped-value")
        );
        assert_eq!(
            serde_json::from_slice::<Value>(lines[1]).unwrap(),
            json!({"ok": true})
        );
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn jsonl_rejects_non_utf8_secret_literals() {
        assert!(matches!(
            JsonlRedactor::new([vec![0xa9]]),
            Err(RedactorError::InvalidUtf8Secret)
        ));
        let mut filter = JsonlRedactor::new(["é".as_bytes().to_vec()]).unwrap();
        let mut output = Vec::new();
        filter
            .filter("{\"x\":\"café\"}".as_bytes(), &mut |b| {
                output.extend_from_slice(b)
            })
            .unwrap();
        filter.finish(&mut |b| output.extend_from_slice(b)).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            json!({"x":"caf[REDACTED]"})
        );
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn arbitrarily_long_strings_stream_without_losing_closing_structure() {
        let value =
            json!({"value": format!("{} literal-secret", "a".repeat(FRAME_LIMIT * 3)), "tail": []});
        let input = serde_json::to_vec(&value).unwrap();
        let expected = filtered(&input, 1).0;
        for seed in 2..10 {
            let (output, _) = filtered(&input, seed);
            assert_eq!(output, expected);
            same_structure(&value, &serde_json::from_slice(&output).unwrap());
        }
        let mut filter = JsonlRedactor::new([]).unwrap();
        let mut emitted = 0;
        for chunk in input[..FRAME_LIMIT * 2].chunks(4096) {
            filter.filter(chunk, &mut |b| emitted += b.len()).unwrap();
        }
        assert!(emitted > FRAME_LIMIT); // Before the string or frame closes.
        assert!(filter.string.as_ref().unwrap().decoded.len() < STRING_PIECE);
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn malformed_lines_remain_malformed_and_next_line_recovers() {
        let cases: &[&[u8]] = &[
            br#"{"value":"literal-secret"!}"#,
            br#"{"password":"\q"}"#,
            br#"{"password":"unterminated"#,
            br#"{"value":01}"#,
            b"{\"password\":\"\xff\"}",
            br#"{"value":"\ud800"}"#,
            br#"{"value":[1,]}"#,
        ];
        for &input in cases {
            let mut joined = input.to_vec();
            joined.extend_from_slice(b"\n{\"ok\":true}\n");
            let expected = filtered(&joined, 1).0;
            for seed in 2..8 {
                let (output, _) = filtered(&joined, seed);
                assert_eq!(output, expected);
                let lines: Vec<_> = output.split(|b| *b == b'\n').collect();
                assert!(serde_json::from_slice::<Value>(lines[0]).is_err());
                assert_eq!(
                    serde_json::from_slice::<Value>(lines[1]).unwrap(),
                    json!({"ok": true})
                );
            }
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn credential_context_survives_containers_and_malformed_fallback() {
        let input = br#"{"password":{"nested":["unshaped-value",37,true,null]},"keep":"safe"}"#;
        for seed in 1..9 {
            let (output, _) = filtered(input, seed);
            assert_eq!(
                serde_json::from_slice::<Value>(&output).unwrap(),
                json!({
                    "password": {"nested": ["[REDACTED]", "[REDACTED]", "[REDACTED]", "[REDACTED]"]}, "keep":"safe"
                })
            );
        }
        let cases: &[&[u8]] = &[
            br#"{"password"=unshaped-value}"#,
            br#"{"password":"\qunshaped-value"}"#,
            br#"{"password":17unshaped-value}"#,
            br#"{"password":{"nested"=unshaped-value}}"#,
        ];
        for input in cases {
            let expected = filtered(input, 1).0;
            for seed in 2..9 {
                let (output, _) = filtered(input, seed);
                assert_eq!(output, expected);
                assert!(serde_json::from_slice::<Value>(&output).is_err());
                assert!(
                    !output
                        .windows(b"unshaped-value".len())
                        .any(|b| b == b"unshaped-value")
                );
            }
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn non_json_root_prefixes_switch_to_plain_redaction_before_streaming() {
        for start in ["this", "falsehood", "not-json", "-no", "9bad"] {
            let input = format!(
                "{start} {} password=unshaped-value\n",
                "x".repeat(MAX_SECRET_LENGTH * 2)
            );
            let mut plain = Redactor::new([b"literal-secret".to_vec(), b"12345".to_vec()]).unwrap();
            let mut expected = Vec::new();
            for chunk in input.as_bytes().chunks(FRAME_LIMIT) {
                plain
                    .filter(chunk, &mut |b| expected.extend_from_slice(b))
                    .unwrap();
            }
            plain
                .finish(&mut |b| expected.extend_from_slice(b))
                .unwrap();
            for seed in 1..9 {
                let (output, _) = filtered(input.as_bytes(), seed);
                assert_eq!(output, expected);
            }
        }
    }

    // Spec: docs/specs/supervisor.md#disclosure-and-safe-git-evidence
    #[test]
    fn long_scalars_use_constant_storage_and_conservative_masking() {
        let input = format!("[0.{}]", "7".repeat(MAX_SECRET_LENGTH * 3));
        let (output, changed) = filtered(input.as_bytes(), 31);
        assert!(changed);
        assert_eq!(
            serde_json::from_slice::<Value>(&output).unwrap(),
            json!(["[REDACTED]"])
        );
        let (output, _) = filtered(b"[true,false,null,-0,1e-7]", 63);
        assert_eq!(output, b"[true,false,null,-0,1e-7]");
    }
}
