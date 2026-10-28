//! The single fixture-backed hook catalogue supplied to config by composition.
use crate::{ClaudeHook, GitHook, Hook};
use provenant_config::HookRegistration;
use provenant_records::Harness;

pub const PRE_TOOL_USE: &str = "pre-tool-use";

/// Composed delivery capabilities. Records owns the Fabric mail fallback;
/// provider hook support belongs to this catalogue beside its registration.
pub fn delivery_capabilities() -> Vec<provenant_records::inbox::DeliveryCapability> {
    let mut capabilities = provenant_records::inbox::delivery_capabilities();
    for capability in &mut capabilities {
        // Library readiness only; live injection requires the hook adapter.
        capability.local_offer_reader = capability.provider == Harness::Claude;
    }
    capabilities.push(provenant_records::inbox::DeliveryCapability {
        provider: Harness::Claude,
        transport: provenant_records::inbox::AnswerTransport::ClaudeHook,
        live_injection: true,
        local_offer_reader: true,
    });
    capabilities
}

pub const PRE_COMPACT: &str = "pre-compact";
pub const COMPACT: &str = "compact";
pub const CHAIR_PROMPT: &str = "chair-prompt";
pub const CHAIR_START: &str = "chair-start";
pub const SESSION_START: &str = "session-start";
pub const STOP: &str = "stop";
pub const WORKTREE_CREATE: &str = "worktree-create";
pub const WORKTREE_REMOVE: &str = "worktree-remove";
pub const PRE_PUSH: &str = "pre-push";
#[derive(Clone, Copy)]
pub enum Handler {
    Guard,
    PreCompact,
    Compact,
    PrePush,
    ChairPrompt,
    ChairStart,
    SessionStart,
    Stop,
    WorktreeCreate,
    WorktreeRemove,
}
pub struct HookSpec {
    pub registration: HookRegistration,
    pub handler: Handler,
    pub fixture: Option<&'static str>,
}
pub static HOOK_SPECS: &[HookSpec] = &[
    HookSpec {
        registration: provenant_config::CLAUDE_HOOKS[0],
        handler: Handler::Guard,
        fixture: Some("pre-tool-use.json"),
    },
    HookSpec {
        registration: provenant_config::CLAUDE_HOOKS[1],
        handler: Handler::PreCompact,
        fixture: Some("pre-compact.json"),
    },
    HookSpec {
        registration: provenant_config::CLAUDE_HOOKS[2],
        handler: Handler::Compact,
        fixture: Some("compact.json"),
    },
    HookSpec {
        registration: HookRegistration {
            harness: None,
            event: PRE_PUSH,
            matcher: None,
            timeout: None,
            command: &["hook", "git", PRE_PUSH],
        },
        handler: Handler::PrePush,
        fixture: None,
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "UserPromptSubmit",
            matcher: None,
            timeout: Some(10),
            command: &["hook", "claude", CHAIR_PROMPT],
        },
        handler: Handler::ChairPrompt,
        fixture: None,
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "SessionStart",
            matcher: None,
            timeout: Some(5),
            command: &["hook", "claude", CHAIR_START],
        },
        handler: Handler::ChairStart,
        fixture: None,
    },
    // Not in `CLAUDE_HOOKS`: confined lanes get only the guard and checkpoint
    // hooks, and these act on the chair's own session and project.
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "SessionStart",
            matcher: Some("startup|resume|clear"),
            timeout: Some(5),
            command: &["hook", "claude", SESSION_START],
        },
        handler: Handler::SessionStart,
        fixture: None,
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "Stop",
            matcher: None,
            timeout: Some(5),
            command: &["hook", "claude", STOP],
        },
        handler: Handler::Stop,
        fixture: None,
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "WorktreeCreate",
            matcher: None,
            timeout: Some(60),
            command: &["hook", "claude", WORKTREE_CREATE],
        },
        handler: Handler::WorktreeCreate,
        fixture: None,
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "WorktreeRemove",
            matcher: None,
            timeout: Some(60),
            command: &["hook", "claude", WORKTREE_REMOVE],
        },
        handler: Handler::WorktreeRemove,
        fixture: None,
    },
];
impl Hook {
    pub fn spec(&self) -> &'static HookSpec {
        let index = match self {
            Self::Claude(ClaudeHook::PreToolUse) => 0,
            Self::Claude(ClaudeHook::PreCompact) => 1,
            Self::Claude(ClaudeHook::Compact) => 2,
            Self::Git(GitHook::PrePush { .. }) => 3,
            Self::Claude(ClaudeHook::ChairPrompt) => 4,
            Self::Claude(ClaudeHook::ChairStart) => 5,
            Self::Claude(ClaudeHook::SessionStart) => 6,
            Self::Claude(ClaudeHook::Stop) => 7,
            Self::Claude(ClaudeHook::WorktreeCreate) => 8,
            Self::Claude(ClaudeHook::WorktreeRemove) => 9,
        };
        // These exhaustive grammar variants correspond to the static rows.
        &HOOK_SPECS[index]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use provenant_records::{ADAPTER_SPECS, inbox::AnswerTransport};

    #[test]
    fn composed_matrix_reports_claude_hook_readiness() {
        let capabilities = delivery_capabilities();
        for spec in ADAPTER_SPECS {
            let mail: Vec<_> = capabilities
                .iter()
                .filter(|cap| {
                    cap.provider == spec.id && cap.transport == AnswerTransport::FabricMail
                })
                .collect();
            assert_eq!(mail.len(), 1, "{} Fabric-mail capability", spec.name);
            assert_eq!(mail[0].local_offer_reader, spec.id == Harness::Claude);
            assert!(!mail[0].live_injection);
        }
        let hooks: Vec<_> = capabilities
            .iter()
            .filter(|cap| cap.transport == AnswerTransport::ClaudeHook)
            .collect();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].provider, Harness::Claude);
        assert!(hooks[0].local_offer_reader && hooks[0].live_injection);
    }
}
