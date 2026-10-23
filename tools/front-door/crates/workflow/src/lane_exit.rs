//! Exit-cycle contracts. This module grants no process or filesystem authority.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A source member can be absent or present with null. Option alone loses this
/// distinction when a record is decoded and retained again.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Member<T> {
    #[default]
    Absent,
    Present(T),
}
impl<T> Member<T> { pub fn is_absent(&self) -> bool { matches!(self, Self::Absent) } }
impl<T: Serialize> Serialize for Member<T> {
    fn serialize<S: serde::Serializer>(&self, serializer:S)->Result<S::Ok,S::Error> {
        match self { Self::Present(value)=>value.serialize(serializer),Self::Absent=>serializer.serialize_unit() }
    }
}
impl<'de,T:Deserialize<'de>> Deserialize<'de> for Member<T> {
    fn deserialize<D:serde::Deserializer<'de>>(deserializer:D)->Result<Self,D::Error> { T::deserialize(deserializer).map(Self::Present) }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProcessInteger { Signed(i64), Unsigned(u64) }

/// Source fixtures carry descendant rows; the published J2 draft also allows
/// the older boolean/count forms. Keep both without externally tagged wrappers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ReapedEvidence { Flag(bool), Descendants(Vec<Value>) }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SparedEvidence { Count(u64), Descendants(Vec<Value>) }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExitRecord {
    pub schema: String,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub pid: Member<Option<ProcessInteger>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub pgid: Member<Option<ProcessInteger>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub exit: Member<Option<ProcessInteger>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub signal: Member<Option<ProcessInteger>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub forced: Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub terminal_event_seen: Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub reaped: Member<Option<ReapedEvidence>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub spared: Member<Option<SparedEvidence>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub capture: Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub census_warnings: Member<Vec<Value>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub io_errors: Member<Vec<Value>>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub started_at: Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub ended_at: Member<Value>,
    #[serde(default,skip_serializing_if="Member::is_absent")]
    pub last_progress_at: Member<Value>,
    #[serde(flatten)]
    pub extra: Map<String,Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReceiptFields {
    pub pid: Option<ProcessInteger>,
    pub pgid: Option<ProcessInteger>,
    pub exit: Option<ProcessInteger>,
    pub forced: Value,
    pub terminal_event_seen: bool,
    pub reaped: Value,
    pub spared: Value,
    pub capture: Value,
    pub warnings: Vec<String>,
    pub started_at: Value,
    pub ended_at: Value,
    pub last_progress_at: Value,
}

#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    #[test]
    fn exit_contract_accepts_real_descendants_and_retains_absent_vs_null() {
        let mut cases=vec![];
        for reaped in [json!(true),json!([]),json!([{"pid":124,"command":"child"}])] {
            for spared in [json!(1),json!([]),json!([{"pid":125}])] {
                cases.push(json!({"schema":"provenant.lane.exit.v1","pid":123,"pgid":123,"exit":null,"signal":15,"forced":"wall_timeout","terminal_event_seen":false,"reaped":reaped,"spared":spared,"census_warnings":["partial census"],"io_errors":["capture: full"],"capture":{"events":{"bytes":5000,"truncated":true}},"observer_lost":false}));
            }
        }
        cases.push(json!({"schema":"provenant.lane.exit.v1"}));
        for raw in &cases {
            let typed:ExitRecord=serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(serde_json::to_value(typed).unwrap(),*raw);
        }
        if let Some(expected)=crate::test_support::oracle("lane_exit_contract",&json!(cases)) {
            for raw in expected.as_array().unwrap() {
                let typed:ReceiptFields=serde_json::from_value(raw.clone()).unwrap();
                assert_eq!(serde_json::to_value(typed).unwrap(),*raw);
            }
        }
        for raw in [json!({"schema":"provenant.lane.exit.v1","pid":true}),json!({"schema":"provenant.lane.exit.v1","pid":1.0})] {
            assert!(serde_json::from_value::<ExitRecord>(raw).is_err());
        }
    }
}
