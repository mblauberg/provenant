use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("expected 32 lowercase hexadecimal ID digits")]
pub struct IdError;

fn decode_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N.checked_mul(2)? {
        return None;
    }
    let mut result = [0; N];
    let (pairs, _) = text.as_bytes().as_chunks::<2>();
    for (out, pair) in result.iter_mut().zip(pairs) {
        *out = crate::bytes::decode_hex_byte(pair)?;
    }
    Some(result)
}

macro_rules! ids {
    ($($name:ident),+ $(,)?) => {$ (
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name([u8; 16]);
        impl $name {
            pub const fn from_bytes(bytes: [u8; 16]) -> Self { Self(bytes) }
            pub const fn as_bytes(&self) -> &[u8; 16] { &self.0 }
            pub fn parse(text: &str) -> Result<Self, IdError> {
                decode_hex(text).map(Self).ok_or(IdError)
            }
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&crate::hex(&self.0))
            }
        }
        impl std::str::FromStr for $name {
            type Err = IdError;
            fn from_str(text: &str) -> Result<Self, Self::Err> { Self::parse(text) }
        }
        impl TryFrom<String> for $name {
            type Error = IdError;
            fn try_from(text: String) -> Result<Self, Self::Error> { Self::parse(&text) }
        }
        impl From<$name> for String {
            fn from(id: $name) -> Self { id.to_string() }
        }
    )+};
}
ids!(
    HostId,
    ProjectId,
    WorkspaceId,
    RepoId,
    ActorId,
    RunId,
    TaskId,
    ClaimId,
    MessageId,
    AttemptId,
    DeliveryId,
    SessionId,
    InstallId,
    ImportId
);

/// Caller-selected idempotency key, as specified by execution-run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct OperationId(String);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("operation ID must be 1-128 ASCII letters/digits/._-, starting alphanumeric")]
pub struct OperationIdError;
impl OperationId {
    pub fn parse(text: &str) -> Result<Self, OperationIdError> {
        if !(1..=128).contains(&text.len())
            || !text
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_alphanumeric())
            || !text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(OperationIdError);
        }
        Ok(Self(text.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Display for OperationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::str::FromStr for OperationId {
    type Err = OperationIdError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}
impl TryFrom<String> for OperationId {
    type Error = OperationIdError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}
impl From<OperationId> for String {
    fn from(id: OperationId) -> Self {
        id.0
    }
}

/// Full Git object ID; the repository owner checks the selected algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum GitOid {
    Sha1([u8; 20]),
    Sha256([u8; 32]),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Git object ID must contain exactly 40 or 64 lowercase hexadecimal digits")]
pub struct GitOidError;
impl GitOid {
    pub fn parse(text: &str) -> Result<Self, GitOidError> {
        match text.len() {
            40 => decode_hex(text).map(Self::Sha1),
            64 => decode_hex(text).map(Self::Sha256),
            _ => None,
        }
        .ok_or(GitOidError)
    }
}
impl std::fmt::Display for GitOid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&match self {
            Self::Sha1(bytes) => crate::hex(bytes),
            Self::Sha256(bytes) => crate::hex(bytes),
        })
    }
}
impl std::str::FromStr for GitOid {
    type Err = GitOidError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}
impl TryFrom<String> for GitOid {
    type Error = GitOidError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}
impl From<GitOid> for String {
    fn from(oid: GitOid) -> Self {
        oid.to_string()
    }
}

/// Provenant record digest, distinct from raw-file and Git identities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest([u8; 32]);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("digest must be sha256: followed by 64 lowercase hexadecimal digits")]
pub struct DigestError;
impl Digest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
    pub fn parse(text: &str) -> Result<Self, DigestError> {
        text.strip_prefix("sha256:")
            .and_then(decode_hex)
            .map(Self)
            .ok_or(DigestError)
    }
}
impl std::fmt::Display for Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sha256:{}", crate::hex(&self.0))
    }
}
impl std::str::FromStr for Digest {
    type Err = DigestError;
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}
impl TryFrom<String> for Digest {
    type Error = DigestError;
    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::parse(&text)
    }
}
impl From<Digest> for String {
    fn from(digest: Digest) -> Self {
        digest.to_string()
    }
}

#[cfg(test)]
mod object_tests {
    use super::*;
    #[test]
    fn full_git_ids_require_lowercase_without_algorithm_confusion() {
        for length in [40, 64] {
            let input = "ab".repeat(length / 2);
            assert!(GitOid::parse(&input.to_ascii_uppercase()).is_err());
            let oid = GitOid::parse(&input).unwrap();
            assert_eq!(oid.to_string(), input.to_ascii_lowercase());
            let wire = serde_json::to_string(&oid).unwrap();
            assert_eq!(serde_json::from_str::<GitOid>(&wire).unwrap(), oid);
        }
        for input in ["abc123", &"a".repeat(39), &"g".repeat(64), &"é".repeat(20)] {
            assert!(GitOid::parse(input).is_err());
        }
        let digest = Digest::from_bytes([0; 32]);
        assert_eq!(Digest::parse(&digest.to_string()).unwrap(), digest);
        assert!(Digest::parse(&"a".repeat(64)).is_err());
        assert!(Digest::parse(&format!("sha256:{}", "A".repeat(64))).is_err());
    }
    #[test]
    fn parses_recorded_git_object_formats() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/git-oids.json"
        )))
        .unwrap();
        let objects = fixture
            .get("objects")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(objects.len(), 2);
        for object in objects {
            let text = object
                .get("oid")
                .and_then(serde_json::Value::as_str)
                .unwrap();
            let oid = GitOid::parse(text).unwrap();
            assert_eq!(oid.to_string(), text);
            match object
                .get("format")
                .and_then(serde_json::Value::as_str)
                .unwrap()
            {
                "sha1" => assert!(matches!(oid, GitOid::Sha1(_))),
                "sha256" => assert!(matches!(oid, GitOid::Sha256(_))),
                format => panic!("unexpected fixture format: {format}"),
            }
        }
    }
    #[test]
    fn operation_keys_are_bounded_and_cannot_be_path_components() {
        for good in ["a", "A.key-1_2", &"x".repeat(128)] {
            assert!(OperationId::parse(good).is_ok());
        }
        for bad in ["", ".x", "-x", "a/b", "a b", "é", &"x".repeat(129)] {
            assert!(OperationId::parse(bad).is_err());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identifiers_are_validated_at_the_wire_boundary() {
        let spelling = "01abcdef2345678901abcdef23456789";
        let id = ProjectId::parse(spelling).unwrap();
        assert_eq!(id.to_string(), spelling);
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            format!("\"{spelling}\"")
        );
        assert_eq!(
            serde_json::from_str::<ProjectId>(&format!("\"{spelling}\"")).unwrap(),
            id
        );
        for bad in [
            "",
            "ab",
            "01ABCDEF2345678901abcdef23456789",
            "éabcdef2345678901abcdef23456789",
            "0000000000000000000000000000000g",
        ] {
            assert!(ProjectId::parse(bad).is_err());
            assert!(serde_json::from_value::<ProjectId>(serde_json::json!(bad)).is_err());
        }
        assert!(serde_json::from_str::<RunId>("12").is_err());
        assert_eq!(RunId::from_bytes([255; 16]).to_string(), "f".repeat(32));
    }

    #[test]
    fn message_ids_round_trip() {
        let id = MessageId::from_bytes([42; 16]);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<MessageId>(&json).unwrap(), id);
        assert!(serde_json::from_str::<MessageId>("\"invalid\"").is_err());
    }
}
