//! Lane Cargo target knob (`lanes.cargo_target`).
//!
//! Controls where a lane's Cargo output is directed:
//! - `private` (default): the worktree's own target in its lane cache,
//!   `~/Library/Caches/provenant-lane-cargo/<digest>/target`, seeded warm by a
//!   copy-on-write clone of the project's host seed generation with every dep-info
//!   aged to the epoch under any build layout (`cargo_seed`). `provenant worktree
//!   seed-target` seeds the same directory for a checkout outside a lane. Removed with
//!   the worktree.
//! - `project`: the writer worktree's own `target/` beside the workspace manifest
//!   selected by the verify port. Cold unless a person already built there.
//!   Read-only lanes and non-Cargo projects use the lane cache.
//! - Absolute path: used as given.
//!
//! Every mode keeps one target per worktree unless an operator points several at one
//! absolute path. Cargo hashes a workspace member's units by its workspace-relative
//! path and decides freshness by comparing source mtimes with the last build's
//! dep-info, so a target shared by worktrees of one repository serves one branch's
//! artifacts to another whose sources are older (#3561). A seed clone is safe
//! because its aged dep-info makes every member rebuild once from this worktree's
//! sources, while registry crates, whose dep-info lists no files, stay fresh.
//!
//! The value and its source appear in the lane preamble and in `provenant config show`.

use provenant_records::{LimitSource, run::HarnessProfile};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The registry knob.
pub const KNOB: &str = "lanes.cargo_target";

/// Lint-only target inside the project target. Cargo locks each target
/// directory, so lints do not queue behind builds; `target/` ignores cover it.
const PROJECT_LINT_DIR: &str = "provenant-lint";

/// Target location mode configured for a lane.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetSetting {
    Project,
    #[default]
    Private,
    Custom(PathBuf),
}

impl CargoTargetSetting {
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed == "project" {
            Some(Self::Project)
        } else if trimmed == "private" {
            Some(Self::Private)
        } else if Path::new(trimmed).is_absolute() {
            Some(Self::Custom(PathBuf::from(trimmed)))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::Project => "project",
            Self::Private => "private",
            Self::Custom(path) => path.to_str().unwrap_or(""),
        }
    }

    pub fn is_project(&self) -> bool {
        matches!(self, Self::Project)
    }

    pub fn is_private(&self) -> bool {
        matches!(self, Self::Private)
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }
}

/// Resolved Cargo target setting with its configuration source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCargoTarget {
    pub value: CargoTargetSetting,
    pub source: String,
}

impl Default for ResolvedCargoTarget {
    fn default() -> Self {
        Self {
            value: CargoTargetSetting::Private,
            source: "default".into(),
        }
    }
}

impl ResolvedCargoTarget {
    pub fn resolve(profile: &HarnessProfile) -> Self {
        Self::resolve_with_brief(profile, "")
    }

    pub fn resolve_with_brief(profile: &HarnessProfile, brief: &str) -> Self {
        if let Some(brief_setting) = brief_target(brief) {
            return Self {
                value: brief_setting,
                source: "brief".into(),
            };
        }
        let knob_val = profile
            .knobs
            .get(KNOB)
            .and_then(|val| val.as_str())
            .and_then(CargoTargetSetting::parse);
        let knob_source = provenant_config::knob_source(profile, KNOB);
        match knob_val {
            Some(value) => Self {
                value,
                source: display_source(knob_source).into(),
            },
            None => Self::default(),
        }
    }

    /// Preamble line naming the target and its source.
    pub fn preamble(&self) -> String {
        format!(
            "Cargo target: {} (source: {}; {KNOB}).\n",
            self.value.as_str(),
            self.source
        )
    }

    /// The writer worktree's Cargo workspace directory when `project` is selected:
    /// the verify port's workspace inside `worktree`, read as data only. `None`
    /// for other modes, read-only lanes and projects without a Cargo manifest.
    pub fn project_workspace(&self, worktree: Option<&Path>, host_state: &Path) -> Option<PathBuf> {
        if !self.value.is_project() {
            return None;
        }
        crate::target_seed::cargo_build_directory(worktree?, host_state)
            .ok()
            .flatten()
    }

    /// Compute the build target and lint target directories. `project_workspace`
    /// comes from [`Self::project_workspace`]; without it `project` falls back to
    /// the lane cache.
    pub fn target_paths(
        &self,
        project_workspace: Option<&Path>,
        private_cache: &Path,
    ) -> (PathBuf, PathBuf) {
        let private = || {
            (
                private_cache.join("target"),
                private_cache.join("target-lint"),
            )
        };
        match &self.value {
            CargoTargetSetting::Custom(path) => {
                let lint = match path.file_name() {
                    Some(name) => path.with_file_name(format!("{}-lint", name.to_string_lossy())),
                    None => PathBuf::from(format!("{}-lint", path.display())),
                };
                (path.clone(), lint)
            }
            CargoTargetSetting::Private => private(),
            CargoTargetSetting::Project => match project_workspace {
                Some(workspace) => {
                    let target = workspace.join("target");
                    let lint = target.join(PROJECT_LINT_DIR);
                    (target, lint)
                }
                None => private(),
            },
        }
    }
}

/// A line `Cargo target: project|private` in the chair brief. A brief is requester text
/// (possibly pasted from an issue, possibly from a sandboxed parent lane), so it can
/// pick a mode but never name a path: a custom path grants the lane read, write and exec
/// there, and comes only from host or project configuration or the launch knob.
fn brief_target(brief: &str) -> Option<CargoTargetSetting> {
    brief.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Cargo target:")?;
        CargoTargetSetting::parse(rest.trim()).filter(|setting| !setting.is_custom())
    })
}

fn display_source(source: LimitSource) -> &'static str {
    match source {
        LimitSource::Launch => "launch",
        other => other.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn knob_default_is_private() {
        let profile = HarnessProfile::default();
        let resolved = ResolvedCargoTarget::resolve(&profile);
        assert_eq!(resolved.value, CargoTargetSetting::Private);
        assert_eq!(resolved.source, "default");
        assert!(resolved.value.is_private());
        assert!(!resolved.value.is_project());
    }

    #[test]
    fn brief_can_pick_a_mode_but_never_a_path() {
        assert_eq!(
            brief_target("Cargo target: private"),
            Some(CargoTargetSetting::Private)
        );
        assert_eq!(
            brief_target("intro\nCargo target: project\n"),
            Some(CargoTargetSetting::Project)
        );
        assert_eq!(
            brief_target("Cargo target: /Users/someone/Library/LaunchAgents"),
            None
        );
    }

    #[test]
    fn knob_parsing_handles_project_private_and_absolute_paths() {
        assert_eq!(
            CargoTargetSetting::parse("project"),
            Some(CargoTargetSetting::Project)
        );
        assert_eq!(
            CargoTargetSetting::parse("private"),
            Some(CargoTargetSetting::Private)
        );
        assert_eq!(
            CargoTargetSetting::parse("/opt/warm-target"),
            Some(CargoTargetSetting::Custom(PathBuf::from(
                "/opt/warm-target"
            )))
        );
        assert_eq!(CargoTargetSetting::parse("relative/path"), None);
        assert_eq!(CargoTargetSetting::parse("unknown"), None);
        // One target for every worktree of a project serves stale artifacts (#3561).
        assert_eq!(CargoTargetSetting::parse("shared"), None);
    }

    #[test]
    fn preamble_line_names_target_and_source() {
        let default_target = ResolvedCargoTarget::default();
        assert_eq!(
            default_target.preamble(),
            "Cargo target: private (source: default; lanes.cargo_target).\n"
        );

        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "project".into());
        profile.knob_sources.insert(KNOB.into(), LimitSource::Host);
        let resolved = ResolvedCargoTarget::resolve(&profile);
        assert_eq!(
            resolved.preamble(),
            "Cargo target: project (source: host; lanes.cargo_target).\n"
        );

        let custom = ResolvedCargoTarget {
            value: CargoTargetSetting::Custom(PathBuf::from("/custom/cache")),
            source: "launch".into(),
        };
        assert_eq!(
            custom.preamble(),
            "Cargo target: /custom/cache (source: launch; lanes.cargo_target).\n"
        );

        let brief_override = ResolvedCargoTarget::resolve_with_brief(
            &profile,
            "Implement feature.\nCargo target: private\n",
        );
        assert_eq!(
            brief_override.preamble(),
            "Cargo target: private (source: brief; lanes.cargo_target).\n"
        );
    }

    #[test]
    fn env_paths_for_project_private_and_custom() {
        let workspace = Path::new("/Users/user/project/.worktrees/lane/tools/build");
        let private_cache = Path::new("/Users/user/Library/Caches/provenant-lane-cargo/abc");

        let project = ResolvedCargoTarget {
            value: CargoTargetSetting::Project,
            source: "host".into(),
        };
        let (target, lint) = project.target_paths(Some(workspace), private_cache);
        assert_eq!(target, workspace.join("target"));
        assert_eq!(lint, workspace.join("target/provenant-lint"));
        // Read-only lanes and non-Cargo projects have no workspace: lane cache.
        let (target, lint) = project.target_paths(None, private_cache);
        assert_eq!(target, private_cache.join("target"));
        assert_eq!(lint, private_cache.join("target-lint"));

        let private = ResolvedCargoTarget {
            value: CargoTargetSetting::Private,
            source: "host".into(),
        };
        let (priv_target, priv_lint) = private.target_paths(Some(workspace), private_cache);
        assert_eq!(priv_target, private_cache.join("target"));
        assert_eq!(priv_lint, private_cache.join("target-lint"));

        let custom = ResolvedCargoTarget {
            value: CargoTargetSetting::Custom(PathBuf::from("/opt/target")),
            source: "launch".into(),
        };
        let (cust_target, cust_lint) = custom.target_paths(Some(workspace), private_cache);
        assert_eq!(cust_target, PathBuf::from("/opt/target"));
        assert_eq!(cust_lint, PathBuf::from("/opt/target-lint"));
    }

    /// The invariant behind #3561: two worktrees of one repository never resolve to
    /// one target unless an operator names the same absolute path.
    #[test]
    fn worktrees_of_one_project_never_share_a_default_target()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let first = root.path().join("first");
        let second = root.path().join("second");
        for worktree in [&first, &second] {
            std::fs::create_dir_all(worktree)?;
            std::fs::write(worktree.join("Cargo.toml"), "[workspace]\n")?;
        }
        for value in [CargoTargetSetting::default(), CargoTargetSetting::Project] {
            let setting = ResolvedCargoTarget {
                value,
                source: "default".into(),
            };
            let resolve = |worktree: &Path, cache: &str| {
                let workspace = setting.project_workspace(Some(worktree), root.path());
                setting.target_paths(workspace.as_deref(), &root.path().join(cache))
            };
            let (first_target, first_lint) = resolve(&first, "cache-1");
            let (second_target, second_lint) = resolve(&second, "cache-2");
            assert_ne!(first_target, second_target, "{:?}", setting.value);
            assert_ne!(first_lint, second_lint, "{:?}", setting.value);
        }
        Ok(())
    }
}
