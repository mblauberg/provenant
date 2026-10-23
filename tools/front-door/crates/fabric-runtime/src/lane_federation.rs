//! Federation cycle policy and the owner seam. Store implementation is separate.
use crate::host_protocol::HostError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;

/// One table owns read/write and ambiguous-transfer semantics for every verb.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LaneVerb {
    Dispatch,
    Lanes,
    Status,
    Cancel,
    Resume,
    Handoff,
    Output,
    Operation,
}
impl LaneVerb {
    pub fn writes(self) -> bool {
        matches!(
            self,
            Self::Dispatch | Self::Cancel | Self::Resume | Self::Handoff
        )
    }
}
pub const PATH_FIELDS: &[&str] = &["cwd", "worktree", "add_dirs", "read_roots"];
pub const TRANSIENT: &[&str] = &[
    "timeout",
    "unreachable",
    "bad_response",
    "hosts_cancelled",
    "peer_command_failed",
];

/// The source has dynamic lane-specific payloads; retain their object shape
/// until those owners expose records rather than guessing fields here.
pub trait LaneFederation {
    fn peer_call(
        &mut self,
        verb: LaneVerb,
        params: &Map<String, Value>,
    ) -> Result<Map<String, Value>, HostError>;
    fn client_call(
        &mut self,
        action: LaneVerb,
        cwd: &Path,
        request: &Map<String, Value>,
        federated_read: bool,
    ) -> Result<Map<String, Value>, HostError>;
    fn placed_call(
        &mut self,
        action: LaneVerb,
        cwd: &Path,
        request: &Map<String, Value>,
        federated_read: bool,
    ) -> Result<Map<String, Value>, HostError>;
}

pub fn validate_operation_id(identifier: &str) -> Result<(), HostError> {
    let bytes = identifier.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(HostError {
            code: "invalid_operation_id".into(),
            message: "Operation ID must be a bounded simple identifier".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verb_wire_names_and_write_policy_match_python_sets() {
        let source = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../scripts/fabric_lanes.py"
        );
        let script = r#"import importlib.util,json,sys
spec=importlib.util.spec_from_file_location('fabric_lanes',sys.argv[1]);m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
print(json.dumps({'verbs':sorted(m.VERBS),'writes':sorted(m.WRITES),'transient':sorted(m.TRANSIENT),'paths':sorted(m.PATH_FIELDS)}))"#;
        let Some(expected) = crate::test_support::python(script, &[source.into()]) else {
            return;
        };
        let verbs = [
            LaneVerb::Dispatch,
            LaneVerb::Lanes,
            LaneVerb::Status,
            LaneVerb::Cancel,
            LaneVerb::Resume,
            LaneVerb::Handoff,
            LaneVerb::Output,
            LaneVerb::Operation,
        ];
        let names = |items: Vec<LaneVerb>| {
            let mut names = items
                .into_iter()
                .map(|verb| {
                    serde_json::to_value(verb)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_owned()
                })
                .collect::<Vec<_>>();
            names.sort();
            names
        };
        let mut transient = TRANSIENT.to_vec();
        transient.sort();
        let mut paths = PATH_FIELDS.to_vec();
        paths.sort();
        assert_eq!(
            serde_json::json!({"verbs":names(verbs.to_vec()),"writes":names(verbs.into_iter().filter(|verb|verb.writes()).collect()),"transient":transient,"paths":paths}),
            expected
        );
    }
    #[test]
    fn operation_ids_are_bounded_without_whitespace() {
        for id in ["x", "a.b_c-d", "A1"] {
            validate_operation_id(id).unwrap();
        }
        for id in ["", "a\n", "-a", "x@y", &"a".repeat(129)] {
            assert!(validate_operation_id(id).is_err());
        }
    }
}
