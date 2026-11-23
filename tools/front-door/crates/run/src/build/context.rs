//! Frozen host-side build context for one slot job.

use super::BaseRef;
use crate::build::GitBinding;
use provenant_config::BuildPort;
use provenant_records::Layer;
use std::{ffi::OsString, path::PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub struct BuildContext {
    pub project: PathBuf,
    pub project_key: String,
    pub worktree: PathBuf,
    pub attempt: Option<String>,
    pub sandboxed: bool,
    pub git: Option<GitBinding>,
    pub port: BuildPort,
    pub strategy_source: Layer,
    pub host_home: PathBuf,
    pub rustup_home: Option<PathBuf>,
    pub cargo_home: PathBuf,
    pub host_path: OsString,
    pub verify_base: Option<BaseRef>,
}
