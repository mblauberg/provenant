//! Small, owner-issued invalidation signal for local state readers.
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "changes.json";

#[derive(Clone, Copy, Debug)]
pub enum Scope {
    Inbox,
    Board,
    Chair,
    Quota,
    Configuration,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Generations {
    pub epoch: Option<u64>,
    pub inbox: Option<u64>,
    pub board: Option<u64>,
    pub chair: Option<u64>,
    pub quota: Option<u64>,
    pub configuration: Option<u64>,
}
