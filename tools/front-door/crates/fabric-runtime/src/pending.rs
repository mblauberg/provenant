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

// port: belongs in provenant-records
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct JsNumberBoundary(pub String);

// port: belongs in provenant-records
pub(crate) fn js_json_number(number: &serde_json::Number) -> Result<String, JsNumberBoundary> {
    if !number.is_f64() {
        if number
            .as_i64()
            .is_some_and(|n| (-9_007_199_254_740_991..=9_007_199_254_740_991).contains(&n))
            || number.as_u64().is_some_and(|n| n <= 9_007_199_254_740_991)
        {
            return Ok(number.to_string());
        }
        return Err(JsNumberBoundary(number.to_string()));
    }
    let text = number.to_string();
    let (mantissa, exponent) = text.split_once('e').map_or((text.as_str(), 0), |(m, e)| {
        (m, e.parse::<i32>().unwrap_or(0))
    });
    let (sign, mantissa) = mantissa
        .strip_prefix('-')
        .map_or(("", mantissa), |m| ("-", m));
    let mut point = mantissa.find('.').unwrap_or(mantissa.len()) as i32 + exponent;
    let digits = mantissa.replace('.', "");
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return Ok("0".into());
    }
    point -= (digits.len() - significant.len()) as i32;
    let digits = significant.trim_end_matches('0');
    let exponent = point - 1;
    Ok(if !(-6..21).contains(&exponent) {
        let (first, rest) = digits.split_at(1);
        let mantissa = if rest.is_empty() {
            first.into()
        } else {
            format!("{first}.{rest}")
        };
        if exponent >= 0 {
            format!("{sign}{mantissa}e+{exponent}")
        } else {
            format!("{sign}{mantissa}e{exponent}")
        }
    } else if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        let (whole, fraction) = digits.split_at(point as usize);
        format!("{sign}{whole}.{fraction}")
    })
}

// port: belongs in provenant-records
/// Finite numeric spelling only. Value object order is already lost; callers
/// with observable insertion order must retain an ordered/raw representation.
pub(crate) fn js_json_value(value: &serde_json::Value) -> Result<String, JsNumberBoundary> {
    use serde_json::Value;
    Ok(match value {
        Value::Number(number) => js_json_number(number)?,
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(js_json_value)
                .collect::<Result<Vec<_>, _>>()?
                .join(",")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| Ok(format!(
                    "{}:{}",
                    serde_json::Value::String(key.clone()),
                    js_json_value(value)?
                )))
                .collect::<Result<Vec<_>, JsNumberBoundary>>()?
                .join(",")
        ),
        _ => value.to_string(),
    })
}
