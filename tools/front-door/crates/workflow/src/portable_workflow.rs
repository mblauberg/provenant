//! Skill-owned project-artifact fallback, without optional adapters.
use crate::policy::strip;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path};

#[derive(Debug, thiserror::Error)]
pub enum PortableError {
    #[error("{0}")]
    Invalid(String),
    #[error("'accepted_artifact_identity'")]
    MissingArtifactIdentity,
    #[error("{path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path}: {source}")]
    Json {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: Python integer is outside the i64/u64 range")]
    NumberOutOfRange { path: std::path::PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArtifactIdentity {
    Digest(DigestIdentity),
    Git(GitIdentity),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DigestIdentity {
    pub digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitIdentity {
    pub git_revision: GitRevision,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GitRevision {
    pub repository: String,
    pub commit: String,
    pub tree: String,
}

fn invalid(message: &str) -> PortableError {
    PortableError::Invalid(message.into())
}
fn hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// Validate before deserialising, retaining the source validator's diagnostics.
pub fn validate_artifact_identity(value: &Value) -> Result<ArtifactIdentity, PortableError> {
    let object = value
        .as_object()
        .filter(|o| o.len() == 1)
        .ok_or_else(|| invalid("accepted artifact identity must be one closed union member"))?;
    if let Some(value) = object.get("digest") {
        let digest = value
            .as_str()
            .filter(|s| {
                s.strip_prefix("sha256:")
                    .is_some_and(|s| s.len() == 64 && hex(s))
            })
            .ok_or_else(|| invalid("accepted artifact digest is invalid"))?;
        return Ok(ArtifactIdentity::Digest(DigestIdentity {
            digest: digest.into(),
        }));
    }
    let revision = object
        .get("git_revision")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("accepted artifact identity must contain digest or git_revision"))?;
    if revision.len() != 3
        || !["repository", "commit", "tree"]
            .iter()
            .all(|key| revision.contains_key(*key))
    {
        return Err(invalid("accepted Git revision fields are invalid"));
    }
    let repository = revision["repository"]
        .as_str()
        .filter(|s| !strip(s).is_empty())
        .ok_or_else(|| invalid("accepted Git repository is invalid"))?;
    let path = Path::new(repository);
    if path.is_absolute() || path.components().any(|p| p == Component::ParentDir) {
        return Err(invalid("accepted Git repository must be safe and relative"));
    }
    let (Some(commit), Some(tree)) = (revision["commit"].as_str(), revision["tree"].as_str())
    else {
        return Err(invalid("accepted Git object identities are invalid"));
    };
    if ![40, 64].contains(&commit.chars().count()) || tree.chars().count() != commit.chars().count()
    {
        return Err(invalid("accepted Git object widths are invalid"));
    }
    if !hex(commit) || !hex(tree) {
        return Err(invalid("accepted Git object identities are invalid"));
    }
    Ok(ArtifactIdentity::Git(GitIdentity {
        git_revision: GitRevision {
            repository: repository.into(),
            commit: commit.into(),
            tree: tree.into(),
        },
    }))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortableArtifact {
    pub schema_version: u8,
    pub skill: String,
    pub artifact_kind: String,
    pub artifact_basis: String,
    pub source_digest: String,
    pub adapters_used: Vec<String>,
    pub status: String,
}

pub fn digest(path: &Path) -> Result<String, PortableError> {
    let bytes = fs::read(path).map_err(|source| PortableError::Io {
        path: path.into(),
        source,
    })?;
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}
pub fn read_object(path: &Path) -> Result<Map<String, Value>, PortableError> {
    crate::pending::read_python_object(path)
}

#[cfg(test)]
mod out_of_range_tests {
    use super::*;

    #[test]
    fn out_of_range_python_integer_is_a_typed_improvement_refusal() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("context.json");
        fs::write(&path, br#"{"value":18446744073709551616}"#).unwrap();
        assert!(matches!(
            read_object(&path),
            Err(PortableError::NumberOutOfRange { path: error_path }) if error_path == path
        ));
    }
}

/// Execute the existing fallback's effects; CLI argument parsing stays at the edge.
pub fn execute(
    skill_root: &Path,
    context_path: &Path,
    output: &Path,
) -> Result<PortableArtifact, PortableError> {
    let skill_root = fs::canonicalize(skill_root).map_err(|source| PortableError::Io {
        path: skill_root.into(),
        source,
    })?;
    let contract = read_object(&skill_root.join("portable-workflow.v1.json"))?;
    let keys = [
        "schema_version",
        "skill",
        "artifact_kind",
        "artifact_basis",
        "required_context_fields",
    ];
    let version = contract.get("schema_version");
    // Python accepts True and 1.0 as equal to 1.
    if contract.len() != keys.len()
        || !keys.iter().all(|key| contract.contains_key(*key))
        || !version.is_some_and(|v| {
            v == &Value::Bool(true)
                || v.as_i64() == Some(1)
                || v.as_u64() == Some(1)
                || (v.is_f64() && v.as_f64() == Some(1.0))
        })
    {
        return Err(invalid("portable workflow contract is invalid"));
    }
    if contract["skill"].as_str() != skill_root.file_name().and_then(|s| s.to_str())
        || contract["artifact_basis"] != "project-artifacts"
    {
        return Err(invalid("portable workflow contract identity is invalid"));
    }
    let kind = contract["artifact_kind"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("portable workflow artifact kind is invalid"))?;
    let fields = contract["required_context_fields"]
        .as_array()
        .filter(|a| !a.is_empty() && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty())))
        .ok_or_else(|| invalid("portable workflow context contract is invalid"))?;
    let context = read_object(context_path)?;
    if fields
        .iter()
        .filter_map(Value::as_str)
        .any(|key| !context.contains_key(key))
    {
        return Err(invalid("portable workflow context is incomplete"));
    }
    let identity = context
        .get("accepted_artifact_identity")
        .ok_or(PortableError::MissingArtifactIdentity)?;
    validate_artifact_identity(identity)?;
    if !skill_root.join("SKILL.md").is_file() {
        return Err(invalid("portable workflow has no owning skill"));
    }
    let artifact = PortableArtifact {
        schema_version: 1,
        skill: contract["skill"].as_str().unwrap_or_default().into(),
        artifact_kind: kind.into(),
        artifact_basis: "project-artifacts".into(),
        source_digest: digest(context_path)?,
        adapters_used: vec![],
        status: "completed".into(),
    };
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|source| PortableError::Io {
            path: parent.into(),
            source,
        })?;
    }
    let value = serde_json::to_value(&artifact).map_err(|source| PortableError::Json {
        path: output.into(),
        source,
    })?;
    fs::write(output, crate::pending::python_json(&value) + "\n").map_err(|source| {
        PortableError::Io {
            path: output.into(),
            source,
        }
    })?;
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn identity_validation_matches_python_for_closed_unions_and_git_paths() {
        let mut cases = vec![
            Value::Null,
            json!({}),
            json!({"digest": "bad"}),
            json!({"digest": 1}),
            json!({"digest": format!("sha256:{}", "a".repeat(64))}),
            json!({"digest": format!("sha256:{}", "A".repeat(64))}),
            json!({"other": {}}),
            json!({"git_revision": []}),
            json!({"git_revision": {}}),
        ];
        for repo in [
            "",
            "  ",
            "../escape",
            "/absolute",
            "a/../b",
            ".",
            "./relative",
            "路径",
        ] {
            cases.push(json!({"git_revision":{"repository":repo, "commit":"a".repeat(40), "tree":"b".repeat(40)}}));
        }
        for (commit, tree) in [
            (json!(1), json!("b".repeat(40))),
            (json!("a".repeat(40)), json!("b".repeat(64))),
            (json!("z".repeat(40)), json!("b".repeat(40))),
            (json!("é".repeat(40)), json!("b".repeat(40))),
            (json!("a".repeat(64)), json!("b".repeat(64))),
        ] {
            cases.push(json!({"git_revision":{"repository":"repo", "commit":commit, "tree":tree}}));
        }
        let actual: Vec<Value> = cases
            .iter()
            .map(|v| {
                validate_artifact_identity(v)
                    .err()
                    .map(|e| Value::String(e.to_string()))
                    .unwrap_or(Value::Null)
            })
            .collect();
        assert!(validate_artifact_identity(&cases[4]).is_ok());
        assert_eq!(
            serde_json::to_value(validate_artifact_identity(&cases[4]).unwrap()).unwrap(),
            cases[4]
        );
        if let Some(expected) = crate::test_support::oracle("portable_workflow", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn artifact_is_sorted_ascii_json_and_hashes_original_context_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("审查");
        fs::create_dir(&skill).unwrap();
        fs::write(skill.join("SKILL.md"), "owner").unwrap();
        let contract = json!({"schema_version":1,"skill":"审查","artifact_kind":"évidence","artifact_basis":"project-artifacts","required_context_fields":["accepted_artifact_identity"]});
        fs::write(
            skill.join("portable-workflow.v1.json"),
            contract.to_string(),
        )
        .unwrap();
        let context = temp.path().join("context.json");
        fs::write(&context, json!({"accepted_artifact_identity":{"digest":format!("sha256:{}", "0".repeat(64))},"extra":null}).to_string()).unwrap();
        let output = temp.path().join("nested/artifact.json");
        let result = execute(&skill, &context, &output).unwrap();
        assert_eq!(result.source_digest, digest(&context).unwrap());
        let bytes = fs::read_to_string(&output).unwrap();
        assert!(
            bytes
                .starts_with("{\"adapters_used\": [], \"artifact_basis\": \"project-artifacts\", ")
        );
        assert!(bytes.contains("\\u00e9vidence"));
        assert!(bytes.ends_with("\n"));
        assert!(bytes.is_ascii());
        fs::write(&context, "{}").unwrap();
        assert_eq!(
            execute(&skill, &context, &output).unwrap_err().to_string(),
            "portable workflow context is incomplete"
        );
    }

    #[test]
    fn finite_json_workflow_effects_and_contract_failures_match_python() {
        let temp = tempfile::tempdir().unwrap();
        let skill = temp.path().join("portable-é");
        fs::create_dir(&skill).unwrap();
        let owner = skill.join("SKILL.md");
        let context_path = temp.path().join("context.json");
        let output = temp.path().join("rust/artifact.json");
        let python_output = temp.path().join("python/artifact.json");
        let base = json!({"schema_version":1,"skill":"portable-é","artifact_kind":"évidence 🦀","artifact_basis":"project-artifacts","required_context_fields":["accepted_artifact_identity"]});
        let context = json!({"accepted_artifact_identity":{"digest":format!("sha256:{}","a".repeat(64))},"unused":null});
        for mutation in 0..15 {
            let mut contract = base.clone();
            let mut context = context.clone();
            fs::write(&owner, "owner").unwrap();
            match mutation {
                1 => contract["schema_version"] = json!(true),
                2 => contract["schema_version"] = json!(1.0),
                3 => contract["schema_version"] = json!(2),
                4 => contract["extra"] = json!(1),
                5 => contract["skill"] = json!("other"),
                6 => contract["artifact_kind"] = json!(""),
                7 => contract["required_context_fields"] = json!([]),
                8 => contract["required_context_fields"] = json!([false]),
                9 => context = json!({}),
                10 => context["accepted_artifact_identity"] = json!({"digest":"bad"}),
                11 => fs::remove_file(&owner).unwrap(),
                12 => contract["artifact_basis"] = json!("other"),
                13 => contract["required_context_fields"] = json!(["other"]),
                14 => {
                    contract["required_context_fields"] = json!(["unused"]);
                    context
                        .as_object_mut()
                        .unwrap()
                        .remove("accepted_artifact_identity");
                }
                _ => {}
            }
            fs::write(
                skill.join("portable-workflow.v1.json"),
                contract.to_string(),
            )
            .unwrap();
            fs::write(&context_path, context.to_string()).unwrap();
            let actual = match execute(&skill, &context_path, &output) {
                Ok(_) => json!({"artifact":fs::read_to_string(&output).unwrap()}),
                Err(error) => json!({"error":error.to_string()}),
            };
            if let Some(expected) = crate::test_support::oracle(
                "portable_workflow",
                &json!([{"op":"execute","skill":skill,"context":context_path,"output":python_output}]),
            ) {
                assert_eq!(actual, expected[0], "mutation {mutation}");
            }
        }
    }
}
