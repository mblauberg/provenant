//! Trusted gate protocol. Launch, group custody and atomic publication await supervisor.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

pub const GATE_ENV: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL", "TERM",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GateOrder {
    pub token: String,
    pub argv: Vec<String>,
    pub cwd: String,
    pub out: String,
    pub err: String,
    pub exit: String,
}

/// The TS reader only validates token and finished_at. Keep other fields
/// lossless, including unknown values, rather than tightening that read policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateExit {
    pub token: String,
    pub finished_at: String,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

pub fn argv_hash(argv: &[String]) -> Result<String, serde_json::Error> {
    let digest = Sha256::digest(serde_json::to_vec(argv)?);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(64);
    for byte in digest {
        hex.push(char::from(HEX[usize::from(byte >> 4)]));
        hex.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(hex)
}

pub fn parse_gate_exit(bytes: &[u8], token: &str) -> Option<GateExit> {
    let value: GateExit = serde_json::from_str(&String::from_utf8_lossy(bytes)).ok()?;
    (value.token == token).then_some(value)
}

/// A complete go line is required. EOF, malformed input or another token is
/// refusal (wrapper exit 3). Remaining order fields belong to the launch edge.
pub fn authorised_go(input: &[u8], token: &str) -> Option<Map<String, Value>> {
    let end = input.iter().position(|b| *b == b'\n')?;
    let Value::Object(order) =
        serde_json::from_str(&String::from_utf8_lossy(&input[..end])).ok()?
    else {
        return None;
    };
    let argv = order.get("argv")?.as_array()?;
    if order.get("token")?.as_str()? != token
        || argv.is_empty()
        || !argv.iter().all(Value::is_string)
    {
        return None;
    }
    Some(order)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn no_command_without_complete_matching_go() {
        for input in [
            b"".as_slice(),
            b"{}",
            b"{}\n",
            b"null\n",
            b"{\"token\":\"other\",\"argv\":[\"sh\"]}\n",
            b"{\"token\":\"t\",\"argv\":[]}\n",
            b"{\"token\":\"t\",\"argv\":[1]}\n",
        ] {
            assert!(authorised_go(input, "t").is_none());
        }
        assert!(authorised_go(b"{\"token\":\"t\",\"argv\":[\"sh\"]}\nignored", "t").is_some());
    }
    #[test]
    fn receipt_decoder_checks_token_and_timestamp_only() {
        assert!(parse_gate_exit(b"", "t").is_none());
        let wire =
            json!({"token":"t","finished_at":"clock","exit_code":7,"signal":null,"extra":true});
        let bytes = serde_json::to_vec(&wire).unwrap();
        assert_eq!(
            serde_json::to_value(parse_gate_exit(&bytes, "t").unwrap()).unwrap(),
            wire
        );
        assert!(parse_gate_exit(&bytes, "other").is_none());
        assert!(parse_gate_exit(b"{\"token\":\"t\",\"finished_at\":1}", "t").is_none());
    }
    #[test]
    fn hash_matches_node_on_unicode_and_escaping_when_available() {
        let argv = vec!["sh".into(), "café\n\"\\".into(), "🦀".into()];
        let Some(expected) = crate::test_support::node(
            "import {argvHash} from './src/graph/gates.ts'; console.log(JSON.stringify(argvHash(JSON.parse(process.argv[1]))))",
            &[serde_json::to_string(&argv).unwrap()]) else { return; };
        assert_eq!(serde_json::json!(argv_hash(&argv).unwrap()), expected);
    }
}

#[cfg(test)]
mod integration_boundary_tests {
    #[test]
    fn out_of_range_extension_number_is_an_open_codec_boundary() {
        // RP-FR1-001 documents why the gate unit must remain partial. This is
        // evidence of the library boundary, not permission to change the reader.
        assert!(serde_json::from_str::<serde_json::Value>(r#"{"n":1e400}"#).is_err());
    }
}
