use serde::{
    Deserialize, Deserializer, Serialize,
    de::{DeserializeOwned, Error as _},
};
use std::io::Read;
use std::path::{Path, PathBuf};
use thiserror::Error;

const ABSOLUTE_MAX_DEPTH: usize = 64;

/// Input bounds for one JSON document. Defaults are for general records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: u64,
    pub max_depth: usize,
    pub max_members: usize,
}

impl Limits {
    pub const CONFIG: Self = Self {
        max_bytes: 1_048_576,
        max_depth: 64,
        max_members: 100_000,
    };
    pub const YAML: Self = Self {
        max_bytes: 262_144,
        max_depth: 32,
        max_members: 10_000,
    };
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 8_388_608,
            max_depth: 64,
            max_members: 100_000,
        }
    }
}

/// Header shared by schema-versioned stored records. Version one is the only
/// supported record version in this crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RecordHeader {
    schema_version: u64,
    #[serde(flatten)]
    extra: crate::ExtraFields,
}

impl RecordHeader {
    pub const V1: Self = Self {
        schema_version: 1,
        extra: std::collections::BTreeMap::new(),
    };

    pub const fn schema_version(&self) -> u64 {
        self.schema_version
    }

    pub fn new(schema_version: u64) -> Result<Self, DecodeError> {
        if schema_version == Self::V1.schema_version {
            Ok(Self {
                schema_version,
                extra: Default::default(),
            })
        } else {
            Err(DecodeError::UnsupportedVersion {
                found: schema_version,
                line: 1,
                column: 1,
            })
        }
    }
}

impl<'de> Deserialize<'de> for RecordHeader {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireHeader {
            schema_version: u64,
            #[serde(flatten)]
            extra: crate::ExtraFields,
        }

        let header = WireHeader::deserialize(deserializer)?;
        if header.schema_version == Self::V1.schema_version {
            Ok(Self {
                schema_version: header.schema_version,
                extra: header.extra,
            })
        } else {
            Err(D::Error::custom("unsupported schema version"))
        }
    }
}

#[derive(Debug, Error)]
pub enum DecodeError {
    #[error("reading JSON record failed")]
    Read(#[source] std::io::Error),
    #[error("JSON record exceeds the {limit}-byte limit")]
    TooLarge { limit: u64 },
    #[error("JSON record has invalid UTF-8 at line {line}, column {column}")]
    InvalidUtf8 { line: usize, column: usize },
    #[error("invalid JSON syntax at line {line}, column {column}")]
    Syntax { line: usize, column: usize },
    #[error("duplicate JSON object member at line {line}, column {column}")]
    DuplicateMember { line: usize, column: usize },
    #[error("JSON integer is outside i64/u64 range at line {line}, column {column}")]
    IntegerOutOfRange { line: usize, column: usize },
    #[error("JSON number is not finite at line {line}, column {column}")]
    NonFinite { line: usize, column: usize },
    #[error("trailing JSON input at line {line}, column {column}")]
    TrailingInput { line: usize, column: usize },
    #[error("JSON nesting exceeds the depth limit at line {line}, column {column}")]
    DepthExceeded {
        line: usize,
        column: usize,
        limit: usize,
    },
    #[error("JSON collections exceed the member limit at line {line}, column {column}")]
    MembersExceeded {
        line: usize,
        column: usize,
        limit: usize,
    },
    #[error("JSON record has no schema_version at line {line}, column {column}")]
    MissingVersion { line: usize, column: usize },
    #[error("schema_version must be an integer at line {line}, column {column}")]
    InvalidVersion { line: usize, column: usize },
    #[error("unsupported schema version {found} at line {line}, column {column}")]
    UnsupportedVersion {
        found: u64,
        line: usize,
        column: usize,
    },
    #[error("{}", format_typed_decode_error(*.line, *.column, .field.as_deref(), *.schema_version))]
    Typed {
        line: usize,
        column: usize,
        field: Option<String>,
        schema_version: Option<u64>,
    },
    #[error(
        "{}",
        format_context_decode_error(
            .path.as_deref(),
            .record_type.as_deref(),
            *.schema_version,
            .source.as_ref()
        )
    )]
    Context {
        path: Option<PathBuf>,
        record_type: Option<String>,
        schema_version: Option<u64>,
        #[source]
        source: Box<DecodeError>,
    },
}

impl DecodeError {
    pub fn at_path(self, path: impl Into<PathBuf>) -> Self {
        self.with_context_parts(Some(path.into()), None, None)
    }

    pub fn for_record(self, record_type: impl Into<String>) -> Self {
        self.with_context_parts(None, Some(record_type.into()), None)
    }

    pub fn with_context(self, path: impl Into<PathBuf>, record_type: impl Into<String>) -> Self {
        self.at_path(path).for_record(record_type)
    }

    /// Return the classified decode error behind optional stored-record
    /// context, so callers can retain upgrade and corruption handling.
    pub fn root_cause(&self) -> &Self {
        match self {
            Self::Context { source, .. } => source.root_cause(),
            other => other,
        }
    }

    fn with_context_parts(
        self,
        path: Option<PathBuf>,
        record_type: Option<String>,
        schema_version: Option<u64>,
    ) -> Self {
        match self {
            Self::Context {
                path: current_path,
                record_type: current_type,
                schema_version: current_version,
                source,
            } => Self::Context {
                path: path.or(current_path),
                record_type: record_type.or(current_type),
                schema_version: schema_version.or(current_version),
                source,
            },
            Self::Typed {
                line,
                column,
                field,
                schema_version: current_version,
            } => Self::Context {
                path,
                record_type,
                schema_version: schema_version.or(current_version),
                source: Box::new(Self::Typed {
                    line,
                    column,
                    field,
                    schema_version: None,
                }),
            },
            Self::UnsupportedVersion {
                found,
                line,
                column,
            } => Self::Context {
                path,
                record_type,
                schema_version: schema_version.or(Some(found)),
                source: Box::new(Self::UnsupportedVersion {
                    found,
                    line,
                    column,
                }),
            },
            other => Self::Context {
                path,
                record_type,
                schema_version,
                source: Box::new(other),
            },
        }
    }
}

fn format_typed_decode_error(
    line: usize,
    column: usize,
    field: Option<&str>,
    schema_version: Option<u64>,
) -> String {
    let mut out = String::new();
    out.push_str("JSON value does not match its record type");
    let mut details = Vec::new();
    if let Some(version) = schema_version {
        details.push(format!("schema version {version}"));
    }
    if let Some(field) = field {
        details.push(format!("missing field `{field}`"));
    }
    if !details.is_empty() {
        out.push_str(&format!(" ({})", details.join(", ")));
    }
    out.push_str(&format!(" at line {line}, column {column}"));
    out
}

fn format_context_decode_error(
    path: Option<&Path>,
    record_type: Option<&str>,
    schema_version: Option<u64>,
    source: &DecodeError,
) -> String {
    let mut out = String::from("stored record");
    if let Some(path) = path {
        out.push_str(&format!(" {}", path.display()));
    }
    let mut details = Vec::new();
    if let Some(record_type) = record_type {
        details.push(format!("record type {record_type}"));
    }
    if let Some(schema_version) = schema_version {
        details.push(format!("schema version {schema_version}"));
    }
    if !details.is_empty() {
        out.push_str(&format!(" ({})", details.join(", ")));
    }
    out.push_str(&format!(": {source}"));
    out
}

fn extract_missing_field(error: &serde_json::Error) -> Option<String> {
    let msg = error.to_string();
    if let Some(start) = msg.find("missing field `") {
        let rest = &msg[start + "missing field `".len()..];
        if let Some(end) = rest.find('`') {
            return Some(rest[..end].to_owned());
        }
    }
    None
}

fn extract_schema_version(bytes: &[u8], facts: &ValidationFacts) -> Option<u64> {
    if facts.schema_version_offset.is_some()
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes)
    {
        return value.get("schema_version").and_then(|v| v.as_u64());
    }
    None
}

fn readable_schema_version(bytes: &[u8]) -> Option<u64> {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()?
        .get("schema_version")?
        .as_u64()
}

#[derive(Debug, Error)]
#[error("could not encode JSON record")]
pub struct EncodeError;

/// Read one bounded JSON document and deserialize it after validating every
/// value, including values the target type ignores.
pub fn decode<T, R>(reader: R, limits: Limits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
    R: Read,
{
    let mut bytes = read_bounded(reader, limits.max_bytes)?;
    decode_bytes(&mut bytes, limits)
}

/// Read and decode one stored record while retaining its path, type, and any
/// schema version that remains readable in the input.
pub fn decode_stored_record<T, R>(
    reader: R,
    limits: Limits,
    path: impl Into<PathBuf>,
    record_type: impl Into<String>,
) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
    R: Read,
{
    let path = path.into();
    let record_type = record_type.into();
    let mut bytes = read_bounded(reader, limits.max_bytes).map_err(|error| {
        error.with_context_parts(Some(path.clone()), Some(record_type.clone()), None)
    })?;
    let schema_version = readable_schema_version(&bytes);
    decode_bytes(&mut bytes, limits)
        .map_err(|error| error.with_context_parts(Some(path), Some(record_type), schema_version))
}

fn decode_bytes<T>(bytes: &mut [u8], limits: Limits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
{
    let facts = validate_document(bytes, limits)?;
    normalise_negative_zeroes(bytes, &facts.negative_zeroes);
    let schema_version = extract_schema_version(bytes, &facts);
    serde_json::from_slice(bytes).map_err(|source| DecodeError::Typed {
        line: source.line(),
        column: source.column(),
        field: extract_missing_field(&source),
        schema_version,
    })
}

/// Decode a flat stored record with a strict `schema_version: 1` header.
pub fn decode_v1<T, R>(reader: R, limits: Limits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
    R: Read,
{
    let mut bytes = read_bounded(reader, limits.max_bytes)?;
    decode_v1_bytes(&mut bytes, limits)
}

/// Decode a flat stored record with strict schema version one while retaining
/// its path, record type, and readable schema version on failures.
pub fn decode_stored_record_v1<T, R>(
    reader: R,
    limits: Limits,
    path: impl Into<PathBuf>,
    record_type: impl Into<String>,
) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
    R: Read,
{
    let path = path.into();
    let record_type = record_type.into();
    let mut bytes = read_bounded(reader, limits.max_bytes).map_err(|error| {
        error.with_context_parts(Some(path.clone()), Some(record_type.clone()), None)
    })?;
    let schema_version = readable_schema_version(&bytes);
    decode_v1_bytes(&mut bytes, limits)
        .map_err(|error| error.with_context_parts(Some(path), Some(record_type), schema_version))
}

fn decode_v1_bytes<T>(bytes: &mut [u8], limits: Limits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
{
    let facts = validate_document(bytes, limits)?;
    normalise_negative_zeroes(bytes, &facts.negative_zeroes);
    let header_value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|source| DecodeError::Typed {
            line: source.line(),
            column: source.column(),
            field: extract_missing_field(&source),
            schema_version: None,
        })?;
    let Some(object) = header_value.as_object() else {
        return Err(DecodeError::MissingVersion { line: 1, column: 1 });
    };
    let Some(version_value) = object.get("schema_version") else {
        return Err(DecodeError::MissingVersion { line: 1, column: 1 });
    };
    let Some(version) = version_value.as_u64() else {
        let (line, column) = facts
            .schema_version_offset
            .map_or((1, 1), |offset| location(bytes, offset));
        return Err(DecodeError::InvalidVersion { line, column });
    };
    if version != 1 {
        let (line, column) = facts
            .schema_version_offset
            .map_or((1, 1), |offset| location(bytes, offset));
        return Err(DecodeError::UnsupportedVersion {
            found: version,
            line,
            column,
        });
    }
    serde_json::from_slice(bytes).map_err(|source| DecodeError::Typed {
        line: source.line(),
        column: source.column(),
        field: extract_missing_field(&source),
        schema_version: Some(version),
    })
}

/// Encode one typed record as compact UTF-8 JSON without a final newline.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, EncodeError> {
    crate::canonical::check_finite(value).map_err(|_| EncodeError)?;
    serde_json::to_vec(value).map_err(|_| EncodeError)
}

fn read_bounded<R: Read>(reader: R, limit: u64) -> Result<Vec<u8>, DecodeError> {
    let mut bytes = Vec::new();
    let mut bounded = reader.take(limit.saturating_add(1));
    bounded.read_to_end(&mut bytes).map_err(DecodeError::Read)?;
    if (bytes.len() as u64) > limit {
        return Err(DecodeError::TooLarge { limit });
    }
    Ok(bytes)
}

struct ValidationFacts {
    negative_zeroes: Vec<usize>,
    schema_version_offset: Option<usize>,
}

fn validate_document(input: &[u8], limits: Limits) -> Result<ValidationFacts, DecodeError> {
    if let Err(error) = std::str::from_utf8(input) {
        let (line, column) = location(input, error.valid_up_to());
        return Err(DecodeError::InvalidUtf8 { line, column });
    }
    let mut parser = JsonValidator {
        input,
        offset: 0,
        members: 0,
        limits: Limits {
            max_depth: limits.max_depth.min(ABSOLUTE_MAX_DEPTH),
            ..limits
        },
        negative_zeroes: Vec::new(),
        schema_version_offset: None,
    };
    parser.space();
    parser.value(0)?;
    parser.space();
    if parser.offset != input.len() {
        let (line, column) = location(input, parser.offset);
        return Err(DecodeError::TrailingInput { line, column });
    }
    Ok(ValidationFacts {
        negative_zeroes: parser.negative_zeroes,
        schema_version_offset: parser.schema_version_offset,
    })
}

fn normalise_negative_zeroes(input: &mut [u8], positions: &[usize]) {
    for position in positions {
        if let Some(digits) = input.get_mut(*position..position.saturating_add(2))
            && let Some((minus, zero)) = digits.split_first_mut()
        {
            if *minus == b'-' {
                *minus = b'0';
            }
            if let Some(zero) = zero.first_mut()
                && *zero == b'0'
            {
                *zero = b' ';
            }
        }
    }
}

struct JsonValidator<'a> {
    input: &'a [u8],
    offset: usize,
    members: usize,
    limits: Limits,
    negative_zeroes: Vec<usize>,
    schema_version_offset: Option<usize>,
}

impl JsonValidator<'_> {
    fn value(&mut self, depth: usize) -> Result<(), DecodeError> {
        self.space();
        if depth > self.limits.max_depth {
            return Err(self.depth_error());
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(|_| ()),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(self.syntax_error()),
        }
    }

    fn object(&mut self, depth: usize) -> Result<(), DecodeError> {
        self.offset += 1;
        self.space();
        if self.consume(b'}') {
            return Ok(());
        }
        let mut keys = std::collections::BTreeSet::new();
        loop {
            self.space();
            if self.peek() != Some(b'"') {
                return Err(self.syntax_error());
            }
            let key_offset = self.offset;
            let key = self.string()?;
            if depth == 0 && key == "schema_version" {
                self.schema_version_offset = Some(key_offset);
            }
            if !keys.insert(key) {
                let (line, column) = location(self.input, key_offset);
                return Err(DecodeError::DuplicateMember { line, column });
            }
            self.member()?;
            self.space();
            if !self.consume(b':') {
                return Err(self.syntax_error());
            }
            self.value(depth + 1)?;
            self.space();
            if self.consume(b'}') {
                return Ok(());
            }
            if !self.consume(b',') {
                return Err(self.syntax_error());
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<(), DecodeError> {
        self.offset += 1;
        self.space();
        if self.consume(b']') {
            return Ok(());
        }
        loop {
            self.member()?;
            self.value(depth + 1)?;
            self.space();
            if self.consume(b']') {
                return Ok(());
            }
            if !self.consume(b',') {
                return Err(self.syntax_error());
            }
        }
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let start = self.offset;
        self.offset += 1;
        while let Some(byte) = self.peek() {
            match byte {
                b'"' => {
                    self.offset += 1;
                    let Some(encoded) = self.input.get(start..self.offset) else {
                        return Err(self.syntax_error());
                    };
                    return serde_json::from_slice(encoded).map_err(|_| {
                        let (line, column) = location(self.input, start);
                        DecodeError::Syntax { line, column }
                    });
                }
                b'\\' => {
                    self.offset += 1;
                    if self.offset >= self.input.len() {
                        return Err(self.syntax_error());
                    }
                    self.offset += 1;
                }
                _ => self.offset += 1,
            }
        }
        Err(self.syntax_error())
    }

    fn number(&mut self) -> Result<(), DecodeError> {
        let start = self.offset;
        if self.consume(b'-') && !matches!(self.peek(), Some(b'0'..=b'9')) {
            return Err(self.syntax_error());
        }
        match self.peek() {
            Some(b'0') => {
                self.offset += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(self.syntax_error());
                }
            }
            Some(b'1'..=b'9') => {
                self.offset += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.offset += 1;
                }
            }
            _ => return Err(self.syntax_error()),
        }
        let mut is_float = false;
        if self.consume(b'.') {
            is_float = true;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.syntax_error());
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.offset += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.offset += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.offset += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.syntax_error());
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.offset += 1;
            }
        }
        let Some(token) = self.input.get(start..self.offset) else {
            return Err(self.syntax_error());
        };
        if token == b"-0" {
            self.negative_zeroes.push(start);
        }
        let in_range = if is_float {
            std::str::from_utf8(token)
                .ok()
                .and_then(|text| text.parse::<f64>().ok())
                .is_some_and(f64::is_finite)
        } else if token.first() == Some(&b'-') {
            std::str::from_utf8(token)
                .ok()
                .is_some_and(|text| text.parse::<i64>().is_ok())
        } else {
            std::str::from_utf8(token)
                .ok()
                .is_some_and(|text| text.parse::<u64>().is_ok())
        };
        if !in_range {
            let (line, column) = location(self.input, start);
            return Err(if is_float {
                DecodeError::NonFinite { line, column }
            } else {
                DecodeError::IntegerOutOfRange { line, column }
            });
        }
        Ok(())
    }

    fn literal(&mut self, literal: &[u8]) -> Result<(), DecodeError> {
        let end = self.offset.saturating_add(literal.len());
        if self.input.get(self.offset..end) == Some(literal) {
            self.offset = end;
            Ok(())
        } else {
            Err(self.syntax_error())
        }
    }

    fn member(&mut self) -> Result<(), DecodeError> {
        self.members = self.members.saturating_add(1);
        if self.members > self.limits.max_members {
            let (line, column) = location(self.input, self.offset);
            Err(DecodeError::MembersExceeded {
                line,
                column,
                limit: self.limits.max_members,
            })
        } else {
            Ok(())
        }
    }

    fn space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.offset += 1;
        }
    }

    fn consume(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.offset += 1;
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.offset).copied()
    }

    fn syntax_error(&self) -> DecodeError {
        let (line, column) = location(self.input, self.offset);
        DecodeError::Syntax { line, column }
    }

    fn depth_error(&self) -> DecodeError {
        let (line, column) = location(self.input, self.offset);
        DecodeError::DepthExceeded {
            line,
            column,
            limit: self.limits.max_depth,
        }
    }
}

fn location(input: &[u8], offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut column = 1usize;
    for byte in input.iter().take(offset.min(input.len())) {
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
mod stored_context_tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct NeedsField {
        #[serde(rename = "schema_version")]
        _schema_version: u64,
        #[serde(rename = "required")]
        _required: String,
    }

    #[test]
    fn stored_syntax_error_keeps_path_and_type() {
        let error = decode_stored_record::<serde_json::Value, _>(
            b"{ ".as_slice(),
            Limits::default(),
            "/state/attempts/example/launch.json",
            "LaunchRecord",
        )
        .expect_err("malformed JSON must fail");

        assert!(matches!(error.root_cause(), DecodeError::Syntax { .. }));
        let message = error.to_string();
        assert!(message.contains("/state/attempts/example/launch.json"));
        assert!(message.contains("record type LaunchRecord"));
    }

    #[test]
    fn stored_typed_error_keeps_readable_schema_and_field_name() {
        let error = decode_stored_record::<NeedsField, _>(
            br#"{"schema_version":1}"#.as_slice(),
            Limits::default(),
            "/state/attempts/example/process-exit.json",
            "StoredExit",
        )
        .expect_err("missing required field must fail");

        assert!(matches!(
            error.root_cause(),
            DecodeError::Typed {
                field: Some(field),
                ..
            } if field == "required"
        ));
        let message = error.to_string();
        assert!(message.contains("/state/attempts/example/process-exit.json"));
        assert!(message.contains("record type StoredExit"));
        assert!(message.contains("schema version 1"));
        assert!(message.contains("missing field `required`"));
    }

    #[test]
    fn stored_v1_error_keeps_unsupported_version_classification() {
        let error = decode_stored_record_v1::<serde_json::Value, _>(
            br#"{"schema_version":2}"#.as_slice(),
            Limits::default(),
            "/state/attempts/example/launch.json",
            "LaunchRecord",
        )
        .expect_err("unsupported version must fail");

        assert!(matches!(
            error.root_cause(),
            DecodeError::UnsupportedVersion { found: 2, .. }
        ));
        assert!(error.to_string().contains("schema version 2"));
    }

    #[test]
    fn stored_size_error_keeps_path_and_type() {
        let error = decode_stored_record::<serde_json::Value, _>(
            b"{}".as_slice(),
            Limits {
                max_bytes: 1,
                ..Limits::default()
            },
            "/state/attempts/example/launch.json",
            "LaunchRecord",
        )
        .expect_err("oversized JSON must fail");

        assert!(matches!(
            error.root_cause(),
            DecodeError::TooLarge { limit: 1 }
        ));
        let message = error.to_string();
        assert!(message.contains("/state/attempts/example/launch.json"));
        assert!(message.contains("record type LaunchRecord"));
    }
}
