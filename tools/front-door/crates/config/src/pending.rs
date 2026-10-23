//! Temporary shared primitives until their owners land on main.
use crate::Result;
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
