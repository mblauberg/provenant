use std::{
    collections::VecDeque,
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

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

// port: belongs in provenant-core
pub(crate) fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "embedded null character in path",
        ));
    }
    enum Part {
        Name(OsString),
        EndLink(PathBuf),
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut pending: VecDeque<_> = absolute
        .components()
        .map(|part| Part::Name(part.as_os_str().into()))
        .collect();
    let mut links = std::collections::BTreeSet::new();
    let mut result = PathBuf::new();
    while let Some(part) = pending.pop_front() {
        let name = match part {
            Part::EndLink(link) => {
                links.remove(&link);
                continue;
            }
            Part::Name(name) => name,
        };
        if name == "." {
            continue;
        }
        if name == ".." {
            result.pop();
            continue;
        }
        result.push(&name);
        if let Ok(link) = fs::read_link(&result) {
            if !links.insert(result.clone()) {
                return Err(io::Error::other("Symlink loop"));
            }
            pending.push_front(Part::EndLink(result.clone()));
            result.pop();
            for component in link.components().rev() {
                pending.push_front(Part::Name(component.as_os_str().into()));
            }
        }
    }
    Ok(result)
}

// port: belongs in provenant-records
pub(crate) fn checked_json(text: &str) -> Result<serde_json::Value, serde_json::Error> {
    use serde::de::IgnoredAny;
    let source = replace_nonfinite_json_constants(text);
    serde_json::from_str::<IgnoredAny>(&source)?;
    let mut normalized = String::with_capacity(source.len());
    let mut chars = source.char_indices().peekable();
    while let Some((start, ch)) = chars.next() {
        normalized.push(ch);
        if ch == '"' {
            while let Some((_, ch)) = chars.next() {
                normalized.push(ch);
                if ch == '\\' {
                    if let Some((_, escaped)) = chars.next() {
                        normalized.push(escaped);
                    }
                } else if ch == '"' {
                    break;
                }
            }
        } else if ch == '-' || ch.is_ascii_digit() {
            let mut end = start + ch.len_utf8();
            while let Some(&(index, next)) = chars.peek() {
                if !(next.is_ascii_digit() || matches!(next, '.' | 'e' | 'E' | '+' | '-')) {
                    break;
                }
                end = index + next.len_utf8();
                chars.next();
            }
            let token = &source[start..end];
            if !token.contains(['.', 'e', 'E'])
                && token.parse::<i64>().is_err()
                && token.parse::<u64>().is_err()
            {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "integer outside i64/u64 range",
                ));
            }
            normalized.pop();
            normalized.push_str(if token == "-0" { "0" } else { token });
        }
    }
    serde_json::from_str(&normalized)
}

fn replace_nonfinite_json_constants(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            let ch = text[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(ch);
            index += ch.len_utf8();
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            in_string = true;
            output.push('"');
            index += 1;
            continue;
        }
        let token = [b"-Infinity".as_slice(), b"Infinity", b"NaN"]
            .into_iter()
            .find(|token| {
                bytes[index..].starts_with(token)
                    && bytes.get(index + token.len()).is_none_or(|next| {
                        matches!(next, b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}')
                    })
            });
        if let Some(token) = token {
            output.push_str("null");
            index += token.len();
        } else {
            let ch = text[index..].chars().next().expect("valid UTF-8 boundary");
            output.push(ch);
            index += ch.len_utf8();
        }
    }
    output
}

// port: belongs in provenant-core
pub(crate) fn python_repr(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => format!(
            "'{}'",
            value
                .replace('\\', "\\\\")
                .replace('\'', "\\'")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        ),
        serde_json::Value::Null => "None".into(),
        serde_json::Value::Bool(true) => "True".into(),
        serde_json::Value::Bool(false) => "False".into(),
        serde_json::Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_repr)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        serde_json::Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!(
                    "{}: {}",
                    python_repr(&serde_json::Value::String(key.clone())),
                    python_repr(value)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}
