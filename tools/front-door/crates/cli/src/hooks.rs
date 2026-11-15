//! Provider hook commands resolved from the adapter capability declaration.
use crate::{GitHook, Hook};
use provenant_records::{ADAPTER_SPECS, Harness, HookAction, HookEvent, HookProtocol};

pub const PRE_TOOL_USE: &str = "pre-tool-use";
pub const PRE_COMPACT: &str = "pre-compact";
pub const COMPACT: &str = "compact";
pub const CHAIR_PROMPT: &str = "chair-prompt";
pub const CHAIR_START: &str = "chair-start";
pub const CHAIR_STOP: &str = "chair-stop";
pub const SESSION_START: &str = "session-start";
pub const STOP: &str = "stop";
pub const WORKTREE_CREATE: &str = "worktree-create";
pub const WORKTREE_REMOVE: &str = "worktree-remove";
pub const PRE_PUSH: &str = "pre-push";

/// Composed delivery capabilities. The provider hook transport is available
/// only when the adapter declaration supports the current guard response.
pub fn delivery_capabilities() -> Vec<provenant_records::inbox::DeliveryCapability> {
    use provenant_records::inbox::{AnswerTransport, DeliveryCapability};

    let mut capabilities = provenant_records::inbox::delivery_capabilities();
    for capability in &mut capabilities {
        capability.local_offer_reader = capability
            .provider
            .spec()
            .install
            .is_some_and(|install| install.answer_delivery.is_supported());
    }
    for spec in ADAPTER_SPECS {
        let Some(install) = spec.install else {
            continue;
        };
        if install.answer_delivery.is_supported() {
            capabilities.push(DeliveryCapability {
                provider: spec.id,
                transport: AnswerTransport::ProviderHook,
                live_injection: true,
                local_offer_reader: true,
            });
        }
    }
    capabilities
}

#[derive(Clone, Copy)]
pub enum Handler {
    Guard,
    PreCompact,
    Compact,
    PrePush,
    ChairPrompt,
    ChairStart,
    ChairStop,
    SessionStart,
    Stop,
    WorktreeCreate,
    WorktreeRemove,
}

#[derive(Clone, Copy)]
pub struct ResolvedHook {
    pub handler: Handler,
    pub client: Option<Harness>,
    pub action: Option<HookAction>,
    pub event: &'static str,
    pub protocol: Option<HookProtocol>,
}

fn handler(action: HookAction) -> Handler {
    match action {
        HookAction::PreToolUse => Handler::Guard,
        HookAction::PreCompact => Handler::PreCompact,
        HookAction::Compact => Handler::Compact,
        HookAction::ChairPrompt => Handler::ChairPrompt,
        HookAction::ChairStart => Handler::ChairStart,
        HookAction::ChairStop => Handler::ChairStop,
        HookAction::SessionStart => Handler::SessionStart,
        HookAction::Stop => Handler::Stop,
        HookAction::WorktreeCreate => Handler::WorktreeCreate,
        HookAction::WorktreeRemove => Handler::WorktreeRemove,
    }
}

fn resolve_provider(name: &str, command: &str) -> Option<ResolvedHook> {
    let spec = ADAPTER_SPECS.iter().find(|spec| spec.name == name)?;
    let install = spec.install?;
    let requested = HookAction::ALL
        .iter()
        .copied()
        .find(|action| action.command() == command)?;
    if requested == HookAction::PreToolUse && !install.answer_delivery.is_supported() {
        return None;
    }
    let HookEvent::Supported { event, .. } =
        install.hook_events.iter().find_map(|event| match event {
            HookEvent::Supported { action, .. } if *action == requested => Some(event),
            _ => None,
        })?
    else {
        return None;
    };
    Some(ResolvedHook {
        handler: handler(requested),
        client: Some(spec.id),
        action: Some(requested),
        event: *event,
        protocol: install.hook_protocol,
    })
}

impl Hook {
    /// Resolve the CLI command against the same declaration used by config and
    /// native hook rendering. Unrecognised values abstain at the process edge.
    pub fn spec(&self) -> Option<ResolvedHook> {
        match self {
            Self::Git(GitHook::PrePush { .. }) => Some(ResolvedHook {
                handler: Handler::PrePush,
                client: None,
                action: None,
                event: PRE_PUSH,
                protocol: None,
            }),
            Self::Provider(values) => {
                let [adapter, action] = values.as_slice() else {
                    return None;
                };
                resolve_provider(adapter.to_str()?, action.to_str()?)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::inbox::AnswerTransport;
    use std::ffi::OsString;

    #[test]
    fn delivery_matrix_projects_declared_answer_support() {
        let capabilities = delivery_capabilities();
        for spec in ADAPTER_SPECS {
            let supported = spec
                .install
                .is_some_and(|install| install.answer_delivery.is_supported());
            let mail: Vec<_> = capabilities
                .iter()
                .filter(|cap| {
                    cap.provider == spec.id && cap.transport == AnswerTransport::FabricMail
                })
                .collect();
            assert_eq!(mail.len(), 1, "{} mail capability", spec.name);
            assert_eq!(mail[0].local_offer_reader, supported);
            assert!(!mail[0].live_injection);

            let hooks: Vec<_> = capabilities
                .iter()
                .filter(|cap| {
                    cap.provider == spec.id && cap.transport == AnswerTransport::ProviderHook
                })
                .collect();
            assert_eq!(
                hooks.len(),
                usize::from(supported),
                "{} hook capability",
                spec.name
            );
            for hook in hooks {
                assert!(hook.local_offer_reader && hook.live_injection);
            }
        }
    }

    #[test]
    fn provider_commands_resolve_to_the_declared_native_event() {
        for spec in ADAPTER_SPECS {
            let Some(install) = spec.install else {
                continue;
            };
            for event in install.hook_events {
                let HookEvent::Supported { action, event, .. } = event else {
                    continue;
                };
                let hook = Hook::Provider(vec![
                    OsString::from(spec.name),
                    OsString::from(action.command()),
                ])
                .spec()
                .expect("declared provider action resolves");
                assert_eq!(hook.client, Some(spec.id));
                assert_eq!(hook.action, Some(*action));
                assert_eq!(hook.event, *event);
                assert_eq!(hook.protocol, install.hook_protocol);
            }
        }
    }

    #[test]
    fn unknown_provider_and_unsupported_action_abstain() {
        assert!(
            Hook::Provider(vec![
                OsString::from("unknown"),
                OsString::from(PRE_TOOL_USE)
            ])
            .spec()
            .is_none()
        );
        assert!(
            Hook::Provider(vec![OsString::from("agy"), OsString::from(COMPACT)])
                .spec()
                .is_none()
        );
        assert!(
            Hook::Provider(vec![OsString::from("codex")])
                .spec()
                .is_none()
        );
    }
}
