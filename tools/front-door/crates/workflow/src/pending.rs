//! Temporary shared types and Python compatibility helpers awaiting their owners.

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
        Value::Number(n) if n.is_f64() => provenant_core::py::python_str(value),
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

// port: belongs in provenant-records
/// An object whose key order is retained, including Python's replacement of a
/// duplicate member's value in its original position. This does not sort keys.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderedMap<T>(Vec<(String, T)>);
impl<T> OrderedMap<T> {
    pub fn get(&self, key: &str) -> Option<&T> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn insert(&mut self, key: String, value: T) {
        if let Some((_, old)) = self.0.iter_mut().find(|(k, _)| k == &key) {
            *old = value;
        } else {
            self.0.push((key, value));
        }
    }
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
    /// Python json.dumps(ensure_ascii=False, separators=(",", ":")), including
    /// CPython's finite float spelling. A byte-sensitive boundary uses this
    /// encoder instead of serde_json's different exponent notation.
    pub fn python_wire(&self) -> String {
        match self {
            Self::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::python_wire)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Object(values) => format!(
                "{{{}}}",
                values
                    .entries()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        Self::String(key.into()).python_wire(),
                        value.python_wire()
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::String(text) => {
                // Serialising an in-memory UTF-8 string cannot fail.
                let mut result = String::from("\"");
                for c in text.chars() {
                    match c {
                        '\"' => result.push_str("\\\""),
                        '\\' => result.push_str("\\\\"),
                        '\n' => result.push_str("\\n"),
                        '\r' => result.push_str("\\r"),
                        '\t' => result.push_str("\\t"),
                        '\u{8}' => result.push_str("\\b"),
                        '\u{c}' => result.push_str("\\f"),
                        c if c < ' ' => result.push_str(&format!("\\u{:04x}", u32::from(c))),
                        c => result.push(c),
                    }
                }
                result.push('\"');
                result
            }
            Self::Number(n) if n.is_f64() => {
                provenant_core::py::python_str(&serde_json::Value::Number(n.clone()))
            }
            Self::Number(n) => n.to_string(),
            Self::Bool(value) => value.to_string(),
            Self::Null => "null".into(),
        }
    }
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

// port: belongs in provenant-records
/// Preserve serde field order and render Python-compatible compact JSON bytes.
pub(crate) fn python_wire<T: serde::Serialize>(value: &T) -> Result<String, serde_json::Error> {
    let ordered: OrderedValue = serde_json::from_slice(&serde_json::to_vec(value)?)?;
    Ok(ordered.python_wire())
}

// port: belongs in provenant-records
/// Python dict.update can introduce scalar keys from a sequence of pairs.
/// Keep key identity separate from its JSON spelling: 1 and "1" coexist, while
/// 1, 1.0 and True name the same Python dictionary entry.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PythonDictionary(Vec<(OrderedValue, OrderedValue)>);
impl PythonDictionary {
    pub(crate) fn insert(&mut self, key: OrderedValue, value: OrderedValue) {
        fn equal(left: &OrderedValue, right: &OrderedValue) -> bool {
            use OrderedValue as V;
            let scalar = |value: &V| match value {
                V::Null => Some(serde_json::Value::Null),
                V::Bool(b) => Some(serde_json::Value::Bool(*b)),
                V::Number(n) => Some(serde_json::Value::Number(n.clone())),
                V::String(s) => Some(serde_json::Value::String(s.clone())),
                _ => None,
            };
            match (scalar(left), scalar(right)) {
                (Some(left), Some(right)) => provenant_core::py::python_json_eq(&left, &right),
                _ => false,
            }
        }
        if let Some((_, old)) = self.0.iter_mut().find(|(old, _)| equal(old, &key)) {
            *old = value;
        } else {
            self.0.push((key, value));
        }
    }
    fn json_key(key: &OrderedValue) -> String {
        match key {
            OrderedValue::String(text) => text.clone(),
            _ => key.python_wire(),
        }
    }
    pub fn python_wire(&self) -> String {
        format!(
            "{{{}}}",
            self.0
                .iter()
                .map(|(key, value)| format!(
                    "{}:{}",
                    OrderedValue::String(Self::json_key(key)).python_wire(),
                    value.python_wire()
                ))
                .collect::<Vec<_>>()
                .join(",")
        )
    }
}
impl serde::Serialize for PythonDictionary {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(&Self::json_key(key), value)?;
        }
        map.end()
    }
}
