//! Preserve Python integers through counters and records; convert only for measurements.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

// port: belongs in provenant-records
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(untagged)]
pub enum Numeric {
    Integer(i128),
    Float(f64),
}
// port: belongs in provenant-records
impl Default for Numeric {
    fn default() -> Self {
        Self::ZERO
    }
}
// port: belongs in provenant-records
impl Numeric {
    pub const ZERO: Self = Self::Integer(0);
    pub const ONE: Self = Self::Integer(1);
    pub fn as_f64(self) -> f64 {
        match self {
            Self::Integer(n) => n as f64,
            Self::Float(n) => n,
        }
    }
    pub fn from_value(value: &Value) -> Option<Self> {
        let n = value.as_number()?;
        let result = if let Some(n) = n.as_i64() {
            Self::Integer(n.into())
        } else if let Some(n) = n.as_u64() {
            Self::Integer(n.into())
        } else {
            Self::Float(n.as_f64()?)
        };
        (result.as_f64() >= 0.0).then_some(result)
    }
    pub fn checked_add(self, other: Self) -> Result<Self> {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a
                .checked_add(b)
                .filter(|n| (i64::MIN as i128..=u64::MAX as i128).contains(n))
                .map(Self::Integer)
                .ok_or(Error::NumberOutOfRange),
            _ => Ok(Self::Float(self.as_f64() + other.as_f64())),
        }
    }
    pub fn subtract(self, other: Self) -> Result<Self> {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a
                .checked_sub(b)
                .filter(|n| (i64::MIN as i128..=u64::MAX as i128).contains(n))
                .map(Self::Integer)
                .ok_or(Error::NumberOutOfRange),
            _ => Ok(Self::Float(self.as_f64() - other.as_f64())),
        }
    }
    pub fn non_negative(self) -> Self {
        if self < Self::ZERO { Self::ZERO } else { self }
    }
    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }
}
// port: belongs in provenant-records
impl PartialEq for Numeric {
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(std::cmp::Ordering::Equal)
    }
}
// port: belongs in provenant-records
impl PartialOrd for Numeric {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Self::Integer(a), Self::Integer(b)) => a.partial_cmp(b),
            (Self::Integer(a), Self::Float(b)) => integer_float_cmp(*a, *b),
            (Self::Float(a), Self::Integer(b)) => {
                integer_float_cmp(*b, *a).map(std::cmp::Ordering::reverse)
            }
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(b),
        }
    }
}
// port: belongs in provenant-records
impl From<i64> for Numeric {
    fn from(n: i64) -> Self {
        Self::Integer(n.into())
    }
}
// port: belongs in provenant-records
impl From<i32> for Numeric {
    fn from(n: i32) -> Self {
        Self::Integer(n.into())
    }
}
// port: belongs in provenant-records
impl From<u64> for Numeric {
    fn from(n: u64) -> Self {
        Self::Integer(n.into())
    }
}
// port: belongs in provenant-records
impl From<f64> for Numeric {
    fn from(n: f64) -> Self {
        Self::Float(n)
    }
}
// port: belongs in provenant-records
impl From<Number> for Numeric {
    fn from(n: Number) -> Self {
        if let Some(n) = n.as_i64() {
            Self::Integer(n.into())
        } else if let Some(n) = n.as_u64() {
            Self::Integer(n.into())
        } else {
            Self::Float(n.as_f64().unwrap_or(0.0))
        }
    }
}

// port: belongs in provenant-records
fn integer_float_cmp(integer: i128, float: f64) -> Option<std::cmp::Ordering> {
    use std::cmp::Ordering;
    if float.is_nan() {
        return None;
    }
    if float >= 170_141_183_460_469_231_731_687_303_715_884_105_728.0 {
        return Some(Ordering::Less);
    }
    if float < -170_141_183_460_469_231_731_687_303_715_884_105_728.0 {
        return Some(Ordering::Greater);
    }
    let order = integer.cmp(&(float as i128));
    if order != Ordering::Equal {
        return Some(order);
    }
    if float.fract() > 0.0 {
        Some(Ordering::Less)
    } else if float.fract() < 0.0 {
        Some(Ordering::Greater)
    } else {
        Some(Ordering::Equal)
    }
}

// port: belongs in provenant-records
impl<'de> Deserialize<'de> for Numeric {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Numeric;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON integer or float")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> std::result::Result<Numeric, E> {
                Ok(Numeric::Integer(v.into()))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> std::result::Result<Numeric, E> {
                Ok(Numeric::Integer(v.into()))
            }
            fn visit_i128<E: serde::de::Error>(self, v: i128) -> std::result::Result<Numeric, E> {
                if (i64::MIN as i128..=u64::MAX as i128).contains(&v) {
                    Ok(Numeric::Integer(v))
                } else {
                    Err(E::custom("JSON integer outside i64/u64 range"))
                }
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> std::result::Result<Numeric, E> {
                Ok(Numeric::Float(v))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_refuses_integers_outside_wire_range() {
        assert!(matches!(
            Numeric::from(u64::MAX).checked_add(Numeric::ONE),
            Err(Error::NumberOutOfRange)
        ));
        assert!(matches!(
            Numeric::from(i64::MIN).subtract(Numeric::ONE),
            Err(Error::NumberOutOfRange)
        ));
    }
}
