//! Typed declarations for commands invoked through repository integration ports.
pub use crate::Layer;
use crate::{CanonicalV1, FileDigest, Precedence};
use serde::{Deserialize, Serialize};

/// The layers a port declaration accepts, lowest first. One declaration drives
/// the merge and every explanation (Spec: docs/specs/state.md#every-setting-reports-its-layer).
pub const PORT_PRECEDENCE: Precedence = Precedence(&[
    Layer::Default,
    Layer::Detected,
    Layer::Host,
    Layer::HostProject,
    Layer::Project,
]);

/// The rank of a port layer in [`PORT_PRECEDENCE`]. Port provenance is kept in
/// `BTreeMap<String, Layer>` keyed by dotted leaf, so this is the one place the
/// layer order is read for the per-leaf fold.
pub fn port_layer_rank(layer: Layer) -> usize {
    PORT_PRECEDENCE
        .0
        .iter()
        .position(|candidate| *candidate == layer)
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitDisposition {
    Refused,
    Unknown,
}

/// Success and unknown codes are declared. Other codes use the declared typed
/// disposition, so a command never relies on an implicit exit-code guess.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandExitCodes {
    pub success: Vec<i32>,
    pub unknown: Vec<i32>,
    pub other: ExitDisposition,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortCommand {
    pub argv: Vec<String>,
    pub exit_codes: CommandExitCodes,
}

impl PortCommand {
    /// Digest the exact stored command declaration in its deterministic schema order.
    pub fn digest(&self) -> FileDigest {
        #[allow(
            clippy::expect_used,
            reason = "PortCommand fields serialize infallibly"
        )]
        let declaration = serde_json::to_vec(self)
            .expect("PortCommand serialization is infallible for its declared fields");
        CanonicalV1::file_digest(&declaration)
    }

    /// Whether this command declaration is complete and unambiguous.
    pub fn is_valid(&self) -> bool {
        let argv_valid = self.argv.first().is_some_and(|program| !program.is_empty());
        let mut codes = std::collections::BTreeSet::new();
        argv_valid
            && !self.exit_codes.success.is_empty()
            && self
                .exit_codes
                .success
                .iter()
                .chain(&self.exit_codes.unknown)
                .all(|code| codes.insert(*code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn port_layer_wire_spellings_and_precedence_are_the_documented_contract() {
        let layers = [
            (Layer::Default, "default"),
            (Layer::Detected, "detected"),
            (Layer::Host, "host"),
            (Layer::HostProject, "host_project"),
            (Layer::Project, "project"),
        ];
        for (layer, spelling) in layers {
            let encoded = serde_json::to_value(layer).expect("layer serialises");
            assert_eq!(encoded, serde_json::json!(spelling));
            assert_eq!(
                serde_json::from_value::<Layer>(encoded).expect("layer decodes"),
                layer
            );
        }
        assert_eq!(
            PORT_PRECEDENCE.0,
            &[
                Layer::Default,
                Layer::Detected,
                Layer::Host,
                Layer::HostProject,
                Layer::Project,
            ]
        );
        assert!(serde_json::from_value::<Layer>(serde_json::json!("builtin")).is_err());
    }

    /// Spec: docs/specs/state.md#every-setting-reports-its-layer
    #[test]
    fn port_merge_keeps_the_same_winner_and_shadowed_layers() {
        use crate::{Source, Sourced};
        fn sourced(layer: Layer, value: &'static str) -> Sourced<&'static str> {
            Sourced {
                value,
                source: Source {
                    layer,
                    origin: None,
                },
            }
        }
        let merge = PORT_PRECEDENCE.merge([
            sourced(Layer::Default, "builtin"),
            sourced(Layer::Project, "project"),
            sourced(Layer::Host, "host"),
            sourced(Layer::Detected, "detected"),
            sourced(Layer::HostProject, "host_project"),
        ]);
        assert_eq!(
            merge.winner.as_ref().map(|candidate| candidate.value),
            Some("project")
        );
        assert_eq!(
            merge
                .shadowed
                .iter()
                .map(|candidate| candidate.value)
                .collect::<Vec<_>>(),
            ["builtin", "host", "detected", "host_project"]
        );
        assert!(port_layer_rank(Layer::Project) > port_layer_rank(Layer::Default));
    }

    #[test]
    fn exit_disposition_round_trips_and_rejects_unknown_values() {
        for disposition in [ExitDisposition::Refused, ExitDisposition::Unknown] {
            let encoded = serde_json::to_value(disposition).expect("disposition serialises");
            assert_eq!(
                serde_json::from_value::<ExitDisposition>(encoded).expect("disposition decodes"),
                disposition
            );
        }
        assert!(serde_json::from_value::<ExitDisposition>(serde_json::json!("success")).is_err());
    }

    #[test]
    fn command_exit_codes_round_trip_and_reject_malformed_values() {
        let cases = [
            CommandExitCodes {
                success: vec![0],
                unknown: vec![75],
                other: ExitDisposition::Refused,
            },
            CommandExitCodes {
                success: vec![0, 1],
                unknown: vec![2],
                other: ExitDisposition::Unknown,
            },
        ];
        for case in cases {
            let encoded = serde_json::to_value(&case).expect("exit codes serialize");
            assert_eq!(
                serde_json::from_value::<CommandExitCodes>(encoded).expect("exit codes decode"),
                case
            );
        }
        assert!(
            serde_json::from_value::<CommandExitCodes>(serde_json::json!({
                "success": ["zero"], "unknown": [], "other": "refused"
            }))
            .is_err()
        );
    }

    #[test]
    fn port_command_round_trips_and_rejects_malformed_values() {
        let cases = [
            PortCommand {
                argv: vec!["check".into()],
                exit_codes: CommandExitCodes {
                    success: vec![0],
                    unknown: vec![75],
                    other: ExitDisposition::Refused,
                },
            },
            PortCommand {
                argv: vec!["release".into(), "${branch}".into()],
                exit_codes: CommandExitCodes {
                    success: vec![0, 1],
                    unknown: vec![2],
                    other: ExitDisposition::Unknown,
                },
            },
        ];
        for case in cases {
            let encoded = serde_json::to_value(&case).expect("command serialises");
            assert_eq!(
                serde_json::from_value::<PortCommand>(encoded).expect("command decodes"),
                case
            );
        }
        // Wrong types are refused; unknown fields are tolerated (forward_compatibility).
        for malformed in [
            serde_json::json!({"argv":[1],"exit_codes":{"success":[0],"unknown":[],"other":"refused"}}),
            serde_json::json!({"argv":["check"],"exit_codes":{"success":[0],"unknown":[],"other":"maybe"}}),
        ] {
            assert!(serde_json::from_value::<PortCommand>(malformed).is_err());
        }
    }
}
