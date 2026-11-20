//! Frozen lane build context passed to shims and in-lane verification.

use super::BaseRef;
use crate::build::GitBinding;
use provenant_config::{BuildPort, PortSource};
use serde::{Deserialize, Serialize};
use std::{ffi::OsString, path::PathBuf};

/// Schema 1 of `PROVENANT_BUILD_CONTEXT`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BuildContext {
    pub schema: u32,
    pub project: PathBuf,
    pub project_key: String,
    pub worktree: PathBuf,
    pub attempt: Option<String>,
    pub sandboxed: bool,
    pub git: Option<GitBinding>,
    pub port: BuildPort,
    pub strategy_source: PortSource,
    pub host_home: PathBuf,
    pub rustup_home: Option<PathBuf>,
    pub cargo_home: PathBuf,
    pub host_path: OsString,
    pub verify_base: Option<BaseRef>,
}

impl BuildContext {
    /// Decode and validate the versioned context payload.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let context: Self = serde_json::from_str(json)?;
        if context.schema != 1 {
            return Err(<serde_json::Error as serde::de::Error>::custom(format!(
                "unsupported build context schema {}",
                context.schema
            )));
        }
        Ok(context)
    }

    /// Serialize the lane context without changing its JSON wire shape.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}
