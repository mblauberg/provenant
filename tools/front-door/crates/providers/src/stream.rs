use crate::ProviderError;
use serde::Serialize;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ProviderEvent {
    Text(String),
    Session(String),
    Model(String),
    Usage {
        input: u64,
        output: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write: Option<u64>,
        /// The long-TTL part of `cache_write`, where the provider splits writes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_write_1h: Option<u64>,
    },
    Refusal,
    /// A completed turn that the provider says ended for another reason than
    /// finishing: a token limit, a turn limit or a pause.
    EarlyStop {
        reason: String,
    },
    QuotaLimited {
        message: String,
        pool: Option<String>,
        /// Seconds from observation until the provider's stated reset.
        resets_in_seconds: Option<u64>,
    },
    RateLimited {
        message: String,
    },
    ModelUnavailable {
        message: String,
    },
    ModelOverloaded {
        message: String,
    },
    AuthRequired,
    Denied {
        action: String,
    },
    /// A denial from which the provider recovered.
    DeniedNote {
        action: String,
    },
    Error,
    TransientError,
    Terminal,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExitObservation {
    pub code: Option<i64>,
    pub timed_out: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum OutcomeStatus {
    Success,
    Failed,
    Refused,
    Partial,
    Malformed,
}
#[derive(Debug, Serialize)]
pub struct ProviderOutcome {
    adapter: &'static str,
    session: Option<String>,
    model: Option<String>,
    status: OutcomeStatus,
    terminal: bool,
    exit: ExitObservation,
    final_events: Vec<ProviderEvent>,
}
impl ProviderOutcome {
    pub fn adapter(&self) -> &str {
        self.adapter
    }
    pub fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }
    pub fn final_events(&self) -> &[ProviderEvent] {
        &self.final_events
    }
    pub fn status(&self) -> OutcomeStatus {
        self.status
    }
    pub fn terminal(&self) -> bool {
        self.terminal
    }
    pub fn exit(&self) -> ExitObservation {
        self.exit
    }
}
pub(crate) type Decoder = fn(&[u8]) -> Result<Vec<ProviderEvent>, ProviderError>;
const MAX_LINE_BYTES: usize = 1_048_576;
/// An oversized Codex `command_execution` item (its `aggregated_output` echoes
/// whatever a command printed) is not evidence; every other oversized line
/// still fails closed. The item type sits in the first bytes of the frame.
fn oversize_ignorable(adapter: &str, head: &[u8]) -> bool {
    adapter == "codex"
        && (head.starts_with(br#"{"type":"item.completed","item":{"id":""#)
            || head.starts_with(br#"{"type":"item.started","item":{"id":""#))
        && head[..head.len().min(256)]
            .windows(26)
            .any(|window| window == br#""type":"command_execution""#)
}
/// Bounded incremental framing and evidence reduction. Only decoded protocol
/// positions can affect identity or completion; prose is never searched.
pub struct StreamState {
    adapter: &'static str,
    decoder: Decoder,
    pending: Vec<u8>,
    discarding: bool,
    retained_events: Vec<ProviderEvent>,
    session: Option<String>,
    model: Option<String>,
    model_conflict: bool,
    expected_model: Option<String>,
    terminal: bool,
    error: bool,
    transient_error: bool,
    refusal: bool,
    auth_required: bool,
    malformed: bool,
    tail_truncated: bool,
}
impl StreamState {
    pub(crate) fn new(adapter: &'static str, decoder: Decoder) -> Self {
        Self {
            adapter,
            decoder,
            pending: Vec::new(),
            discarding: false,
            retained_events: Vec::new(),
            session: None,
            model: None,
            model_conflict: false,
            expected_model: None,
            terminal: false,
            error: false,
            transient_error: false,
            refusal: false,
            auth_required: false,
            malformed: false,
            tail_truncated: false,
        }
    }
    pub(crate) fn bind_identity(&mut self, session: &str, model: &str) {
        self.session = Some(session.into());
        self.expected_model = Some(model.into());
    }
    pub(crate) fn check_adapter(&self, adapter: &str) -> Result<(), ProviderError> {
        if self.adapter == adapter {
            Ok(())
        } else {
            Err(ProviderError::IdentityMismatch)
        }
    }
    pub(crate) fn authentication_required(&self) -> bool {
        self.auth_required
    }
    pub(crate) fn observe(&mut self, bytes: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
        if self.adapter == "command" {
            const TAIL_BYTES: usize = provenant_records::COMMAND_OUTPUT_TAIL_BYTES;
            let total = self.pending.len().saturating_add(bytes.len());
            self.tail_truncated |= total > TAIL_BYTES;
            if bytes.len() >= TAIL_BYTES {
                self.pending.clear();
                self.pending
                    .extend_from_slice(&bytes[bytes.len() - TAIL_BYTES..]);
            } else {
                let discard = total.saturating_sub(TAIL_BYTES);
                self.pending.drain(..discard);
                self.pending.extend_from_slice(bytes);
            }
            return Ok(Vec::new());
        }
        if self.malformed {
            return Err(ProviderError::Malformed);
        }
        let mut events = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                if std::mem::take(&mut self.discarding) {
                    continue;
                }
                let line = std::mem::take(&mut self.pending);
                if line.is_empty() {
                    continue;
                }
                match self.line(&line) {
                    Ok(decoded) => events.extend(decoded),
                    Err(error) => {
                        self.malformed = true;
                        self.retained_events = events;
                        return Err(error);
                    }
                }
            } else if self.discarding {
            } else {
                if self.pending.len() == MAX_LINE_BYTES {
                    if oversize_ignorable(self.adapter, &self.pending) {
                        // Tool output echoed back by the provider carries no
                        // identity, answer or completion evidence: drop the line.
                        self.discarding = true;
                        self.pending.clear();
                        continue;
                    }
                    self.malformed = true;
                    self.pending.clear();
                    self.retained_events = events;
                    return Err(ProviderError::Overflow);
                }
                self.pending.push(*byte);
            }
        }
        Ok(events)
    }
    fn line(&mut self, bytes: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
        let events = (self.decoder)(bytes)?;
        for event in &events {
            match event {
                ProviderEvent::Session(session) => {
                    if session.is_empty() || self.session.as_ref().is_some_and(|old| old != session)
                    {
                        return Err(ProviderError::IdentityMismatch);
                    }
                    self.session = Some(session.clone());
                }
                ProviderEvent::Model(model) => {
                    if self
                        .expected_model
                        .as_ref()
                        .is_some_and(|expected| expected != model)
                    {
                        return Err(ProviderError::IdentityMismatch);
                    }
                    if model.is_empty() {
                        return Err(ProviderError::Malformed);
                    }
                    if self.model.as_ref().is_some_and(|old| old != model) {
                        self.model_conflict = true;
                    }
                    self.model = Some(model.clone());
                }
                ProviderEvent::Terminal => {
                    self.terminal = true;
                    self.transient_error = false;
                }
                ProviderEvent::Error | ProviderEvent::Denied { .. } => self.error = true,
                ProviderEvent::TransientError => self.transient_error = true,
                ProviderEvent::Refusal
                | ProviderEvent::QuotaLimited { .. }
                | ProviderEvent::RateLimited { .. }
                | ProviderEvent::ModelUnavailable { .. }
                | ProviderEvent::ModelOverloaded { .. } => self.refusal = true,
                ProviderEvent::AuthRequired => self.auth_required = true,
                _ => {}
            }
        }
        Ok(events)
    }
    pub(crate) fn finish(
        mut self,
        exit: ExitObservation,
    ) -> Result<ProviderOutcome, ProviderError> {
        let mut final_events = std::mem::take(&mut self.retained_events);
        if self.adapter == "command" {
            let mut text = String::new();
            if self.tail_truncated {
                text.push_str("[output truncated; retained tail]\n");
            }
            let tail = String::from_utf8_lossy(&self.pending);
            for line in tail.lines() {
                text.push_str("| ");
                // VerdictCapture treats any verdict: substring as a marker,
                // even inside quotes. Escape log markers before appending the
                // sole authoritative exit-derived verdict.
                for (i, ch) in line.char_indices() {
                    if ch == ':'
                        && i >= 7
                        && line.as_bytes()[i - 7..i].eq_ignore_ascii_case(b"verdict")
                    {
                        text.push('\\');
                    }
                    text.push(ch);
                }
                text.push('\n');
            }
            let passed = exit.code == Some(0) && !exit.timed_out;
            text.push_str(if passed {
                "VERDICT: PASS\n"
            } else {
                "VERDICT: FAIL\n"
            });
            self.pending.clear();
            self.terminal = true;
            final_events.push(ProviderEvent::Text(text));
        }
        if !self.pending.is_empty() && !self.malformed {
            let line = std::mem::take(&mut self.pending);
            match self.line(&line) {
                Ok(events) => final_events.extend(events),
                Err(_) => self.malformed = true,
            }
        }
        let status = if self.malformed {
            OutcomeStatus::Malformed
        } else if self.refusal || self.auth_required {
            OutcomeStatus::Refused
        } else if self.error || self.transient_error {
            OutcomeStatus::Failed
        } else if exit.timed_out {
            OutcomeStatus::Partial
        } else if exit.code != Some(0) {
            OutcomeStatus::Failed
        } else if self.terminal {
            OutcomeStatus::Success
        } else {
            OutcomeStatus::Partial
        };
        Ok(ProviderOutcome {
            adapter: self.adapter,
            session: self.session,
            model: if self.model_conflict || self.malformed {
                None
            } else {
                self.model
            },
            status,
            terminal: self.terminal,
            exit,
            final_events,
        })
    }
}
pub(crate) fn wire<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ProviderError> {
    // Providers may repeat a member inside an event they otherwise ignore (Codex
    // `web_search` items carry two `id`s). Only that case falls back to plain
    // serde_json, which keeps the last value; the line is already length-bounded.
    match provenant_records::decode(bytes, provenant_records::Limits::CONFIG) {
        Ok(value) => Ok(value),
        Err(provenant_records::DecodeError::DuplicateMember { .. }) => {
            serde_json::from_slice(bytes).map_err(|_| ProviderError::Malformed)
        }
        Err(_) => Err(ProviderError::Malformed),
    }
}
