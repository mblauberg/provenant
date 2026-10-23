//! Cycle-breaking values for agy staging. Filesystem effects belong to the later port unit.
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const PRIVATE_BIN: &str = ".gemini/antigravity-cli/bin";
pub const UNLINKED: [&str; 4] = [
    "agentapi",
    "antigravity-oauth-token",
    "mcp_oauth_tokens.json",
    "oauth_creds.json",
];
pub const RENAMED_STATE: [&str; 5] = [
    "history.jsonl",
    "jetbox_summaries_proto.pb",
    "jetski_state.pbtxt",
    "last_check.timestamp",
    "installation_id",
];
pub const STATE_COPY_MAX: u64 = 64 * 1024 * 1024;
pub const DATABASE: &str = "conversation_summaries.db";
pub const COPY_BACK_LOCK: &str = ".provenant-copy-back.lock";
pub const COPY_BACK_WAIT_SECONDS: u64 = 10;

/// Identity at staging. Absent entries remain distinct from empty files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_ns: i128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgyHome {
    pub path: PathBuf,
    pub name: String,
    pub before: BTreeMap<String, Option<StateIdentity>>,
}

/// Borrowed plan projection avoids the provider_exec/agy_home import cycle.
#[derive(Clone, Copy, Debug)]
pub struct HomePlan<'a> {
    pub adapter: &'a str,
    pub confinement: Option<&'a str>,
    pub sandbox: &'a str,
}

impl HomePlan<'_> {
    pub fn stages_home(self) -> bool {
        self.adapter == "agy" && self.confinement == Some("sandbox-exec")
    }

    pub fn needs_ps_shim(self, platform: &str) -> bool {
        platform == "darwin"
            && ((self.adapter == "codex" && self.sandbox != "full")
                || self.confinement == Some("sandbox-exec"))
    }
}

/// Token and executable prefixes are excluded even when they have a suffix.
pub fn links_entry(name: &str) -> bool {
    !UNLINKED.iter().any(|prefix| name.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_confined_agy_stages_home() {
        for adapter in ["agy", "codex", "claude"] {
            for confinement in [None, Some("native"), Some("sandbox-exec")] {
                let plan = HomePlan {
                    adapter,
                    confinement,
                    sandbox: "full",
                };
                assert_eq!(
                    plan.stages_home(),
                    adapter == "agy" && confinement == Some("sandbox-exec")
                );
            }
        }
    }

    #[test]
    fn shim_is_required_for_all_macos_sandboxes() {
        for adapter in ["agy", "codex", "claude"] {
            for sandbox in ["full", "read_only", "workspace_write"] {
                for confinement in [None, Some("sandbox-exec")] {
                    let plan = HomePlan {
                        adapter,
                        confinement,
                        sandbox,
                    };
                    assert!(!plan.needs_ps_shim("linux"));
                    assert_eq!(
                        plan.needs_ps_shim("darwin"),
                        adapter == "codex" && sandbox != "full"
                            || confinement == Some("sandbox-exec")
                    );
                }
            }
        }
    }

    #[test]
    fn overlay_excludes_secret_and_shim_prefixes() {
        for prefix in UNLINKED {
            assert!(!links_entry(prefix));
            assert!(!links_entry(&format!("{prefix}.backup")));
        }
        for name in ["history.jsonl", DATABASE, "dangling-link", "settings.json"] {
            assert!(links_entry(name));
        }
    }
}
