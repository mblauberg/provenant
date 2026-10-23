//! CPython-compatible JSON rendering for ordered Rust records.
use serde::ser::{
    Impossible, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Serialize, Serializer};
use std::fmt;
use thiserror::Error;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DumpsOptions {
    pub sort_keys: bool,
    pub ensure_ascii: bool,
    pub indent: Option<usize>,
    pub compact: bool,
}

impl Default for DumpsOptions {
    fn default() -> Self {
        Self {
            sort_keys: false,
            ensure_ascii: true,
            indent: None,
            compact: false,
        }
    }
}

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("JSON serialization failed: {0}")]
    Serialize(String),
    #[error("JSON object key is not a supported scalar")]
    Key,
    #[error("JSON object keys of different Python types cannot be sorted")]
    UnorderableKeys,
    #[error("sorting NaN object keys requires Python's non-total sorting contract")]
    NaNKey,
    #[error(transparent)]
    NumberOutOfRange(#[from] crate::checked_json::NumberOutOfRange),
}

impl serde::ser::Error for EncodeError {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::Serialize(message.to_string())
    }
}

#[derive(Debug)]
enum ObjectKey {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    String(String),
}
impl ObjectKey {
    fn text(&self) -> String {
        match self {
            Self::Null => "null".into(),
            Self::Bool(value) => value.to_string(),
            Self::Signed(value) => value.to_string(),
            Self::Unsigned(value) => value.to_string(),
            Self::Float(value) => float_text(*value),
            Self::String(value) => value.clone(),
        }
    }

    fn numeric(&self) -> Option<crate::numeric::Numeric> {
        match self {
            Self::Bool(false) => Some(crate::numeric::Numeric::Integer(0)),
            Self::Bool(true) => Some(crate::numeric::Numeric::Integer(1)),
            Self::Signed(value) => Some(crate::numeric::Numeric::Integer(*value)),
            Self::Unsigned(value) => Some(crate::numeric::Numeric::Unsigned(*value)),
            Self::Float(value) => Some(crate::numeric::Numeric::Float(*value)),
            Self::Null | Self::String(_) => None,
        }
    }

    fn python_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::String(left), Self::String(right)) => left == right,
            _ => match (self.numeric(), other.numeric()) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            },
        }
    }

    fn comparable_with(&self, other: &Self) -> bool {
        matches!((self, other), (Self::String(_), Self::String(_)))
            || (self.numeric().is_some() && other.numeric().is_some())
    }

    fn sort_cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::String(left), Self::String(right)) => left.cmp(right),
            (Self::Null, Self::Null) => std::cmp::Ordering::Equal,
            _ => match (self.numeric(), other.numeric()) {
                (Some(left), Some(right)) => left
                    .partial_cmp(&right)
                    .unwrap_or(std::cmp::Ordering::Equal),
                _ => std::cmp::Ordering::Equal,
            },
        }
    }
}

#[derive(Debug)]
enum Json {
    Null,
    Bool(bool),
    Signed(i64),
    Unsigned(u64),
    Float(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(ObjectKey, Json)>),
}

struct Tree;
impl Serializer for Tree {
    type Ok = Json;
    type Error = EncodeError;
    type SerializeSeq = Sequence;
    type SerializeTuple = Sequence;
    type SerializeTupleStruct = Sequence;
    type SerializeTupleVariant = TupleVariant;
    type SerializeMap = Object;
    type SerializeStruct = Struct;
    type SerializeStructVariant = StructVariant;

    fn serialize_bool(self, v: bool) -> Result<Json, EncodeError> {
        Ok(Json::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<Json, EncodeError> {
        Ok(Json::Signed(v.into()))
    }
    fn serialize_i16(self, v: i16) -> Result<Json, EncodeError> {
        Ok(Json::Signed(v.into()))
    }
    fn serialize_i32(self, v: i32) -> Result<Json, EncodeError> {
        Ok(Json::Signed(v.into()))
    }
    fn serialize_i64(self, v: i64) -> Result<Json, EncodeError> {
        Ok(Json::Signed(v))
    }
    fn serialize_i128(self, v: i128) -> Result<Json, EncodeError> {
        if let Ok(v) = i64::try_from(v) {
            Ok(Json::Signed(v))
        } else if let Ok(v) = u64::try_from(v) {
            Ok(Json::Unsigned(v))
        } else {
            Err(EncodeError::NumberOutOfRange(
                crate::checked_json::NumberOutOfRange {
                    value: v.to_string(),
                },
            ))
        }
    }
    fn serialize_u8(self, v: u8) -> Result<Json, EncodeError> {
        Ok(Json::Unsigned(v.into()))
    }
    fn serialize_u16(self, v: u16) -> Result<Json, EncodeError> {
        Ok(Json::Unsigned(v.into()))
    }
    fn serialize_u32(self, v: u32) -> Result<Json, EncodeError> {
        Ok(Json::Unsigned(v.into()))
    }
    fn serialize_u64(self, v: u64) -> Result<Json, EncodeError> {
        Ok(Json::Unsigned(v))
    }
    fn serialize_u128(self, v: u128) -> Result<Json, EncodeError> {
        u64::try_from(v).map(Json::Unsigned).map_err(|_| {
            EncodeError::NumberOutOfRange(crate::checked_json::NumberOutOfRange {
                value: v.to_string(),
            })
        })
    }
    fn serialize_f32(self, v: f32) -> Result<Json, EncodeError> {
        Ok(Json::Float(v.into()))
    }
    fn serialize_f64(self, v: f64) -> Result<Json, EncodeError> {
        Ok(Json::Float(v))
    }
    fn serialize_char(self, v: char) -> Result<Json, EncodeError> {
        Ok(Json::String(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<Json, EncodeError> {
        Ok(Json::String(v.into()))
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<Json, EncodeError> {
        Err(EncodeError::Serialize("bytes are not JSON values".into()))
    }
    fn serialize_none(self) -> Result<Json, EncodeError> {
        Ok(Json::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<Json, EncodeError> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<Json, EncodeError> {
        Ok(Json::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<Json, EncodeError> {
        Ok(Json::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
    ) -> Result<Json, EncodeError> {
        Ok(Json::String(variant.into()))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<Json, EncodeError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        v: &T,
    ) -> Result<Json, EncodeError> {
        let mut object = Object::default();
        object.put_field(variant, v.serialize(Tree)?);
        Ok(Json::Object(object.entries))
    }
    fn serialize_seq(self, len: Option<usize>) -> Result<Sequence, EncodeError> {
        Ok(Sequence(Vec::with_capacity(len.unwrap_or(0))))
    }
    fn serialize_tuple(self, len: usize) -> Result<Sequence, EncodeError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_struct(self, _: &'static str, len: usize) -> Result<Sequence, EncodeError> {
        self.serialize_seq(Some(len))
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<TupleVariant, EncodeError> {
        Ok(TupleVariant {
            name: variant,
            values: Vec::with_capacity(len),
        })
    }
    fn serialize_map(self, len: Option<usize>) -> Result<Object, EncodeError> {
        Ok(Object {
            entries: Vec::with_capacity(len.unwrap_or(0)),
            key: None,
        })
    }
    fn serialize_struct(self, _: &'static str, len: usize) -> Result<Struct, EncodeError> {
        Ok(Struct(Object {
            entries: Vec::with_capacity(len),
            key: None,
        }))
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<StructVariant, EncodeError> {
        Ok(StructVariant {
            name: variant,
            fields: Object {
                entries: Vec::with_capacity(len),
                key: None,
            },
        })
    }
}

#[derive(Default)]
struct Object {
    entries: Vec<(ObjectKey, Json)>,
    key: Option<ObjectKey>,
}
impl Object {
    fn put(&mut self, key: ObjectKey, value: Json) {
        // Dict assignment retains the first equal key and its position. Equality
        // applies across bool/int/float; distinct keys 1 and "1" remain distinct
        // even though their JSON string spellings collide.
        if let Some((_, previous)) = self.entries.iter_mut().find(|(old, _)| old.python_eq(&key)) {
            *previous = value;
        } else {
            self.entries.push((key, value));
        }
    }

    fn put_field(&mut self, key: &'static str, value: Json) {
        self.entries.push((ObjectKey::String(key.into()), value));
    }
}
#[derive(Default)]
struct Sequence(Vec<Json>);
impl SerializeSeq for Sequence {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), EncodeError> {
        self.0.push(value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        Ok(Json::Array(self.0))
    }
}
impl SerializeTuple for Sequence {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), EncodeError> {
        self.0.push(value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        Ok(Json::Array(self.0))
    }
}
impl SerializeTupleStruct for Sequence {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), EncodeError> {
        self.0.push(value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        Ok(Json::Array(self.0))
    }
}
impl SerializeMap for Object {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<(), EncodeError> {
        self.key = Some(key.serialize(KeyTree)?);
        Ok(())
    }
    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), EncodeError> {
        let key = self
            .key
            .take()
            .ok_or_else(|| EncodeError::Serialize("map value without key".into()))?;
        self.put(key, value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        if self.key.is_some() {
            return Err(EncodeError::Serialize("map key without value".into()));
        }
        Ok(Json::Object(self.entries))
    }
}
impl SerializeStruct for Struct {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), EncodeError> {
        self.0.put_field(key, value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        Ok(Json::Object(self.0.entries))
    }
}
struct Struct(Object);
impl SerializeStructVariant for StructVariant {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), EncodeError> {
        self.fields.put_field(key, value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        let mut object = Object::default();
        object.put_field(self.name, Json::Object(self.fields.entries));
        Ok(Json::Object(object.entries))
    }
}
struct StructVariant {
    name: &'static str,
    fields: Object,
}
struct TupleVariant {
    name: &'static str,
    values: Vec<Json>,
}
impl SerializeTupleVariant for TupleVariant {
    type Ok = Json;
    type Error = EncodeError;
    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), EncodeError> {
        self.values.push(value.serialize(Tree)?);
        Ok(())
    }
    fn end(self) -> Result<Json, EncodeError> {
        let mut object = Object::default();
        object.put_field(self.name, Json::Array(self.values));
        Ok(Json::Object(object.entries))
    }
}

struct KeyTree;
impl Serializer for KeyTree {
    type Ok = ObjectKey;
    type Error = EncodeError;
    type SerializeSeq = Impossible<ObjectKey, EncodeError>;
    type SerializeTuple = Impossible<ObjectKey, EncodeError>;
    type SerializeTupleStruct = Impossible<ObjectKey, EncodeError>;
    type SerializeTupleVariant = Impossible<ObjectKey, EncodeError>;
    type SerializeMap = Impossible<ObjectKey, EncodeError>;
    type SerializeStruct = Impossible<ObjectKey, EncodeError>;
    type SerializeStructVariant = Impossible<ObjectKey, EncodeError>;
    fn serialize_bool(self, v: bool) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Bool(v))
    }
    fn serialize_i8(self, v: i8) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Signed(v.into()))
    }
    fn serialize_i16(self, v: i16) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Signed(v.into()))
    }
    fn serialize_i32(self, v: i32) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Signed(v.into()))
    }
    fn serialize_i64(self, v: i64) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Signed(v))
    }
    fn serialize_i128(self, v: i128) -> Result<ObjectKey, EncodeError> {
        if let Ok(value) = i64::try_from(v) {
            Ok(ObjectKey::Signed(value))
        } else if let Ok(value) = u64::try_from(v) {
            Ok(ObjectKey::Unsigned(value))
        } else {
            Err(EncodeError::NumberOutOfRange(
                crate::checked_json::NumberOutOfRange {
                    value: v.to_string(),
                },
            ))
        }
    }
    fn serialize_u8(self, v: u8) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Unsigned(v.into()))
    }
    fn serialize_u16(self, v: u16) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Unsigned(v.into()))
    }
    fn serialize_u32(self, v: u32) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Unsigned(v.into()))
    }
    fn serialize_u64(self, v: u64) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Unsigned(v))
    }
    fn serialize_u128(self, v: u128) -> Result<ObjectKey, EncodeError> {
        u64::try_from(v).map(ObjectKey::Unsigned).map_err(|_| {
            EncodeError::NumberOutOfRange(crate::checked_json::NumberOutOfRange {
                value: v.to_string(),
            })
        })
    }
    fn serialize_f32(self, v: f32) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Float(v.into()))
    }
    fn serialize_f64(self, v: f64) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Float(v))
    }
    fn serialize_char(self, v: char) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::String(v.to_string()))
    }
    fn serialize_str(self, v: &str) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::String(v.into()))
    }
    fn serialize_bytes(self, _: &[u8]) -> Result<ObjectKey, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_none(self) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Null)
    }
    fn serialize_some<T: ?Sized + Serialize>(self, v: &T) -> Result<ObjectKey, EncodeError> {
        v.serialize(self)
    }
    fn serialize_unit(self) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Null)
    }
    fn serialize_unit_struct(self, _: &'static str) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::Null)
    }
    fn serialize_unit_variant(
        self,
        _: &'static str,
        _: u32,
        v: &'static str,
    ) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::String(v.into()))
    }
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        v: &T,
    ) -> Result<ObjectKey, EncodeError> {
        v.serialize(self)
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _: &'static str,
        _: u32,
        v: &'static str,
        _: &T,
    ) -> Result<ObjectKey, EncodeError> {
        Ok(ObjectKey::String(v.into()))
    }
    fn serialize_seq(self, _: Option<usize>) -> Result<Self::SerializeSeq, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_tuple(self, _: usize) -> Result<Self::SerializeTuple, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_tuple_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleStruct, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_tuple_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeTupleVariant, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_map(self, _: Option<usize>) -> Result<Self::SerializeMap, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_struct(
        self,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStruct, EncodeError> {
        Err(EncodeError::Key)
    }
    fn serialize_struct_variant(
        self,
        _: &'static str,
        _: u32,
        _: &'static str,
        _: usize,
    ) -> Result<Self::SerializeStructVariant, EncodeError> {
        Err(EncodeError::Key)
    }
}

fn float_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".into()
    } else if value == f64::INFINITY {
        "Infinity".into()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".into()
    } else {
        match serde_json::Number::from_f64(value) {
            Some(number) => provenant_core::py::python_str(&serde_json::Value::Number(number)),
            None => value.to_string(),
        }
    }
}

#[derive(Debug, Error)]
#[error("path cannot be represented as a Python JSON string")]
pub enum PathEncodingError {
    Unsupported,
}

/// Encode `str(path)` with Python's Unix surrogateescape and ensure_ascii rules.
pub fn path_json(path: &std::path::Path) -> Result<String, PathEncodingError> {
    #[cfg(not(unix))]
    if path.to_str().is_none() {
        return Err(PathEncodingError::Unsupported);
    }
    use std::fmt::Write;
    let mut output = String::from("\"");
    for chunk in path.as_os_str().as_encoded_bytes().utf8_chunks() {
        for c in chunk.valid().chars() {
            match c {
                '\"' => output.push_str("\\\""),
                '\\' => output.push_str("\\\\"),
                '\u{8}' => output.push_str("\\b"),
                '\u{c}' => output.push_str("\\f"),
                '\n' => output.push_str("\\n"),
                '\r' => output.push_str("\\r"),
                '\t' => output.push_str("\\t"),
                '\u{20}'..='\u{7e}' => output.push(c),
                _ => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        write!(&mut output, "\\u{unit:04x}")
                            .map_err(|_| PathEncodingError::Unsupported)?;
                    }
                }
            }
        }
        for byte in chunk.invalid() {
            write!(&mut output, "\\udc{byte:02x}").map_err(|_| PathEncodingError::Unsupported)?;
        }
    }
    output.push('\"');
    Ok(output)
}

fn quote(text: &str, ensure_ascii: bool, out: &mut String) {
    use std::fmt::Write;
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < '\u{20}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if ensure_ascii && c >= '\u{7f}' => {
                let mut units = [0; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn render(
    value: &Json,
    options: DumpsOptions,
    depth: usize,
    out: &mut String,
) -> Result<(), EncodeError> {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
        Json::Signed(v) => out.push_str(&v.to_string()),
        Json::Unsigned(v) => out.push_str(&v.to_string()),
        Json::Float(v) => out.push_str(&float_text(*v)),
        Json::String(v) => quote(v, options.ensure_ascii, out),
        Json::Array(values) => {
            out.push('[');
            if !values.is_empty() {
                let pretty = options.indent.is_some();
                if pretty {
                    out.push('\n');
                }
                for (index, item) in values.iter().enumerate() {
                    if pretty {
                        out.push_str(
                            &" ".repeat(options.indent.unwrap_or(0).saturating_mul(depth + 1)),
                        );
                    }
                    render(item, options, depth + 1, out)?;
                    if index + 1 != values.len() {
                        out.push(',');
                        if !pretty && !options.compact {
                            out.push(' ');
                        }
                    }
                    if pretty {
                        out.push('\n');
                    }
                }
                if pretty {
                    out.push_str(&" ".repeat(options.indent.unwrap_or(0).saturating_mul(depth)));
                }
            }
            out.push(']');
        }
        Json::Object(entries) => {
            out.push('{');
            if !entries.is_empty() {
                let mut entries: Vec<_> = entries.iter().collect();
                if options.sort_keys {
                    if entries
                        .iter()
                        .any(|(key, _)| matches!(key, ObjectKey::Float(value) if value.is_nan()))
                    {
                        return Err(EncodeError::NaNKey);
                    }
                    let first = &entries[0].0;
                    if entries
                        .iter()
                        .skip(1)
                        .any(|(key, _)| !first.comparable_with(key))
                    {
                        return Err(EncodeError::UnorderableKeys);
                    }
                    entries.sort_by(|a, b| a.0.sort_cmp(&b.0));
                }
                let pretty = options.indent.is_some();
                if pretty {
                    out.push('\n');
                }
                for (index, (key, item)) in entries.iter().enumerate() {
                    if pretty {
                        out.push_str(
                            &" ".repeat(options.indent.unwrap_or(0).saturating_mul(depth + 1)),
                        );
                    }
                    quote(&key.text(), options.ensure_ascii, out);
                    out.push(':');
                    if !options.compact {
                        out.push(' ');
                    }
                    render(item, options, depth + 1, out)?;
                    if index + 1 != entries.len() {
                        out.push(',');
                        if !pretty && !options.compact {
                            out.push(' ');
                        }
                    }
                    if pretty {
                        out.push('\n');
                    }
                }
                if pretty {
                    out.push_str(&" ".repeat(options.indent.unwrap_or(0).saturating_mul(depth)));
                }
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Render any serde record with Python `json.dumps` ordering, indentation, ASCII,
/// and separator behavior. Struct field and map insertion order is retained unless sorted.
pub fn dumps<T: ?Sized + Serialize>(
    value: &T,
    options: DumpsOptions,
) -> Result<String, EncodeError> {
    let value = value.serialize(Tree)?;
    let mut output = String::new();
    render(&value, options, 0, &mut output)?;
    Ok(output)
}

/// Source `telemetry.identity(*values)`: hash Python's sorted default JSON tuple bytes.
pub fn identity(values: &[serde_json::Value]) -> Result<String, EncodeError> {
    Ok(crate::sha256_hex(
        dumps(
            values,
            DumpsOptions {
                sort_keys: true,
                ..DumpsOptions::default()
            },
        )?
        .as_bytes(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    use serde::ser::SerializeMap;
    use std::process::Command;

    enum MapKey<'a> {
        Integer(i64),
        Bool(bool),
        Float(f64),
        Text(&'a str),
    }
    impl Serialize for MapKey<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            match self {
                Self::Integer(value) => serializer.serialize_i64(*value),
                Self::Bool(value) => serializer.serialize_bool(*value),
                Self::Float(value) => serializer.serialize_f64(*value),
                Self::Text(value) => serializer.serialize_str(value),
            }
        }
    }
    struct KeyMap<'a>(Vec<(MapKey<'a>, &'a str)>);
    impl Serialize for KeyMap<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(self.0.len()))?;
            for (key, value) in &self.0 {
                map.serialize_entry(key, value)?;
            }
            map.end()
        }
    }

    fn bounded_output(
        command: &mut Command,
    ) -> Result<crate::oracle::CapturedOutput, std::io::Error> {
        crate::oracle::run(command)
    }

    #[derive(Serialize)]
    struct Ordered<'a> {
        z: &'a str,
        a: &'a str,
        n: f64,
        nested: Vec<&'a str>,
    }

    #[test]
    fn float_thresholds_match_python_json_defaults() {
        let values = [
            0.0,
            -0.0,
            1e-7,
            1e-6,
            1e-5,
            0.0001,
            1e15,
            1e16,
            1e20,
            1.2345678901234567e-5,
        ];
        assert_eq!(
            dumps(&values, DumpsOptions::default()).unwrap(),
            "[0.0, -0.0, 1e-07, 1e-06, 1e-05, 0.0001, 1000000000000000.0, 1e+16, 1e+20, 1.2345678901234568e-05]"
        );
    }

    #[test]
    fn integer_encoding_reports_typed_wire_range_errors() {
        assert!(matches!(
            dumps(&(i128::MIN), DumpsOptions::default()),
            Err(EncodeError::NumberOutOfRange(
                crate::checked_json::NumberOutOfRange { .. }
            ))
        ));
        assert!(matches!(
            dumps(&(u128::MAX), DumpsOptions::default()),
            Err(EncodeError::NumberOutOfRange(
                crate::checked_json::NumberOutOfRange { .. }
            ))
        ));
    }

    #[test]
    fn python_dump_modes_preserve_order_and_float_spelling() {
        let value = Ordered {
            z: "é😀\u{7f}",
            a: "line\n",
            n: 1e-5,
            nested: vec!["x", "y"],
        };
        let cases = [
            (
                DumpsOptions::default(),
                r#"{"z": "\u00e9\ud83d\ude00\u007f", "a": "line\n", "n": 1e-05, "nested": ["x", "y"]}"#,
            ),
            (
                DumpsOptions {
                    sort_keys: true,
                    ..Default::default()
                },
                r#"{"a": "line\n", "n": 1e-05, "nested": ["x", "y"], "z": "\u00e9\ud83d\ude00\u007f"}"#,
            ),
            (
                DumpsOptions {
                    ensure_ascii: false,
                    indent: Some(2),
                    ..Default::default()
                },
                "{\n  \"z\": \"é😀\u{7f}\",\n  \"a\": \"line\\n\",\n  \"n\": 1e-05,\n  \"nested\": [\n    \"x\",\n    \"y\"\n  ]\n}",
            ),
            (
                DumpsOptions {
                    compact: true,
                    ..Default::default()
                },
                r#"{"z":"\u00e9\ud83d\ude00\u007f","a":"line\n","n":1e-05,"nested":["x","y"]}"#,
            ),
        ];
        for (options, expected) in cases {
            assert_eq!(dumps(&value, options).unwrap(), expected);
        }
    }

    #[cfg(unix)]
    #[test]
    fn path_json_matches_python_surrogateescape_and_ascii() {
        use std::{io::Write, os::unix::ffi::OsStrExt, process::Stdio};
        let fixtures: Vec<Vec<u8>> = vec![
            b"/p/\xff".to_vec(),
            b"/a/\x7f\x08\x0c\t\n\r\"\\".to_vec(),
            "/prød😀".as_bytes().to_vec(),
        ];
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        let input = serde_json::to_vec(&fixtures).unwrap();
        command
            .args([
                "-c",
                "import json,os,sys; print(json.dumps([json.dumps(os.fsdecode(bytes(v))) for v in json.load(sys.stdin)]))",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            crate::oracle::spawn(&mut command).expect("configured Python oracle failed to start");
        if let Err(error) = child
            .child_mut()
            .stdin
            .take()
            .expect("Python oracle stdin was not piped")
            .write_all(&input)
        {
            let _ = child.wait();
            panic!("could not send path fixtures to Python oracle: {error}");
        }
        let output = child
            .wait()
            .expect("Python oracle failed or timed out after interpreter gate");
        assert!(
            output.status.success(),
            "Python oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
        let actual: Vec<String> = fixtures
            .iter()
            .map(|raw| path_json(std::path::Path::new(std::ffi::OsStr::from_bytes(raw))).unwrap())
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn equal_numeric_map_keys_overwrite_without_moving_the_first_key() {
        let map = KeyMap(vec![
            (MapKey::Integer(1), "int"),
            (MapKey::Text("x"), "x"),
            (MapKey::Bool(true), "bool"),
            (MapKey::Float(1.0), "float"),
        ]);
        let actual = dumps(&map, DumpsOptions::default()).unwrap();
        assert_eq!(actual, r#"{"1": "float", "x": "x"}"#);
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        let output = crate::oracle::run(command.args(["-c", "import json; d={1:'int','x':'x'}; d[True]='bool'; d[1.0]='float'; print(json.dumps(d))"])).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), actual);
    }

    #[test]
    fn sorted_nan_keys_have_an_explicit_library_boundary() {
        let map = KeyMap(vec![
            (MapKey::Float(3.0), "a"),
            (MapKey::Float(2.0), "b"),
            (MapKey::Float(f64::NAN), "c"),
            (MapKey::Float(1.0), "d"),
        ]);
        assert!(matches!(
            dumps(
                &map,
                DumpsOptions {
                    sort_keys: true,
                    ..Default::default()
                }
            ),
            Err(EncodeError::NaNKey)
        ));
        assert!(
            dumps(&map, DumpsOptions::default())
                .unwrap()
                .contains("NaN")
        );
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        let output = crate::oracle::run(command.args(["-c", "import json; print(json.dumps({3.0:'a',2.0:'b',float('nan'):'c',1.0:'d'},sort_keys=True))"])).unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap().trim(),
            r#"{"2.0": "b", "NaN": "c", "1.0": "d", "3.0": "a"}"#
        );
    }

    #[test]
    fn a_single_null_key_needs_no_ordering() {
        let map = std::collections::BTreeMap::from([(None::<u8>, "v")]);
        assert_eq!(
            dumps(
                &map,
                DumpsOptions {
                    sort_keys: true,
                    ..Default::default()
                }
            )
            .unwrap(),
            r#"{"null": "v"}"#
        );
    }

    #[test]
    fn python_map_keys_sort_before_stringification_and_keep_collisions() {
        let numeric = KeyMap(vec![
            (MapKey::Integer(10), "ten"),
            (MapKey::Float(2.5), "two and a half"),
            (MapKey::Integer(2), "two"),
        ]);
        let collision = KeyMap(vec![
            (MapKey::Integer(1), "int"),
            (MapKey::Text("1"), "str"),
        ]);
        let incomparable = KeyMap(vec![
            (MapKey::Integer(1), "number"),
            (MapKey::Text("a"), "text"),
        ]);
        let numeric_json = dumps(
            &numeric,
            DumpsOptions {
                sort_keys: true,
                ..Default::default()
            },
        )
        .unwrap();
        let collision_json = dumps(&collision, DumpsOptions::default()).unwrap();
        assert_eq!(
            numeric_json,
            r#"{"2": "two", "2.5": "two and a half", "10": "ten"}"#
        );
        assert_eq!(collision_json, r#"{"1": "int", "1": "str"}"#);
        assert!(matches!(
            dumps(
                &incomparable,
                DumpsOptions {
                    sort_keys: true,
                    ..Default::default()
                }
            ),
            Err(EncodeError::UnorderableKeys)
        ));

        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        command
            .args([
                "-c",
                r#"import json
print(json.dumps({10:'ten',2.5:'two and a half',2:'two'},sort_keys=True))
print(json.dumps({1:'int','1':'str'}))
try:
 json.dumps({1:'number','a':'text'},sort_keys=True)
except TypeError:
 print('TypeError')"#,
            ])
            .stdin(std::process::Stdio::null());
        let output = crate::oracle::run(&mut command)
            .expect("Python oracle failed or timed out after interpreter gate");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = String::from_utf8(output.stdout).unwrap();
        assert_eq!(
            expected,
            format!("{numeric_json}\n{collision_json}\nTypeError\n")
        );
    }

    #[test]
    fn differential_python_json_dump_options() {
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        command
            .args([
                "-c",
                "import json; d={'z':'é😀\\x7f','a':'line\\n','n':1e-5,'nested':['x','y']}; print(json.dumps(d, sort_keys=True, ensure_ascii=False, indent=2))",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let output = bounded_output(&mut command)
            .expect("Python oracle failed or timed out after interpreter gate");
        assert!(
            output.status.success(),
            "Python oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected = String::from_utf8(output.stdout).unwrap();
        let value = Ordered {
            z: "é😀\u{7f}",
            a: "line\n",
            n: 1e-5,
            nested: vec!["x", "y"],
        };
        assert_eq!(
            dumps(
                &value,
                DumpsOptions {
                    sort_keys: true,
                    ensure_ascii: false,
                    indent: Some(2),
                    compact: false
                }
            )
            .unwrap()
                + "\n",
            expected
        );
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn identity_matches_the_python_source_for_unicode_float_and_key_order() {
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../scripts/telemetry.py");
        let value = crate::ordered_json::from_str(
            r#"[{"z":"é😀","a":0.09090909090909091},9007199254740993,-0.0]"#,
        )
        .unwrap()
        .to_value();
        let values = value.as_array().unwrap();
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        let input = dumps(values, DumpsOptions::default()).unwrap();
        let output = crate::oracle::run(command.args(["-c", "import importlib.util,json,sys; s=importlib.util.spec_from_file_location('telemetry_oracle',sys.argv[1]); m=importlib.util.module_from_spec(s); s.loader.exec_module(m); print(m.identity(*json.loads(sys.argv[2])))"]).arg(source).arg(input).stdin(std::process::Stdio::null())).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            identity(values).unwrap() + "\n",
            String::from_utf8(output.stdout).unwrap()
        );
    }
}
