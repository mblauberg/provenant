//! Provider control capabilities and the receipt for a queued control message.
//! The matrix is data read from each adapter's declared row; nothing here
//! confers workflow authority.
use crate::{Harness, inbox::AnswerTransport};
use provenant_core::{MessageId, OperationId};
use serde::{Deserialize, Serialize};

/// Longest steer or chair message text, in bytes.
pub const CONTROL_TEXT_BYTES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchKind {
    /// A dispatched worker attempt.
    Lane,
    /// An interactive chair session.
    Chair,
}

/// What a provider can do with a control message, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlSupport {
    /// Injected at a tool boundary of the live session.
    LiveInjection,
    /// Left in Fabric mail for the session to read.
    MailOnly,
    /// No live path; only `resume` continues the work.
    ContinuationOnly,
    Unsupported,
}
impl ControlSupport {
    /// Whether a control message can reach the work without a new launch.
    pub fn delivers(self) -> bool {
        matches!(self, Self::LiveInjection | Self::MailOnly)
    }
    pub fn reason(self) -> &'static str {
        match self {
            Self::LiveInjection => "injected at a tool boundary of the live session",
            Self::MailOnly => "delivered as Fabric mail for the session to read",
            Self::ContinuationOnly => "no live control path; use resume to continue",
            Self::Unsupported => "the adapter offers no control path for this launch",
        }
    }
}

/// Declared control support of one adapter, per launch kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControlCapabilities {
    pub lane: ControlSupport,
    pub chair: ControlSupport,
}
impl ControlCapabilities {
    pub fn for_launch(&self, launch: LaunchKind) -> ControlSupport {
        match launch {
            LaunchKind::Lane => self.lane,
            LaunchKind::Chair => self.chair,
        }
    }
}

/// Existing actions listed beside the control path so a console can disable
/// the ones a provider cannot take, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionSupport {
    pub action: &'static str,
    pub supported: bool,
    pub reason: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControlCapability {
    pub provider: Harness,
    pub launch: LaunchKind,
    pub support: ControlSupport,
    pub transport: Option<AnswerTransport>,
    pub reason: &'static str,
    pub actions: Vec<ActionSupport>,
}

fn action(action: &'static str, supported: bool, reason: &'static str) -> ActionSupport {
    ActionSupport {
        action,
        supported,
        reason,
    }
}

/// The published matrix: one row per adapter and launch kind.
pub fn control_capabilities() -> Vec<ControlCapability> {
    let mut rows = Vec::new();
    for spec in crate::ADAPTER_SPECS {
        for launch in [LaunchKind::Lane, LaunchKind::Chair] {
            let support = spec.launch.control.for_launch(launch);
            let lane = launch == LaunchKind::Lane;
            let owned = |ok: bool, no: &'static str| if ok { "supported" } else { no };
            rows.push(ControlCapability {
                provider: spec.id,
                launch,
                support,
                transport: support.delivers().then_some(AnswerTransport::FabricMail),
                reason: support.reason(),
                actions: vec![
                    action("cancel", lane, owned(lane, "applies to lanes only")),
                    action(
                        "resume",
                        lane && spec.launch.resume,
                        owned(
                            lane && spec.launch.resume,
                            "the adapter has no provider continuation",
                        ),
                    ),
                    action(
                        "land",
                        lane && spec.launch.write,
                        owned(lane && spec.launch.write, "only worktree writers land"),
                    ),
                ],
            });
        }
    }
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Steer,
    ChairMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlState {
    Queued,
    Offered,
    Acknowledged,
    Unsupported,
}

/// Receipt for one control request. `unsupported` records no side effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlReceipt {
    #[serde(flatten)]
    pub extra: crate::ExtraFields,
    pub operation_id: OperationId,
    pub control: ControlKind,
    /// The attempt ID or exact `HOST/PROJECT/SESSION` target.
    pub target: String,
    pub launch: LaunchKind,
    pub support: ControlSupport,
    pub transport: Option<AnswerTransport>,
    pub state: ControlState,
    pub reason: String,
    pub message_id: Option<MessageId>,
}

/// Stable refusal code for an unsupported control receipt. Every edge that
/// reports one, CLI or MCP, uses this code and `ControlReceipt::refusal_message`.
pub const CONTROL_UNSUPPORTED: &str = "control_unsupported";

impl ControlReceipt {
    pub fn refusal_message(&self) -> String {
        format!("control unsupported: {}", self.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_has_one_row_per_adapter_and_launch_kind() {
        let rows = control_capabilities();
        assert_eq!(rows.len(), crate::ADAPTER_SPECS.len() * 2);
        for spec in crate::ADAPTER_SPECS {
            for launch in [LaunchKind::Lane, LaunchKind::Chair] {
                assert_eq!(
                    rows.iter()
                        .filter(|row| row.provider == spec.id && row.launch == launch)
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn lanes_declare_no_delivery_until_a_lane_transport_exists() {
        for row in control_capabilities() {
            assert_eq!(row.transport.is_some(), row.support.delivers());
            if row.launch == LaunchKind::Lane {
                assert!(!row.support.delivers(), "{:?}", row.provider);
            }
        }
    }

    #[test]
    fn resumable_adapters_are_continuation_only_and_commands_are_unsupported() {
        for row in control_capabilities()
            .iter()
            .filter(|row| row.launch == LaunchKind::Lane)
        {
            let resumable = row.provider.spec().launch.resume;
            assert_eq!(
                row.support,
                if resumable {
                    ControlSupport::ContinuationOnly
                } else {
                    ControlSupport::Unsupported
                }
            );
            let resume = row.actions.iter().find(|a| a.action == "resume").unwrap();
            assert_eq!(resume.supported, resumable);
        }
    }

    #[test]
    fn matrix_serialises_stable_names() {
        let row = control_capabilities().remove(0);
        let value = serde_json::to_value(&row).unwrap();
        assert_eq!(value["provider"], "claude");
        assert_eq!(value["launch"], "lane");
        assert_eq!(value["support"], "continuation_only");
        let names: Vec<_> = value["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["action"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["cancel", "resume", "land"]);
    }
}
