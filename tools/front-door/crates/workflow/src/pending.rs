//! Temporary shared types and Python compatibility helpers awaiting their owners.

// port: belongs in provenant-core
// Unicode decimal digits, as accepted by Python re \d and int.
#[allow(dead_code)]
pub(crate) fn decimal_digit(c: char) -> Option<u8> {
    let code = u32::from(c);
    // port: belongs in provenant-core
    const ZEROS: &[u32] = &[
        0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
        0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
        0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0,
        0xff10, 0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450,
        0x114d0, 0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50,
        0x11d50, 0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce,
        0x1d7d8, 0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
    ];
    ZEROS
        .iter()
        .find_map(|&zero| code.checked_sub(zero).filter(|&d| d < 10).map(|d| d as u8))
}

// port: belongs in provenant-core
#[allow(dead_code)]
pub(crate) fn python_digit(c: char) -> bool {
    decimal_digit(c).is_some()
        || matches!(
            u32::from(c),
            0xb2 | 0xb3
                | 0xb9
                | 0x1369
                | 0x136a
                | 0x136b
                | 0x136c
                | 0x136d
                | 0x136e
                | 0x136f
                | 0x1370
                | 0x1371
                | 0x19da
                | 0x2070
                | 0x2074
                | 0x2075
                | 0x2076
                | 0x2077
                | 0x2078
                | 0x2079
                | 0x2080
                | 0x2081
                | 0x2082
                | 0x2083
                | 0x2084
                | 0x2085
                | 0x2086
                | 0x2087
                | 0x2088
                | 0x2089
                | 0x2460
                | 0x2461
                | 0x2462
                | 0x2463
                | 0x2464
                | 0x2465
                | 0x2466
                | 0x2467
                | 0x2468
                | 0x2474
                | 0x2475
                | 0x2476
                | 0x2477
                | 0x2478
                | 0x2479
                | 0x247a
                | 0x247b
                | 0x247c
                | 0x2488
                | 0x2489
                | 0x248a
                | 0x248b
                | 0x248c
                | 0x248d
                | 0x248e
                | 0x248f
                | 0x2490
                | 0x24ea
                | 0x24f5
                | 0x24f6
                | 0x24f7
                | 0x24f8
                | 0x24f9
                | 0x24fa
                | 0x24fb
                | 0x24fc
                | 0x24fd
                | 0x24ff
                | 0x2776
                | 0x2777
                | 0x2778
                | 0x2779
                | 0x277a
                | 0x277b
                | 0x277c
                | 0x277d
                | 0x277e
                | 0x2780
                | 0x2781
                | 0x2782
                | 0x2783
                | 0x2784
                | 0x2785
                | 0x2786
                | 0x2787
                | 0x2788
                | 0x278a
                | 0x278b
                | 0x278c
                | 0x278d
                | 0x278e
                | 0x278f
                | 0x2790
                | 0x2791
                | 0x2792
                | 0x10a40
                | 0x10a41
                | 0x10a42
                | 0x10a43
                | 0x10e60
                | 0x10e61
                | 0x10e62
                | 0x10e63
                | 0x10e64
                | 0x10e65
                | 0x10e66
                | 0x10e67
                | 0x10e68
                | 0x11052
                | 0x11053
                | 0x11054
                | 0x11055
                | 0x11056
                | 0x11057
                | 0x11058
                | 0x11059
                | 0x1105a
                | 0x1f100
                | 0x1f101
                | 0x1f102
                | 0x1f103
                | 0x1f104
                | 0x1f105
                | 0x1f106
                | 0x1f107
                | 0x1f108
                | 0x1f109
                | 0x1f10a
        )
}

// port: belongs in provenant-confinement
#[derive(Debug, thiserror::Error)]
pub enum OwnedFileError {
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    MultipleLinks(String),
    #[error("{message}")]
    Io {
        message: String,
        #[source]
        source: std::io::Error,
    },
}

// port: belongs in provenant-records
/// Python json.dumps(sort_keys=True), for string/integer artifact records.
pub(crate) fn python_json(value: &serde_json::Value) -> String {
    use serde_json::Value;
    fn string(s: &str) -> String {
        let mut result = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => result.push_str("\\\""),
                '\\' => result.push_str("\\\\"),
                '\n' => result.push_str("\\n"),
                '\r' => result.push_str("\\r"),
                '\t' => result.push_str("\\t"),
                '\u{8}' => result.push_str("\\b"),
                '\u{c}' => result.push_str("\\f"),
                c if !(' '..='~').contains(&c) => {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        result.push_str(&format!("\\u{unit:04x}"));
                    }
                }
                c => result.push(c),
            }
        }
        result.push('"');
        result
    }
    match value {
        Value::String(s) => string(s),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(python_json).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", string(k), python_json(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        _ => value.to_string(),
    }
}

/// Temporary finite JSON reader. Non-finite values and lone surrogates remain
/// parser integration gaps; integer overflow uses typed NumberOutOfRange.
pub(crate) fn read_python_object(
    path: &std::path::Path,
) -> Result<serde_json::Map<String, serde_json::Value>, crate::portable_workflow::PortableError> {
    use crate::portable_workflow::PortableError;
    use serde_json::Value;
    use std::fs;

    let bytes = fs::read(path).map_err(|source| PortableError::Io {
        path: path.into(),
        source,
    })?;
    let value: Value =
        provenant_records::checked_json::from_slice(&bytes).map_err(|error| match error {
            provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                PortableError::NumberOutOfRange { path: path.into() }
            }
            provenant_records::checked_json::DecodeError::Json(source) => PortableError::Json {
                path: path.into(),
                source,
            },
        })?;
    value.as_object().cloned().ok_or_else(|| {
        PortableError::Invalid(format!("{} must contain one JSON object", path.display()))
    })
}

// port: belongs in provenant-core
pub(crate) fn equals_one(value: &serde_json::Value) -> bool {
    use serde_json::Value;
    match value {
        Value::Bool(value) => *value,
        Value::Number(number) => {
            number.as_i64() == Some(1) || (number.is_f64() && number.as_f64() == Some(1.0))
        }
        _ => false,
    }
}

// port: belongs in provenant-core
pub(crate) fn splitlines(text: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            result.push(&text[start..i]);
            start = i + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, c)| *c == '\n') {
                chars.next();
                start += 1;
            }
        }
    }
    if start < text.len() {
        result.push(&text[start..]);
    }
    result
}

mod python_word;

// port: belongs in provenant-core
pub(crate) fn python_word(c: char) -> bool {
    python_word::is_word(c)
}

// port: belongs in provenant-records
/// An object whose key order is retained, including Python's replacement of a
/// duplicate member's value in its original position. This does not sort keys.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<T>(Vec<(String, T)>);
impl<T> OrderedMap<T> {
    pub(crate) fn entries_mut(&mut self) -> impl Iterator<Item = (&str, &mut T)> {
        self.0.iter_mut().map(|(k, v)| (k.as_str(), v))
    }
    pub fn entries(&self) -> impl Iterator<Item = (&str, &T)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }
}
impl<T: serde::Serialize> serde::Serialize for OrderedMap<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
impl<'de, T: serde::Deserialize<'de>> serde::Deserialize<'de> for OrderedMap<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor<T>(std::marker::PhantomData<T>);
        impl<'de, T: serde::Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
            type Value = OrderedMap<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut entries: Vec<(String, T)> = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, T>()? {
                    if let Some((_, old)) = entries.iter_mut().find(|(k, _)| k == &key) {
                        *old = value;
                    } else {
                        entries.push((key, value));
                    }
                }
                Ok(OrderedMap(entries))
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}

// port: belongs in provenant-records
/// A required JSON member may contain null. Deserialising Option without this
/// hook would also silently accept an absent member and materialise a null.
pub(crate) fn required_option<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::Deserialize<'de>,
{
    <Option<T> as serde::Deserialize>::deserialize(deserializer)
}

// port: belongs in provenant-records
/// Open JSON at cycle interfaces, retaining object order at every nesting level.
/// Decode through the shared checked-JSON boundary before accepting raw numbers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum OrderedValue {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<OrderedValue>),
    Object(OrderedMap<OrderedValue>),
}

// port: belongs in provenant-records
/// Typed retained record with the source's member order, recursively. The
/// template carries order only: serialisation always takes values from `fields`.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedRecord<T> {
    pub fields: T,
    template: OrderedValue,
}
impl<T> std::ops::Deref for OrderedRecord<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.fields
    }
}
impl<T> std::ops::DerefMut for OrderedRecord<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.fields
    }
}
impl<T: serde::de::DeserializeOwned> OrderedRecord<T> {
    pub fn decode(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        checked_ordered_decode(raw)
    }
}
impl<'de, T: serde::de::DeserializeOwned> serde::Deserialize<'de> for OrderedRecord<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let template = OrderedValue::deserialize(deserializer)?;
        let wire = serde_json::to_vec(&template).map_err(serde::de::Error::custom)?;
        let fields = serde_json::from_slice(&wire).map_err(serde::de::Error::custom)?;
        Ok(Self { fields, template })
    }
}
impl<T: serde::Serialize> serde::Serialize for OrderedRecord<T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        fn ordered(value: &mut OrderedValue, template: &OrderedValue) {
            match (value, template) {
                (OrderedValue::Object(values), OrderedValue::Object(template)) => {
                    let mut rest = std::mem::take(&mut values.0);
                    for (key, old) in &template.0 {
                        if let Some(index) = rest.iter().position(|(k, _)| k == key) {
                            let (key, mut value) = rest.remove(index);
                            ordered(&mut value, old);
                            values.0.push((key, value));
                        }
                    }
                    values.0.extend(rest);
                }
                (OrderedValue::Array(values), OrderedValue::Array(template)) => {
                    for (value, old) in values.iter_mut().zip(template) {
                        ordered(value, old);
                    }
                }
                _ => {}
            }
        }
        let raw = serde_json::to_vec(&self.fields).map_err(serde::ser::Error::custom)?;
        let mut value: OrderedValue =
            serde_json::from_slice(&raw).map_err(serde::ser::Error::custom)?;
        ordered(&mut value, &self.template);
        value.serialize(serializer)
    }
}

// port: belongs in provenant-records
/// Object wire encoding for an insertion-ordered vector of environment pairs.
pub(crate) mod ordered_pairs {
    pub fn serialize<S: serde::Serializer>(
        pairs: &[(String, String)],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(pairs.len()))?;
        for (key, value) in pairs {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(String, String)>, D::Error> {
        use serde::Deserialize;
        super::OrderedMap::<String>::deserialize(deserializer).map(|map| map.0)
    }
}

impl<T> Default for OrderedMap<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl OrderedValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }
    pub fn truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(b) => *b,
            Self::Number(n) => {
                provenant_core::py::python_truth(&serde_json::Value::Number(n.clone()))
            }
            Self::String(s) => !s.is_empty(),
            Self::Array(a) => !a.is_empty(),
            Self::Object(o) => !o.0.is_empty(),
        }
    }
    /// Python json.dumps defaults, retaining dict order and float spelling.
    pub fn python_json(&self) -> String {
        match self {
            Self::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::python_json)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Object(values) => format!(
                "{{{}}}",
                values
                    .entries()
                    .map(|(key, value)| format!(
                        "{}: {}",
                        python_json(&serde_json::Value::String(key.into())),
                        value.python_json()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Number(n) if n.is_f64() => {
                provenant_core::py::python_str(&serde_json::Value::Number(n.clone()))
            }
            Self::String(s) => python_json(&serde_json::Value::String(s.clone())),
            Self::Number(n) => n.to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Null => "null".into(),
        }
    }
}

// port: belongs in provenant-records
/// Apply the shared codec's integer normalisation without sorting objects.
pub(crate) fn checked_ordered_decode<T: serde::de::DeserializeOwned>(
    raw: &[u8],
) -> Result<T, provenant_records::checked_json::DecodeError> {
    fn normalise(value: &mut OrderedValue, checked: &serde_json::Value) {
        match value {
            OrderedValue::Number(number) => {
                if let Some(checked) = checked.as_number() {
                    *number = checked.clone();
                }
            }
            OrderedValue::Object(object) => {
                for (key, value) in object.entries_mut() {
                    if let Some(checked) = checked.get(key) {
                        normalise(value, checked);
                    }
                }
            }
            OrderedValue::Array(array) => {
                if let Some(checked) = checked.as_array() {
                    for (value, checked) in array.iter_mut().zip(checked) {
                        normalise(value, checked);
                    }
                }
            }
            _ => {}
        }
    }
    let checked = provenant_records::checked_json::from_slice(raw)?;
    let mut value: OrderedValue = serde_json::from_slice(raw)?;
    normalise(&mut value, &checked);
    Ok(serde_json::from_slice(&serde_json::to_vec(&value)?)?)
}
