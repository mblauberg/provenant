//! Quoting for command strings that a shell will interpret.
//!
//! One implementation per quoting behaviour lives here; callers name the
//! behaviour they need instead of spelling out the escape rules again.

use std::ffi::OsStr;
use thiserror::Error;

/// A path cannot be represented as one safe shell word.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum ShellError {
    #[error("path contains a control character and cannot be quoted safely")]
    InvalidPath,
}

/// Single-quote `text` for a POSIX shell: an apostrophe becomes `'\''`.
pub fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Quote `text` for a POSIX shell only when it holds a byte outside the
/// unquoted safe set, leaving ordinary paths readable.
pub fn quote_if_needed(text: &str) -> String {
    if text
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"/._-".contains(&byte))
    {
        text.to_owned()
    } else {
        quote(text)
    }
}

/// Quote `text` as one literal word for sh, bash, zsh and fish.
///
/// A backslash inside single quotes is an escape for fish, so an apostrophe
/// and a backslash each take a double-quoted form that every one of those
/// shells reads literally. Empty plain parts are skipped so no two single
/// quotes become adjacent, which zsh would read as `RC_QUOTES`.
pub fn quote_portable(text: &str) -> String {
    if text.is_empty() {
        return "''".into();
    }
    let mut quoted = String::new();
    let mut plain = String::new();
    for c in text.chars() {
        let literal = match c {
            '\'' => "\"'\"",
            '\\' => "\"\\\\\"",
            _ => {
                plain.push(c);
                continue;
            }
        };
        if !plain.is_empty() {
            quoted.push_str(&format!("'{plain}'"));
            plain.clear();
        }
        quoted.push_str(literal);
    }
    if !plain.is_empty() {
        quoted.push_str(&format!("'{plain}'"));
    }
    quoted
}

/// Single-quote an `OsStr` for a POSIX shell, writing non-UTF-8 bytes as
/// `printf` octal escapes so every byte survives the round trip.
pub fn quote_os(value: &OsStr) -> Result<String, ShellError> {
    use std::os::unix::ffi::OsStrExt;
    if value
        .to_str()
        .is_some_and(|text| text.chars().any(char::is_control))
    {
        return Err(ShellError::InvalidPath);
    }
    let mut quoted = String::from("'");
    for &byte in value.as_bytes() {
        match byte {
            0..=31 | 127 => return Err(ShellError::InvalidPath),
            b'\'' => quoted.push_str("'\\''"),
            32..=126 => quoted.push(char::from(byte)),
            _ => quoted.push_str(&format!("'$(printf '\\{byte:03o}')'")),
        }
    }
    quoted.push('\'');
    Ok(quoted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_apostrophes_for_posix_shells() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn quote_if_needed_leaves_safe_words_alone() {
        assert_eq!(quote_if_needed("/tmp/a-b_c.d"), "/tmp/a-b_c.d");
        assert_eq!(quote_if_needed("two words"), "'two words'");
    }

    #[test]
    fn quote_portable_handles_apostrophes_backslashes_and_empty_text() {
        assert_eq!(quote_portable(""), "''");
        assert_eq!(quote_portable("plain"), "'plain'");
        assert_eq!(quote_portable("it's"), "'it'\"'\"'s'");
        assert_eq!(quote_portable("a\\b"), "'a'\"\\\\\"'b'");
    }

    #[test]
    fn quote_os_refuses_control_bytes() {
        use std::os::unix::ffi::OsStrExt;
        assert!(quote_os(OsStr::from_bytes(b"bad\npath")).is_err());
        assert_eq!(quote_os(OsStr::from_bytes(b"it's")).unwrap(), "'it'\\''s'");
        assert_eq!(
            quote_os(OsStr::from_bytes(&[b'a', 0x80])).unwrap(),
            "'a'$(printf '\\200')'"
        );
    }
}
