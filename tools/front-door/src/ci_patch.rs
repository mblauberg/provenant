//! Schema for the generated-artefact CI handoff.

use provenant_core::GitOid;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[allow(dead_code)]
pub(crate) enum GeneratorStatus {
    Clean,
    Drift,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct GeneratorManifest {
    pub name: String,
    pub status: GeneratorStatus,
    pub files: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct GeneratedManifest {
    pub repo: String,
    pub workflow: String,
    pub run_id: u64,
    pub run_attempt: u32,
    pub head_sha: GitOid,
    pub generators: Vec<GeneratorManifest>,
    pub patch_sha256: String,
}

/// Kept as executable JSON so the contract fixture can be consumed by the
/// batch-level tests without coupling them to CI or a forge account.
#[cfg(test)]
pub(crate) const MANIFEST_FIXTURE: &str = r#"{
  "repo": "owner/repo",
  "workflow": "ci.yml",
  "run_id": 123,
  "run_attempt": 1,
  "head_sha": "0123456789abcdef0123456789abcdef01234567",
  "generators": [
    {"name": "swift-fixtures", "status": "drift", "files": ["fixtures/a.swift"]}
  ],
  "patch_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
}"#;

#[cfg(test)]
mod tests {
    use super::{GeneratedManifest, GeneratorStatus, MANIFEST_FIXTURE};

    #[test]
    fn manifest_fixture_covers_generated_patch_identity() {
        let manifest: GeneratedManifest = serde_json::from_str(MANIFEST_FIXTURE).unwrap();
        assert_eq!(manifest.repo, "owner/repo");
        assert_eq!(manifest.run_attempt, 1);
        assert_eq!(manifest.generators[0].status, GeneratorStatus::Drift);
        assert_eq!(manifest.generators[0].files, ["fixtures/a.swift"]);
    }
}
