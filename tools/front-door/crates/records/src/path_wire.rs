use std::path::PathBuf;

use provenant_core::{RelativePath, RelativePathError};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Portable tagged representation for a contained path in a durable record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "encoding",
    content = "path",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PathWire {
    Utf8(String),
    NativeBytes(String),
}

#[derive(Debug, Error)]
pub enum PathWireError {
    #[error("path is not a valid relative path")]
    Relative(#[from] RelativePathError),
    #[error("native path encoding is unavailable on this platform")]
    UnsupportedEncoding,
    #[error("native path bytes must be lowercase hexadecimal")]
    InvalidHex,
    #[error("native path bytes are not valid for this platform")]
    InvalidNativePath,
}

impl PathWire {
    pub fn from_relative(path: &RelativePath) -> Result<Self, PathWireError> {
        let native = path.as_path();
        if let Some(text) = native.to_str() {
            Ok(Self::Utf8(text.to_owned()))
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                Ok(Self::NativeBytes(hex(native.as_os_str().as_bytes())))
            }
            #[cfg(not(unix))]
            {
                Err(PathWireError::UnsupportedEncoding)
            }
        }
    }

    pub fn to_relative_path(&self) -> Result<RelativePath, PathWireError> {
        let path = match self {
            Self::Utf8(text) => PathBuf::from(text),
            Self::NativeBytes(encoded) => decode_native_bytes(encoded)?,
        };
        RelativePath::parse(path).map_err(PathWireError::Relative)
    }

    pub fn as_path_text(&self) -> Option<&str> {
        match self {
            Self::Utf8(text) => Some(text),
            Self::NativeBytes(_) => None,
        }
    }
}

fn decode_native_bytes(encoded: &str) -> Result<PathBuf, PathWireError> {
    if !encoded.len().is_multiple_of(2)
        || !encoded
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(PathWireError::InvalidHex);
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    let (pairs, remainder) = encoded.as_bytes().as_chunks::<2>();
    for pair in pairs {
        let Some(high) = pair.first().and_then(|byte| hex_nibble(*byte)) else {
            return Err(PathWireError::InvalidHex);
        };
        let Some(low) = pair.get(1).and_then(|byte| hex_nibble(*byte)) else {
            return Err(PathWireError::InvalidHex);
        };
        bytes.push((high << 4) | low);
    }
    if !remainder.is_empty() {
        return Err(PathWireError::InvalidHex);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let text = String::from_utf8(bytes).map_err(|_| PathWireError::UnsupportedEncoding)?;
        Ok(PathBuf::from(text))
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}
