use serde::Serializer;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

/// One workspace-relative source file and its exact contents.
///
/// The caller is responsible for enumerating the complete source inventory
/// and for reading each file through its safe filesystem boundary. This pure
/// record layer validates names and hashes the supplied bytes only.
#[derive(Clone, Copy, Debug)]
pub struct SourceInput<'a> {
    pub name: &'a str,
    pub bytes: &'a [u8],
}

/// Failures while validating or hashing a source inventory.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SourceDigestError {
    #[error("source inventory must contain at least one file")]
    EmptyInventory,
    #[error("source inventory contains a non-normal workspace-relative POSIX name")]
    InvalidName,
    #[error("source inventory contains duplicate names")]
    DuplicateName,
    #[error("source inventory entry is too large to frame")]
    LengthOverflow,
    #[error("source digest must be sha256: followed by 64 lowercase hexadecimal digits")]
    InvalidDigest,
}

/// Source-tree identity. This is deliberately distinct from `FileDigest`:
/// it hashes a framed inventory of names and bytes, not one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceDigest([u8; 32]);

impl SourceDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn parse(text: &str) -> Result<Self, SourceDigestError> {
        provenant_core::Digest::parse(text)
            .map(|digest| Self(*digest.as_bytes()))
            .map_err(|_| SourceDigestError::InvalidDigest)
    }
}

impl serde::Serialize for SourceDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> serde::Deserialize<'de> for SourceDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let text = <String as serde::Deserialize>::deserialize(deserializer)?;
        Self::parse(&text).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Display for SourceDigest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "sha256:{}", provenant_core::hex(&self.0))
    }
}

/// Hash a complete source inventory using the versioned, length-framed format.
///
/// Names are sorted by their UTF-8 bytes. Each entry contributes its
/// big-endian u64 name length, name bytes, big-endian u64 content length, and
/// exact content bytes after the `provenant-source-v1\0` domain prefix.
pub fn source_digest(inputs: &[SourceInput<'_>]) -> Result<SourceDigest, SourceDigestError> {
    if inputs.is_empty() {
        return Err(SourceDigestError::EmptyInventory);
    }

    let mut ordered: Vec<&SourceInput<'_>> = inputs.iter().collect();
    for input in &ordered {
        if !is_normal_workspace_name(input.name) {
            return Err(SourceDigestError::InvalidName);
        }
    }
    ordered.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    if ordered.windows(2).any(|pair| pair[0].name == pair[1].name) {
        return Err(SourceDigestError::DuplicateName);
    }

    let mut digest = Sha256::new();
    digest.update(b"provenant-source-v1\0");
    for input in ordered {
        let name_len =
            u64::try_from(input.name.len()).map_err(|_| SourceDigestError::LengthOverflow)?;
        let bytes_len =
            u64::try_from(input.bytes.len()).map_err(|_| SourceDigestError::LengthOverflow)?;
        digest.update(name_len.to_be_bytes());
        digest.update(input.name.as_bytes());
        digest.update(bytes_len.to_be_bytes());
        digest.update(input.bytes);
    }
    Ok(SourceDigest(digest.finalize().into()))
}

fn is_normal_workspace_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.ends_with('/')
        && !name.contains('\\')
        && !name.as_bytes().contains(&0)
        && name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

#[cfg(test)]
mod tests {
    use super::{SourceDigest, SourceDigestError, SourceInput, source_digest};

    #[test]
    fn hashes_sorted_framed_names_and_exact_contents() {
        let forward = [
            SourceInput {
                name: "Cargo.toml",
                bytes: b"[package]\n",
            },
            SourceInput {
                name: "src/lib.rs",
                bytes: b"pub fn main() {}\n",
            },
        ];
        let reverse = [forward[1], forward[0]];
        let digest = source_digest(&forward).expect("valid source inventory");
        assert_eq!(source_digest(&reverse).expect("permuted inventory"), digest);
        assert_eq!(
            digest.to_string(),
            "sha256:27c2cd4c01c7c999250e539c1093352e941a88544b8a49c721dc08ea33cd75bf"
        );

        let changed_boundary = [
            SourceInput {
                name: "Cargo.toml",
                bytes: b"[package]\nsrc/lib.rs",
            },
            SourceInput {
                name: "src/lib.rs",
                bytes: b"pub fn main() {}\n",
            },
        ];
        assert_ne!(
            source_digest(&changed_boundary).expect("different contents"),
            digest
        );
    }

    #[test]
    fn rejects_empty_duplicate_and_non_normal_names() {
        assert_eq!(source_digest(&[]), Err(SourceDigestError::EmptyInventory));

        let duplicate = [
            SourceInput {
                name: "src/lib.rs",
                bytes: b"one",
            },
            SourceInput {
                name: "src/lib.rs",
                bytes: b"two",
            },
        ];
        assert_eq!(
            source_digest(&duplicate),
            Err(SourceDigestError::DuplicateName)
        );

        for name in [
            "/absolute",
            "../escape",
            "a/../b",
            "./local",
            "a//b",
            "a/",
            "a\\b",
            "a\0b",
        ] {
            let invalid = [SourceInput { name, bytes: b"x" }];
            assert_eq!(
                source_digest(&invalid),
                Err(SourceDigestError::InvalidName),
                "accepted invalid name {name:?}"
            );
        }
    }

    #[test]
    fn source_digest_wire_form_is_checked_and_distinctly_typed() {
        let digest = SourceDigest::from_bytes([0xab; 32]);
        let text = format!("sha256:{}", "ab".repeat(32));
        assert_eq!(digest.to_string(), text);
        assert_eq!(SourceDigest::parse(&text).expect("valid digest"), digest);
        assert_eq!(
            serde_json::to_string(&digest).expect("serialise digest"),
            format!("\"{text}\"")
        );
        assert_eq!(
            serde_json::from_str::<SourceDigest>(&format!("\"{text}\"")).expect("decode digest"),
            digest
        );
        let uppercase = format!("sha256:{}", "A".repeat(64));
        let short = format!("sha256:{}", "a".repeat(63));
        for invalid in ["sha256:xyz", uppercase.as_str(), short.as_str()] {
            assert_eq!(
                SourceDigest::parse(invalid),
                Err(SourceDigestError::InvalidDigest)
            );
        }
    }
}
