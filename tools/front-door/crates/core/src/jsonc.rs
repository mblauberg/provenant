//! Partial port: RP-1002 records the remaining Python compatibility work.
//! JSONC comments and trailing commas, preserving string contents.
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum JsoncError {
    #[error("unterminated block comment: line {line} column {column} (char {offset})")]
    UnterminatedComment {
        offset: usize,
        line: usize,
        column: usize,
    },
    #[error("invalid JSONC")]
    Json(#[from] serde_json::Error),
}

/// Remove comments and trailing commas without moving character positions.
/// `offset` in comment errors counts Unicode characters, as Python does.
pub fn prepare_jsonc(text: &str) -> Result<(String, bool), JsoncError> {
    let mut chars: Vec<_> = text.chars().collect();
    let (mut quoted, mut escaped, mut comment, mut index) = (false, false, false, 0);
    while index < chars.len() {
        let current = chars[index];
        let following = chars.get(index + 1).copied();
        if quoted {
            if escaped {
                escaped = false;
            } else if current == '\\' {
                escaped = true;
            } else if current == '"' {
                quoted = false;
            }
            index += 1;
            continue;
        }
        if current == '"' {
            quoted = true;
            index += 1;
            continue;
        }
        if current == '/' && following == Some('/') {
            comment = true;
            while index < chars.len() && !matches!(chars[index], '\r' | '\n') {
                chars[index] = ' ';
                index += 1;
            }
            continue;
        }
        if current == '/' && following == Some('*') {
            comment = true;
            let start = index;
            index += 2;
            while index + 1 < chars.len() && chars[index..index + 2] != ['*', '/'] {
                index += 1;
            }
            if index + 1 >= chars.len() {
                let line = chars[..start].iter().filter(|&&c| c == '\n').count() + 1;
                let column = start
                    - chars[..start]
                        .iter()
                        .rposition(|&c| c == '\n')
                        .map_or(0, |p| p + 1)
                    + 1;
                return Err(JsoncError::UnterminatedComment {
                    offset: start,
                    line,
                    column,
                });
            }
            index += 2;
            for c in &mut chars[start..index] {
                if !matches!(*c, '\r' | '\n') {
                    *c = ' ';
                }
            }
            continue;
        }
        index += 1;
    }
    quoted = false;
    escaped = false;
    for index in 0..chars.len() {
        let current = chars[index];
        if quoted {
            if escaped {
                escaped = false;
            } else if current == '\\' {
                escaped = true;
            } else if current == '"' {
                quoted = false;
            }
        } else if current == '"' {
            quoted = true;
        } else if current == ',' {
            let following = chars[index + 1..]
                .iter()
                .find(|&&c| !crate::py::python_whitespace(c));
            if matches!(following, Some('}' | ']')) {
                chars[index] = ' ';
            }
        }
    }
    Ok((chars.into_iter().collect(), comment))
}

/// JSONC syntax port. Python's extended number and lone-surrogate values still
/// need a compatibility representation before this can replace the Python reader.
pub fn parse_jsonc(text: &str) -> Result<(Value, bool), JsoncError> {
    let (prepared, comments) = prepare_jsonc(text)?;
    Ok((serde_json::from_str(&prepared)?, comments))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comments_and_commas_inside_strings_are_data() {
        let (v, comments) = parse_jsonc(r#"{/*comment*/"url":"https://x/*literal*/", "text":",}", "escaped":"\"//", "array":[1,2,],}//end"#).unwrap();
        assert!(comments);
        assert_eq!(
            v,
            serde_json::json!({"url":"https://x/*literal*/","text":",}","escaped":"\"//","array":[1,2]})
        );
    }
    #[test]
    fn trailing_commas_alone_do_not_report_comments() {
        assert_eq!(
            parse_jsonc("{\"a\":1,}").unwrap(),
            (serde_json::json!({"a":1}), false)
        );
        assert_eq!(
            parse_jsonc("[1,]").unwrap(),
            (serde_json::json!([1]), false)
        );
        assert_eq!(parse_jsonc("//only\r\n42").unwrap(), (42.into(), true));
    }
    #[test]
    fn malformed_json_and_unterminated_comments_fail() {
        for text in ["", "[1,,]", "{broken}", "\"unterminated", "/*ok*/ NaNx"] {
            assert!(parse_jsonc(text).is_err(), "{text}");
        }
        assert!(matches!(
            prepare_jsonc("\"é\"\n/*"),
            Err(JsoncError::UnterminatedComment {
                offset: 4,
                line: 2,
                column: 1
            })
        ));
        assert!(matches!(
            prepare_jsonc("/*"),
            Err(JsoncError::UnterminatedComment { offset: 0, .. })
        ));
    }
    #[test]
    fn replacement_preserves_character_positions_and_newlines() {
        let (text, _) = prepare_jsonc("[/*é\r\nx*/1,]").unwrap();
        assert_eq!(text, "[   \r\n   1 ]");
        assert_eq!(parse_jsonc(r#"{"x":1,"x":2}"#).unwrap().0["x"], 2);
    }
}

#[cfg(test)]
mod differential {
    use super::*;
    #[test]
    fn python_jsonc_fixtures() {
        let fixtures = [
            r#"{/*é*/"url":"https://x", "a":[1,2,],}"#,
            r#"{"text":"\\\"//", "x":",}"}//comment"#,
            "[//x\r\n1,/*x*/2,]",
            r#"{"x":1,"x":2}"#,
            "/* unterminated",
            "[1,,]",
            "/*é*/[]",
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixtures.json");
        std::fs::write(&path, serde_json::to_vec(&fixtures).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/lib/jsonc.py",
            r#"
values = json.loads(pathlib.Path(sys.argv[3]).read_text())
results = []
for value in values:
    try:
        v,c = m.parse_jsonc(value)
        results.append([v,c])
    except ValueError:
        results.append(None)
print(json.dumps(results))
"#,
            &[&path],
        ) else {
            return;
        };
        let actual: Vec<_> = fixtures
            .iter()
            .map(|text| {
                parse_jsonc(text)
                    .ok()
                    .map(|(v, c)| serde_json::json!([v, c]))
            })
            .collect();
        assert_eq!(serde_json::to_value(actual).unwrap(), expected);
    }
}

#[cfg(test)]
mod remaining_parity {
    use super::*;
    #[test]
    #[ignore = "RP-1002: Python number and lone-surrogate representation"]
    fn python_extended_jsonc_values() {
        let fixtures = ["NaN", "Infinity", "-Infinity", "1e400", r#""\udcff""#];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixtures.json");
        std::fs::write(&path, serde_json::to_vec(&fixtures).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/lib/jsonc.py",
            r#"
results=[]
for value in json.loads(pathlib.Path(sys.argv[3]).read_text()):
    try:
        m.parse_jsonc(value)
        results.append(True)
    except ValueError:
        results.append(False)
print(json.dumps(results))
"#,
            &[&path],
        ) else {
            return;
        };
        assert_eq!(
            serde_json::json!(
                fixtures
                    .iter()
                    .map(|v| parse_jsonc(v).is_ok())
                    .collect::<Vec<_>>()
            ),
            expected
        );
    }
}

#[cfg(test)]
mod diagnostics_tests {
    use super::*;
    #[test]
    fn unterminated_comments_match_python_character_diagnostics() {
        let fixtures = serde_json::json!([
            "/*",
            "// first\n/*",
            "[\"é😀\", /* unfinished",
            "/* first */\r\n\t/* end",
            "/*a*/\n/*é\r\n*/\n/*",
            "[1,/*"
        ]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fixtures.json");
        std::fs::write(&path, serde_json::to_vec(&fixtures).unwrap()).unwrap();
        let Some(expected) = crate::test_support::python(
            "scripts/lib/jsonc.py",
            r#"
results=[]
for text in json.loads(pathlib.Path(sys.argv[3]).read_text()):
    try: m.parse_jsonc(text); results.append(None)
    except json.JSONDecodeError as e: results.append([str(e),e.pos,e.lineno,e.colno])
print(json.dumps(results))
"#,
            &[&path],
        ) else {
            return;
        };
        let actual: Vec<_> = fixtures
            .as_array()
            .unwrap()
            .iter()
            .map(|text| {
                let error = prepare_jsonc(text.as_str().unwrap()).unwrap_err();
                match &error {
                    JsoncError::UnterminatedComment {
                        offset,
                        line,
                        column,
                    } => serde_json::json!([error.to_string(), offset, line, column]),
                    _ => panic!("expected unterminated-comment diagnostic"),
                }
            })
            .collect();
        assert_eq!(serde_json::json!(actual), expected);
    }
    #[test]
    fn python_whitespace_does_not_change_json_whitespace_grammar() {
        for c in ['\u{1c}', '\u{1d}', '\u{1e}', '\u{1f}', '\u{85}', '\u{a0}'] {
            let (prepared, comments) = prepare_jsonc(&format!("[1,{c}]")).unwrap();
            assert_eq!(prepared, format!("[1 {c}]"));
            assert!(!comments);
            // isspace affects comma lookahead; json.loads still rejects these.
            assert!(parse_jsonc(&format!("[1,{c}]")).is_err());
        }
    }
}

#[cfg(test)]
mod integer_boundary_tests {
    use super::*;
    #[test]
    #[ignore = "RP-core-3: records checked decoder integration is blocked by R2 and missing extended-value API"]
    fn integer_ingress_never_rounds_an_out_of_range_token() {
        assert!(parse_jsonc("18446744073709551616").is_err());
        assert!(parse_jsonc("-9223372036854775809").is_err());
        assert_eq!(
            parse_jsonc("18446744073709551615").unwrap().0.as_u64(),
            Some(u64::MAX)
        );
        assert_eq!(parse_jsonc("-0").unwrap().0.to_string(), "0");
    }
}

/// Separate the pure JSONC transformation from the record owner's decoder.
/// Callers can pass `provenant_records::checked_json::from_str` without adding
/// an illegal core -> records dependency. A decoder's typed errors are retained.
#[derive(Debug, Error)]
pub enum JsoncDecodeError<E> {
    #[error(transparent)]
    Prepare(#[from] JsoncError),
    #[error(transparent)]
    Decode(E),
}
pub fn parse_jsonc_with<T, E>(
    text: &str,
    decode: impl FnOnce(&str) -> Result<T, E>,
) -> Result<(T, bool), JsoncDecodeError<E>> {
    let (prepared, comments) = prepare_jsonc(text)?;
    Ok((
        decode(&prepared).map_err(JsoncDecodeError::Decode)?,
        comments,
    ))
}
