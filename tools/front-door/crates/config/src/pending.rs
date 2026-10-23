//! Temporary shared primitives until their owners land on main.
use crate::Result;
use serde_json::Value;
use std::path::Path;

// port: belongs in provenant-records
pub(crate) fn json_bytes<T: serde::Serialize>(record: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(record).map_err(|e| crate::invalid(e.to_string()))?;
    Ok(format!(
        "{}\n",
        crate::ordered::dumps(&crate::ordered::Json::from_value(value), true, true)?
    )
    .into_bytes())
}

// port: belongs in provenant-records
pub(crate) fn json_string(value: &str) -> Result<String> {
    let text = serde_json::to_string(value).map_err(|e| crate::invalid(e.to_string()))?;
    let mut output = String::new();
    for c in text.chars() {
        if c < '\u{7f}' {
            output.push(c);
        } else {
            use std::fmt::Write;
            for unit in c.encode_utf16(&mut [0; 2]) {
                write!(&mut output, "\\u{unit:04x}").map_err(|e| crate::invalid(e.to_string()))?;
            }
        }
    }
    Ok(output)
}

// port: belongs in provenant-records
pub(crate) fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(crate::Error::PendingRecord {
            feature: "non-UTF-8 path encoding",
        })
}

// port: belongs in provenant-records
pub(crate) fn schema_one(value: &serde_json::Value) -> bool {
    provenant_core::py::python_json_eq(value, &serde_json::json!(1))
}

// port: belongs in provenant-records
pub(crate) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                DIGITS[(b >> 4) as usize] as char,
                DIGITS[(b & 15) as usize] as char,
            ]
        })
        .collect()
}

// port: belongs in provenant-core
pub(crate) fn path_sort_key(name: &std::ffi::OsStr) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    let mut bytes = name.as_bytes();
    let mut result = vec![];
    while !bytes.is_empty() {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                result.extend(text.chars().map(u32::from));
                break;
            }
            Err(error) => {
                let (valid, rest) = bytes.split_at(error.valid_up_to());
                result.extend(String::from_utf8_lossy(valid).chars().map(u32::from));
                let length = error.error_len().unwrap_or(rest.len());
                result.extend(rest[..length].iter().map(|byte| 0xdc00 + u32::from(*byte)));
                bytes = &rest[length..];
            }
        }
    }
    result
}

// port: belongs in provenant-core
pub(crate) fn parse_int(text: &str) -> Result<Option<Value>> {
    let (negative, digits) = if let Some(rest) = text.strip_prefix('-') {
        (true, rest)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return Ok(None);
    }
    let mut ascii = String::new();
    if negative {
        ascii.push('-');
    }
    let mut digit_count = 0;
    // CPython's default decimal conversion limit excludes signs and underscores.
    // Runtime interpreter overrides remain a pending host/codec limitation.
    // Python int accepts Unicode decimal digits and single underscores.
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let code = c as u32;
        let Some(digit) = DECIMAL_STARTS
            .iter()
            .find_map(|start| code.checked_sub(*start).filter(|v| *v < 10))
        else {
            return Ok(None);
        };
        digit_count += 1;
        if digit_count > 4300 {
            return Ok(None);
        }
        ascii.push(char::from(b'0' + digit as u8));
    }
    if let Ok(v) = ascii.parse::<i64>() {
        Ok(Some(Value::from(v)))
    } else if let Ok(v) = ascii.parse::<u64>() {
        Ok(Some(Value::from(v)))
    } else {
        Err(provenant_records::checked_json::NumberOutOfRange { value: ascii }.into())
    }
}
// Unicode 16 Nd starts, including the five mathematical digit sets.
// port: belongs in provenant-core
const DECIMAL_STARTS: &[u32] = &[
    0x30, 0x660, 0x6f0, 0x7c0, 0x966, 0x9e6, 0xa66, 0xae6, 0xb66, 0xbe6, 0xc66, 0xce6, 0xd66,
    0xde6, 0xe50, 0xed0, 0xf20, 0x1040, 0x1090, 0x17e0, 0x1810, 0x1946, 0x19d0, 0x1a80, 0x1a90,
    0x1b50, 0x1bb0, 0x1c40, 0x1c50, 0xa620, 0xa8d0, 0xa900, 0xa9d0, 0xa9f0, 0xaa50, 0xabf0, 0xff10,
    0x104a0, 0x10d30, 0x10d40, 0x11066, 0x110f0, 0x11136, 0x111d0, 0x112f0, 0x11450, 0x114d0,
    0x11650, 0x116c0, 0x116d0, 0x116da, 0x11730, 0x118e0, 0x11950, 0x11bf0, 0x11c50, 0x11d50,
    0x11da0, 0x11f50, 0x16130, 0x16a60, 0x16ac0, 0x16b50, 0x16d70, 0x1ccf0, 0x1d7ce, 0x1d7d8,
    0x1d7e2, 0x1d7ec, 0x1d7f6, 0x1e140, 0x1e2f0, 0x1e4f0, 0x1e5f1, 0x1e950, 0x1fbf0,
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    fn read_json(path: &Path, _context: &str) -> crate::Result<serde_json::Value> {
        Ok(crate::ordered::decode(&fs::read(path).unwrap(), "test", path)?.value())
    }
    #[test]
    fn python_json_bytes_cover_float_thresholds_and_ascii_escaping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input");
        let value = serde_json::json!({"empty": [{},[]], "unicode": "é😀\u{7f}", "numbers": [0.0, -0.0, 1e-7, 1e-6, 1e-5, 0.0001, 1e15, 1e16, 1e20, 1.2345678901234567e-5]});
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        let Some(old) = crate::oracle::python(
            "scripts/managed_installation_manifest.py",
            "import json; print(json.dumps(json.loads(p.read_text()),indent=2,sort_keys=True))",
            &path,
        ) else {
            return;
        };
        assert_eq!(json_bytes(&value).unwrap(), old);
    }
    #[test]
    fn lossless_integer_boundary_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record");
        fs::write(&path, b"{\"n\": 18446744073709551616}").unwrap();
        assert!(matches!(
            read_json(&path, "unreadable"),
            Err(crate::Error::CheckedJson {
                source: provenant_records::checked_json::DecodeError::NumberOutOfRange(_),
                ..
            })
        ));
        fs::write(
            &path,
            b"{\"n\": 18446744073709551615, \"s\": \"18446744073709551616\"}",
        )
        .unwrap();
        assert_eq!(
            read_json(&path, "unreadable").unwrap()["n"].as_u64(),
            Some(u64::MAX)
        );
    }
    #[test]
    fn profile_integer_overflow_is_typed_instead_of_becoming_a_string() {
        for text in [
            "18446744073709551616",
            "-9223372036854775809",
            "１８４４６７４４０７３７０９５５１６１６",
        ] {
            assert!(matches!(
                parse_int(text),
                Err(crate::Error::NumberOutOfRange(_))
            ));
        }
        assert_eq!(
            parse_int("18446744073709551615").unwrap(),
            Some(Value::from(u64::MAX))
        );
        assert_eq!(
            parse_int("-9223372036854775808").unwrap(),
            Some(Value::from(i64::MIN))
        );
        assert_eq!(parse_int("12__3").unwrap(), None);
        assert_eq!(parse_int(&"0".repeat(5000)).unwrap(), None);
    }
    #[test]
    fn non_utf8_paths_never_silently_change_the_receipt() {
        use std::os::unix::ffi::OsStringExt;
        let path =
            std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/invalid-\xff".to_vec()));
        assert!(matches!(
            path_text(&path),
            Err(crate::Error::PendingRecord { .. })
        ));
    }
}
