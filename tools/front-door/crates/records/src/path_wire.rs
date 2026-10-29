use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::borrow::Cow;
use std::path::PathBuf;

use provenant_core::{RelativePath, RelativePathError};
use thiserror::Error;

/// Portable tagged representation for a contained path in a durable record.
/// Its private state can only be created from a checked relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathWire {
    path: RelativePath,
    encoding: PathEncoding,
    extra: crate::ExtraFields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathEncoding {
    Utf8,
    NativeBytes,
}

#[derive(Debug, Error)]
pub enum PathWireError {
    #[error("path is not a valid relative path")]
    Relative(#[from] RelativePathError),
    #[error("native path encoding is unavailable on this platform")]
    UnsupportedEncoding,
    #[error("path encoding must be `utf8` or `native_bytes`")]
    InvalidEncoding,
    #[error("native path encoding requires a path that is not valid UTF-8")]
    NonCanonicalEncoding,
    #[error("native path bytes must be lowercase hexadecimal")]
    InvalidHex,
    #[error("native path bytes are not valid for this platform")]
    InvalidNativePath,
}

#[derive(Deserialize)]
struct RawPathWire {
    #[serde(flatten)]
    extra: crate::ExtraFields,
    encoding: String,
    path: String,
}

impl TryFrom<RawPathWire> for PathWire {
    type Error = PathWireError;

    fn try_from(raw: RawPathWire) -> Result<Self, Self::Error> {
        let (encoding, path) = match raw.encoding.as_str() {
            "utf8" => {
                let path = PathBuf::from(raw.path);
                let relative = RelativePath::parse(path)?;
                if relative.as_path().to_str().is_none() {
                    return Err(PathWireError::NonCanonicalEncoding);
                }
                (PathEncoding::Utf8, relative)
            }
            "native_bytes" => {
                let path = decode_native_bytes(&raw.path)?;
                let relative = RelativePath::parse(path)?;
                if relative.as_path().to_str().is_some() {
                    return Err(PathWireError::NonCanonicalEncoding);
                }
                (PathEncoding::NativeBytes, relative)
            }
            _ => return Err(PathWireError::InvalidEncoding),
        };
        Ok(Self {
            path,
            encoding,
            extra: raw.extra,
        })
    }
}

impl PathWire {
    pub fn from_relative(path: &RelativePath) -> Result<Self, PathWireError> {
        let encoding = if path.as_path().to_str().is_some() {
            PathEncoding::Utf8
        } else {
            #[cfg(unix)]
            {
                PathEncoding::NativeBytes
            }
            #[cfg(not(unix))]
            {
                return Err(PathWireError::UnsupportedEncoding);
            }
        };
        Ok(Self {
            extra: Default::default(),
            path: path.clone(),
            encoding,
        })
    }

    pub fn to_relative_path(&self) -> Result<RelativePath, PathWireError> {
        Ok(self.path.clone())
    }

    pub(crate) fn encoded_len(&self) -> usize {
        let bytes = self.path.as_path().as_os_str().len();
        if self.encoding == PathEncoding::NativeBytes {
            bytes.saturating_mul(2)
        } else {
            bytes
        }
    }

    pub fn as_path_text(&self) -> Option<&str> {
        self.path.as_path().to_str()
    }
}

impl Serialize for PathWire {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (encoding, path) = match self.encoding {
            PathEncoding::Utf8 => (
                "utf8",
                Cow::Borrowed(self.path.as_path().to_str().ok_or_else(|| {
                    serde::ser::Error::custom("UTF-8 path encoding has an invalid native path")
                })?),
            ),
            PathEncoding::NativeBytes => {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    (
                        "native_bytes",
                        Cow::Owned(provenant_core::hex(
                            self.path.as_path().as_os_str().as_bytes(),
                        )),
                    )
                }
                #[cfg(not(unix))]
                {
                    return Err(serde::ser::Error::custom(
                        "native path encoding is unavailable on this platform",
                    ));
                }
            }
        };

        let mut state = serializer.serialize_map(Some(2 + self.extra.len()))?;
        state.serialize_entry("encoding", encoding)?;
        state.serialize_entry("path", path.as_ref())?;
        for (key, value) in &self.extra {
            state.serialize_entry(key, value)?;
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for PathWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = RawPathWire::deserialize(deserializer)?;
        Self::try_from(raw).map_err(serde::de::Error::custom)
    }
}

fn decode_native_bytes(encoded: &str) -> Result<PathBuf, PathWireError> {
    let bytes = provenant_core::unhex(encoded).ok_or(PathWireError::InvalidHex)?;
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
