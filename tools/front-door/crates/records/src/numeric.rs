//! Exact JSON integer and float arithmetic with Python numeric ordering.
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};
use std::cmp::Ordering;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(untagged)]
pub enum Numeric {
    Integer(i64),
    Unsigned(u64),
    Float(f64),
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum NumericError {
    #[error("integer result is outside the i64/u64 wire range")]
    NumberOutOfRange,
}

impl Default for Numeric {
    fn default() -> Self {
        Self::ZERO
    }
}
impl Numeric {
    pub const ZERO: Self = Self::Integer(0);
    pub const ONE: Self = Self::Integer(1);

    pub fn try_from_integer(value: i128) -> Result<Self, NumericError> {
        checked_integer(Some(value))
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Self::Integer(n) => n as f64,
            Self::Unsigned(n) => n as f64,
            Self::Float(n) => n,
        }
    }

    /// Convert a JSON number without rounding integers through f64 or imposing a domain policy.
    pub fn from_value(value: &Value) -> Option<Self> {
        let n = value.as_number()?;
        n.as_i64()
            .map(Self::Integer)
            .or_else(|| n.as_u64().map(Self::Unsigned))
            .or_else(|| n.as_f64().map(Self::Float))
    }

    pub fn checked_add(self, other: Self) -> Result<Self, NumericError> {
        match (self.integer_value(), other.integer_value()) {
            (Some(a), Some(b)) => checked_integer(a.checked_add(b)),
            _ => Ok(Self::Float(self.as_f64() + other.as_f64())),
        }
    }

    pub fn subtract(self, other: Self) -> Result<Self, NumericError> {
        match (self.integer_value(), other.integer_value()) {
            (Some(a), Some(b)) => checked_integer(a.checked_sub(b)),
            _ => Ok(Self::Float(self.as_f64() - other.as_f64())),
        }
    }

    pub fn non_negative(self) -> Self {
        if self < Self::ZERO {
            Self::ZERO
        } else {
            self
        }
    }
    pub fn is_zero(self) -> bool {
        self == Self::ZERO
    }

    fn integer_value(self) -> Option<i128> {
        match self {
            Self::Integer(value) => Some(i128::from(value)),
            Self::Unsigned(value) => Some(i128::from(value)),
            Self::Float(_) => None,
        }
    }
}

fn checked_integer(value: Option<i128>) -> Result<Numeric, NumericError> {
    let value = value.ok_or(NumericError::NumberOutOfRange)?;
    if let Ok(value) = i64::try_from(value) {
        Ok(Numeric::Integer(value))
    } else if let Ok(value) = u64::try_from(value) {
        Ok(Numeric::Unsigned(value))
    } else {
        Err(NumericError::NumberOutOfRange)
    }
}

impl PartialEq for Numeric {
    fn eq(&self, other: &Self) -> bool {
        self.partial_cmp(other) == Some(Ordering::Equal)
    }
}
impl PartialOrd for Numeric {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        match (self.integer_value(), other.integer_value()) {
            (Some(a), Some(b)) => a.partial_cmp(&b),
            (Some(a), None) => integer_float_cmp(a, other.as_f64()),
            (None, Some(b)) => integer_float_cmp(b, self.as_f64()).map(Ordering::reverse),
            (None, None) => self.as_f64().partial_cmp(&other.as_f64()),
        }
    }
}
impl From<i64> for Numeric {
    fn from(n: i64) -> Self {
        Self::Integer(n)
    }
}
impl From<i32> for Numeric {
    fn from(n: i32) -> Self {
        Self::Integer(i64::from(n))
    }
}
impl From<u64> for Numeric {
    fn from(n: u64) -> Self {
        Self::Unsigned(n)
    }
}
impl From<f64> for Numeric {
    fn from(n: f64) -> Self {
        Self::Float(n)
    }
}
impl From<Number> for Numeric {
    fn from(n: Number) -> Self {
        n.as_i64()
            .map(Self::Integer)
            .or_else(|| n.as_u64().map(Self::Unsigned))
            .or_else(|| n.as_f64().map(Self::Float))
            .unwrap_or(Self::Float(f64::NAN))
    }
}
impl<'de> Deserialize<'de> for Numeric {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Numeric;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON integer or float in the i64/u64 range")
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Numeric, E> {
                Ok(Numeric::Integer(v))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Numeric, E> {
                Ok(Numeric::Unsigned(v))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Numeric, E> {
                Ok(Numeric::Float(v))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

/// Compare an exact integer with a float without first rounding the integer.
fn integer_float_cmp(integer: i128, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    const I128_UPPER_EXCLUSIVE: f64 = 170_141_183_460_469_231_731_687_303_715_884_105_728.0;
    if float >= I128_UPPER_EXCLUSIVE {
        return Some(Ordering::Less);
    }
    if float < -I128_UPPER_EXCLUSIVE {
        return Some(Ordering::Greater);
    }
    let whole = float as i128;
    let order = integer.cmp(&whole);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn exact_integer_float_order_matches_python_numeric_equality() {
        assert!(Numeric::Integer(9_007_199_254_740_993) > Numeric::Float(9_007_199_254_740_992.0));
        assert!(Numeric::Integer(9_007_199_254_740_992) == Numeric::Float(9_007_199_254_740_992.0));
        assert!(Numeric::Unsigned(u64::MAX) < Numeric::Float(u64::MAX as f64));
        assert!(Numeric::Integer(i64::MIN) == Numeric::Float(i64::MIN as f64));
        assert!(Numeric::Integer(-1) > Numeric::Float(-1.5));
        assert!(Numeric::Integer(-2) < Numeric::Float(-1.5));
        assert_eq!(
            Numeric::Integer(1).partial_cmp(&Numeric::Float(f64::NAN)),
            None
        );
    }

    #[test]
    fn integer_float_differential_matches_python() {
        use std::{io::Write, process::Stdio};
        let pairs = vec![
            (
                Numeric::Integer(9_007_199_254_740_993),
                Numeric::Float(9_007_199_254_740_992.0),
            ),
            (
                Numeric::Integer(9_007_199_254_740_992),
                Numeric::Float(9_007_199_254_740_992.0),
            ),
            (Numeric::Unsigned(u64::MAX), Numeric::Float(u64::MAX as f64)),
            (Numeric::Integer(i64::MIN), Numeric::Float(i64::MIN as f64)),
            (Numeric::Integer(-1), Numeric::Float(-1.5)),
            (Numeric::Integer(-2), Numeric::Float(-1.5)),
        ];
        let input = serde_json::to_vec(&pairs).unwrap();
        let Some(mut command) = crate::oracle::python() else {
            return;
        };
        command
            .args([
                "-c",
                "import json,sys; pairs=json.load(sys.stdin); print(json.dumps([0 if a == b else (1 if a > b else -1) for a,b in pairs]))",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child =
            crate::oracle::spawn(&mut command).expect("configured Python oracle failed to start");
        if let Err(error) = child
            .child_mut()
            .stdin
            .take()
            .expect("Python oracle stdin was not piped")
            .write_all(&input)
        {
            let _ = child.wait();
            panic!("could not send numeric fixture to Python oracle: {error}");
        }
        let output = child
            .wait()
            .expect("Python oracle failed or timed out after interpreter gate");
        assert!(
            output.status.success(),
            "Python oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Vec<i8> = serde_json::from_slice(&output.stdout).unwrap();
        let actual: Vec<i8> = pairs
            .iter()
            .map(|(a, b)| match a.partial_cmp(b).unwrap() {
                Ordering::Less => -1,
                Ordering::Equal => 0,
                Ordering::Greater => 1,
            })
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn checked_integer_arithmetic_stays_in_python_json_wire_range() {
        assert_eq!(
            Numeric::from(u64::MAX).checked_add(Numeric::ONE),
            Err(NumericError::NumberOutOfRange)
        );
        assert_eq!(
            Numeric::from(i64::MIN).subtract(Numeric::ONE),
            Err(NumericError::NumberOutOfRange)
        );
        assert_eq!(
            Numeric::from(40_i64).checked_add(Numeric::from(2_i32)),
            Ok(Numeric::Integer(42))
        );
    }

    #[test]
    fn values_keep_integer_form_and_refuse_boolean_coercion() {
        assert_eq!(
            Numeric::from_value(&serde_json::json!(9_007_199_254_740_993_u64)),
            Some(Numeric::Integer(9_007_199_254_740_993))
        );
        assert_eq!(
            Numeric::from_value(&serde_json::json!(u64::MAX)),
            Some(Numeric::Unsigned(u64::MAX))
        );
        assert_eq!(
            Numeric::try_from_integer(u64::MAX as i128),
            Ok(Numeric::Unsigned(u64::MAX))
        );
        assert_eq!(
            Numeric::try_from_integer(u64::MAX as i128 + 1),
            Err(NumericError::NumberOutOfRange)
        );
        assert_eq!(
            Numeric::from_value(&serde_json::json!(-2)),
            Some(Numeric::Integer(-2))
        );
        assert_eq!(Numeric::from_value(&serde_json::json!(true)), None);
    }
}
