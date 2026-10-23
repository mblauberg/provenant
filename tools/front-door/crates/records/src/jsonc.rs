use crate::json::{DecodeError, Limits};
use serde::de::DeserializeOwned;
use std::io::Read;

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

pub struct JsoncDocument {
    source: String,
    projection: Vec<u8>,
    tokens: Vec<JsoncToken>,
    limits: Limits,
}

impl JsoncDocument {
    pub fn read<R: Read>(_reader: R, _limits: Limits) -> Result<Self, DecodeError> {
        Err(DecodeError::Syntax { line: 1, column: 1 })
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
        crate::json::decode(&self.projection[..], self.limits)
    }
}

pub fn decode_jsonc<T, R>(reader: R, limits: Limits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
    R: Read,
{
    JsoncDocument::read(reader, limits)?.decode()
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
        assert_eq!(document.source().as_bytes().len(), document.projection().len());
        assert!(document.tokens().iter().any(|token| {
            token.kind == JsoncTokenKind::TrailingComma
                && source.as_bytes().get(token.start) == Some(&b',')
        }));
        assert!(document.tokens().iter().any(|token| {
            token.kind == JsoncTokenKind::LineComment
                && source.as_bytes().get(token.start..token.end) == Some(b"// endpoint")
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
    fn reports_utf8_offsets_in_source_bytes() {
        let source = b"{\"label\":\"\xc3\xa9\",\n \xff: 1}";
        let error = JsoncDocument::read(Cursor::new(source), Limits::CONFIG)
            .expect_err("invalid UTF-8 must be rejected before comment scanning");
        assert!(matches!(error, DecodeError::InvalidUtf8 { line: 2, column: 2 }));
    }

    #[test]
    fn enforces_the_source_byte_limit_before_parsing() {
        let source = b"{\"x\":1}";
        let limits = Limits { max_bytes: 6, ..Limits::CONFIG };
        let error = JsoncDocument::read(Cursor::new(source), limits)
            .expect_err("one byte beyond the configured bound must fail");
        assert!(matches!(error, DecodeError::TooLarge { limit: 6 }));
    }

    #[test]
    fn malformed_json_is_not_repaired_by_comma_removal() {
        for source in [b"[,]".as_slice(), b"{,}", b"[1,,]"] {
            assert!(decode_jsonc::<serde_json::Value, _>(Cursor::new(source), Limits::CONFIG)
                .is_err());
        }
    }
}
