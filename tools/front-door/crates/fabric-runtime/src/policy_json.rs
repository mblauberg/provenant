//! Field-local Python policy projection. Unread values stay opaque source bytes.
//! This is not a replacement for the records owner's checked JSON codec.
use serde::de::IgnoredAny;
use serde_json::{Map, Value};

/// Python accepts non-finite constants. Mask them only for grammar validation;
/// preserve all byte offsets so selected fields can be decoded independently.
fn grammar(text: &str) -> String {
    let mut result = text.as_bytes().to_vec();
    let mut index = 0;
    while index < result.len() {
        if result[index] == b'"' {
            index = string_end(&result, index);
            continue;
        }
        let token = ["-Infinity", "Infinity", "NaN"]
            .into_iter()
            .find(|token| text.as_bytes()[index..].starts_with(token.as_bytes()));
        if let Some(token) = token {
            result[index] = b'0';
            result[index + 1..index + token.len()].fill(b' ');
            index += token.len();
        } else {
            index += 1;
        }
    }
    // Only ASCII bytes were replaced; UTF-8 is preserved.
    String::from_utf8_lossy(&result).into_owned()
}
fn string_end(bytes: &[u8], mut index: usize) -> usize {
    index += 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return index + 1,
            _ => index += 1,
        }
    }
    bytes.len()
}
fn whitespace(bytes: &[u8], index: &mut usize) {
    while bytes.get(*index).is_some_and(u8::is_ascii_whitespace) {
        *index += 1;
    }
}
fn value_end(bytes: &[u8], start: usize) -> usize {
    if bytes[start] == b'"' {
        return string_end(bytes, start);
    }
    if matches!(bytes[start], b'{' | b'[') {
        let mut depth = 0;
        let mut index = start;
        while index < bytes.len() {
            match bytes[index] {
                b'"' => {
                    index = string_end(bytes, index);
                    continue;
                }
                b'{' | b'[' => depth += 1,
                b'}' | b']' => {
                    depth -= 1;
                    if depth == 0 {
                        return index + 1;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        return index;
    }
    let mut end = start;
    while bytes
        .get(end)
        .is_some_and(|b| !matches!(b, b',' | b'}' | b']') && !b.is_ascii_whitespace())
    {
        end += 1;
    }
    end
}

pub(crate) fn field(
    text: &str,
    selected: &str,
) -> Result<Option<Map<String, Value>>, serde_json::Error> {
    serde_json::from_str::<IgnoredAny>(&grammar(text))?;
    let bytes = text.as_bytes();
    let mut index = 0;
    whitespace(bytes, &mut index);
    if bytes.get(index) != Some(&b'{') {
        return Ok(None);
    }
    index += 1;
    let mut result = Map::new();
    loop {
        whitespace(bytes, &mut index);
        if bytes.get(index) == Some(&b'}') {
            break;
        }
        let end = string_end(bytes, index);
        // A surrogate key is legal in Python but can never equal a selected
        // ASCII policy key. Leave it opaque rather than rejecting the file.
        let key = serde_json::from_str::<String>(&text[index..end]).ok();
        index = end;
        whitespace(bytes, &mut index);
        index += 1; // Colon, proved by the grammar validation above.
        whitespace(bytes, &mut index);
        let end = value_end(bytes, index);
        if key.as_deref() == Some(selected) {
            // The selected field is interpreted separately. Unsupported scalar
            // encodings count as an invalid value here, never a file failure.
            let raw = &text[index..end];
            let value =
                provenant_records::checked_json::from_str(&grammar(raw)).unwrap_or(Value::Null);
            result.insert(selected.into(), value);
        }
        index = end;
        whitespace(bytes, &mut index);
        if bytes.get(index) == Some(&b',') {
            index += 1;
        } else {
            break;
        }
    }
    Ok(Some(result))
}

/// JavaScript grammar validation with opaque, ordered top-level values.
/// Unlike the Python projection, bare NaN/Infinity are invalid JSON here.
pub(crate) fn js_fields(
    text: &str,
) -> Result<Option<crate::pending::OrderedMap<&str>>, serde_json::Error> {
    serde_json::from_str::<IgnoredAny>(text)?;
    let bytes = text.as_bytes();
    let mut index = 0;
    whitespace(bytes, &mut index);
    if bytes.get(index) != Some(&b'{') {
        return Ok(None);
    }
    index += 1;
    let mut result = crate::pending::OrderedMap::default();
    loop {
        whitespace(bytes, &mut index);
        if bytes.get(index) == Some(&b'}') {
            break;
        }
        let end = string_end(bytes, index);
        let key = serde_json::from_str::<String>(&text[index..end]).ok();
        index = end;
        whitespace(bytes, &mut index);
        index += 1;
        whitespace(bytes, &mut index);
        let end = value_end(bytes, index);
        if let Some(key) = key {
            result.insert(key, &text[index..end]);
        }
        index = end;
        whitespace(bytes, &mut index);
        if bytes.get(index) == Some(&b',') {
            index += 1;
        } else {
            break;
        }
    }
    Ok(Some(result))
}
