//! Typed declarations for commands invoked through repository integration ports.
use crate::{CanonicalV1, FileDigest};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortSource {
    Builtin,
    Host,
    Project,
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

    #[test]
    fn port_source_round_trips_and_rejects_unknown_values() {
        for source in [PortSource::Builtin, PortSource::Host, PortSource::Project] {
            let encoded = serde_json::to_value(source).expect("source serializes");
            assert_eq!(
                serde_json::from_value::<PortSource>(encoded).expect("source decodes"),
                source
            );
        }
        assert!(serde_json::from_value::<PortSource>(serde_json::json!("unknown")).is_err());
    }

    #[test]
    fn exit_disposition_round_trips_and_rejects_unknown_values() {
        for disposition in [ExitDisposition::Refused, ExitDisposition::Unknown] {
            let encoded = serde_json::to_value(disposition).expect("disposition serializes");
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
            let encoded = serde_json::to_value(&case).expect("command serializes");
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
