//! JSON values that retain Python dictionary insertion order.
use serde::{
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::{Number, Value};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(untagged)]
pub enum OrderedValue {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<OrderedValue>),
    Object(OrderedObject),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrderedObject(pub Vec<(String, OrderedValue)>);

impl OrderedObject {
    pub fn get(&self, key: &str) -> Option<&OrderedValue> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    /// Updating a duplicate key preserves its first insertion position, as Python dict does.
    pub fn insert(&mut self, key: String, value: OrderedValue) {
        if let Some((_, old)) = self.0.iter_mut().find(|(name, _)| name == &key) {
            *old = value;
        } else {
            self.0.push((key, value));
        }
    }
}

impl Serialize for OrderedObject {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}
struct OrderedSeed<'a> {
    numbers: Option<&'a mut std::vec::IntoIter<Number>>,
}
impl<'de> DeserializeSeed<'de> for OrderedSeed<'_> {
    type Value = OrderedValue;
    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_any(OrderedVisitor {
            numbers: self.numbers,
        })
    }
}
struct OrderedVisitor<'a> {
    numbers: Option<&'a mut std::vec::IntoIter<Number>>,
}
impl OrderedVisitor<'_> {
    fn number<E: serde::de::Error>(mut self, fallback: Number) -> Result<OrderedValue, E> {
        Ok(OrderedValue::Number(
            self.numbers
                .as_deref_mut()
                .and_then(Iterator::next)
                .unwrap_or(fallback),
        ))
    }
}
impl<'de> Visitor<'de> for OrderedVisitor<'_> {
    type Value = OrderedValue;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a JSON value")
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(OrderedValue::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
        Ok(OrderedValue::Null)
    }
    fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
        Ok(OrderedValue::Bool(value))
    }
    fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
        self.number(Number::from(value))
    }
    fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
        self.number(Number::from(value))
    }
    fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
        self.number(Number::from_f64(value).ok_or_else(|| E::custom("non-finite JSON number"))?)
    }
    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
        Ok(OrderedValue::String(value.into()))
    }
    fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
        Ok(OrderedValue::String(value))
    }
    fn visit_seq<A: SeqAccess<'de>>(mut self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element_seed(OrderedSeed {
            numbers: self.numbers.as_deref_mut(),
        })? {
            values.push(value);
        }
        Ok(OrderedValue::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(mut self, mut map: A) -> Result<Self::Value, A::Error> {
        let mut object = OrderedObject::default();
        while let Some(key) = map.next_key::<String>()? {
            let value = map.next_value_seed(OrderedSeed {
                numbers: self.numbers.as_deref_mut(),
            })?;
            object.insert(key, value);
        }
        Ok(OrderedValue::Object(object))
    }
}
impl<'de> Deserialize<'de> for OrderedValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        OrderedSeed { numbers: None }.deserialize(deserializer)
    }
}
impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        match OrderedValue::deserialize(deserializer)? {
            OrderedValue::Object(object) => Ok(object),
            _ => Err(serde::de::Error::custom("a JSON object was required")),
        }
    }
}

impl OrderedValue {
    /// Python repr with recursive dictionary insertion order retained.
    pub fn python_repr(&self) -> String {
        match self {
            Self::Object(object) => {
                let values: Vec<_> = object
                    .0
                    .iter()
                    .map(|(_, value)| value.python_repr())
                    .collect();
                provenant_core::py::python_dict_repr(
                    object
                        .0
                        .iter()
                        .zip(&values)
                        .map(|((key, _), value)| (key.as_str(), value.as_str())),
                )
            }
            Self::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::python_repr)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::String(value) => provenant_core::py::python_repr(value),
            value => provenant_core::py::python_str(&value.to_value()),
        }
    }
    /// Python str differs from repr only for strings in JSON-shaped values.
    pub fn python_str(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            value => value.python_repr(),
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&OrderedObject> {
        if let Self::Object(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?.get(key)
    }
    /// Convert only when the consumer does not rely on object order.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(v) => Value::Bool(*v),
            Self::Number(v) => Value::Number(v.clone()),
            Self::String(v) => Value::String(v.clone()),
            Self::Array(v) => Value::Array(v.iter().map(Self::to_value).collect()),
            Self::Object(v) => {
                Value::Object(v.0.iter().map(|(k, v)| (k.clone(), v.to_value())).collect())
            }
        }
    }
}
impl From<Value> for OrderedValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(v) => Self::Bool(v),
            Value::Number(v) => Self::Number(v),
            Value::String(v) => Self::String(v),
            Value::Array(v) => Self::Array(v.into_iter().map(Self::from).collect()),
            Value::Object(v) => Self::Object(OrderedObject(
                v.into_iter().map(|(k, v)| (k, Self::from(v))).collect(),
            )),
        }
    }
}

/// Parse JSON after the shared checked integer and negative-zero boundary.
pub fn from_str(text: &str) -> Result<OrderedValue, crate::DecodeError> {
    let normalized = crate::checked_json::normalize(text)?;
    let mut numbers = crate::checked_json::number_tokens(normalized.as_bytes())?.into_iter();
    let mut parser = serde_json::Deserializer::from_slice(normalized.as_bytes());
    let value = OrderedSeed {
        numbers: Some(&mut numbers),
    }
    .deserialize(&mut parser)?;
    parser.end()?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn insertion_order_duplicates_and_checked_integer_range() {
        let value = from_str(r#"{"z":1,"a":2,"z":3}"#).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(
            object.0.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
            ["z", "a"]
        );
        assert_eq!(
            object.get("z"),
            Some(&OrderedValue::Number(Number::from(3)))
        );
        let error = from_str(r#"{"ignored":18446744073709551616}"#).unwrap_err();
        assert!(matches!(error, crate::DecodeError::NumberOutOfRange(_)));
    }
}

#[cfg(test)]
mod repr_tests {
    use super::*;
    #[test]
    fn nested_python_repr_retains_input_order() {
        let value = from_str(r#"{"z":false,"a":[null,"é",{"x":-0.0}]}"#).unwrap();
        assert_eq!(
            value.python_repr(),
            "{'z': False, 'a': [None, 'é', {'x': -0.0}]}"
        );
        let Some(mut python) = crate::oracle::python() else {
            return;
        };
        let output = crate::oracle::run(python.args([
            "-c",
            "import json; print(repr(json.loads('{\"z\":false,\"a\":[null,\"é\",{\"x\":-0.0}]}')))",
        ]))
        .unwrap();
        assert!(output.status.success());
        assert_eq!(
            value.python_repr() + "\n",
            String::from_utf8(output.stdout).unwrap()
        );
    }
}
