//! Minimal insertion-ordered JSON while the records owner has no ordered codec.
// port: belongs in provenant-records
use serde::{
    de::{MapAccess, SeqAccess, Visitor},
    ser::{SerializeMap, SerializeSeq},
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::{Number, Value};
use std::{fmt, ops::Index};

#[derive(Debug, Clone, PartialEq)]
// port: belongs in provenant-records
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}
// port: belongs in provenant-records
impl Default for Json {
    fn default() -> Self {
        Self::Object(vec![])
    }
}
// port: belongs in provenant-records
impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => s.serialize_unit(),
            Self::Bool(v) => s.serialize_bool(*v),
            Self::Number(v) => v.serialize(s),
            Self::String(v) => s.serialize_str(v),
            Self::Array(values) => {
                let mut seq = s.serialize_seq(Some(values.len()))?;
                for v in values {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Self::Object(values) => {
                let mut map = s.serialize_map(Some(values.len()))?;
                for (k, v) in values {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}
// port: belongs in provenant-records
impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct JsonVisitor;
        impl<'de> Visitor<'de> for JsonVisitor {
            type Value = Json;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Json, E> {
                Ok(Json::Bool(v))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Json, E> {
                Ok(Json::Number(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Json, E> {
                Ok(Json::Number(v.into()))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Json, E> {
                Number::from_f64(v)
                    .map(Json::Number)
                    .ok_or_else(|| E::custom("non-finite JSON number"))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Json, E> {
                Ok(Json::String(v.into()))
            }
            fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Json, E> {
                Ok(Json::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut values = vec![];
                while let Some(v) = a.next_element()? {
                    values.push(v);
                }
                Ok(Json::Array(values))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut values: Vec<(String, Json)> = vec![];
                while let Some((k, v)) = a.next_entry()? {
                    if let Some((_, old)) = values.iter_mut().find(|(key, _)| *key == k) {
                        *old = v;
                    } else {
                        values.push((k, v));
                    }
                }
                Ok(Json::Object(values))
            }
        }
        d.deserialize_any(JsonVisitor)
    }
}
// port: belongs in provenant-records
impl Json {
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.object()?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
    pub fn object(&self) -> Option<&[(String, Self)]> {
        if let Self::Object(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn array(&self) -> Option<&[Self]> {
        if let Self::Array(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn string(&self) -> Option<&str> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn truth(&self) -> bool {
        provenant_core::py::python_truth(&self.value())
    }
    pub fn value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(v) => Value::Bool(*v),
            Self::Number(v) => Value::Number(v.clone()),
            Self::String(v) => Value::String(v.clone()),
            Self::Array(v) => Value::Array(v.iter().map(Self::value).collect()),
            Self::Object(v) => {
                Value::Object(v.iter().map(|(k, v)| (k.clone(), v.value())).collect())
            }
        }
    }
    pub fn set(&mut self, key: impl Into<String>, value: Self) -> crate::Result<()> {
        let Self::Object(items) = self else {
            return Err(crate::invalid("JSON value is not an object"));
        };
        let key = key.into();
        if let Some((_, old)) = items.iter_mut().find(|(k, _)| k == &key) {
            *old = value;
        } else {
            items.push((key, value));
        }
        Ok(())
    }
    pub fn remove(&mut self, key: &str) {
        if let Self::Object(items) = self {
            items.retain(|(k, _)| k != key);
        }
    }
    pub fn from_value(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(v) => Self::Bool(v),
            Value::Number(v) => Self::Number(v),
            Value::String(v) => Self::String(v),
            Value::Array(v) => Self::Array(v.into_iter().map(Self::from_value).collect()),
            Value::Object(v) => Self::Object(
                v.into_iter()
                    .map(|(k, v)| (k, Self::from_value(v)))
                    .collect(),
            ),
        }
    }
    fn normalize(&mut self, checked: &Value) {
        match (self, checked) {
            (Self::Object(items), Value::Object(map)) => {
                for (k, v) in items {
                    if let Some(checked) = map.get(k) {
                        v.normalize(checked);
                    }
                }
            }
            (Self::Array(items), Value::Array(values)) => {
                for (v, checked) in items.iter_mut().zip(values) {
                    v.normalize(checked);
                }
            }
            (value, checked) => *value = Self::from_value(checked.clone()),
        }
    }
}
// port: belongs in provenant-records
impl Index<&str> for Json {
    type Output = Self;
    fn index(&self, k: &str) -> &Self {
        self.get(k).unwrap_or(&Json::Null)
    }
}
// port: belongs in provenant-records
pub(crate) fn decode(
    bytes: &[u8],
    context: &'static str,
    path: &std::path::Path,
) -> crate::Result<Json> {
    let checked = provenant_records::checked_json::from_slice(bytes).map_err(|source| {
        crate::Error::CheckedJson {
            context,
            path: path.into(),
            source,
        }
    })?;
    let mut value: Json = serde_json::from_slice(bytes).map_err(|source| crate::Error::Json {
        context,
        path: path.into(),
        source,
    })?;
    value.normalize(&checked);
    Ok(value)
}

/// Python json.dumps layout with order retained, ascii escaping and exact numbers.
// port: belongs in provenant-records
pub(crate) fn dumps(value: &Json, pretty: bool, sorted: bool) -> crate::Result<String> {
    fn encode(
        value: &Json,
        pretty: bool,
        sorted: bool,
        depth: usize,
        out: &mut String,
    ) -> crate::Result<()> {
        let newline = |out: &mut String, depth: usize| {
            if pretty {
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
        };
        match value {
            Json::Array(items) if !items.is_empty() => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                        if !pretty {
                            out.push(' ');
                        }
                    }
                    newline(out, depth + 1);
                    encode(v, pretty, sorted, depth + 1, out)?;
                }
                newline(out, depth);
                out.push(']');
            }
            Json::Object(items) if !items.is_empty() => {
                let mut items: Vec<_> = items.iter().collect();
                if sorted {
                    items.sort_by(|a, b| a.0.cmp(&b.0));
                }
                out.push('{');
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                        if !pretty {
                            out.push(' ');
                        }
                    }
                    newline(out, depth + 1);
                    out.push_str(&crate::pending::json_string(k)?);
                    out.push_str(": ");
                    encode(v, pretty, sorted, depth + 1, out)?;
                }
                newline(out, depth);
                out.push('}');
            }
            Json::String(s) => out.push_str(&crate::pending::json_string(s)?),
            Json::Number(_) => out.push_str(&provenant_core::py::python_str(&value.value())),
            _ => out.push_str(&value.value().to_string()),
        }
        Ok(())
    }
    let mut out = String::new();
    encode(value, pretty, sorted, 0, &mut out)?;
    Ok(out)
}

// port: belongs in provenant-records
pub(crate) fn serialize_pairs<S: Serializer, T: Serialize>(
    items: &[(String, T)],
    s: S,
) -> Result<S::Ok, S::Error> {
    let mut map = s.serialize_map(Some(items.len()))?;
    for (k, v) in items {
        map.serialize_entry(k, v)?;
    }
    map.end()
}
// port: belongs in provenant-records
pub(crate) fn deserialize_pairs<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Vec<(String, T)>, D::Error> {
    struct Pairs<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> Visitor<'de> for Pairs<T> {
        type Value = Vec<(String, T)>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("object")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Self::Value, A::Error> {
            let mut items: Vec<(String, T)> = vec![];
            while let Some((k, v)) = a.next_entry()? {
                if let Some((_, old)) = items.iter_mut().find(|(key, _)| *key == k) {
                    *old = v;
                } else {
                    items.push((k, v));
                }
            }
            Ok(items)
        }
    }
    d.deserialize_map(Pairs(std::marker::PhantomData))
}

// port: belongs in provenant-records
pub(crate) fn entries<'a>(
    map: &'a serde_json::Map<String, Value>,
    original: Option<&'a Json>,
) -> Vec<(&'a str, &'a Value)> {
    if let Some(items) = original.and_then(Json::object) {
        items
            .iter()
            .filter_map(|(k, _)| map.get(k).map(|v| (k.as_str(), v)))
            .collect()
    } else {
        map.iter().map(|(k, v)| (k.as_str(), v)).collect()
    }
}

// port: belongs in provenant-records
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(bound(serialize = "T: Serialize", deserialize = "T: Deserialize<'de>"))]
pub struct OrderedMap<T>(
    #[serde(
        serialize_with = "serialize_pairs",
        deserialize_with = "deserialize_pairs"
    )]
    Vec<(String, T)>,
);
// port: belongs in provenant-records
impl<T> Default for OrderedMap<T> {
    fn default() -> Self {
        Self(vec![])
    }
}
// port: belongs in provenant-records
impl<T> OrderedMap<T> {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get(&self, key: &str) -> Option<&T> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    pub fn insert(&mut self, key: String, value: T) -> Option<T> {
        if let Some((_, old)) = self.0.iter_mut().find(|(k, _)| k == &key) {
            Some(std::mem::replace(old, value))
        } else {
            self.0.push((key, value));
            None
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<T> {
        let position = self.0.iter().position(|(k, _)| k == key)?;
        Some(self.0.remove(position).1)
    }
    pub fn iter(&self) -> std::slice::Iter<'_, (String, T)> {
        self.0.iter()
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(k, _)| k)
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
}
// port: belongs in provenant-records
impl<'a, T> IntoIterator for &'a OrderedMap<T> {
    type Item = &'a (String, T);
    type IntoIter = std::slice::Iter<'a, (String, T)>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

// port: belongs in provenant-records
impl<T> FromIterator<(String, T)> for OrderedMap<T> {
    fn from_iter<I: IntoIterator<Item = (String, T)>>(iter: I) -> Self {
        let mut map = Self::new();
        for (k, v) in iter {
            map.insert(k, v);
        }
        map
    }
}

// port: belongs in provenant-records
impl Json {
    pub(crate) fn python_str(&self) -> String {
        if let Self::String(text) = self {
            text.clone()
        } else {
            self.python_repr()
        }
    }
    fn python_repr(&self) -> String {
        match self {
            Self::Object(items) => {
                let rendered = items
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.python_repr()))
                    .collect::<Vec<_>>();
                provenant_core::py::python_dict_repr(rendered.iter().map(|(k, v)| (*k, v.as_str())))
            }
            Self::Array(items) => format!(
                "[{}]",
                items
                    .iter()
                    .map(Self::python_repr)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => provenant_core::py::python_value_repr(&self.value()),
        }
    }
}
