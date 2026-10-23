use crate::json::{DecodeError, Limits};
use serde::de::DeserializeOwned;
use std::io::{Cursor, Read};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsoncTokenKind {
    ObjectStart,
    ObjectEnd,
    ArrayStart,
    ArrayEnd,
    Colon,
    Comma,
    TrailingComma,
    String,
    Number,
    True,
    False,
    Null,
    LineComment,
    BlockComment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JsoncToken {
    pub kind: JsoncTokenKind,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug)]
pub struct JsoncDocument {
    source: String,
    projection: Vec<u8>,
    tokens: Vec<JsoncToken>,
    limits: Limits,
}

impl JsoncDocument {
    /// Read, validate, and tokenize one bounded JSONC document.
    ///
    /// Comments and trailing commas become spaces in `projection`, so every
    /// byte span continues to refer to the original source.
    pub fn read<R: Read>(reader: R, limits: Limits) -> Result<Self, DecodeError> {
        let mut bytes = Vec::new();
        reader
            .take(limits.max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(DecodeError::Read)?;
        let byte_len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if byte_len > limits.max_bytes {
            return Err(DecodeError::TooLarge {
                limit: limits.max_bytes,
            });
        }
        if let Err(error) = std::str::from_utf8(&bytes) {
            let (line, column) = location(&bytes, error.valid_up_to());
            return Err(DecodeError::InvalidUtf8 { line, column });
        }

        let mut projection = bytes.clone();
        let mut tokens = scan(&bytes, &mut projection)?;
        mark_trailing_commas(&mut tokens, &mut projection);
        crate::json::decode::<serde_json::Value, _>(Cursor::new(&projection), limits)?;

        let source = String::from_utf8(bytes).map_err(|error| {
            let offset = error.utf8_error().valid_up_to();
            let (line, column) = location(error.as_bytes(), offset);
            DecodeError::InvalidUtf8 { line, column }
        })?;
        Ok(Self {
            source,
            projection,
            tokens,
            limits,
        })
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn projection(&self) -> &[u8] {
        &self.projection
    }

    pub fn tokens(&self) -> &[JsoncToken] {
        &self.tokens
    }

    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, DecodeError> {
        crate::json::decode(self.projection.as_slice(), self.limits)
    }
}

pub fn decode_jsonc<T, R>(reader: R, limits: Limits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
    R: Read,
{
    JsoncDocument::read(reader, limits)?.decode()
}

fn scan(source: &[u8], projection: &mut [u8]) -> Result<Vec<JsoncToken>, DecodeError> {
    let mut tokens = Vec::new();
    let mut offset = 0usize;
    while let Some(byte) = source.get(offset).copied() {
        match byte {
            b' ' | b'\t' | b'\r' | b'\n' => offset += 1,
            b'/' if source.get(offset.saturating_add(1)) == Some(&b'/') => {
                let start = offset;
                offset += 2;
                while let Some(byte) = source.get(offset) {
                    if matches!(byte, b'\r' | b'\n') {
                        break;
                    }
                    offset += 1;
                }
                blank(projection, start, offset);
                tokens.push(JsoncToken {
                    kind: JsoncTokenKind::LineComment,
                    start,
                    end: offset,
                });
            }
            b'/' if source.get(offset.saturating_add(1)) == Some(&b'*') => {
                let start = offset;
                offset += 2;
                let mut end = None;
                while offset < source.len() {
                    if source.get(offset..offset.saturating_add(2)) == Some(b"*/") {
                        offset += 2;
                        end = Some(offset);
                        break;
                    }
                    offset += 1;
                }
                let Some(end) = end else {
                    return Err(syntax_error(source, start));
                };
                blank(projection, start, end);
                tokens.push(JsoncToken {
                    kind: JsoncTokenKind::BlockComment,
                    start,
                    end,
                });
            }
            b'/' => return Err(syntax_error(source, offset)),
            b'"' => {
                let start = offset;
                offset = scan_string(source, offset)?;
                tokens.push(JsoncToken {
                    kind: JsoncTokenKind::String,
                    start,
                    end: offset,
                });
            }
            b'{' => push_token(&mut tokens, JsoncTokenKind::ObjectStart, &mut offset),
            b'}' => push_token(&mut tokens, JsoncTokenKind::ObjectEnd, &mut offset),
            b'[' => push_token(&mut tokens, JsoncTokenKind::ArrayStart, &mut offset),
            b']' => push_token(&mut tokens, JsoncTokenKind::ArrayEnd, &mut offset),
            b':' => push_token(&mut tokens, JsoncTokenKind::Colon, &mut offset),
            b',' => push_token(&mut tokens, JsoncTokenKind::Comma, &mut offset),
            b'-' | b'0'..=b'9' => {
                let start = offset;
                offset = scan_atom(source, offset);
                tokens.push(JsoncToken {
                    kind: JsoncTokenKind::Number,
                    start,
                    end: offset,
                });
            }
            b't' | b'f' | b'n' => {
                let start = offset;
                offset = scan_atom(source, offset);
                let kind = match source.get(start..offset) {
                    Some(b"true") => JsoncTokenKind::True,
                    Some(b"false") => JsoncTokenKind::False,
                    Some(b"null") => JsoncTokenKind::Null,
                    _ => return Err(syntax_error(source, start)),
                };
                tokens.push(JsoncToken {
                    kind,
                    start,
                    end: offset,
                });
            }
            _ => return Err(syntax_error(source, offset)),
        }
    }
    Ok(tokens)
}

fn scan_string(source: &[u8], start: usize) -> Result<usize, DecodeError> {
    let mut offset = start.saturating_add(1);
    while let Some(byte) = source.get(offset) {
        match byte {
            b'"' => return Ok(offset.saturating_add(1)),
            b'\\' => {
                offset = offset.saturating_add(2);
            }
            _ => offset = offset.saturating_add(1),
        }
    }
    Err(syntax_error(source, start))
}

fn scan_atom(source: &[u8], start: usize) -> usize {
    let mut offset = start;
    while let Some(byte) = source.get(offset) {
        if *byte == b'/'
            && source
                .get(offset.saturating_add(1))
                .is_some_and(|next| matches!(next, b'/' | b'*'))
        {
            break;
        }
        if matches!(
            byte,
            b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}' | b':' | b'[' | b'{'
        ) {
            break;
        }
        offset += 1;
    }
    offset
}

fn mark_trailing_commas(tokens: &mut [JsoncToken], projection: &mut [u8]) {
    let mut trailing = Vec::new();
    let mut next_kind = None;
    let mut candidate = None;
    for index in (0..tokens.len()).rev() {
        let Some(kind) = tokens.get(index).map(|token| token.kind) else {
            continue;
        };
        if is_comment(kind) {
            continue;
        }
        if let Some(comma_index) = candidate.take() {
            if ends_value(kind) {
                trailing.push(comma_index);
            }
        }
        if kind == JsoncTokenKind::Comma
            && next_kind.is_some_and(|next| {
                matches!(next, JsoncTokenKind::ArrayEnd | JsoncTokenKind::ObjectEnd)
            })
        {
            candidate = Some(index);
        }
        next_kind = Some(kind);
    }
    for index in trailing {
        if let Some(token) = tokens.get_mut(index) {
            if let Some(byte) = projection.get_mut(token.start) {
                *byte = b' ';
            }
            token.kind = JsoncTokenKind::TrailingComma;
        }
    }
}

fn ends_value(kind: JsoncTokenKind) -> bool {
    matches!(
        kind,
        JsoncTokenKind::String
            | JsoncTokenKind::Number
            | JsoncTokenKind::True
            | JsoncTokenKind::False
            | JsoncTokenKind::Null
            | JsoncTokenKind::ArrayEnd
            | JsoncTokenKind::ObjectEnd
    )
}

fn is_comment(kind: JsoncTokenKind) -> bool {
    matches!(
        kind,
        JsoncTokenKind::LineComment | JsoncTokenKind::BlockComment
    )
}

fn push_token(tokens: &mut Vec<JsoncToken>, kind: JsoncTokenKind, offset: &mut usize) {
    let start = *offset;
    *offset += 1;
    tokens.push(JsoncToken {
        kind,
        start,
        end: *offset,
    });
}

fn blank(projection: &mut [u8], start: usize, end: usize) {
    for byte in projection.iter_mut().take(end).skip(start) {
        if !matches!(*byte, b'\r' | b'\n') {
            *byte = b' ';
        }
    }
}

fn syntax_error(source: &[u8], offset: usize) -> DecodeError {
    let (line, column) = location(source, offset);
    DecodeError::Syntax { line, column }
}

fn location(source: &[u8], offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for byte in source.iter().take(offset.min(source.len())) {
        if *byte == b'\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::{decode_jsonc, JsoncDocument, JsoncTokenKind};
    use crate::json::{DecodeError, Limits};
    use serde::Deserialize;
    use std::io::Cursor;

    #[derive(Debug, Deserialize, PartialEq)]
    struct Config {
        url: String,
        names: Vec<String>,
    }

    #[test]
    fn accepts_comments_and_trailing_commas_while_preserving_source_spans() {
        let source = "{\n  // endpoint\n  \"url\": \"https://host/a/*literal*/\",\n  \"names\": [\"é\",], /* end */\n}";
        let document = JsoncDocument::read(Cursor::new(source.as_bytes()), Limits::CONFIG)
            .expect("valid JSONC should be accepted");
        let decoded: Config = document.decode().expect("typed config should decode");

        assert_eq!(decoded.url, "https://host/a/*literal*/");
        assert_eq!(decoded.names, ["é"]);
        assert_eq!(document.source(), source);
        assert_eq!(document.source().len(), document.projection().len());
        assert!(document.tokens().iter().any(|token| {
            token.kind == JsoncTokenKind::TrailingComma
                && source.as_bytes().get(token.start) == Some(&b',')
        }));
        assert!(document.tokens().iter().any(|token| {
            token.kind == JsoncTokenKind::LineComment
                && source.as_bytes().get(token.start..token.end) == Some(b"// endpoint")
        }));
        assert!(document.tokens().iter().any(|token| {
            token.kind == JsoncTokenKind::String
                && source.as_bytes().get(token.start..token.end) == Some("\"é\"".as_bytes())
        }));
    }

    #[test]
    fn string_escape_boundaries_do_not_start_comments_or_lose_tokens() {
        let source = r#"{"url":"quote: \\\" // /* still string */","names":["x",]}"#;
        let decoded: Config = decode_jsonc(Cursor::new(source.as_bytes()), Limits::CONFIG)
            .expect("comment markers inside strings are data");
        assert_eq!(decoded.url, "quote: \\\" // /* still string */");
        assert_eq!(decoded.names, ["x"]);
    }

    #[test]
    fn rejects_unterminated_block_comment_at_its_source_location() {
        let error = JsoncDocument::read(Cursor::new(b"{\n /* open"), Limits::CONFIG)
            .expect_err("unterminated block comments must fail closed");
        assert!(matches!(error, DecodeError::Syntax { line: 2, column: 2 }));
    }

    #[test]
    fn rejects_duplicate_fields_even_when_jsonc_has_comments_and_trailing_commas() {
        let source = br#"{"name":1, /* retained location */ "name":2,}"#;
        let error = decode_jsonc::<serde_json::Value, _>(Cursor::new(source), Limits::CONFIG)
            .expect_err("duplicate keys must be rejected by strict JSON validation");
        assert!(matches!(error, DecodeError::DuplicateMember { .. }));
    }

    #[test]
    fn comments_can_touch_a_scalar_token() {
        for source in [
            br#"{"value":1/* scalar comment */,}"#.as_slice(),
            b"{\"value\":true// scalar comment\n}",
            br#"{"value":false/* scalar comment */}"#,
            br#"{"value":null/* scalar comment */}"#,
        ] {
            decode_jsonc::<serde_json::Value, _>(Cursor::new(source), Limits::CONFIG)
                .expect("comments are trivia even without surrounding whitespace");
        }
    }

    #[test]
    fn reports_utf8_offsets_in_source_bytes() {
        let source = b"{\"label\":\"\xc3\xa9\", \xff: 1}";
        let error = JsoncDocument::read(Cursor::new(source), Limits::CONFIG)
            .expect_err("invalid UTF-8 must be rejected before comment scanning");
        assert!(matches!(
            error,
            DecodeError::InvalidUtf8 {
                line: 1,
                column: 16
            }
        ));
    }

    #[test]
    fn enforces_the_source_byte_limit_before_parsing() {
        let source = b"{\"x\":1}";
        let limits = Limits {
            max_bytes: 6,
            ..Limits::CONFIG
        };
        let error = JsoncDocument::read(Cursor::new(source), limits)
            .expect_err("one byte beyond the configured bound must fail");
        assert!(matches!(error, DecodeError::TooLarge { limit: 6 }));
    }

    #[test]
    fn malformed_json_is_not_repaired_by_comma_removal() {
        for source in [b"[,]".as_slice(), b"{,}", b"[1,,]"] {
            assert!(
                decode_jsonc::<serde_json::Value, _>(Cursor::new(source), Limits::CONFIG).is_err()
            );
        }
    }
}
