//! Temporary shared primitives until their owners land on main.
use crate::{io, Result};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

// port: belongs in provenant-core
pub(crate) fn resolve(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| io(path, e))?.join(path)
    };
    resolve_inner(&absolute, 0)
}

// port: belongs in provenant-core
fn resolve_inner(path: &Path, depth: usize) -> Result<PathBuf> {
    if depth > 40 {
        return Err(crate::invalid(format!("symlink loop: {}", path.display())));
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => {
                result.push(other.as_os_str());
                match fs::symlink_metadata(&result) {
                    Ok(meta) if meta.is_symlink() => {
                        let link = fs::read_link(&result).map_err(|e| io(&result, e))?;
                        let target = if link.is_absolute() {
                            link
                        } else {
                            result.parent().unwrap_or(Path::new("/")).join(link)
                        };
                        result = resolve_inner(&target, depth + 1)?;
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io(&result, e)),
                }
            }
        }
    }
    Ok(result)
}

// port: belongs in provenant-supervisor
// Safe std-only staging. Create-new prevents clobbering another staging file.
pub(crate) fn atomic_write(path: &Path, prefix: &str, bytes: &[u8]) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| io(parent, e))?;
    for sequence in 0..1024 {
        let temp = parent.join(format!("{prefix}{}.{sequence}.tmp", std::process::id()));
        let mut file = match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&temp, e)),
        };
        let result = (|| {
            file.write_all(bytes).map_err(|e| io(&temp, e))?;
            file.sync_all().map_err(|e| io(&temp, e))?;
            drop(file);
            fs::rename(&temp, path).map_err(|e| io(path, e))
        })();
        let cleanup = fs::remove_file(&temp);
        result?;
        if let Err(e) = cleanup {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(io(&temp, e));
            }
        }
        return Ok(());
    }
    Err(crate::invalid(
        "could not reserve an installation staging file",
    ))
}

// port: belongs in provenant-supervisor
pub(crate) fn replace_link(destination: &Path, source: &Path) -> Result<()> {
    let parent = destination.parent().unwrap_or(Path::new("."));
    for sequence in 0..1024 {
        let temp = parent.join(format!(".workflow.{}.{sequence}.tmp", std::process::id()));
        match std::os::unix::fs::symlink(source, &temp) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(io(&temp, e)),
        }
        let result = fs::rename(&temp, destination).map_err(|e| io(destination, e));
        let cleanup = fs::remove_file(&temp);
        result?;
        if let Err(e) = cleanup {
            if e.kind() != std::io::ErrorKind::NotFound {
                return Err(io(&temp, e));
            }
        }
        return Ok(());
    }
    Err(crate::invalid("could not reserve a workflow staging link"))
}

// port: belongs in provenant-records
pub(crate) fn json_bytes<T: serde::Serialize>(record: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(record).map_err(|e| crate::invalid(e.to_string()))?;
    let mut output = String::new();
    encode_json(&value, 0, &mut output)?;
    output.push('\n');
    Ok(output.into_bytes())
}

// port: belongs in provenant-records
fn json_string(value: &str) -> Result<String> {
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
fn encode_json(value: &serde_json::Value, depth: usize, output: &mut String) -> Result<()> {
    use serde_json::Value;
    let indent = "  ".repeat(depth);
    match value {
        Value::Array(items) if !items.is_empty() => {
            output.push_str("[\n");
            for (index, value) in items.iter().enumerate() {
                output.push_str(&indent);
                output.push_str("  ");
                encode_json(value, depth + 1, output)?;
                if index + 1 != items.len() {
                    output.push(',');
                }
                output.push('\n');
            }
            output.push_str(&indent);
            output.push(']');
        }
        Value::Object(items) if !items.is_empty() => {
            output.push_str("{\n");
            for (index, (key, value)) in items.iter().enumerate() {
                output.push_str(&indent);
                output.push_str("  ");
                output.push_str(&json_string(key)?);
                output.push_str(": ");
                encode_json(value, depth + 1, output)?;
                if index + 1 != items.len() {
                    output.push(',');
                }
                output.push('\n');
            }
            output.push_str(&indent);
            output.push('}');
        }
        Value::String(s) => output.push_str(&json_string(s)?),
        Value::Number(n) if n.is_f64() => {
            if let Some(value) = n.as_f64() {
                let abs = value.abs();
                if abs != 0.0 && !(1e-4..1e16).contains(&abs) {
                    let scientific = format!("{value:e}");
                    if let Some((mantissa, exponent)) = scientific.split_once('e') {
                        let exponent: i32 = exponent
                            .parse()
                            .map_err(|e: std::num::ParseIntError| crate::invalid(e.to_string()))?;
                        output.push_str(&format!("{mantissa}e{exponent:+03}"));
                    }
                } else {
                    let decimal = value.to_string();
                    output.push_str(&decimal);
                    if !decimal.contains('.') {
                        output.push_str(".0");
                    }
                }
            }
        }
        _ => output.push_str(&value.to_string()),
    }
    Ok(())
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
    value == &serde_json::Value::Bool(true) || value.as_f64() == Some(1.0)
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

// port: belongs in provenant-records
pub(crate) fn read_json(path: &Path, context: &'static str) -> Result<serde_json::Value> {
    let bytes = fs::read(path).map_err(|source| crate::Error::Read {
        context,
        path: path.into(),
        source,
    })?;
    // serde_json's default number model would silently round larger Python ints.
    // Refuse them until the records owner supplies its lossless wire codec.
    let mut quoted = false;
    let mut escaped = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte.is_ascii_digit() || byte == b'-' {
            let start = index;
            while index < bytes.len()
                && (bytes[index].is_ascii_digit()
                    || matches!(bytes[index], b'.' | b'e' | b'E' | b'+' | b'-'))
            {
                index += 1;
            }
            let token = &bytes[start..index];
            if !token.iter().any(|b| matches!(b, b'.' | b'e' | b'E')) {
                if let Ok(text) = std::str::from_utf8(token) {
                    if text.parse::<i64>().is_err() && text.parse::<u64>().is_err() {
                        return Err(crate::Error::PendingRecord {
                            feature: "arbitrary-precision integer encoding",
                        });
                    }
                }
            }
            continue;
        }
        index += 1;
    }
    serde_json::from_slice(&bytes).map_err(|source| crate::Error::Json {
        context,
        path: path.into(),
        source,
    })
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
            Err(crate::Error::PendingRecord { .. })
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
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/invalid-\xff".to_vec()));
        assert!(matches!(
            path_text(&path),
            Err(crate::Error::PendingRecord { .. })
        ));
    }
}
