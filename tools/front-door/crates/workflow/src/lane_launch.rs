//! Source launch-builder wire shapes. Execution remains with the supervisor.
use crate::lane_exit::{Member, ProcessInteger, ReceiptFields};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::num::NonZeroU64;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LaunchError {
    #[error("lane durations must be finite positive seconds")]
    Duration,
    #[error("invalid batch cancel identity")]
    BatchIdentity,
    #[error("launch integer is outside the u64 range")]
    NumberOutOfRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Milliseconds(NonZeroU64);
impl Milliseconds {
    pub fn get(self) -> u64 {
        self.0.get()
    }
    pub fn from_seconds(seconds: &Value) -> Result<Self, LaunchError> {
        if let Some(n) = seconds.as_u64().filter(|n| *n > 0) {
            return n
                .checked_mul(1000)
                .and_then(NonZeroU64::new)
                .map(Self)
                .ok_or(LaunchError::NumberOutOfRange);
        }
        let n = seconds.as_f64().ok_or(LaunchError::Duration)?;
        Self::from_float_seconds(n)
    }
    pub fn from_float_seconds(seconds: f64) -> Result<Self, LaunchError> {
        if !seconds.is_finite() || seconds <= 0.0 {
            return Err(LaunchError::Duration);
        }
        let ms = (seconds * 1000.0).ceil();
        if !ms.is_finite() || ms >= 18_446_744_073_709_551_616.0 {
            return Err(LaunchError::NumberOutOfRange);
        }
        NonZeroU64::new(ms as u64)
            .map(Self)
            .ok_or(LaunchError::Duration)
    }
}

/// Validation before constructing a batch cancellation path. Falsy identities
/// ask the dispatch owner for its published identity instead.
pub fn batch_cancel_identity(value: &Value) -> Result<Option<&str>, LaunchError> {
    if !provenant_core::py::python_truth(value) {
        return Ok(None);
    }
    let text = value.as_str().ok_or(LaunchError::BatchIdentity)?;
    let suffix = text
        .strip_prefix("batch-")
        .filter(|s| !s.is_empty())
        .ok_or(LaunchError::BatchIdentity)?;
    if !suffix.chars().all(crate::pending::python_digit) {
        return Err(LaunchError::BatchIdentity);
    }
    Ok(Some(text))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub schema: String,
    pub argv: Vec<String>,
    pub cwd: String,
    #[serde(with = "crate::pending::ordered_pairs")]
    pub env: Vec<(String, String)>,
    pub stdin: StdinSpec,
    pub attempt_dir: String,
    pub run_token: String,
    pub capture: CaptureSpec,
    pub silence_observation_ms: Milliseconds,
    pub timeouts_ms: Timeouts,
    pub progress: ProgressSpec,
    pub cancel_markers: Vec<String>,
    pub observer: ObserverCommand,
    pub spare: SparePolicy,
}
impl LaunchSpec {
    pub fn from_json(raw: &[u8]) -> Result<Self, provenant_records::checked_json::DecodeError> {
        crate::pending::checked_ordered_decode(raw)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StdinSpec {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub path: Member<StdinPath>,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(transparent)]
pub struct StdinPath(String);
#[derive(Debug, thiserror::Error)]
#[error("stdin path must be non-empty")]
pub struct EmptyStdinPath;
impl StdinPath {
    pub fn new(path: String) -> Result<Self, EmptyStdinPath> {
        if path.is_empty() {
            Err(EmptyStdinPath)
        } else {
            Ok(Self(path))
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl<'de> Deserialize<'de> for StdinPath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSpec {
    pub events: String,
    pub stderr: String,
    pub max_bytes: NonZeroU64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timeouts {
    pub startup: Milliseconds,
    pub wall: Milliseconds,
    pub settle: Milliseconds,
    pub child_grace: Milliseconds,
    pub root_flush: Milliseconds,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgressSpec {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub writer_tree: Member<WriterTree>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriterTree {
    pub root: String,
    pub exclude: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObserverCommand {
    pub argv: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SparePolicy {
    pub code_mode_host: bool,
}

/// Source stdout notifications also accept the older `type` discriminator. The
/// raw notification remains open until the launcher applies that precedence.
pub type LaunchNotification = crate::pending::OrderedRecord<LaunchNotificationFields>;
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LaunchNotificationFields {
    #[serde(default, skip_serializing_if = "Member::is_absent")]
    pub event: Member<Value>,
    #[serde(default, skip_serializing_if = "Member::is_absent", rename = "type")]
    pub legacy_type: Member<Value>,
    #[serde(flatten)]
    pub fields: crate::pending::OrderedMap<crate::pending::OrderedValue>,
}

/// Provider identity is observation only, without an OS process handle.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderIdentity {
    pub pid: crate::pending::OrderedValue,
    pub returncode: Option<ProcessInteger>,
}
/// This is a native return value, not a JSON enum with an invented wire tag.
#[derive(Debug, Clone, PartialEq)]
pub enum LaunchOutcome {
    Unsupported {
        warning: String,
    },
    Supported {
        record: crate::pending::OrderedMap<crate::pending::OrderedValue>,
        fields: Box<ReceiptFields>,
        provider: Option<ProviderIdentity>,
        started_at: crate::pending::OrderedValue,
        warning: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn source_builder_records_round_trip_with_optional_stdin_and_writer_progress() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let mut cases = vec![];
        for mode in ["worktree_write", "read_only"] {
            for stdin in [true, false] {
                cases.push(json!({"root":root,"mode":mode,"stdin":stdin}));
            }
        }
        if let Some(outputs) = crate::test_support::oracle("lane_launch_contract", &json!(cases)) {
            for raw in outputs.as_array().unwrap() {
                let wire = raw.as_str().unwrap();
                let typed = LaunchSpec::from_json(wire.as_bytes()).unwrap();
                assert_eq!(serde_json::to_string(&typed).unwrap(), wire);
                assert_eq!(typed.timeouts_ms.wall.get(), 1125);
                assert_eq!(typed.timeouts_ms.startup.get(), 25);
                assert_eq!(typed.silence_observation_ms.get(), 125);
                assert_eq!(
                    typed
                        .env
                        .iter()
                        .find(|(key, _)| key == "SECRET_REFERENCE")
                        .map(|(_, v)| v)
                        .unwrap(),
                    "synthetic-value"
                );
                assert_eq!(typed.capture.events, "events.jsonl");
            }
        }
        for raw in [
            json!({"event":"started","pid":123}),
            json!({"event":"progress","last_progress_at":"stamp"}),
            json!({"type":"started","pid":null,"unknown":true}),
        ] {
            let typed: LaunchNotification = serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(
                serde_json::to_string(&typed).unwrap(),
                serde_json::to_string(&raw).unwrap()
            );
        }
    }
    #[test]
    fn notification_and_environment_preserve_source_order() {
        let raw = r#"{"event":"started","z":{"z":1,"a":2},"a":3}"#;
        let typed: LaunchNotification = serde_json::from_str(raw).unwrap();
        assert_eq!(serde_json::to_string(&typed).unwrap(), raw);
        #[derive(Serialize, Deserialize)]
        struct Environment(#[serde(with = "crate::pending::ordered_pairs")] Vec<(String, String)>);
        let raw = r#"{"Z":"1","A":"2"}"#;
        let typed: Environment = serde_json::from_str(raw).unwrap();
        assert_eq!(typed.0[0].0, "Z");
        assert_eq!(serde_json::to_string(&typed).unwrap(), raw);
    }
    #[test]
    fn launch_wire_rejects_zero_limits_and_deadlines() {
        assert!(serde_json::from_str::<CaptureSpec>(
            r#"{"events":"e","stderr":"s","max_bytes":0}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Timeouts>(
            r#"{"startup":0,"wall":1,"settle":1,"child_grace":1,"root_flush":1}"#
        )
        .is_err());
    }
    #[test]
    fn source_duration_and_batch_failures_match() {
        let cases = json!([{"raw":"true"},{"raw":"NaN"},{"raw":"0"},{"raw":"-1"},{"raw":"0.0001"},{"raw":"1.0001"}]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let result = if c["raw"] == "NaN" {
                    Milliseconds::from_float_seconds(f64::NAN)
                } else {
                    Milliseconds::from_seconds(
                        &serde_json::from_str::<Value>(c["raw"].as_str().unwrap()).unwrap(),
                    )
                };
                match result {
                    Ok(ms) => json!({"wire":ms.get().to_string()}),
                    Err(e) => json!({"error":e.to_string()}),
                }
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_duration", &cases) {
            assert_eq!(json!(actual), expected);
        }
        let cases = json!([{"batch_id":true},{"batch_id":"batch-"},{"batch_id":"batch-no"},{"batch_id":"../batch-123"},{"batch_id":"batch-²"},{"batch_id":"batch-123"}]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|c| match batch_cancel_identity(&c["batch_id"]) {
                Ok(_) => Value::Null,
                Err(e) => json!(e.to_string()),
            })
            .collect();
        if let Some(expected) = crate::test_support::oracle("lane_batch", &cases) {
            assert_eq!(json!(actual), expected);
        }
    }
    #[test]
    fn stdin_builder_contract_has_no_null_path_variant() {
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":""})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":null})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({"path":""})).is_err());
        assert!(serde_json::from_value::<StdinSpec>(json!({})).is_ok());
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("launch notification is missing {0}")]
    MissingMember(&'static str),
    #[error("Python integer is outside the i64/u64 range")]
    NumberOutOfRange,
    #[error("lane run exited {returncode} without a valid exit record: {tail}")]
    MissingExit { returncode: i32, tail: String },
    #[error(transparent)]
    Exit(#[from] crate::lane_exit::ExitError),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("provider is still held by lane run")]
pub struct ProviderStillHeld;
impl ProviderIdentity {
    pub fn new(pid: crate::pending::OrderedValue) -> Self {
        Self {
            pid,
            returncode: None,
        }
    }
    pub fn poll(&self) -> Option<&ProcessInteger> {
        self.returncode.as_ref()
    }
    pub fn wait(&self) -> Result<&ProcessInteger, ProviderStillHeld> {
        self.returncode.as_ref().ok_or(ProviderStillHeld)
    }
}

/// Bookkeeping failures only produce warnings. This seam has no process or
/// signalling handle and cannot grant authority to stop a provider.
pub trait LaunchCallbacks {
    fn has_progress_callback(&self) -> bool;
    fn on_start(&mut self, provider: &ProviderIdentity) -> std::io::Result<()>;
    fn on_progress(&mut self, at: &crate::pending::OrderedValue) -> std::io::Result<()>;
    fn warning(&mut self, message: String);
}

/// Synchronous stdout protocol reducer. The OS owner supplies bounded reads and
/// owns waiting, cancellation markers and signals. No subprocess is started here.
#[derive(Debug)]
pub struct LaunchProtocol {
    pending: Vec<u8>,
    pub provider: Option<ProviderIdentity>,
    pub started_at: crate::pending::OrderedValue,
    terminal_record: Option<crate::pending::OrderedMap<crate::pending::OrderedValue>>,
}
impl LaunchProtocol {
    pub fn feed(
        &mut self,
        data: &[u8],
        callbacks: &mut impl LaunchCallbacks,
    ) -> Result<(), ProtocolError> {
        use crate::pending::{OrderedMap, OrderedValue as V};
        // The source reads <=65536 bytes at a time. Split arbitrary transport
        // chunks at that same bound before applying its pending-buffer policy.
        for chunk in data.chunks(65536) {
            self.pending.extend_from_slice(chunk);
            while let Some(end) = self.pending.iter().position(|&byte| byte == b'\n') {
                let line: Vec<_> = self.pending.drain(..=end).collect();
                if end > 65536 {
                    continue;
                }
                let event: OrderedMap<V> =
                    match crate::pending::checked_ordered_decode(&line[..end]) {
                        Ok(event) => event,
                        Err(provenant_records::checked_json::DecodeError::NumberOutOfRange(_)) => {
                            return Err(ProtocolError::NumberOutOfRange);
                        }
                        Err(_) => continue,
                    };
                let kind = event
                    .get("event")
                    .or_else(|| event.get("type"))
                    .and_then(V::as_str);
                if kind == Some("started") && self.provider.is_none() {
                    let pid = event
                        .get("pid")
                        .ok_or(ProtocolError::MissingMember("pid"))?
                        .clone();
                    self.provider = Some(ProviderIdentity::new(pid));
                    self.started_at = event.get("started_at").cloned().unwrap_or(V::Null);
                    if let Some(provider) = &self.provider {
                        if let Err(error) = callbacks.on_start(provider) {
                            callbacks.warning(format!(
                                "provider start bookkeeping unavailable: {error}"
                            ));
                        }
                    }
                } else if kind == Some("progress") && callbacks.has_progress_callback() {
                    let at = event
                        .get("last_progress_at")
                        .ok_or(ProtocolError::MissingMember("last_progress_at"))?;
                    if let Err(error) = callbacks.on_progress(at) {
                        callbacks.warning(format!(
                            "provider progress bookkeeping unavailable: {error}"
                        ));
                    }
                } else if event.get("schema").and_then(V::as_str) == Some("provenant.lane.exit.v1")
                {
                    self.terminal_record = Some(event);
                }
            }
            if self.pending.len() > 65536 {
                self.pending.clear();
            }
        }
        Ok(())
    }

    /// The caller passes None only when the bounded no-follow file reader raised
    /// OSError/ValueError. A malformed receipt object is not a read failure and
    /// must be translated, rather than silently replaced by the stdout record.
    pub fn settle(
        mut self,
        returncode: i32,
        published_record: Option<crate::pending::OrderedMap<crate::pending::OrderedValue>>,
        stderr: &[u8],
        cancelled_request: bool,
    ) -> Result<LaunchOutcome, ProtocolError> {
        use crate::pending::OrderedValue as V;
        let warning = String::from_utf8_lossy(&stderr[..stderr.len().min(65536)]).into_owned();
        if returncode == 64 && self.provider.is_none() {
            return Ok(LaunchOutcome::Unsupported { warning });
        }
        let record = published_record.or(self.terminal_record).ok_or_else(|| {
            let chars = warning.chars().count();
            ProtocolError::MissingExit {
                returncode,
                tail: warning.chars().skip(chars.saturating_sub(300)).collect(),
            }
        })?;
        let mut fields = crate::lane_exit::receipt_from_object(&record)?;
        if cancelled_request && fields.forced.as_str() == Some("interrupted") {
            fields.forced = V::String("cancelled".into());
        }
        if self.provider.is_none() {
            let pid = match &fields.pid {
                Some(ProcessInteger::Signed(pid)) if *pid != 0 => Some(V::Number((*pid).into())),
                Some(ProcessInteger::Unsigned(pid)) if *pid != 0 => Some(V::Number((*pid).into())),
                _ => None,
            };
            self.provider = pid.map(ProviderIdentity::new);
        }
        if let Some(provider) = &mut self.provider {
            provider.returncode = fields.exit.clone();
        }
        Ok(LaunchOutcome::Supported {
            record,
            fields: Box::new(fields),
            provider: self.provider,
            started_at: self.started_at,
            warning,
        })
    }
}

impl Default for LaunchProtocol {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            provider: None,
            started_at: crate::pending::OrderedValue::Null,
            terminal_record: None,
        }
    }
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use crate::pending::OrderedValue as V;
    use serde_json::json;

    #[derive(Default)]
    struct Callbacks {
        progress: bool,
        fail: bool,
        calls: Vec<serde_json::Value>,
        warnings: Vec<String>,
    }
    impl LaunchCallbacks for Callbacks {
        fn has_progress_callback(&self) -> bool {
            self.progress
        }
        fn on_start(&mut self, provider: &ProviderIdentity) -> std::io::Result<()> {
            self.calls.push(json!(["started", provider.pid]));
            if self.fail {
                Err(std::io::Error::other("full"))
            } else {
                Ok(())
            }
        }
        fn on_progress(&mut self, at: &V) -> std::io::Result<()> {
            self.calls.push(json!(["progress", at]));
            if self.fail {
                Err(std::io::Error::other("full"))
            } else {
                Ok(())
            }
        }
        fn warning(&mut self, message: String) {
            self.warnings.push(message);
        }
    }
    #[test]
    fn unsupported_publication_loss_and_bookkeeping_match_real_source_run() {
        let record =
            r#"{"schema":"provenant.lane.exit.v1","pid":123,"exit":0,"capture":{"z":1,"a":2}}"#;
        let started = "{\"type\":\"started\",\"pid\":123,\"started_at\":\"stamp\"}\n";
        let cases = json!([
            {"stdout":"","stderr":"unsupported\n","returncode":64},
            {"stdout":format!("{record}\n"),"stderr":"publication unavailable\n","returncode":75},
            {"stdout":format!("{started}{record}\n"),"returncode":64},
            {"stdout":format!("{started}{{\"event\":\"progress\",\"last_progress_at\":{{\"z\":1,\"a\":2}}}}\n"),"published":record,"bookkeeping_error":true},
            {"stdout":"{\"event\":null,\"type\":\"started\",\"pid\":5}\n","published":record},
            {"stdout":format!("{record}\n"),"published":r#"{"schema":"bad"}"#},
            {"stdout":"garbage\n[]\n{}\n","stderr":"é🙂","returncode":1},
            {"stdout":"{\"event\":\"started\",\"pid\":null}\n","published":record},
            {"stdout":"{\"event\":\"progress\"}\n","published":record,"progress":false}
        ]);
        let actual: Vec<_> = cases
            .as_array()
            .unwrap()
            .iter()
            .map(|case| {
                let mut callbacks = Callbacks {
                    progress: case["progress"].as_bool().unwrap_or(true),
                    fail: case["bookkeeping_error"].as_bool().unwrap_or(false),
                    ..Callbacks::default()
                };
                let mut protocol = LaunchProtocol::default();
                protocol
                    .feed(case["stdout"].as_str().unwrap().as_bytes(), &mut callbacks)
                    .unwrap();
                let published = case["published"]
                    .as_str()
                    .map(|s| serde_json::from_str(s).unwrap());
                let result = protocol.settle(
                    case["returncode"].as_i64().unwrap_or(0) as i32,
                    published,
                    case["stderr"].as_str().unwrap_or("").as_bytes(),
                    false,
                );
                let output = match result {
                    Ok(outcome) => outcome.python_wire().unwrap(),
                    Err(error) => {
                        return json!({"error":error.to_string(),"callbacks":callbacks.calls})
                    }
                };
                json!({"wire":output,"callbacks":callbacks.calls})
            })
            .collect();
        if let Some(expected) =
            crate::test_support::oracle_script("oracle_lane.py", "launch", &cases)
        {
            assert_eq!(json!(actual), expected);
        }
    }

    #[test]
    fn cancellation_mapping_has_no_signal_authority_and_does_not_change_other_reasons() {
        for (forced, expected) in [
            ("supervisor_signal", "cancelled"),
            ("wall_timeout", "timed_out"),
        ] {
            let record =
                serde_json::from_value(json!({"schema":"provenant.lane.exit.v1","forced":forced}))
                    .unwrap();
            let outcome = LaunchProtocol::default()
                .settle(0, Some(record), b"", true)
                .unwrap();
            let LaunchOutcome::Supported { fields, .. } = outcome else {
                panic!("supported expected")
            };
            assert_eq!(fields.forced.as_str(), Some(expected));
        }
        let identity = ProviderIdentity::new(V::Null);
        assert!(identity.wait().is_err());
        assert!(identity.poll().is_none());
    }
}

impl LaunchOutcome {
    /// Render the source namespace's retained fields in construction order.
    /// Explicit Python encoding is required for finite exponent spelling.
    pub fn python_wire(&self) -> Result<String, serde_json::Error> {
        #[derive(Serialize)]
        struct Provider<'a> {
            pid: &'a crate::pending::OrderedValue,
            returncode: &'a Option<ProcessInteger>,
        }
        match self {
            Self::Unsupported { warning } => Ok(format!(
                "{{\"unsupported\":true,\"warning\":{}}}",
                crate::pending::python_wire(warning)?
            )),
            Self::Supported {
                record,
                fields,
                provider,
                started_at,
                warning,
            } => {
                let provider = provider.as_ref().map(|p| Provider {
                    pid: &p.pid,
                    returncode: &p.returncode,
                });
                Ok(format!("{{\"unsupported\":false,\"record\":{},\"fields\":{},\"provider\":{},\"started_at\":{},\"warning\":{}}}",
                    crate::pending::python_wire(record)?, crate::pending::python_wire(fields)?,
                    crate::pending::python_wire(&provider)?, started_at.python_wire(), crate::pending::python_wire(warning)?))
            }
        }
    }
}
