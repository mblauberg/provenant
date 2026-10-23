//! Trusted gate protocol. Launch, group custody and atomic publication await supervisor.
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

pub const GATE_ENV: &[&str] = &["PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL", "TERM"];

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
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(argv)?)))
}

pub fn read_gate_exit(path: &Path, token: &str) -> Option<GateExit> {
    let value: GateExit = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (value.token == token).then_some(value)
}

/// A complete go line is required. EOF, malformed input or another token is
/// refusal (wrapper exit 3). Remaining order fields belong to the launch edge.
pub fn authorised_go(input: &[u8], token: &str) -> Option<Map<String, Value>> {
    let end = input.iter().position(|b| *b == b'\n')?;
    let Value::Object(order) = serde_json::from_str(&String::from_utf8_lossy(&input[..end])).ok()? else { return None };
    let argv = order.get("argv")?.as_array()?;
    if order.get("token")?.as_str()? != token || argv.is_empty() || !argv.iter().all(Value::is_string) {
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
        for input in [b"".as_slice(), b"{}", b"{}\n", b"null\n", b"{\"token\":\"other\",\"argv\":[\"sh\"]}\n",
            b"{\"token\":\"t\",\"argv\":[]}\n", b"{\"token\":\"t\",\"argv\":[1]}\n"] {
            assert!(authorised_go(input, "t").is_none());
        }
        assert!(authorised_go(b"{\"token\":\"t\",\"argv\":[\"sh\"]}\nignored", "t").is_some());
    }
    #[test]
    fn receipt_reader_checks_token_and_timestamp_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exit");
        assert!(read_gate_exit(&path,"t").is_none());
        let wire=json!({"token":"t","finished_at":"clock","exit_code":7,"signal":null,"extra":true});
        fs::write(&path,serde_json::to_vec(&wire).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(read_gate_exit(&path,"t").unwrap()).unwrap(),wire);
        assert!(read_gate_exit(&path,"other").is_none());
        fs::write(&path,b"{\"token\":\"t\",\"finished_at\":1}").unwrap();
        assert!(read_gate_exit(&path,"t").is_none());
    }
    #[test]
    fn hash_matches_node_on_unicode_and_escaping_when_available() {
        let argv=vec!["sh".into(),"café\n\"\\".into(),"🦀".into()];
        let output=match std::process::Command::new("node").args(["-e", "process.stdout.write(require('crypto').createHash('sha256').update(JSON.stringify(JSON.parse(process.argv[1]))).digest('hex'))", &serde_json::to_string(&argv).unwrap()]).output() {
            Ok(output)=>output, Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return, Err(error)=>panic!("{error}")
        };
        assert!(output.status.success());
        assert_eq!(argv_hash(&argv).unwrap(),String::from_utf8(output.stdout).unwrap());
    }
}
