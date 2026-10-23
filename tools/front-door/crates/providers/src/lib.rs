//! Adapter profiles, argv and environment builders, output classification and continuation.
#![forbid(unsafe_code)]

pub mod adapters;

mod pending;

#[cfg(test)]
mod oracle;

/// Errors at the provider JSON input boundary.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("JSON integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("invalid provider JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
}

/// Decode dynamic provider input without rounding Python integers to floats.
/// Use this boundary for `Plan` and JSON routes passed to the adapter builders.
pub fn decode_json<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, DecodeError> {
    pending::check_json_integer_ranges(input)?;
    Ok(serde_json::from_slice(input)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_boundary_refuses_oversized_integers_with_a_typed_error() {
        for token in [
            "18446744073709551616",
            "18446744073709551617",
            "-9223372036854775809",
        ] {
            let input = format!(r#"{{"cli_version":{token}}}"#);
            assert!(
                matches!(
                    decode_json::<serde_json::Value>(input.as_bytes()),
                    Err(DecodeError::NumberOutOfRange)
                ),
                "{token}"
            );
        }
        let value: serde_json::Value = decode_json(br#"[-9223372036854775808,18446744073709551615,9007199254740993,2.0,1e20,"18446744073709551616","\\\"18446744073709551617"]"#).unwrap();
        assert_eq!(value[0].as_i64(), Some(i64::MIN));
        assert_eq!(value[1].as_u64(), Some(u64::MAX));
        assert_eq!(value[2].as_u64(), Some(9_007_199_254_740_993));
        assert!(value[3].is_f64());
        assert!(value[4].is_f64());
        assert!(value[5].is_string());
        assert!(value[6].is_string());
    }
}
