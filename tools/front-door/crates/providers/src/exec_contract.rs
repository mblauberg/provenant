//! `fabric.exec-plan.v1` contract at the provider/run seam, not a second executor.
use crate::adapters::{Mode, StdinMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Unknown metadata retains insertion order. This is a wire object, not a list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Fields<T>(pub Vec<(String, T)>);
pub type Metadata = Fields<Value>;
impl<T: Serialize> Serialize for Fields<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}
impl<'de, T: Deserialize<'de>> Deserialize<'de> for Fields<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V<T>(std::marker::PhantomData<T>);
        impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for V<T> {
            type Value = Fields<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("metadata object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Fields<T>, A::Error> {
                let mut values = Vec::new();
                while let Some(pair) = a.next_entry()? {
                    values.push(pair);
                }
                Ok(Fields(values))
            }
        }
        d.deserialize_map(V(std::marker::PhantomData))
    }
}

/// Retains open route members; projection/validation is the dispatch contract.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Route(pub Metadata);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum WriteBoundary {
    #[serde(rename = "sandbox-exec")]
    SandboxExec {
        writable_paths: Vec<String>,
        #[serde(flatten)]
        metadata: Metadata,
    },
    #[serde(rename = "provider-native")]
    ProviderNative {
        sandbox: String,
        #[serde(flatten)]
        metadata: Metadata,
    },
    #[serde(rename = "none")]
    None {
        writable_paths: (),
        #[serde(flatten)]
        metadata: Metadata,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppliedPlan {
    pub sandbox: Option<String>,
    pub network: Option<bool>,
    pub add_dirs: Vec<String>,
    pub shared_write_paths: Fields<bool>,
    pub locks_dir: Option<String>,
    pub guarantee: String,
    pub confinement: String,
    pub write_boundary: WriteBoundary,
    pub capabilities: Vec<String>,
    pub protected_paths: Vec<String>,
    #[serde(flatten)]
    pub metadata: Metadata,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecPlan {
    pub schema: String,
    pub adapter: String,
    pub route: Route,
    pub model: String,
    pub effort: String,
    pub prompt: String,
    pub network_requested: Option<bool>,
    pub cwd: String,
    pub workspace_root: String,
    pub protected_paths: Vec<String>,
    pub mode: Mode,
    pub worktree: Option<String>,
    pub run_dir: String,
    pub original_prompt_file: Option<String>,
    pub timeout_seconds: f64,
    pub startup_seconds: f64,
    pub grace_seconds: f64,
    pub session_id: Option<String>,
    pub resume_session: Option<String>,
    pub stdin_policy: StdinMode,
    pub output_format: String,
    pub run_id: String,
    pub chair: String,
    pub route_label: String,
    pub warnings: Vec<String>,
    pub boundary_prompt: String,
    pub requested_model: Option<String>,
    pub requested_effort: Option<String>,
    pub intent: String,
    pub applied: AppliedPlan,
    pub agy_sandbox: bool,
    #[serde(flatten)]
    pub metadata: Metadata,
    pub argv: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exec_plan_contract_round_trips_real_planner_output() {
        let Some(plans): Option<Vec<Value>> = crate::oracle::python(
            r#"
import json,sys,os,tempfile
from pathlib import Path
from skills.orchestrate.scripts.provider_exec import build_plan
os.environ['PROVENANT_NO_OS_CONFINEMENT']='1'
with tempfile.TemporaryDirectory() as td:
 root=str(Path(td).resolve())
 plans=[build_plan(adapter,{'resolved_model':'fixture','effort':'high'},'hello',cwd=root,workspace_root=root,preface=False,timeout_seconds=30,startup_seconds=5,instruction_policy='deny',custom={'sentinel':True}) for adapter in ['copilot','cursor']]
 print(json.dumps(plans))
"#,
            &Value::Null,
            None,
        ) else {
            return;
        };
        for value in plans {
            let plan: ExecPlan = crate::decode_json(&serde_json::to_vec(&value).unwrap()).unwrap();
            assert_eq!(serde_json::to_value(plan).unwrap(), value);
        }
    }
    #[test]
    fn write_boundary_wire_variants_are_internally_tagged() {
        for value in [
            serde_json::json!({"kind":"sandbox-exec","writable_paths":["/tmp"],"state_dir_skipped":"link"}),
            serde_json::json!({"kind":"provider-native","sandbox":"workspace-write","profile":"provenant-fixture","filesystem":{"/a":"write"}}),
            serde_json::json!({"kind":"none","writable_paths":null}),
        ] {
            let boundary: WriteBoundary = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(serde_json::to_value(boundary).unwrap(), value);
        }
    }
}
