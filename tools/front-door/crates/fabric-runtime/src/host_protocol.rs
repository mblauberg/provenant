//! Typed host/client cycle contracts. SSH custody and strict byte decoding are
//! not implemented here; each transport implementation owns those effects.
use crate::{field::Field, pending::OrderedMap};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use thiserror::Error;

pub const PROTOCOL_VERSION: u64 = 1;
pub const MAX_REQUEST_BYTES: usize = 65_536;
pub const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub const MAX_JSON_DEPTH: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Error)]
#[error("{message}")]
#[serde(deny_unknown_fields)]
pub struct HostError {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerConfig {
    pub ssh_destination: String,
    pub peer_command: String,
    pub connect_timeout: u64,
    pub response_deadline: Number,
}
impl PeerConfig {
    pub fn new(ssh_destination: String) -> Self {
        Self {
            ssh_destination,
            peer_command: ".local/bin/provenant peer".into(),
            connect_timeout: 5,
            response_deadline: 15.into(),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub default_host: Field<String>,
    #[serde(default, skip_serializing_if = "Field::is_missing")]
    pub modes: Field<OrderedMap<String>>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HostsConfig {
    pub local_host: String,
    pub peers: OrderedMap<PeerConfig>,
    pub projects: OrderedMap<Placement>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PeerRequest {
    pub protocol_version: u64,
    pub verb: String,
    pub params: Map<String, Value>,
}

/// Source success/failure envelopes have boolean discriminants. Custom serde
/// keeps `ok` a boolean and rejects mixed success/error forms.
#[derive(Clone, Debug, PartialEq)]
pub enum PeerResponse {
    Success {
        protocol_version: u64,
        result: Map<String, Value>,
    },
    Failure {
        protocol_version: u64,
        error: HostError,
    },
}
impl Serialize for PeerResponse {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut record = serializer.serialize_struct("PeerResponse", 3)?;
        match self {
            Self::Success {
                protocol_version,
                result,
            } => {
                record.serialize_field("protocol_version", protocol_version)?;
                record.serialize_field("ok", &true)?;
                record.serialize_field("result", result)?;
            }
            Self::Failure {
                protocol_version,
                error,
            } => {
                record.serialize_field("protocol_version", protocol_version)?;
                record.serialize_field("ok", &false)?;
                record.serialize_field("error", error)?;
            }
        }
        record.end()
    }
}
impl<'de> Deserialize<'de> for PeerResponse {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            protocol_version: u64,
            ok: bool,
            #[serde(default)]
            result: Field<Map<String, Value>>,
            #[serde(default)]
            error: Field<HostError>,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.protocol_version == 0 {
            return Err(serde::de::Error::custom("invalid protocol version"));
        }
        match (wire.ok, wire.result, wire.error) {
            (true, Field::Value(result), Field::Missing) => Ok(Self::Success {
                protocol_version: wire.protocol_version,
                result,
            }),
            (false, Field::Missing, Field::Value(error)) => Ok(Self::Failure {
                protocol_version: wire.protocol_version,
                error,
            }),
            _ => Err(serde::de::Error::custom("invalid peer envelope")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolDecision {
    pub compatible: bool,
    pub reads_flagged: bool,
    pub writes_allowed: bool,
}
pub fn protocol_decision(remote: &Value) -> ProtocolDecision {
    // Python's exact type check rejects true and 1.0 even though they equal 1.
    let compatible = remote.as_u64() == Some(PROTOCOL_VERSION);
    ProtocolDecision {
        compatible,
        reads_flagged: !compatible,
        writes_allowed: compatible,
    }
}

pub trait PeerTransport {
    fn exchange(
        &mut self,
        peer: &PeerConfig,
        request: &PeerRequest,
        deadline_seconds: f64,
    ) -> Result<PeerResponse, HostError>;
}
/// Client results additionally carry reachability and protocol warnings, whose
/// dynamic doctor/lane payload is left with that payload's owning unit.
pub trait PeerClient {
    fn call(
        &mut self,
        verb: &str,
        params: Option<&Map<String, Value>>,
        write: bool,
    ) -> Result<Map<String, Value>, HostError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn response_wire_forms_are_boolean_and_exclusive() {
        for wire in [
            json!({"protocol_version":1,"ok":true,"result":{}}),
            json!({"protocol_version":2,"ok":false,"error":{"code":"x","message":"y"}}),
        ] {
            let response: PeerResponse = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(serde_json::to_value(response).unwrap(), wire);
        }
        for wire in [
            json!({"protocol_version":1,"ok":true,"result":{},"error":null}),
            json!({"protocol_version":0,"ok":true,"result":{}}),
            json!({"protocol_version":1,"ok":"true","result":{}}),
        ] {
            assert!(serde_json::from_value::<PeerResponse>(wire).is_err());
        }
    }
    #[test]
    fn protocol_decisions_match_python_for_integral_floats_and_bools() {
        let values = json!([1, 1.0, true, false, null, 0, 2, "1"]);
        let source = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../../scripts/fabric_hosts.py"
        );
        let script = r#"import importlib.util,json,sys
spec=importlib.util.spec_from_file_location('fabric_hosts',sys.argv[1]);m=importlib.util.module_from_spec(spec);sys.modules[spec.name]=m;spec.loader.exec_module(m)
print(json.dumps([m.protocol_decision(value) for value in json.loads(sys.argv[2])]))"#;
        let Some(expected) =
            crate::test_support::python(script, &[source.into(), values.to_string()])
        else {
            return;
        };
        assert_eq!(
            json!(values
                .as_array()
                .unwrap()
                .iter()
                .map(protocol_decision)
                .collect::<Vec<_>>()),
            expected
        );
    }
}
