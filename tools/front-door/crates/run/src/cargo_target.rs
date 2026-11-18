//! Lane Cargo target knob (`lanes.cargo_target`).
//!
//! Controls where a lane's Cargo output is directed:
//! - `shared` (default): one writable warm target per project under the host cache (`~/Library/Caches/provenant-shared-cargo/<project_key>/target`),
//!   avoiding cold builds across lanes.
//! - `private`: per-worktree copy-on-write seed clone under `~/Library/Caches/provenant-lane-cargo/<digest>/target`.
//! - Absolute path: used as given.
//!
//! The value and its source appear in the lane preamble and in `provenant config show`.

use provenant_records::{LimitSource, run::HarnessProfile};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The registry knob.
pub const KNOB: &str = "lanes.cargo_target";

/// Target location mode configured for a lane.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CargoTargetSetting {
    #[default]
    Shared,
    Private,
    Custom(PathBuf),
}

impl CargoTargetSetting {
    pub fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        if trimmed == "shared" {
            Some(Self::Shared)
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
            Self::Shared => "shared",
            Self::Private => "private",
            Self::Custom(path) => path.to_str().unwrap_or(""),
        }
    }

    pub fn is_shared(&self) -> bool {
        matches!(self, Self::Shared)
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
            value: CargoTargetSetting::Shared,
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
            None => Self {
                value: CargoTargetSetting::Shared,
                source: "default".into(),
            },
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

    /// Compute the build target and lint target directories.
    pub fn target_paths(
        &self,
        home: &Path,
        common_git_dir: Option<&Path>,
        private_cache: &Path,
    ) -> (PathBuf, PathBuf) {
        match &self.value {
            CargoTargetSetting::Custom(path) => {
                let lint = match path.file_name() {
                    Some(name) => path.with_file_name(format!("{}-lint", name.to_string_lossy())),
                    None => PathBuf::from(format!("{}-lint", path.display())),
                };
                (path.clone(), lint)
            }
            CargoTargetSetting::Private => (
                private_cache.join("target"),
                private_cache.join("target-lint"),
            ),
            CargoTargetSetting::Shared => {
                if let Some(dir) = common_git_dir {
                    let key = crate::target_seed::project_key(dir);
                    let cache = home.join("Library/Caches/provenant-shared-cargo").join(key);
                    (cache.join("target"), cache.join("target-lint"))
                } else {
                    (
                        private_cache.join("target"),
                        private_cache.join("target-lint"),
                    )
                }
            }
        }
    }
}

/// A line `Cargo target: shared|private|<path>` in the chair brief.
fn brief_target(brief: &str) -> Option<CargoTargetSetting> {
    brief.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("Cargo target:")?;
        CargoTargetSetting::parse(rest.trim())
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
    fn knob_default_is_shared() {
        let profile = HarnessProfile::default();
        let resolved = ResolvedCargoTarget::resolve(&profile);
        assert_eq!(resolved.value, CargoTargetSetting::Shared);
        assert_eq!(resolved.source, "default");
        assert!(resolved.value.is_shared());
        assert!(!resolved.value.is_private());
    }

    #[test]
    fn knob_parsing_handles_shared_private_and_absolute_paths() {
        assert_eq!(
            CargoTargetSetting::parse("shared"),
            Some(CargoTargetSetting::Shared)
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
    }

    #[test]
    fn preamble_line_names_target_and_source() {
        let default_target = ResolvedCargoTarget::default();
        assert_eq!(
            default_target.preamble(),
            "Cargo target: shared (source: default; lanes.cargo_target).\n"
        );

        let mut profile = HarnessProfile::default();
        profile.knobs.insert(KNOB.into(), "private".into());
        profile.knob_sources.insert(KNOB.into(), LimitSource::Host);
        let resolved = ResolvedCargoTarget::resolve(&profile);
        assert_eq!(
            resolved.preamble(),
            "Cargo target: private (source: host; lanes.cargo_target).\n"
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
            "Implement feature.\nCargo target: /brief/target\n",
        );
        assert_eq!(
            brief_override.preamble(),
            "Cargo target: /brief/target (source: brief; lanes.cargo_target).\n"
        );
    }

    #[test]
    fn env_paths_for_shared_private_and_custom() {
        let home = Path::new("/Users/user");
        let common_git = Path::new("/Users/user/project/.git");
        let private_cache = Path::new("/Users/user/Library/Caches/provenant-lane-cargo/abc");

        let shared = ResolvedCargoTarget {
            value: CargoTargetSetting::Shared,
            source: "default".into(),
        };
        let (shared_target, shared_lint) =
            shared.target_paths(home, Some(common_git), private_cache);
        let key = crate::target_seed::project_key(common_git);
        assert_eq!(
            shared_target,
            home.join("Library/Caches/provenant-shared-cargo")
                .join(&key)
                .join("target")
        );
        assert_eq!(
            shared_lint,
            home.join("Library/Caches/provenant-shared-cargo")
                .join(&key)
                .join("target-lint")
        );

        let private = ResolvedCargoTarget {
            value: CargoTargetSetting::Private,
            source: "host".into(),
        };
        let (priv_target, priv_lint) = private.target_paths(home, Some(common_git), private_cache);
        assert_eq!(priv_target, private_cache.join("target"));
        assert_eq!(priv_lint, private_cache.join("target-lint"));

        let custom = ResolvedCargoTarget {
            value: CargoTargetSetting::Custom(PathBuf::from("/opt/target")),
            source: "launch".into(),
        };
        let (cust_target, cust_lint) = custom.target_paths(home, Some(common_git), private_cache);
        assert_eq!(cust_target, PathBuf::from("/opt/target"));
        assert_eq!(cust_lint, PathBuf::from("/opt/target-lint"));
    }

    #[test]
    fn shared_target_is_identical_for_same_project() {
        let home = Path::new("/Users/user");
        let common_git = Path::new("/Users/user/project/.git");
        let priv_cache_1 = Path::new("/Users/user/Library/Caches/provenant-lane-cargo/lane1");
        let priv_cache_2 = Path::new("/Users/user/Library/Caches/provenant-lane-cargo/lane2");

        let shared = ResolvedCargoTarget::default();
        let (target_1, _) = shared.target_paths(home, Some(common_git), priv_cache_1);
        let (target_2, _) = shared.target_paths(home, Some(common_git), priv_cache_2);

        assert_eq!(target_1, target_2);
    }
}
