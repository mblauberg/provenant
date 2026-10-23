//! Code-transport cycle records. Worktree custody and bundle publication are
//! supplied by the run/supervisor owners, never implemented by this facade.
use crate::{field::Field, host_protocol::HostError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const CHUNK_BYTES: u64 = 32 * 1024;
pub const DOWNLOAD_CHUNK_BYTES: u64 = 512 * 1024;
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_HAVE_CANDIDATES: usize = 64;
pub const MAX_BUNDLE_HEADER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CodeMetadata {
    pub base_revision: String,
    pub branch: String,
    pub worktree: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub read_only: Field<bool>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub subdir: Field<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HaveRequest {
    pub candidates: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HaveResponse {
    pub have: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadRequest {
    pub operation_id: String,
    pub metadata: CodeMetadata,
    pub offset: u64,
    pub data: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadRequest {
    pub operation_id: String,
    pub offset: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DownloadResponse {
    pub data: String,
    pub offset: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultRequest {
    pub id: String,
}

pub fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl CodeMetadata {
    pub fn validate(&self) -> Result<(), HostError> {
        let fail = |code: &str, message: &str| HostError {
            code: code.into(),
            message: message.into(),
        };
        if !valid_oid(&self.base_revision) {
            return Err(fail(
                "invalid_revision",
                "Pass an exact lowercase commit object ID",
            ));
        }
        if !matches!(self.read_only, Field::Missing | Field::Value(true)) {
            return Err(fail(
                "invalid_code_transport",
                "Read-only transport marker must be true",
            ));
        }
        if !matches!(self.subdir, Field::Missing) {
            let valid = matches!(&self.subdir,Field::Value(path) if self.read_only==Field::Value(true) && path.chars().count()<=1024
                && path.split('/').all(|part| !matches!(part,""|"."|".."|".git") && !part.contains('\0')));
            if !valid {
                return Err(fail(
                    "invalid_code_transport",
                    "Reader subdirectory must be a relative path inside the checkout",
                ));
            }
        }
        let safe = (1..=64).contains(&self.worktree.len())
            && self
                .worktree
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && self
                .worktree
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
        if self.branch.is_empty()
            || self.branch.starts_with('-')
            || self.worktree != self.branch.replace('/', "-")
            || !safe
        {
            return Err(fail(
                "invalid_code_transport",
                "Writer worktree must have its safe branch-derived name",
            ));
        }
        if !(1..=MAX_BUNDLE_BYTES).contains(&self.size) {
            return Err(fail(
                "code_transport_too_large",
                "Git bundle limit is 64 MiB",
            ));
        }
        if self.sha256.len() != 64 || !valid_oid(&self.sha256) {
            return Err(fail(
                "invalid_code_transport",
                "Bundle checksum must be SHA-256",
            ));
        }
        Ok(())
    }
}

/// Dynamic result/verification records stay opaque until the worktree owner
/// exposes its verified-writer contract. No guessed receipt fields are added.
pub trait CodeTransport {
    fn have(&mut self, project: &str, request: &HaveRequest) -> Result<HaveResponse, HostError>;
    fn upload(
        &mut self,
        project: &str,
        request: &UploadRequest,
    ) -> Result<Map<String, Value>, HostError>;
    fn download(
        &mut self,
        project: &str,
        request: &DownloadRequest,
    ) -> Result<DownloadResponse, HostError>;
    fn result(
        &mut self,
        project: &str,
        request: &ResultRequest,
    ) -> Result<Map<String, Value>, HostError>;
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn metadata_has_exact_wire_fields_and_preserves_omissions() {
        let wire = json!({"base_revision":"a".repeat(40),"branch":"port/topic","worktree":"port-topic","size":1,"sha256":"b".repeat(64)});
        let metadata: CodeMetadata = serde_json::from_value(wire.clone()).unwrap();
        metadata.validate().unwrap();
        assert_eq!(serde_json::to_value(&metadata).unwrap(), wire);
        let mut invalid = wire;
        invalid["extra"] = json!(1);
        assert!(serde_json::from_value::<CodeMetadata>(invalid).is_err());
    }
    #[test]
    fn metadata_policy_matches_python_acceptance_and_errors() {
        let base = json!({"base_revision":"a".repeat(40),"branch":"port/topic","worktree":"port-topic","size":32,"sha256":"b".repeat(64)});
        let mut cases = vec![base.clone()];
        for (key, value) in [
            ("base_revision", json!("x")),
            ("read_only", json!(false)),
            ("subdir", json!("../x")),
            ("worktree", json!("bad")),
            ("size", json!(0)),
            ("sha256", json!("C".repeat(64))),
        ] {
            let mut case = base.clone();
            case[key] = value;
            cases.push(case);
        }
        let source = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../scripts/fabric_git.py"
        );
        let script = r#"import importlib.util,json,pathlib,sys
from types import SimpleNamespace
def load(name,path):
 spec=importlib.util.spec_from_file_location(name,path);m=importlib.util.module_from_spec(spec);sys.modules[name]=m;spec.loader.exec_module(m);return m
m=load('fabric_git',sys.argv[1]);hosts=load('fabric_hosts',pathlib.Path(sys.argv[1]).with_name('fabric_hosts.py'))
transport=m.CodeTransport(SimpleNamespace(h=hosts))
result=[]
for value in json.loads(sys.argv[2]):
 try:transport.metadata(value);result.append(None)
 except hosts.HostError as error:result.append(error.wire())
print(json.dumps(result))"#;
        let Some(expected) =
            crate::test_support::python(script, &[source.into(), json!(cases).to_string()])
        else {
            return;
        };
        let actual: Vec<_> = cases
            .into_iter()
            .map(|value| {
                serde_json::from_value::<CodeMetadata>(value)
                    .unwrap()
                    .validate()
                    .err()
            })
            .collect();
        assert_eq!(json!(actual), expected);
    }
}
