use serde::Serialize;
use serde::ser::{
    self, Impossible, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use thiserror::Error;

/// Canonical JSON failures are typed so callers can reject a digest request
/// without exposing serialized record contents.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CanonicalError {
    #[error("record could not be serialized as canonical JSON")]
    Serialize,
    #[error("canonical JSON does not permit floating-point numbers")]
    Float,
    #[error("canonical JSON integer is outside i64/u64 range")]
    IntegerOutOfRange,
    #[error("canonical JSON object contains duplicate keys")]
    DuplicateKey,
    #[error("canonical JSON map has an invalid key/value sequence")]
    InvalidMap,
    #[error("canonical JSON nesting exceeds the limit")]
    TooDeep,
    #[error("record kind must be non-empty and contain no NUL byte")]
    InvalidKind,
}

impl ser::Error for CanonicalError {
    fn custom<T: std::fmt::Display>(_message: T) -> Self {
        Self::Serialize
    }
}

/// The stable, versioned byte form used for Provenant record digests.
pub struct CanonicalV1;

impl CanonicalV1 {
    /// Encode compact JSON with recursively byte-sorted keys and no final LF.
    pub fn bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
        let value = value.serialize(ValueSerializer { depth: 0 })?;
        let mut output = Vec::new();
        write_value(&value, &mut output);
        Ok(output)
    }

    /// Hash a record with an explicit kind and schema-version domain.
    pub fn record_digest<T: Serialize>(
        kind: &str,
        schema_version: u64,
        value: &T,
    ) -> Result<provenant_core::Digest, CanonicalError> {
        if kind.is_empty() || kind.as_bytes().contains(&0) {
            return Err(CanonicalError::InvalidKind);
        }
        let canonical = Self::bytes(value)?;
        record_digest(kind, schema_version, &canonical)
    }

    /// Hash exact file bytes. This digest has a distinct wire type in records
    /// even though it uses the same SHA-256 primitive.
    pub fn file_digest(bytes: &[u8]) -> FileDigest {
        FileDigest(Sha256::digest(bytes).into())
    }
}

/// SHA-256 over the versioned domain separator and already-canonical bytes.
pub fn record_digest(
    kind: &str,
    schema_version: u64,
    canonical_bytes: &[u8],
) -> Result<provenant_core::Digest, CanonicalError> {
    if kind.is_empty() || kind.as_bytes().contains(&0) {
        return Err(CanonicalError::InvalidKind);
    }
    let mut digest = Sha256::new();
    digest.update(b"provenant-record\0");
    digest.update(kind.as_bytes());
    digest.update(b"\0");
    digest.update(schema_version.to_string().as_bytes());
    digest.update(b"\0");
    digest.update(canonical_bytes);
    let bytes: [u8; 32] = digest.finalize().into();
    Ok(provenant_core::Digest::from_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::{CanonicalV1, record_digest};
    use serde_json::json;

    #[test]
    fn record_digest_uses_exact_domain_framing_and_canonical_input_bytes() {
        let canonical = br#"{"a":1}"#;
        let digest = record_digest("attempt", 1, canonical).expect("record digest");
        assert_eq!(
            digest.to_string(),
            "sha256:90932cf36a28f4b07774b6fd79c078f609f79933354730c18ae02aba0cd81f09"
        );
        assert_eq!(
            CanonicalV1::record_digest("attempt", 1, &json!({"a": 1})).expect("typed digest"),
            digest
        );
        assert!(record_digest("", 1, canonical).is_err());
        assert!(record_digest("attempt\0extra", 1, canonical).is_err());
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileDigest([u8; 32]);

impl FileDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl serde::Serialize for FileDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl std::fmt::Display for FileDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[derive(Debug)]
enum CanonicalValue {
    Null,
    Bool(bool),
    Integer(String),
    String(String),
    Sequence(Vec<CanonicalValue>),
    Mapping(BTreeMap<String, CanonicalValue>),
}

#[derive(Clone, Copy)]
struct ValueSerializer {
    depth: usize,
}

impl ValueSerializer {
    fn check_depth(self) -> Result<(), CanonicalError> {
        if self.depth > 64 {
            Err(CanonicalError::TooDeep)
        } else {
            Ok(())
        }
    }

    fn sequence(self) -> Result<SequenceBuilder, CanonicalError> {
        self.check_depth()?;
        Ok(SequenceBuilder {
            depth: self.depth,
            values: Vec::new(),
        })
    }

    fn mapping(self) -> Result<MappingBuilder, CanonicalError> {
        self.check_depth()?;
        Ok(MappingBuilder {
            depth: self.depth,
            values: BTreeMap::new(),
            pending_key: None,
        })
    }
}

impl Serializer for ValueSerializer {
    type Ok = CanonicalValue;
    type Error = CanonicalError;
    type SerializeSeq = SequenceBuilder;
    type SerializeTuple = SequenceBuilder;
    type SerializeTupleStruct = SequenceBuilder;
    type SerializeTupleVariant = TupleVariantBuilder;
    type SerializeMap = MappingBuilder;
    type SerializeStruct = MappingBuilder;
    type SerializeStructVariant = StructVariantBuilder;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        self.check_depth()?;
        Ok(CanonicalValue::Bool(value))
    }

    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        self.serialize_i64(i64::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        self.check_depth()?;
        Ok(CanonicalValue::Integer(value.to_string()))
    }

    fn serialize_i128(self, value: i128) -> Result<Self::Ok, Self::Error> {
        let value = i64::try_from(value).map_err(|_| CanonicalError::IntegerOutOfRange)?;
        self.serialize_i64(value)
    }

    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        self.serialize_u64(u64::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        self.check_depth()?;
        Ok(CanonicalValue::Integer(value.to_string()))
    }

    fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
        let value = u64::try_from(value).map_err(|_| CanonicalError::IntegerOutOfRange)?;
        self.serialize_u64(value)
    }

    fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Float)
    }

    fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Float)
    }

    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(&value.to_string())
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        self.check_depth()?;
        Ok(CanonicalValue::String(value.to_owned()))
    }

    fn serialize_bytes(self, value: &[u8]) -> Result<Self::Ok, Self::Error> {
        let mut sequence = self.sequence()?;
        for byte in value {
            SerializeSeq::serialize_element(&mut sequence, byte)?;
        }
        SerializeSeq::end(sequence)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.serialize_unit()
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.check_depth()?;
        Ok(CanonicalValue::Null)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.serialize_str(variant)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let mut mapping = self.mapping()?;
        mapping.insert_value(variant.to_owned(), value)?;
        Ok(CanonicalValue::Mapping(mapping.values))
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        self.sequence()
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.sequence()
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.sequence()
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.check_depth()?;
        Ok(TupleVariantBuilder {
            variant: variant.to_owned(),
            sequence: SequenceBuilder {
                depth: self.depth.saturating_add(1),
                values: Vec::new(),
            },
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        self.mapping()
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.mapping()
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.check_depth()?;
        Ok(StructVariantBuilder {
            variant: variant.to_owned(),
            mapping: MappingBuilder {
                depth: self.depth.saturating_add(1),
                values: BTreeMap::new(),
                pending_key: None,
            },
        })
    }
}

struct SequenceBuilder {
    depth: usize,
    values: Vec<CanonicalValue>,
}

impl SequenceBuilder {
    fn push<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), CanonicalError> {
        let depth = self.depth.saturating_add(1);
        self.values
            .push(value.serialize(ValueSerializer { depth })?);
        Ok(())
    }

    fn finish(self) -> CanonicalValue {
        CanonicalValue::Sequence(self.values)
    }
}

impl SerializeSeq for SequenceBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.finish())
    }
}

impl SerializeTuple for SequenceBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.finish())
    }
}

impl SerializeTupleStruct for SequenceBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(self.finish())
    }
}

struct TupleVariantBuilder {
    variant: String,
    sequence: SequenceBuilder,
}

impl SerializeTupleVariant for TupleVariantBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.sequence.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        let mut values = BTreeMap::new();
        values.insert(self.variant, self.sequence.finish());
        Ok(CanonicalValue::Mapping(values))
    }
}

struct MappingBuilder {
    depth: usize,
    values: BTreeMap<String, CanonicalValue>,
    pending_key: Option<String>,
}

impl MappingBuilder {
    fn insert_value<T: ?Sized + Serialize>(
        &mut self,
        key: String,
        value: &T,
    ) -> Result<(), CanonicalError> {
        let depth = self.depth.saturating_add(1);
        let value = value.serialize(ValueSerializer { depth })?;
        if self.values.insert(key, value).is_some() {
            return Err(CanonicalError::DuplicateKey);
        }
        Ok(())
    }
}

impl SerializeMap for MappingBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), Self::Error> {
        if self.pending_key.is_some() {
            return Err(CanonicalError::InvalidMap);
        }
        self.pending_key = Some(key.serialize(KeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Self::Error> {
        let key = self.pending_key.take().ok_or(CanonicalError::InvalidMap)?;
        self.insert_value(key, value)
    }

    fn serialize_entry<K: ?Sized + Serialize, V: ?Sized + Serialize>(
        &mut self,
        key: &K,
        value: &V,
    ) -> Result<(), Self::Error> {
        self.insert_value(key.serialize(KeySerializer)?, value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        if self.pending_key.is_some() {
            return Err(CanonicalError::InvalidMap);
        }
        Ok(CanonicalValue::Mapping(self.values))
    }
}

impl SerializeStruct for MappingBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.insert_value(key.to_owned(), value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        Ok(CanonicalValue::Mapping(self.values))
    }
}

struct StructVariantBuilder {
    variant: String,
    mapping: MappingBuilder,
}

impl SerializeStructVariant for StructVariantBuilder {
    type Ok = CanonicalValue;
    type Error = CanonicalError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error> {
        self.mapping.insert_value(key.to_owned(), value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        let mut values = BTreeMap::new();
        values.insert(self.variant, CanonicalValue::Mapping(self.mapping.values));
        Ok(CanonicalValue::Mapping(values))
    }
}

struct KeySerializer;

impl Serializer for KeySerializer {
    type Ok = String;
    type Error = CanonicalError;
    type SerializeSeq = Impossible<String, CanonicalError>;
    type SerializeTuple = Impossible<String, CanonicalError>;
    type SerializeTupleStruct = Impossible<String, CanonicalError>;
    type SerializeTupleVariant = Impossible<String, CanonicalError>;
    type SerializeMap = Impossible<String, CanonicalError>;
    type SerializeStruct = Impossible<String, CanonicalError>;
    type SerializeStructVariant = Impossible<String, CanonicalError>;

    fn serialize_bool(self, value: bool) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_i8(self, value: i8) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_i16(self, value: i16) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_i32(self, value: i32) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_i64(self, value: i64) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_i128(self, value: i128) -> Result<Self::Ok, Self::Error> {
        let value = i64::try_from(value).map_err(|_| CanonicalError::IntegerOutOfRange)?;
        Ok(value.to_string())
    }

    fn serialize_u8(self, value: u8) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_u16(self, value: u16) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_u32(self, value: u32) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_u64(self, value: u64) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_u128(self, value: u128) -> Result<Self::Ok, Self::Error> {
        let value = u64::try_from(value).map_err(|_| CanonicalError::IntegerOutOfRange)?;
        Ok(value.to_string())
    }

    fn serialize_f32(self, _value: f32) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Float)
    }

    fn serialize_f64(self, _value: f64) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Float)
    }

    fn serialize_char(self, value: char) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_string())
    }

    fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
        Ok(value.to_owned())
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_some<T>(self, _value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Err(CanonicalError::Serialize)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        Ok(variant.to_owned())
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        Err(CanonicalError::Serialize)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Err(CanonicalError::Serialize)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Err(CanonicalError::Serialize)
    }
}

fn write_value(value: &CanonicalValue, output: &mut Vec<u8>) {
    match value {
        CanonicalValue::Null => output.extend_from_slice(b"null"),
        CanonicalValue::Bool(true) => output.extend_from_slice(b"true"),
        CanonicalValue::Bool(false) => output.extend_from_slice(b"false"),
        CanonicalValue::Integer(integer) => output.extend_from_slice(integer.as_bytes()),
        CanonicalValue::String(string) => write_string(string, output),
        CanonicalValue::Sequence(values) => {
            output.push(b'[');
            for (index, item) in values.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_value(item, output);
            }
            output.push(b']');
        }
        CanonicalValue::Mapping(values) => {
            output.push(b'{');
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            for (index, (key, item)) in entries.iter().enumerate() {
                if index > 0 {
                    output.push(b',');
                }
                write_string(key, output);
                output.push(b':');
                write_value(item, output);
            }
            output.push(b'}');
        }
    }
}

fn write_string(value: &str, output: &mut Vec<u8>) {
    output.push(b'"');
    for character in value.chars() {
        match character {
            '"' => output.extend_from_slice(b"\\\""),
            '\\' => output.extend_from_slice(b"\\\\"),
            '\u{0008}' => output.extend_from_slice(b"\\b"),
            '\t' => output.extend_from_slice(b"\\t"),
            '\n' => output.extend_from_slice(b"\\n"),
            '\u{000c}' => output.extend_from_slice(b"\\f"),
            '\r' => output.extend_from_slice(b"\\r"),
            character if character <= '\u{001f}' => {
                let byte = character as u8;
                output.extend_from_slice(b"\\u00");
                output.push(hex_digit(byte >> 4));
                output.push(hex_digit(byte & 0x0f));
            }
            character => {
                let mut buffer = [0_u8; 4];
                output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    output.push(b'"');
}

fn hex_digit(value: u8) -> u8 {
    match value {
        0..=9 => b'0' + value,
        10..=15 => b'a' + (value - 10),
        _ => b'0',
    }
}
