//! Harness compatibility assessment.
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompatibilityStatus {
    Supported,
    Unsupported,
    Broken,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Assessment {
    pub adapter: String,
    pub version: Option<String>,
    pub status: CompatibilityStatus,
    pub reason: Option<String>,
    pub issue: Option<u64>,
    pub recommended: Option<String>,
    pub source: provenant_records::Layer,
}
pub struct Table;
impl Table {
    pub fn bundled() -> Self {
        Self
    }
    pub fn effective(
        _state: &Path,
        _project: Option<&Path>,
    ) -> (Self, Vec<provenant_config::overlays::OverlayWarning>) {
        (Self::bundled(), vec![])
    }
    pub fn assess(&self, adapter: &str, version: Option<&str>) -> Assessment {
        Assessment {
            adapter: adapter.into(),
            version: version.map(str::to_owned),
            status: CompatibilityStatus::Unknown,
            reason: None,
            issue: None,
            recommended: None,
            source: provenant_records::Layer::Default,
        }
    }
}
/// The policy code for a status; None for Supported and Unknown.
pub fn policy_code(_status: CompatibilityStatus) -> Option<&'static str> {
    None
}
