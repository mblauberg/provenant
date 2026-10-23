// port: belongs in provenant-core
pub(crate) fn trim_ecmascript(value: &str) -> &str {
    value.trim_matches(|c: char| {
        matches!(c,
            '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
            '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' |
            '\u{205f}' | '\u{3000}' | '\u{feff}'
        )
    })
}

// port: belongs in provenant-records
/// Insertion-ordered wire object, including replacement without moving a key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderedMap<T>(pub Vec<(String, T)>);
impl<T> Default for OrderedMap<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}
impl<T> OrderedMap<T> {
    pub fn get(&self, key: &str) -> Option<&T> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    pub fn insert(&mut self, key: String, value: T) {
        if let Some((_, existing)) = self.0.iter_mut().find(|(k, _)| *k == key) {
            *existing = value;
        } else {
            self.0.push((key, value));
        }
    }
    pub fn remove(&mut self, key: &str) -> Option<T> {
        self.0
            .iter()
            .position(|(k, _)| k == key)
            .map(|index| self.0.remove(index).1)
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
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut result = OrderedMap::default();
                while let Some((key, value)) = map.next_entry()? {
                    result.insert(key, value);
                }
                Ok(result)
            }
        }
        deserializer.deserialize_map(Visitor(std::marker::PhantomData))
    }
}
