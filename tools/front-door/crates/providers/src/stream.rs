use crate::ProviderError;
use serde::Serialize;
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ProviderEvent {
    Text(String),
    Session(String),
    Model(String),
    Usage { input: u64, output: u64 },
    Refusal,
    Error,
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
}
pub(crate) type Decoder = fn(&[u8]) -> Result<Vec<ProviderEvent>, ProviderError>;
const MAX_LINE_BYTES: usize = 1_048_576;
/// Bounded incremental framing and evidence reduction. Only decoded protocol
/// positions can affect identity or completion; prose is never searched.
pub struct StreamState {
    adapter: &'static str,
    decoder: Decoder,
    pending: Vec<u8>,
    session: Option<String>,
    model: Option<String>,
    model_conflict: bool,
    terminal: bool,
    error: bool,
    refusal: bool,
    malformed: bool,
}
impl StreamState {
    pub(crate) fn new(adapter: &'static str, decoder: Decoder) -> Self {
        Self {
            adapter,
            decoder,
            pending: Vec::new(),
            session: None,
            model: None,
            model_conflict: false,
            terminal: false,
            error: false,
            refusal: false,
            malformed: false,
        }
    }
    pub(crate) fn check_adapter(&self, adapter: &str) -> Result<(), ProviderError> {
        if self.adapter == adapter {
            Ok(())
        } else {
            Err(ProviderError::IdentityMismatch)
        }
    }
    pub(crate) fn authentication_required(&self) -> bool {
        self.refusal
    }
    pub(crate) fn observe(&mut self, bytes: &[u8]) -> Result<Vec<ProviderEvent>, ProviderError> {
        if self.malformed {
            return Err(ProviderError::Malformed);
        }
        let mut events = Vec::new();
        // Per-call output and each retained line are bounded independently.
        if bytes.len() > MAX_LINE_BYTES {
            self.malformed = true;
            return Err(ProviderError::Overflow);
        }
        for byte in bytes {
            if *byte == b'\n' {
                let line = std::mem::take(&mut self.pending);
                if line.is_empty() {
                    continue;
                }
                match self.line(&line) {
                    Ok(decoded) => events.extend(decoded),
                    Err(error) => {
                        self.malformed = true;
                        return Err(error);
                    }
                }
            } else {
                if self.pending.len() == MAX_LINE_BYTES {
                    self.malformed = true;
                    self.pending.clear();
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
                    if model.is_empty() {
                        return Err(ProviderError::Malformed);
                    }
                    if self.model.as_ref().is_some_and(|old| old != model) {
                        self.model_conflict = true;
                    }
                    self.model = Some(model.clone());
                }
                ProviderEvent::Terminal => self.terminal = true,
                ProviderEvent::Error => self.error = true,
                ProviderEvent::Refusal => self.refusal = true,
                _ => {}
            }
        }
        Ok(events)
    }
    pub(crate) fn finish(
        mut self,
        exit: ExitObservation,
    ) -> Result<ProviderOutcome, ProviderError> {
        let mut final_events = Vec::new();
        if !self.pending.is_empty() && !self.malformed {
            let line = std::mem::take(&mut self.pending);
            match self.line(&line) {
                Ok(events) => final_events = events,
                Err(_) => self.malformed = true,
            }
        }
        let status = if self.malformed {
            OutcomeStatus::Malformed
        } else if self.refusal {
            OutcomeStatus::Refused
        } else if self.error {
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
            exit,
            final_events,
        })
    }
}
pub(crate) fn wire<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, ProviderError> {
    provenant_records::decode(bytes, provenant_records::Limits::CONFIG)
        .map_err(|_| ProviderError::Malformed)
}
