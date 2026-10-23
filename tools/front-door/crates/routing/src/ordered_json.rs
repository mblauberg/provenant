//! JSON objects with Python dictionary insertion order, without workspace feature changes.
// port: belongs in provenant-records
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{MapAccess, Visitor},
    ser::SerializeMap,
};
use serde_json::{Number, Value};
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OrderedValue {
    Null,
    Bool(bool),
    Number(Number),
    String(String),
    Array(Vec<OrderedValue>),
    Object(OrderedObject),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OrderedObject(pub Vec<(String, OrderedValue)>);

impl OrderedObject {
    pub fn get(&self, key: &str) -> Option<&OrderedValue> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
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
impl<'de> Deserialize<'de> for OrderedObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = OrderedObject;
            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a JSON object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut object = OrderedObject::default();
                while let Some((key, value)) = map.next_entry()? {
                    object.insert(key, value);
                }
                Ok(object)
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}
impl OrderedValue {
    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(text) = self {
            Some(text)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&OrderedObject> {
        if let Self::Object(object) = self {
            Some(object)
        } else {
            None
        }
    }
    pub fn get(&self, key: &str) -> Option<&Self> {
        self.as_object()?.get(key)
    }
    /// Convert only for consumers whose contract does not depend on object order.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Null => Value::Null,
            Self::Bool(value) => Value::Bool(*value),
            Self::Number(value) => Value::Number(value.clone()),
            Self::String(value) => Value::String(value.clone()),
            Self::Array(values) => Value::Array(values.iter().map(Self::to_value).collect()),
            Self::Object(object) => Value::Object(
                object
                    .0
                    .iter()
                    .map(|(k, v)| (k.clone(), v.to_value()))
                    .collect(),
            ),
        }
    }
}
impl From<Value> for OrderedValue {
    fn from(value: Value) -> Self {
        match value {
            Value::Null => Self::Null,
            Value::Bool(value) => Self::Bool(value),
            Value::Number(value) => Self::Number(value),
            Value::String(value) => Self::String(value),
            Value::Array(values) => Self::Array(values.into_iter().map(Self::from).collect()),
            Value::Object(object) => Self::Object(OrderedObject(
                object
                    .into_iter()
                    .map(|(k, v)| (k, Self::from(v)))
                    .collect(),
            )),
        }
    }
}
