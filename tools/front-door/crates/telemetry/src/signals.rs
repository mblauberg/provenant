//! Text-free observations and explicitly inferred reply labels.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    UserDirection,
    UserStatusRequest,
    LaneNotification,
    CiNotification,
    ToolError,
    ProvenantCliError,
    Continuation,
    Mixed,
    #[default]
    Unknown,
}

/// Observed structural events, never the content that caused the label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Event {
    UserMessage,
    ToolResult,
    ToolError,
    ProvenantCliError,
    LaneNotification,
    CiNotification,
    Continuation,
    Compaction,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyContext {
    pub recorded: Vec<Event>,
    /// A label derived from recorded structural events; not a recorded cause.
    pub inferred: Trigger,
    pub compactions: u64,
    #[serde(default)]
    pub unknown_evidence: bool,
}

impl ReplyContext {
    pub(crate) fn observe(&mut self, event: Event) {
        if event == Event::Compaction {
            self.compactions = self.compactions.saturating_add(1);
        }
        if !self.recorded.contains(&event) {
            self.recorded.push(event);
        }
        let label = match event {
            Event::UserMessage => Trigger::UserDirection,
            Event::ToolResult => Trigger::Continuation,
            Event::ToolError => Trigger::ToolError,
            Event::ProvenantCliError => Trigger::ProvenantCliError,
            Event::LaneNotification => Trigger::LaneNotification,
            Event::CiNotification => Trigger::CiNotification,
            Event::Continuation | Event::Compaction => Trigger::Continuation,
        };
        self.infer(label);
    }

    fn infer(&mut self, label: Trigger) {
        if label == Trigger::Unknown {
            self.unknown_evidence = true;
            if self.inferred != Trigger::Unknown {
                self.inferred = Trigger::Mixed;
            }
            return;
        }
        if self.unknown_evidence {
            self.inferred = Trigger::Mixed;
            return;
        }
        self.inferred = match self.inferred {
            Trigger::Unknown => label,
            previous if previous == label => previous,
            _ => Trigger::Mixed,
        };
    }

    /// Inspect native Claude event structure transiently. Only enums/counts leave
    /// this function. Unrecognised system records do not invent a trigger.
    pub(crate) fn observe_native(&mut self, value: &serde_json::Value) {
        match value["type"].as_str() {
            Some("user") => {
                let content = &value["message"]["content"];
                if let Some(text) = content.as_str() {
                    self.observe_text(text);
                }
                if let Some(parts) = content.as_array() {
                    for part in parts {
                        match part["type"].as_str() {
                            Some("text") => {
                                if let Some(text) = part["text"].as_str() {
                                    self.observe_text(text);
                                }
                            }
                            Some("tool_result") => self.observe_result(part),
                            _ => {}
                        }
                    }
                }
            }
            Some("system") => match value["subtype"].as_str() {
                Some("compact_boundary") => self.observe(Event::Compaction),
                Some("lane_notification") => self.observe(Event::LaneNotification),
                Some("ci_notification") => self.observe(Event::CiNotification),
                Some("provenant_cli_error") => self.observe(Event::ProvenantCliError),
                Some("continuation") => self.observe(Event::Continuation),
                _ => {}
            },
            _ => {}
        }
    }

    fn observe_result(&mut self, part: &serde_json::Value) {
        let event = if part["is_error"] == true {
            Event::ToolError
        } else {
            Event::ToolResult
        };
        // Recognise the Provenant JSON command envelope transiently. This is
        // inference from a tool's output, not a recorded CLI-error event.
        let cli_error = part["content"]
            .as_str()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
            .is_some_and(|value| {
                value["schema_version"] == 1
                    && value["command"].is_string()
                    && value.get("data").is_some()
                    && value["error"]["code"].is_string()
                    && matches!(
                        value["status"].as_str(),
                        Some(
                            "failed" | "refused" | "invalid" | "error" | "unavailable" | "timeout"
                        )
                    )
            });
        if cli_error {
            if !self.recorded.contains(&event) {
                self.recorded.push(event);
            }
            self.infer(Trigger::ProvenantCliError);
        } else {
            self.observe(event);
        }
    }

    fn observe_text(&mut self, text: &str) {
        // A deliberately narrow lexical inference, kept separate from the raw
        // event. Questions outside this set remain ordinary user messages.
        let status = matches!(
            text.trim().to_ascii_lowercase().as_str(),
            "status?"
                | "status"
                | "what is the status?"
                | "what's the status?"
                | "any update?"
                | "any updates?"
        );
        if !self.recorded.contains(&Event::UserMessage) {
            self.recorded.push(Event::UserMessage);
        }
        let trimmed = text.trim();
        let label = if trimmed.starts_with("<task-notification>") {
            Trigger::LaneNotification
        } else if trimmed.starts_with("<ci-notification>") {
            Trigger::CiNotification
        } else if trimmed.starts_with('<') {
            Trigger::Unknown
        } else if status {
            Trigger::UserStatusRequest
        } else {
            Trigger::UserDirection
        };
        self.infer(label);
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Thinking {
    pub replies: u64,
    pub reasoning_known: u64,
    pub reasoning_unknown: u64,
    /// Provider-reported reasoning tokens, never output or effort proxies.
    pub reasoning_tokens: u64,
    pub output_tokens_proxy: u64,
    pub compactions: u64,
}

/// Aggregate all replies, including the unknown trigger/reasoning denominators.
pub fn thinking(rows: &[crate::ingest::Row]) -> BTreeMap<Trigger, Thinking> {
    let mut groups = BTreeMap::<Trigger, Thinking>::new();
    for row in rows {
        let trigger = row
            .reply
            .as_ref()
            .map_or(Trigger::Unknown, |reply| reply.inferred);
        let group = groups.entry(trigger).or_default();
        group.replies += 1;
        group.output_tokens_proxy = group.output_tokens_proxy.saturating_add(row.output);
        group.compactions += row.reply.as_ref().map_or(0, |reply| reply.compactions);
        if let Some(tokens) = row.reasoning_tokens {
            group.reasoning_known += 1;
            group.reasoning_tokens = group.reasoning_tokens.saturating_add(tokens);
        } else {
            group.reasoning_unknown += 1;
        }
    }
    groups
}
