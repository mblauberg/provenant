//! The single fixture-backed hook catalogue supplied to config by composition.
use crate::{ClaudeHook, GitHook, Hook};
use provenant_config::HookRegistration;
use provenant_records::Harness;

pub const PRE_TOOL_USE: &str = "pre-tool-use";
pub const PRE_COMPACT: &str = "pre-compact";
pub const COMPACT: &str = "compact";
pub const PRE_PUSH: &str = "pre-push";
#[derive(Clone, Copy)]
pub enum Handler {
    Guard,
    PreCompact,
    Compact,
    PrePush,
}
pub struct HookSpec {
    pub registration: HookRegistration,
    pub handler: Handler,
    pub fixture: Option<&'static str>,
}
pub static HOOK_SPECS: &[HookSpec] = &[
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "PreToolUse",
            matcher: Some("Bash"),
            timeout: None,
            command: &["hook", "claude", PRE_TOOL_USE],
        },
        handler: Handler::Guard,
        fixture: Some("pre-tool-use.json"),
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "PreCompact",
            matcher: None,
            timeout: Some(5),
            command: &["hook", "claude", PRE_COMPACT],
        },
        handler: Handler::PreCompact,
        fixture: Some("pre-compact.json"),
    },
    HookSpec {
        registration: HookRegistration {
            harness: Some(Harness::Claude),
            event: "SessionStart",
            matcher: Some("compact"),
            timeout: Some(5),
            command: &["hook", "claude", COMPACT],
        },
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
];
impl Hook {
    pub fn spec(&self) -> &'static HookSpec {
        let index = match self {
            Self::Claude(ClaudeHook::PreToolUse) => 0,
            Self::Claude(ClaudeHook::PreCompact) => 1,
            Self::Claude(ClaudeHook::Compact) => 2,
            Self::Git(GitHook::PrePush { .. }) => 3,
        };
        // These four exhaustive grammar variants correspond to the static rows.
        &HOOK_SPECS[index]
    }
}
