use crate::{FileDigest, PathWire};
use serde::{Deserialize, Deserializer, Serialize};

/// Stored artefact reference. Unlike an output fact, an input may lack a digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub path: PathWire,
    #[serde(deserialize_with = "digest")]
    pub digest: Option<FileDigest>,
}

// FileDigest remains Serialize-only for edge facts. Only this stored record
// accepts it as input, with the same canonical SHA-256 spelling as core Digest.
fn digest<'de, D: Deserializer<'de>>(decoder: D) -> Result<Option<FileDigest>, D::Error> {
    Option::<provenant_core::Digest>::deserialize(decoder)
        .map(|digest| digest.map(|value| FileDigest::from_bytes(*value.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_artifact_round_trips_and_validates_optional_digest() {
        for digest in [None, Some(FileDigest::from_bytes([7; 32]))] {
            let reference = ArtifactRef {
                extra: Default::default(),
                path: PathWire::from_relative(
                    &provenant_core::RelativePath::parse("result.txt").unwrap(),
                )
                .unwrap(),
                digest,
            };
            let bytes = serde_json::to_vec(&reference).unwrap();
            assert_eq!(
                serde_json::from_slice::<ArtifactRef>(&bytes).unwrap(),
                reference
            );
        }
        for digest in [
            "sha256:xyz",
            "sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            let value = serde_json::json!({"path":{"encoding":"utf8","path":"a"},"digest":digest});
            assert!(serde_json::from_value::<ArtifactRef>(value).is_err());
        }
    }
}
