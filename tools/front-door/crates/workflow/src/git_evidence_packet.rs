//! Streaming packet structure policy. Readers retain their own EOF rejection order.
use crate::{policy::hashable, PolicyError};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum PacketError {
    #[error("Git evidence packet header exceeds its 64 KiB limit")]
    HeaderLimit,
    #[error("Git evidence packet header is invalid")]
    InvalidHeader,
    #[error("Git evidence packet is missing its status section")]
    MissingStatus,
    #[error(transparent)]
    Utf8(#[from] std::str::Utf8Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
}

/// Preserves Python's bool/int/float equality for the schema version and its
/// TypeError on an unhashable working_tree after the preceding predicates pass.
pub fn validate_header(metadata: &Value) -> Result<bool, PolicyError> {
    let Some(m) = metadata.as_object() else {
        return Ok(false);
    };
    let fields = [
        "schema_version",
        "record_type",
        "repository",
        "git_root",
        "head",
        "diff_base",
        "working_tree",
        "diff_from",
        "paths",
        "encoding",
    ];
    if m.len() != fields.len() || !fields.iter().all(|k| m.contains_key(*k)) {
        return Ok(false);
    }
    let oid = |key: &str| {
        m[key].as_str().is_some_and(|s| {
            (40..=64).contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    if !crate::pending::equals_one(&m["schema_version"])
        || m["record_type"] != "provenant-git-evidence"
        || !m["repository"].is_string()
        || !m["git_root"].is_string()
        || !oid("head")
        || !oid("diff_base")
    {
        return Ok(false);
    }
    hashable(&m["working_tree"], "working_tree")?;
    Ok(m["working_tree"]
        .as_str()
        .is_some_and(|s| ["clean", "dirty"].contains(&s))
        && m["diff_from"].as_str().is_some_and(|s| !s.is_empty())
        && m["paths"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
        && m["encoding"] == "utf-8-replacement")
}

pub struct PacketScanner {
    max_header_bytes: usize,
    validate_on_feed: bool,
    metadata: Value,
    header: Vec<u8>,
    body_prefix: Vec<u8>,
    tail: Vec<u8>,
}
impl PacketScanner {
    pub fn new(max_header_bytes: usize, validate_on_feed: bool) -> Self {
        Self {
            max_header_bytes,
            validate_on_feed,
            metadata: Value::Null,
            header: Vec::new(),
            body_prefix: Vec::new(),
            tail: Vec::new(),
        }
    }
    pub fn feed(&mut self, chunk: &[u8]) -> Result<(), PacketError> {
        const STATUS: &[u8] = b"--- status ---\n";
        const DIFF: &[u8] = b"\n--- diff ---\n";
        let mut remaining = chunk;
        if self.metadata.is_null() {
            self.header.extend_from_slice(chunk);
            let separator = self.header.iter().position(|&b| b == b'\n');
            if separator.is_some_and(|s| s > self.max_header_bytes)
                || (separator.is_none() && self.header.len() > self.max_header_bytes)
            {
                return Err(PacketError::HeaderLimit);
            }
            let Some(separator) = separator else {
                return Ok(());
            };
            std::str::from_utf8(&self.header[..separator])?;
            self.metadata = provenant_records::checked_json::from_slice(&self.header[..separator])
                .map_err(|e| match e {
                    provenant_records::checked_json::DecodeError::NumberOutOfRange(_) => {
                        PacketError::NumberOutOfRange
                    }
                    provenant_records::checked_json::DecodeError::Json(e) => PacketError::Json(e),
                })?;
            if self.validate_on_feed && !validate_header(&self.metadata).unwrap_or(false) {
                return Err(PacketError::InvalidHeader);
            }
            remaining = &self.header[separator + 1..];
        }
        self.body_prefix.extend_from_slice(
            &remaining[..remaining.len().min(STATUS.len() - self.body_prefix.len())],
        );
        if self.body_prefix.len() >= STATUS.len() && self.body_prefix != STATUS {
            return Err(PacketError::MissingStatus);
        }
        self.tail.extend_from_slice(remaining);
        if self.tail.windows(DIFF.len()).any(|w| w == DIFF) {
            self.tail = DIFF.to_vec();
        } else if self.tail.len() >= DIFF.len() {
            self.tail.drain(..self.tail.len() - (DIFF.len() - 1));
        }
        Ok(())
    }
    pub fn has_status(&self) -> bool {
        self.body_prefix == b"--- status ---\n"
    }
    pub fn has_diff(&self) -> bool {
        self.tail
            .windows(b"\n--- diff ---\n".len())
            .any(|w| w == b"\n--- diff ---\n")
    }
    pub fn finish(&self) -> &Value {
        &self.metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn header() -> Value {
        json!({"schema_version":1,"record_type":"provenant-git-evidence","repository":"","git_root":"/repo","head":"a".repeat(40),"diff_base":"b".repeat(64),"working_tree":"dirty","diff_from":"HEAD","paths":["é"],"encoding":"utf-8-replacement"})
    }
    #[test]
    fn chunk_boundaries_and_eof_policy_match_python() {
        let packet = format!("{}\n--- status ---\nM x\n--- diff ---\nbody", header());
        let mut cases = vec![];
        for size in [1, 2, 5, 13, 1000] {
            // Byte-sized chunks are represented as byte arrays, so Unicode is not replaced.
            let byte_chunks: Vec<_> = packet.as_bytes().chunks(size).map(|c| c.to_vec()).collect();
            cases.push(json!({"chunks":byte_chunks,"limit":65536,"validate":true}));
        }
        for chunks in [
            vec![b"{}".to_vec()],
            vec![b"{}\nx".to_vec()],
            vec![b"[]\n--- status ---\n".to_vec()],
            vec![b"123456".to_vec()],
            vec![b"{}\n--- status ---\n".to_vec()],
        ] {
            cases.push(json!({"chunks":chunks,"limit":5,"validate":false}));
        }
        let actual: Vec<_> = cases.iter().map(|c| {
            let mut scanner = PacketScanner::new(c["limit"].as_u64().unwrap() as usize,c["validate"].as_bool().unwrap());
            for bytes in c["chunks"].as_array().unwrap() {
                let bytes: Vec<u8> = serde_json::from_value(bytes.clone()).unwrap();
                if let Err(e) = scanner.feed(&bytes) { return json!({"error":e.to_string()}); }
            }
            json!({"metadata":scanner.finish(),"status":scanner.has_status(),"diff":scanner.has_diff()})
        }).collect();
        if let Some(expected) = crate::test_support::oracle("git_evidence_packet", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn header_is_closed_and_python_numeric_equality_is_preserved() {
        let mut cases = vec![Value::Null, json!({}), header()];
        for (key, values) in [
            (
                "schema_version",
                vec![json!(true), json!(1.0), json!(false), json!(2)],
            ),
            (
                "working_tree",
                vec![json!([]), json!({}), json!(null), json!("clean")],
            ),
            (
                "head",
                vec![
                    json!("a".repeat(39)),
                    json!("A".repeat(40)),
                    json!("a".repeat(41)),
                ],
            ),
            ("paths", vec![json!([null]), json!([])]),
        ] {
            for value in values {
                let mut h = header();
                h[key] = value;
                cases.push(h);
            }
        }
        let actual: Vec<_> = cases
            .iter()
            .map(|c| match validate_header(c) {
                Ok(v) => json!(v),
                Err(PolicyError::Unhashable { kind, .. }) => crate::test_support::unhashable(kind),
                Err(e) => panic!("{e}"),
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("git_evidence_header", &json!(cases)) {
            assert_eq!(json!(actual), expected);
        }
    }
}
