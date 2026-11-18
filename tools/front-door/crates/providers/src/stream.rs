use crate::ProviderError;
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ProviderEvent {
    Text(String),
    /// The next text is a new whole message, so it must start on its own line.
    /// Line-based captures (handoff, verdict) would otherwise miss a message
    /// that follows prose with no line ending. Adapters that stream deltas
    /// never emit this.
    MessageStart,
    Session(String),
    Model(String),
    /// Digest of Claude's assistant text and its model, pending the final result.
    AnswerModel {
        model: String,
        digest: [u8; 32],
    },
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
    Unclassified {
        message: String,
    },
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
    /// Claude or Cursor assistant text, held by the stream state as a fallback answer.
    /// It never leaves the stream state.
    AssistantText(String),
    /// A tool call or tool result. It invalidates earlier assistant text as the
    /// fallback answer and never leaves the stream state.
    ToolActivity,
    /// The final result was empty, so the last assistant text stands in for it.
    ResultFallback,
    /// The provider completed a turn; text before it was that turn's answer.
    /// Decoders emit `tool_activity: false`; the stream state records whether
    /// a tool ran since the previous turn ended.
    TurnEnd {
        tool_activity: bool,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExitObservation {
    pub code: Option<i64>,
    pub timed_out: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeStatus {
    Success,
    Failed,
    Refused,
    Partial,
    Malformed,
}
impl OutcomeStatus {
    pub const ALL: &'static [Self] = &[
        Self::Success,
        Self::Failed,
        Self::Refused,
        Self::Partial,
        Self::Malformed,
    ];
}

#[cfg(test)]
#[test]
fn outcome_status_declaration_covers_the_wire_variants() {
    let mut names = std::collections::BTreeSet::new();
    for status in OutcomeStatus::ALL {
        // The exhaustive match forces new variants to extend the wire table.
        let name = match status {
            OutcomeStatus::Success => "Success",
            OutcomeStatus::Failed => "Failed",
            OutcomeStatus::Refused => "Refused",
            OutcomeStatus::Partial => "Partial",
            OutcomeStatus::Malformed => "Malformed",
        };
        assert!(names.insert(name));
        let encoded = provenant_records::encode(status).unwrap();
        assert_eq!(serde_json::from_slice::<String>(&encoded).unwrap(), name);
        let decoded: OutcomeStatus =
            provenant_records::decode(encoded.as_slice(), provenant_records::Limits::default())
                .unwrap();
        assert_eq!(decoded, *status);
    }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    protocol_error: Option<String>,
    /// A malformed outcome lost framing (undecodable or cut-off bytes), as
    /// opposed to a complete frame whose content the adapter rejects.
    #[serde(skip)]
    framing_loss: bool,
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
    pub fn protocol_error(&self) -> Option<&str> {
        self.protocol_error.as_deref()
    }
    /// True when a malformed outcome is missing evidence rather than a
    /// complete provider frame reporting a failure.
    pub fn framing_loss(&self) -> bool {
        self.framing_loss
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
const MAX_LINE_BYTES: usize = provenant_os::exec::PROVIDER_LINE_LIMIT;
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
    /// Immutable operation policy, retained for the lifetime of the stream.
    pub policy: std::sync::Arc<provenant_records::policy::Policy>,
    decoder: Decoder,
    pending: Vec<u8>,
    discarding: bool,
    retained_events: Vec<ProviderEvent>,
    session: Option<String>,
    expected_session: Option<String>,
    model: Option<String>,
    model_conflict: bool,
    expected_model: Option<String>,
    answer_models: Vec<(String, [u8; 32])>,
    terminal: bool,
    error: bool,
    transient_error: bool,
    refusal: bool,
    auth_required: bool,
    malformed: bool,
    line_count: usize,
    malformed_line: Option<usize>,
    protocol_error: Option<String>,
    incomplete_eof_fragment: bool,
    tail_truncated: bool,
    last_assistant_text: Option<String>,
    result_text_seen: bool,
    early_stop: bool,
    turn_tool_activity: bool,
}
impl StreamState {
    pub(crate) fn new(
        adapter: &'static str,
        decoder: Decoder,
        policy: &provenant_records::policy::Policy,
    ) -> Self {
        Self {
            adapter,
            policy: std::sync::Arc::new(policy.clone()),
            decoder,
            pending: Vec::new(),
            discarding: false,
            retained_events: Vec::new(),
            session: None,
            expected_session: None,
            model: None,
            model_conflict: false,
            expected_model: None,
            answer_models: Vec::new(),
            terminal: false,
            error: false,
            transient_error: false,
            refusal: false,
            auth_required: false,
            malformed: false,
            line_count: 0,
            malformed_line: None,
            protocol_error: None,
            incomplete_eof_fragment: false,
            tail_truncated: false,
            last_assistant_text: None,
            result_text_seen: false,
            early_stop: false,
            turn_tool_activity: false,
        }
    }
    /// Whether the actual EOF record ended before its JSON value was complete.
    pub fn incomplete_eof_fragment(&self) -> bool {
        self.incomplete_eof_fragment
    }
    pub(crate) fn bind_identity(&mut self, session: &str, model: &str) {
        self.expect_session(session);
        self.expected_model = Some(model.into());
    }
    /// Bind session reports to the retained provider session before a resumed
    /// turn begins. The expected value stays separate from observed evidence,
    /// so a child that emits no session report cannot inherit a false match.
    pub fn expect_session(&mut self, session: &str) {
        self.expected_session = Some(session.into());
    }
    /// The retained session this stream must report, when it is a continuation.
    pub fn expected_session(&self) -> Option<&str> {
        self.expected_session.as_deref()
    }
    /// Bind streamed model reports to the admitted route before a live turn
    /// begins. A mismatch is an identity conflict, not a replacement model.
    pub fn expect_model(&mut self, model: &str) {
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
    fn raw_stream(&self) -> bool {
        provenant_records::AdapterSpec::get(self.adapter)
            .is_some_and(|spec| spec.stdout == provenant_records::StreamContract::Raw)
    }
    pub(crate) fn observe(&mut self, bytes: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
        if self.raw_stream() {
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
            return Err(match self.malformed_line {
                Some(line) => ProviderError::MalformedFrame { line },
                None => ProviderError::Malformed,
            });
        }
        let mut events = Vec::new();
        for byte in bytes {
            if *byte == b'\n' {
                self.line_count += 1;
                let current_line = self.line_count;
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
                        let error = match error {
                            ProviderError::IncompleteFrame | ProviderError::Malformed => {
                                self.malformed_line = Some(current_line);
                                ProviderError::MalformedFrame { line: current_line }
                            }
                            other => other,
                        };
                        self.protocol_error = Some(error.to_string());
                        return Err(error);
                    }
                }
            } else if self.discarding {
            } else {
                if self.pending.len() == MAX_LINE_BYTES {
                    if oversize_ignorable(self.adapter, &self.pending) {
                        self.discarding = true;
                        self.pending.clear();
                        continue;
                    }
                    self.malformed = true;
                    self.pending.clear();
                    self.retained_events = events;
                    let line = self.line_count + 1;
                    self.malformed_line = Some(line);
                    let error = ProviderError::MalformedFrame { line };
                    self.protocol_error = Some(error.to_string());
                    return Err(error);
                }
                self.pending.push(*byte);
            }
        }
        Ok(events)
    }
    pub(crate) fn observe_eof_fragment(
        &mut self,
        bytes: &[u8],
    ) -> Result<Vec<ProviderEvent>, ProviderError> {
        let mut events = self.observe(bytes)?;
        if self.raw_stream() || self.pending.is_empty() {
            return Ok(events);
        }
        let line = std::mem::take(&mut self.pending);
        match self.line(&line) {
            Ok(mut final_events) => {
                events.append(&mut final_events);
                Ok(events)
            }
            Err(error) => {
                self.malformed = true;
                self.retained_events = events;
                self.incomplete_eof_fragment = matches!(error, ProviderError::IncompleteFrame);
                let error = match error {
                    ProviderError::IncompleteFrame | ProviderError::Malformed => {
                        let line = self.line_count + 1;
                        self.malformed_line = Some(line);
                        ProviderError::MalformedFrame { line }
                    }
                    other => other,
                };
                self.protocol_error = Some(error.to_string());
                Err(error)
            }
        }
    }
    fn line(&mut self, bytes: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
        let mut events = (self.decoder)(bytes)?;
        let mut answer_model = None;
        for event in &events {
            match event {
                ProviderEvent::Session(session) => {
                    if session.is_empty()
                        || self
                            .expected_session
                            .as_ref()
                            .is_some_and(|expected| expected != session)
                        || self.session.as_ref().is_some_and(|old| old != session)
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
                        return Err(ProviderError::MalformedResult(
                            "provider reported an empty model".into(),
                        ));
                    }
                    if self.model.as_ref().is_some_and(|old| old != model) {
                        self.model_conflict = true;
                    }
                    self.model = Some(model.clone());
                }
                ProviderEvent::AnswerModel { model, digest } => {
                    if model.is_empty() || model.len() > 4096 {
                        return Err(ProviderError::MalformedResult(
                            "provider answer model is empty or longer than 4096 bytes".into(),
                        ));
                    }
                    if self.answer_models.len() == 1024 {
                        self.answer_models.remove(0);
                    }
                    self.answer_models.push((model.clone(), *digest));
                }
                ProviderEvent::AssistantText(text) => {
                    self.last_assistant_text = Some(text.clone());
                }
                ProviderEvent::ToolActivity => self.last_assistant_text = None,
                ProviderEvent::Text(text) if matches!(self.adapter, "claude" | "cursor") => {
                    self.result_text_seen |= !text.trim().is_empty();
                    if self.adapter == "claude"
                        && let Some(model) = self.resolve_answer_model(text)?
                    {
                        answer_model = Some(model);
                    }
                }
                ProviderEvent::EarlyStop { .. } => self.early_stop = true,
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
        for event in &mut events {
            match event {
                ProviderEvent::ToolActivity => self.turn_tool_activity = true,
                ProviderEvent::TurnEnd { tool_activity } => {
                    *tool_activity = std::mem::take(&mut self.turn_tool_activity);
                }
                _ => {}
            }
        }
        events.retain(|event| {
            !matches!(
                event,
                ProviderEvent::AssistantText(_) | ProviderEvent::ToolActivity
            )
        });
        if let Some(model) = answer_model {
            events.push(ProviderEvent::Model(model));
        }
        Ok(events)
    }
    fn resolve_answer_model(&mut self, text: &str) -> Result<Option<String>, ProviderError> {
        let digest = *provenant_records::CanonicalV1::file_digest(text.as_bytes()).bytes();
        let mut models = self
            .answer_models
            .iter()
            .filter(|(_, answer)| *answer == digest)
            .map(|(model, _)| model);
        if let Some(model) = models.next() {
            if models.any(|other| other != model) {
                self.model_conflict = true;
            } else {
                if self
                    .expected_model
                    .as_ref()
                    .is_some_and(|expected| expected != model)
                {
                    return Err(ProviderError::IdentityMismatch);
                }
                self.model = Some(model.clone());
                return Ok(Some(model.clone()));
            }
        }
        Ok(None)
    }
    pub(crate) fn finish(
        mut self,
        exit: ExitObservation,
    ) -> Result<ProviderOutcome, ProviderError> {
        let mut final_events = std::mem::take(&mut self.retained_events);
        let mut protocol_error = self.protocol_error.take();
        if self.raw_stream() {
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
        // A malformed provider result can carry a useful typed diagnostic
        // without losing the JSONL boundary. Only line framing failures mean
        // the retained capture cannot support a terminal decision.
        let mut framing_loss = self.malformed_line.is_some();
        if !self.pending.is_empty() && !self.malformed {
            let line = std::mem::take(&mut self.pending);
            match self.line(&line) {
                Ok(events) => final_events.extend(events),
                Err(error) => {
                    self.malformed = true;
                    framing_loss = matches!(
                        error,
                        ProviderError::Malformed | ProviderError::IncompleteFrame
                    );
                    let diagnostic = error.to_string();
                    let error = match error {
                        ProviderError::IncompleteFrame | ProviderError::Malformed => {
                            let line = self.line_count + 1;
                            self.malformed_line = Some(line);
                            ProviderError::MalformedFrame { line }
                        }
                        other => other,
                    };
                    protocol_error = Some(diagnostic);
                }
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
        // A turn that ran a tool and never produced its own `result` (an error
        // result, a kill or a timeout) is still a turn: record its boundary so
        // an earlier turn's handoff does not stand over unfinished work.
        if std::mem::take(&mut self.turn_tool_activity) {
            final_events.push(ProviderEvent::TurnEnd {
                tool_activity: true,
            });
        }
        // Recover only a clean, complete terminal stream, using the same
        // answer identity checks as an ordinary result event.
        if matches!(self.adapter, "claude" | "cursor")
            && status == OutcomeStatus::Success
            && !self.early_stop
            && !self.result_text_seen
            && let Some(text) = self.last_assistant_text.take()
        {
            if self.adapter == "claude"
                && let Some(model) = self.resolve_answer_model(&text)?
            {
                final_events.push(ProviderEvent::Model(model));
            }
            final_events.push(ProviderEvent::Text(text));
            final_events.push(ProviderEvent::ResultFallback);
        }
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
            protocol_error,
            framing_loss,
        })
    }
}
pub(crate) fn wire<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ProviderError> {
    // Provider frames are foreign content, not Provenant records, so strict record
    // decoding does not apply: it rejects shapes (repeated members, wide integers,
    // deep or large nesting) inside frames the adapter ignores, and failing a
    // finished lane over them is wrong. The resource limit is the line bound
    // (`MAX_LINE_BYTES`, enforced in `observe` before a line reaches here);
    // `serde_json` adds its own recursion limit. Typed fields still refuse values
    // they cannot hold.
    serde_json::from_slice(bytes).map_err(|error| {
        if error.classify() == serde_json::error::Category::Eof {
            ProviderError::IncompleteFrame
        } else {
            ProviderError::Malformed
        }
    })
}
