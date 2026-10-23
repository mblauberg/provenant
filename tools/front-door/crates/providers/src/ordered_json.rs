//! Provider event JSON keeps Python dictionary insertion order and duplicate evidence.
use serde::{
    de::{self, MapAccess, SeqAccess, Visitor},
    Deserialize, Deserializer,
};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Atom(Value),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}
impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Node;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_unit<E: de::Error>(self) -> Result<Node, E> {
                Ok(Node::Atom(Value::Null))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Node, E> {
                Ok(Node::Atom(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Node, E> {
                Ok(Node::Atom(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Node, E> {
                Ok(Node::Atom(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Node, E> {
                Ok(Node::Atom(Value::from(v)))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Node, E> {
                Ok(Node::Atom(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> Result<Node, E> {
                Ok(Node::Atom(v.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_element()? {
                    out.push(v);
                }
                Ok(Node::Array(out))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Node, A::Error> {
                let mut out = Vec::new();
                while let Some(v) = a.next_entry()? {
                    out.push(v);
                }
                Ok(Node::Object(out))
            }
        }
        d.deserialize_any(V)
    }
}
impl Node {
    pub fn get(&self, key: &str) -> Option<&Node> {
        match self {
            Self::Object(p) => p.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn string(&self) -> Option<&str> {
        match self {
            Self::Atom(Value::String(s)) => Some(s),
            _ => None,
        }
    }
    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }
    pub fn is_array(&self) -> bool {
        matches!(self, Self::Array(_))
    }
    pub fn value(&self) -> Value {
        match self {
            Self::Atom(v) => v.clone(),
            Self::Array(a) => Value::Array(a.iter().map(Node::value).collect()),
            Self::Object(p) => {
                Value::Object(p.iter().map(|(k, v)| (k.clone(), v.value())).collect())
            }
        }
    }
    pub fn truth(&self) -> bool {
        match self {
            Self::Atom(v) => provenant_core::py::python_truth(v),
            Self::Array(a) => !a.is_empty(),
            Self::Object(o) => !o.is_empty(),
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Atom(Value::Null))
    }
    pub fn is_false(&self) -> bool {
        matches!(self, Self::Atom(Value::Bool(false)))
    }
    pub fn number(&self) -> Option<Value> {
        match self {
            Self::Atom(v @ (Value::Number(_) | Value::Bool(_))) => Some(v.clone()),
            _ => None,
        }
    }
    /// Unique entries in insertion order, with last duplicate value winning.
    pub fn entries(&self) -> Vec<(&str, &Node)> {
        let Self::Object(p) = self else { return vec![] };
        let mut out: Vec<(&str, &Node)> = Vec::new();
        for (k, v) in p {
            if let Some(slot) = out.iter_mut().find(|(key, _)| *key == k) {
                slot.1 = v;
            } else {
                out.push((k, v));
            }
        }
        out
    }
    pub fn objects(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        self.collect_objects(&mut out);
        out
    }
    fn collect_objects<'a>(&'a self, out: &mut Vec<&'a Node>) {
        match self {
            Self::Object(_) => {
                out.push(self);
                for (_, v) in self.entries() {
                    v.collect_objects(out);
                }
            }
            Self::Array(a) => {
                for v in a {
                    v.collect_objects(out)
                }
            }
            _ => {}
        }
    }
    pub fn duplicates(&self) -> bool {
        match self {
            Self::Object(p) => {
                p.len() != self.entries().len() || p.iter().any(|(_, v)| v.duplicates())
            }
            Self::Array(a) => a.iter().any(Node::duplicates),
            _ => false,
        }
    }
    pub fn dumps(&self, ascii: bool) -> String {
        match self {
            Self::Atom(Value::String(s)) => json_string(s, ascii),
            Self::Atom(Value::Number(n)) => {
                provenant_core::py::python_str(&Value::Number(n.clone()))
            }
            Self::Atom(v) => v.to_string(),
            Self::Array(a) => format!(
                "[{}]",
                a.iter()
                    .map(|v| v.dumps(ascii))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Object(_) => format!(
                "{{{}}}",
                self.entries()
                    .into_iter()
                    .map(|(k, v)| format!("{}: {}", json_string(k, ascii), v.dumps(ascii)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    pub fn python_str(&self) -> String {
        match self {
            Self::Object(_) => format!(
                "{{{}}}",
                self.entries()
                    .into_iter()
                    .map(|(k, v)| format!("{}: {}", provenant_core::py::python_repr(k), v.repr()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Array(a) => format!(
                "[{}]",
                a.iter().map(Node::repr).collect::<Vec<_>>().join(", ")
            ),
            Self::Atom(v) => provenant_core::py::python_str(v),
        }
    }
    fn repr(&self) -> String {
        match self {
            Self::Atom(Value::String(s)) => provenant_core::py::python_repr(s),
            _ => self.python_str(),
        }
    }
}
pub(crate) fn json_string(s: &str, ascii: bool) -> String {
    // JSON string encoding is infallible; serde_json's Value renderer uses the
    // same encoder without a fallible writer boundary.
    let encoded = Value::String(s.into()).to_string();
    if !ascii {
        return encoded;
    }
    let mut out = String::new();
    for c in encoded.chars() {
        if c.is_ascii() && c != '\u{7f}' {
            out.push(c);
        } else {
            let mut buf = [0; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

pub(crate) fn parse(text: &str) -> Result<Node, crate::DecodeError> {
    // Validate through the shared owner before decoding the ordered event tree.
    crate::decode_json(text.as_bytes())
}

impl serde::Serialize for Node {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeSeq};
        match self {
            Self::Atom(v) => v.serialize(s),
            Self::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Self::Object(_) => {
                let entries = self.entries();
                let mut map = s.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

/// Dynamic context input keeps dictionary order for Claude's Python str().
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct ContextCeiling(Node);
impl ContextCeiling {
    pub(crate) fn truth(&self) -> bool {
        self.0.truth()
    }
    pub(crate) fn python_str(&self) -> String {
        self.0.python_str()
    }
    pub(crate) fn value(&self) -> Value {
        self.0.value()
    }
}
impl From<Value> for ContextCeiling {
    fn from(value: Value) -> Self {
        fn node(v: Value) -> Node {
            match v {
                Value::Array(a) => Node::Array(a.into_iter().map(node).collect()),
                Value::Object(o) => {
                    Node::Object(o.into_iter().map(|(k, v)| (k, node(v))).collect())
                }
                v => Node::Atom(v),
            }
        }
        Self(node(value))
    }
}
