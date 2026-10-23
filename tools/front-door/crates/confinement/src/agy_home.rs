//! Cycle-breaking values for agy staging. Filesystem effects belong to the later port unit.
use std::collections::BTreeMap;
use std::path::PathBuf;

mod unit_01;
pub use unit_01::links_entry;

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
pub const OVERLAY_NAME_PREFIX: &str = "agy-home-";
pub const OVERLAY_NAME_SUFFIX_HEX_LEN: usize = 12;
pub const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
pub const PRIVATE_FILE_MODE: u32 = 0o600;
pub const STATE_FILE_MODE: u32 = 0o644;
pub const EXECUTABLE_MODE: u32 = 0o755;

/// Identity at staging. Absent entries remain distinct from empty files.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateIdentity {
    pub device: u64,
    pub inode: u64,
    pub size: u64,
    pub modified_ns: i64,
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
