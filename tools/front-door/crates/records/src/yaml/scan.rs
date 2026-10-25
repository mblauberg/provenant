//! Lexical helpers: quoted scalars, comments, pair splitting and scalar
//! classification. No parser state; value construction stays in the parent.
use super::{Span, YamlError, YamlErrorKind};

pub(super) fn parse_quoted(text: &str, span: Span) -> Result<(String, usize), YamlError> {
    let Some(quote) = text.as_bytes().first().copied() else {
        return Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column));
    };
    if quote == b'\'' {
        parse_single_quoted(text, span)
    } else if quote == b'"' {
        parse_double_quoted(text, span)
    } else {
        Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column))
    }
}

pub(super) fn parse_single_quoted(text: &str, span: Span) -> Result<(String, usize), YamlError> {
    let mut output = String::new();
    let mut position = 1usize;
    while let Some(byte) = text.as_bytes().get(position).copied() {
        if byte == b'\'' {
            if text.as_bytes().get(position + 1) == Some(&b'\'') {
                output.push('\'');
                position += 2;
            } else {
                return Ok((output, position + 1));
            }
        } else {
            let Some(character) = text.get(position..).and_then(|tail| tail.chars().next()) else {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + position,
                ));
            };
            output.push(character);
            position += character.len_utf8();
        }
    }
    Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column))
}

pub(super) fn parse_double_quoted(text: &str, span: Span) -> Result<(String, usize), YamlError> {
    let mut output = String::new();
    let mut position = 1usize;
    while let Some(character) = text.get(position..).and_then(|tail| tail.chars().next()) {
        match character {
            '"' => return Ok((output, position + 1)),
            '\\' => {
                position += 1;
                let Some(escape) = text.get(position..).and_then(|tail| tail.chars().next()) else {
                    return Err(error(
                        YamlErrorKind::InvalidSyntax,
                        span.line,
                        span.column + position,
                    ));
                };
                position += escape.len_utf8();
                let decoded = match escape {
                    '0' => '\0',
                    'a' => '\u{0007}',
                    'b' => '\u{0008}',
                    't' => '\t',
                    'n' => '\n',
                    'v' => '\u{000b}',
                    'f' => '\u{000c}',
                    'r' => '\r',
                    'e' => '\u{001b}',
                    ' ' => ' ',
                    '"' => '"',
                    '/' => '/',
                    '\\' => '\\',
                    'N' => '\u{0085}',
                    '_' => '\u{00a0}',
                    'L' => '\u{2028}',
                    'P' => '\u{2029}',
                    'x' => parse_hex_escape(text, &mut position, 2, span)?,
                    'u' => parse_hex_escape(text, &mut position, 4, span)?,
                    'U' => parse_hex_escape(text, &mut position, 8, span)?,
                    _ => {
                        return Err(error(
                            YamlErrorKind::UnsupportedEscape,
                            span.line,
                            span.column + position,
                        ));
                    }
                };
                output.push(decoded);
            }
            character if character <= '\u{001f}' => {
                return Err(error(
                    YamlErrorKind::InvalidSyntax,
                    span.line,
                    span.column + position,
                ));
            }
            character => {
                output.push(character);
                position += character.len_utf8();
            }
        }
    }
    Err(error(YamlErrorKind::InvalidSyntax, span.line, span.column))
}

pub(super) fn parse_hex_escape(
    text: &str,
    position: &mut usize,
    digits: usize,
    span: Span,
) -> Result<char, YamlError> {
    let Some(slice) = text.get(*position..position.saturating_add(digits)) else {
        return Err(error(
            YamlErrorKind::UnsupportedEscape,
            span.line,
            span.column + *position,
        ));
    };
    let mut value = 0u32;
    for byte in slice.bytes() {
        let nibble = match byte {
            b'0'..=b'9' => u32::from(byte - b'0'),
            b'a'..=b'f' => u32::from(byte - b'a' + 10),
            b'A'..=b'F' => u32::from(byte - b'A' + 10),
            _ => {
                return Err(error(
                    YamlErrorKind::UnsupportedEscape,
                    span.line,
                    span.column + *position,
                ));
            }
        };
        value = value.saturating_mul(16).saturating_add(nibble);
    }
    *position = position.saturating_add(digits);
    char::from_u32(value).ok_or_else(|| {
        error(
            YamlErrorKind::UnsupportedEscape,
            span.line,
            span.column + *position,
        )
    })
}

pub(super) fn split_pair(text: &str) -> Option<(&str, &str, usize)> {
    let bytes = text.as_bytes();
    let leading = text.len().saturating_sub(text.trim_start().len());
    let quoted_key = matches!(bytes.get(leading), Some(b'"' | b'\''));
    let mut position = 0usize;
    let mut quote = None;
    while let Some(byte) = bytes.get(position).copied() {
        if !quoted_key {
            if byte == b':' && bytes.get(position + 1).is_none_or(u8::is_ascii_whitespace) {
                let key = text.get(..position)?.trim();
                let raw = text.get(position + 1..)?;
                let value_column = position.saturating_add(2);
                return Some((key, raw, value_column));
            }
            position += 1;
            continue;
        }
        match (quote, byte) {
            (Some(b'"'), b'\\') => position = position.saturating_add(2),
            (Some(active), value) if active == value => {
                if active == b'\'' && bytes.get(position + 1) == Some(&b'\'') {
                    position = position.saturating_add(2);
                } else {
                    quote = None;
                    position += 1;
                }
            }
            (Some(_), _) => position += 1,
            (None, b'"' | b'\'') => {
                quote = Some(byte);
                position += 1;
            }
            (None, b':') if bytes.get(position + 1).is_none_or(u8::is_ascii_whitespace) => {
                let key = text.get(..position)?.trim();
                let raw = text.get(position + 1..)?;
                let value_column = position.saturating_add(2);
                return Some((key, raw, value_column));
            }
            (None, _) => position += 1,
        }
    }
    None
}

pub(super) fn strip_comment(text: &str) -> &str {
    let bytes = text.as_bytes();
    let leading = text.len().saturating_sub(text.trim_start().len());
    let scalar = text.get(leading..).map_or("", |value| value);
    let flow_sequence = scalar.starts_with('[');
    let quoted_scalar = matches!(scalar.as_bytes().first(), Some(b'"' | b'\''));
    let mut position = 0usize;
    let mut quote = None;
    while let Some(byte) = bytes.get(position).copied() {
        if !flow_sequence && !quoted_scalar {
            if byte == b'#'
                && position > 0
                && bytes.get(position - 1).is_some_and(u8::is_ascii_whitespace)
            {
                return text.get(..position).map_or("", |value| value);
            }
            position += 1;
            continue;
        }
        match (quote, byte) {
            (Some(b'"'), b'\\') => position = position.saturating_add(2),
            (Some(active), value) if active == value => {
                if active == b'\'' && bytes.get(position + 1) == Some(&b'\'') {
                    position = position.saturating_add(2);
                } else {
                    quote = None;
                    position += 1;
                }
            }
            (Some(_), _) => position += 1,
            (None, b'"' | b'\'') => {
                quote = Some(byte);
                position += 1;
            }
            (None, b'#')
                if position == 0
                    || bytes.get(position - 1).is_some_and(u8::is_ascii_whitespace) =>
            {
                return text.get(..position).map_or("", |value| value);
            }
            (None, _) => position += 1,
        }
    }
    text
}

pub(super) fn sequence_rest(text: &str) -> Option<&str> {
    if text == "-" {
        Some("")
    } else if text.starts_with("- ") {
        text.get(2..)
    } else {
        None
    }
}

pub(super) fn is_sequence_line(text: &str) -> bool {
    sequence_rest(text).is_some()
}

pub(super) fn contains_yaml_directive_token(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(|part| part.starts_with('&') || part.starts_with('*') || part.starts_with('!'))
}

pub(super) fn starts_with_plain_scalar_indicator(text: &str) -> bool {
    matches!(
        text.as_bytes().first(),
        Some(b'-' | b'?' | b':' | b',' | b']' | b'}' | b'%' | b'@' | b'`')
    )
}

pub(super) fn contains_forbidden_plain_colon(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.iter().enumerate().any(|(index, byte)| {
        *byte == b':'
            && bytes
                .get(index + 1)
                .is_none_or(|next| next.is_ascii_whitespace())
    })
}

pub(super) fn ambiguous_word(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "yes" | "no" | "on" | "off" | "y" | "n" | "null" | "none" | "true" | "false"
    ) && !matches!(text, "true" | "false")
}

pub(super) fn numeric_looking(text: &str) -> bool {
    let bytes = text.as_bytes();
    match bytes.first().copied() {
        Some(b'0'..=b'9') => true,
        Some(b'.') => matches!(bytes.get(1), Some(b'0'..=b'9' | b'i' | b'I' | b'n' | b'N')),
        Some(b'+' | b'-') => matches!(bytes.get(1), Some(b'0'..=b'9' | b'.')),
        _ => false,
    }
}

pub(super) fn skip_spaces(bytes: &[u8], position: &mut usize) {
    while bytes.get(*position).is_some_and(u8::is_ascii_whitespace) {
        *position = position.saturating_add(1);
    }
}

pub(super) fn without_cr(line: &str) -> &str {
    line.strip_suffix('\r').map_or(line, |value| value)
}

pub(super) fn error(kind: YamlErrorKind, line: usize, column: usize) -> YamlError {
    YamlError { kind, line, column }
}
