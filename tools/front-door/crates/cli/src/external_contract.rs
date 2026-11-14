//! Declared outward interfaces. Internal state files are not a public contract.
use serde::{Deserialize, Serialize};

/// Current outward interfaces, represented as data so documentation can be generated from one declaration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalContract {
    pub schema_version: u32,
    pub interfaces: Vec<ExternalInterface>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExternalInterface {
    LaneEnvironment { name: String },
    JsonOutput { command: String, schema: JsonOutputSchema },
    WaitJsonLines { command: String, event_fields: Vec<String>, terminal_schema: SchemaReference },
    ProcessTreeJson { command: String, environment: String, envelope: SchemaReference, sample: SchemaReference },
    CommandSurface { commands: Vec<String> },
    StableCommands { state: StableCommandState },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum JsonOutputSchema {
    CommandResult { envelope: SchemaReference },
    WorkIdentity { envelope: SchemaReference, identity: SchemaReference },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaOwner {
    CommandResult,
    WorkKey,
    ProcessTreeSample,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaReference {
    pub owner: SchemaOwner,
    pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "commands", rename_all = "snake_case")]
pub enum StableCommandState {
    /// The CLI documents that it makes no cross-version stability promise.
    NoneDeclared,
    Declared(Vec<String>),
}

const fn command_result_schema() -> SchemaReference {
    SchemaReference {
        owner: SchemaOwner::CommandResult,
        version: provenant_records::wire::ENVELOPE_VERSION,
    }
}

/// The sole declaration of outward interfaces for the current release.
pub fn external_contract() -> ExternalContract {
    use provenant_core::{RUN_STATE_DIRECTORY_ENV, STATE_DIRECTORY_ENV};
    use provenant_run::{LANE_DEPTH_ENV, PARENT_ATTEMPT_ENV};
    use provenant_supervisor::process_tree;

    let envelope = command_result_schema();
    ExternalContract {
        schema_version: 1,
        interfaces: vec![
            ExternalInterface::LaneEnvironment { name: "PROVENANT_AGENT_LABEL".into() },
            ExternalInterface::LaneEnvironment { name: LANE_DEPTH_ENV.into() },
            ExternalInterface::LaneEnvironment { name: PARENT_ATTEMPT_ENV.into() },
            ExternalInterface::LaneEnvironment { name: RUN_STATE_DIRECTORY_ENV.into() },
            ExternalInterface::LaneEnvironment { name: STATE_DIRECTORY_ENV.into() },
            ExternalInterface::LaneEnvironment { name: "HOME".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_ROOT".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_GIT".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_SANDBOX".into() },
            ExternalInterface::LaneEnvironment { name: process_tree::ENV.into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_LANE_CARGO_LINT_TARGET".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_BUILD_SLOT_STATE".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_BUILD_SLOTS".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_LOCAL_CARGO".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_BUILD_DISK_FLOOR".into() },
            ExternalInterface::LaneEnvironment { name: "PROVENANT_BUILD_DISK_RULE".into() },
            ExternalInterface::JsonOutput {
                command: "commands emitting command-result JSON".into(),
                schema: JsonOutputSchema::CommandResult { envelope },
            },
            ExternalInterface::JsonOutput {
                command: "work-record JSON outputs".into(),
                schema: JsonOutputSchema::WorkIdentity {
                    envelope,
                    identity: SchemaReference {
                        owner: SchemaOwner::WorkKey,
                        version: provenant_records::STATE_FORMAT_VERSION,
                    },
                },
            },
            ExternalInterface::WaitJsonLines {
                command: "provenant wait --json".into(),
                event_fields: vec!["event".into(), "target".into(), "settle".into(), "line".into(), "digests".into()],
                terminal_schema: envelope,
            },
            ExternalInterface::ProcessTreeJson {
                command: "provenant process-tree --json".into(),
                environment: process_tree::ENV.into(),
                envelope,
                sample: SchemaReference {
                    owner: SchemaOwner::ProcessTreeSample,
                    version: u64::from(process_tree::SAMPLE_SCHEMA_VERSION),
                },
            },
            ExternalInterface::CommandSurface { commands: crate::usage_verbs() },
            ExternalInterface::StableCommands { state: StableCommandState::NoneDeclared },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn every_declared_interface_kind_is_reachable_and_round_trips() {
        let declared = external_contract();
        let encoded = serde_json::to_vec(&declared).unwrap();
        let decoded: ExternalContract = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, declared);

        let kinds: BTreeSet<_> = declared
            .interfaces
            .iter()
            .map(|interface| match interface {
                ExternalInterface::LaneEnvironment { .. } => "lane_environment",
                ExternalInterface::JsonOutput { .. } => "json_output",
                ExternalInterface::WaitJsonLines { .. } => "wait_json_lines",
                ExternalInterface::ProcessTreeJson { .. } => "process_tree_json",
                ExternalInterface::CommandSurface { .. } => "command_surface",
                ExternalInterface::StableCommands { .. } => "stable_commands",
            })
            .collect();
        assert_eq!(
            kinds,
            BTreeSet::from([
                "lane_environment",
                "json_output",
                "wait_json_lines",
                "process_tree_json",
                "command_surface",
                "stable_commands",
            ])
        );

        let schemas: BTreeSet<_> = declared
            .interfaces
            .iter()
            .filter_map(|interface| match interface {
                ExternalInterface::JsonOutput { schema, .. } => Some(match schema {
                    JsonOutputSchema::CommandResult { .. } => "command_result",
                    JsonOutputSchema::WorkIdentity { .. } => "work_identity",
                }),
                _ => None,
            })
            .collect();
        assert_eq!(schemas, BTreeSet::from(["command_result", "work_identity"]));

        let owners: BTreeSet<_> = declared
            .interfaces
            .iter()
            .flat_map(|interface| match interface {
                ExternalInterface::JsonOutput { schema, .. } => match schema {
                    JsonOutputSchema::CommandResult { envelope } => vec![envelope.owner],
                    JsonOutputSchema::WorkIdentity { envelope, identity } => {
                        vec![envelope.owner, identity.owner]
                    }
                },
                ExternalInterface::WaitJsonLines { terminal_schema, .. } => vec![terminal_schema.owner],
                ExternalInterface::ProcessTreeJson { envelope, sample, .. } => vec![envelope.owner, sample.owner],
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(
            owners,
            BTreeSet::from([
                SchemaOwner::CommandResult,
                SchemaOwner::WorkKey,
                SchemaOwner::ProcessTreeSample,
            ])
        );

        for state in [StableCommandState::NoneDeclared, StableCommandState::Declared(vec!["example".into()])] {
            let encoded = serde_json::to_vec(&state).unwrap();
            let decoded: StableCommandState = serde_json::from_slice(&encoded).unwrap();
            assert_eq!(decoded, state);
        }
    }
}
