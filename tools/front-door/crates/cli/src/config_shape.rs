//! Enumerate the setting paths a domain's serde types accept.
//!
//! Serde's derive passes the full field list of a struct to
//! [`serde::Deserializer::deserialize_struct`]. A probe deserializer records
//! those names at every level (through options, sequences, maps and nested
//! structs) while supplying values the field types accept. The result is the
//! typed schema: the settings a domain really deserializes, independent of
//! the separately published metadata and registry lists.
use serde::de::{
    self, DeserializeOwned, DeserializeSeed, EnumAccess, IntoDeserializer, MapAccess, SeqAccess,
    VariantAccess, Visitor,
};
use std::{cell::RefCell, collections::BTreeSet, rc::Rc};

#[derive(Debug)]
struct ProbeError(String);
impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ProbeError {}
impl de::Error for ProbeError {
    fn custom<T: std::fmt::Display>(message: T) -> Self {
        Self(message.to_string())
    }
}

/// Every leaf path `T` accepts, with `{key}` standing for one keyed-map entry.
pub fn typed_paths<T: DeserializeOwned>() -> BTreeSet<String> {
    let seen = Rc::new(RefCell::new(BTreeSet::new()));
    let _ = T::deserialize(Probe {
        path: String::new(),
        seen: &seen,
    });
    let paths = seen.borrow();
    paths
        .iter()
        .filter(|path| {
            !paths
                .iter()
                .any(|other| other.starts_with(&format!("{path}.")))
        })
        .cloned()
        .collect()
}

/// A `{name}` segment in a declared path stands for one keyed entry, so it
/// matches any concrete key (including another placeholder).
pub fn matches(pattern: &str, path: &str) -> bool {
    let mut pattern = pattern.split('.');
    let mut path = path.split('.');
    loop {
        match (pattern.next(), path.next()) {
            (None, None) => return true,
            (Some(a), Some(b)) if a == b || free_form(a) => {}
            _ => return false,
        }
    }
}

fn free_form(segment: &str) -> bool {
    (segment.starts_with('{') && segment.ends_with('}'))
        || (segment.starts_with('<') && segment.ends_with('>'))
}

/// A declared setting path covers a typed path when they match, when the
/// declaration names a table whose remaining typed segments are keyed entries,
/// when the typed path is a subfield of an array entry, or when the declaration
/// lists subfields under a table the typed path names.
pub fn covers(pattern: &str, path: &str) -> bool {
    if matches(pattern, path) {
        return true;
    }
    let p: Vec<&str> = pattern.split('.').collect();
    let mut t: Vec<&str> = path.split('.').collect();
    // Subfields of an array-of-tables entry belong to the array setting.
    if let Some(index) = t.iter().position(|segment| segment.contains("item")) {
        t.truncate(index + 1);
    }
    if p.len() < t.len()
        && p.iter().zip(&t).all(|(a, b)| a == b || free_form(a))
        && t[p.len()..].iter().all(|segment| free_form(segment))
    {
        return true;
    }
    if p.len() > t.len() && t.iter().zip(&p).all(|(a, b)| a == b || free_form(b)) {
        return true;
    }
    false
}

/// Is `path` covered by any declaration in `patterns`?
pub fn covered_by(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|pattern| covers(pattern, path))
}

#[derive(Clone)]
struct Probe<'a> {
    path: String,
    seen: &'a Rc<RefCell<BTreeSet<String>>>,
}
impl Probe<'_> {
    fn child(&self, key: &str) -> Self {
        let probe = self.descendant(key);
        self.seen.borrow_mut().insert(probe.path.clone());
        probe
    }
    /// A child whose path is only recorded if the child is a table itself.
    /// Sequences of scalar values are one setting, not one setting per item.
    fn descendant(&self, key: &str) -> Self {
        let path = if self.path.is_empty() {
            key.to_owned()
        } else {
            format!("{}.{}", self.path, key)
        };
        Self {
            path,
            seen: self.seen,
        }
    }
    fn record_self(&self) {
        if !self.path.is_empty() {
            self.seen.borrow_mut().insert(self.path.clone());
        }
    }
}

struct ProbeMap<'a> {
    probe: Probe<'a>,
    keys: &'static [&'static str],
    index: usize,
}
impl<'de, 'a> MapAccess<'de> for ProbeMap<'a> {
    type Error = ProbeError;
    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some(key) = self.keys.get(self.index) else {
            return Ok(None);
        };
        seed.deserialize(key.to_string().into_deserializer())
            .map(Some)
    }
    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let key = self.keys[self.index];
        self.index += 1;
        seed.deserialize(self.probe.child(key))
    }
}

struct ProbeSeq<'a> {
    probe: Probe<'a>,
    index: usize,
}
impl<'de, 'a> SeqAccess<'de> for ProbeSeq<'a> {
    type Error = ProbeError;
    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        if self.index > 0 {
            return Ok(None);
        }
        self.index += 1;
        seed.deserialize(self.probe.descendant("{item}")).map(Some)
    }
}

struct ProbeEnum<'a> {
    probe: Probe<'a>,
    variant: &'static str,
}
impl<'de, 'a> EnumAccess<'de> for ProbeEnum<'a> {
    type Error = ProbeError;
    type Variant = ProbeVariant<'a>;
    fn variant_seed<V: DeserializeSeed<'de>>(
        self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        let variant = seed.deserialize(self.variant.into_deserializer())?;
        Ok((variant, ProbeVariant { probe: self.probe }))
    }
}
struct ProbeVariant<'a> {
    probe: Probe<'a>,
}
impl<'de, 'a> VariantAccess<'de> for ProbeVariant<'a> {
    type Error = ProbeError;
    fn unit_variant(self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        seed.deserialize(self.probe)
    }
    fn tuple_variant<V: Visitor<'de>>(self, _: usize, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_seq(ProbeSeq {
            probe: self.probe,
            index: 0,
        })
    }
    fn struct_variant<V: Visitor<'de>>(
        self,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_map(ProbeMap {
            probe: self.probe,
            keys: &[],
            index: 0,
        })
    }
}

impl<'de> de::Deserializer<'de> for Probe<'_> {
    type Error = ProbeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_unit()
    }
    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_bool(true)
    }
    fn deserialize_i8<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_i8(1)
    }
    fn deserialize_i16<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_i16(1)
    }
    fn deserialize_i32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_i32(1)
    }
    fn deserialize_i64<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_i64(1)
    }
    fn deserialize_u8<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_u8(1)
    }
    fn deserialize_u16<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_u16(1)
    }
    fn deserialize_u32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_u32(1)
    }
    fn deserialize_u64<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_u64(1)
    }
    fn deserialize_f32<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_f32(1.0)
    }
    fn deserialize_f64<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_f64(1.0)
    }
    fn deserialize_char<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_char('x')
    }
    fn deserialize_str<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_str("probe")
    }
    fn deserialize_string<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_string("probe".to_owned())
    }
    fn deserialize_bytes<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_bytes(b"probe")
    }
    fn deserialize_byte_buf<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_byte_buf(b"probe".to_vec())
    }
    fn deserialize_option<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_some(self)
    }
    fn deserialize_unit<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_unit()
    }
    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        v: V,
    ) -> Result<V::Value, Self::Error> {
        v.visit_unit()
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        v: V,
    ) -> Result<V::Value, Self::Error> {
        v.visit_newtype_struct(self)
    }
    fn deserialize_seq<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_seq(ProbeSeq {
            probe: self,
            index: 0,
        })
    }
    fn deserialize_tuple<V: Visitor<'de>>(self, _: usize, v: V) -> Result<V::Value, Self::Error> {
        v.visit_seq(ProbeSeq {
            probe: self,
            index: 0,
        })
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        _: usize,
        v: V,
    ) -> Result<V::Value, Self::Error> {
        v.visit_seq(ProbeSeq {
            probe: self,
            index: 0,
        })
    }
    fn deserialize_map<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        self.record_self();
        v.visit_map(ProbeMap {
            probe: self,
            keys: &["{key}"],
            index: 0,
        })
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _: &'static str,
        fields: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Self::Error> {
        self.record_self();
        v.visit_map(ProbeMap {
            probe: self,
            keys: fields,
            index: 0,
        })
    }
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _: &'static str,
        variants: &'static [&'static str],
        v: V,
    ) -> Result<V::Value, Self::Error> {
        v.visit_enum(ProbeEnum {
            variant: variants.first().copied().unwrap_or("probe"),
            probe: self,
        })
    }
    fn deserialize_identifier<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_str("probe")
    }
    fn deserialize_ignored_any<V: Visitor<'de>>(self, v: V) -> Result<V::Value, Self::Error> {
        v.visit_unit()
    }
}
