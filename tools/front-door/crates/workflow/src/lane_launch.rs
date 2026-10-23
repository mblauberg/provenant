//! Source launch-builder wire shapes. Execution remains with the supervisor.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use crate::lane_exit::{ExitRecord,Member,ProcessInteger,ReceiptFields};

#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct LaunchSpec {
    pub schema:String,
    pub argv:Vec<String>,
    pub cwd:String,
    pub env:BTreeMap<String,String>,
    pub stdin:StdinSpec,
    pub attempt_dir:String,
    pub run_token:String,
    pub capture:CaptureSpec,
    pub silence_observation_ms:u64,
    pub timeouts_ms:Timeouts,
    pub progress:ProgressSpec,
    pub cancel_markers:Vec<String>,
    pub observer:ObserverCommand,
    pub spare:SparePolicy,
}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct StdinSpec {
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub path:Member<Option<String>>,
}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct CaptureSpec { pub events:String,pub stderr:String,pub max_bytes:u64 }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct Timeouts { pub startup:u64,pub wall:u64,pub settle:u64,pub child_grace:u64,pub root_flush:u64 }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ProgressSpec {
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub writer_tree:Member<WriterTree>,
}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct WriterTree { pub root:String,pub exclude:Vec<String> }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct ObserverCommand { pub argv:Vec<String> }
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct SparePolicy { pub code_mode_host:bool }

/// Source stdout notifications also accept the older `type` discriminator. The
/// raw notification remains open until the launcher applies that precedence.
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct LaunchNotification {
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub event:Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent",rename="type")]
    pub legacy_type:Member<Value>,
    #[serde(flatten)]
    pub fields:serde_json::Map<String,Value>,
}

/// Provider identity is observation only, without an OS process handle.
#[derive(Debug,Clone,PartialEq)]
pub struct ProviderIdentity { pub pid:ProcessInteger,pub returncode:Option<ProcessInteger> }
/// This is a native return value, not a JSON enum with an invented wire tag.
#[derive(Debug,Clone,PartialEq)]
pub enum LaunchOutcome {
    Unsupported { warning:String },
    Supported { record:Box<ExitRecord>,fields:Box<ReceiptFields>,provider:Option<ProviderIdentity>,started_at:Value,warning:String },
}

#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn source_builder_records_round_trip_with_optional_stdin_and_writer_progress() {
        let temp=tempfile::tempdir().unwrap();
        let root=temp.path().canonicalize().unwrap();
        let mut cases=vec![];
        for mode in ["worktree_write","read_only"] { for stdin in [true,false] {cases.push(json!({"root":root,"mode":mode,"stdin":stdin}));} }
        if let Some(outputs)=crate::test_support::oracle("lane_launch_contract",&json!(cases)) {
            for raw in outputs.as_array().unwrap() {
                let typed:LaunchSpec=serde_json::from_value(raw.clone()).unwrap();
                assert_eq!(serde_json::to_value(&typed).unwrap(),*raw);
                assert_eq!(typed.timeouts_ms.wall,1125);
                assert_eq!(typed.timeouts_ms.startup,25);
                assert_eq!(typed.silence_observation_ms,125);
                assert_eq!(typed.env.get("SECRET_REFERENCE").unwrap(),"synthetic-value");
                assert_eq!(typed.capture.events,"events.jsonl");
            }
        }
        for raw in [json!({"event":"started","pid":123}),json!({"event":"progress","last_progress_at":"stamp"}),json!({"type":"started","pid":null,"unknown":true})] {
            let typed:LaunchNotification=serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(),raw);
        }
    }
}
