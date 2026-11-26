//! Settings for the one hermetic smoke lifecycle, shared by `run smoke` and
//! `upgrade --verify`. Every effective value and its source is reportable, so a
//! caller can see which layer chose a fixture root, a sweep window, a branch
//! prefix, an unverified exit policy or a template parameter.

use provenant_config::{ConfigError, DomainConfig, DomainSection, FieldMetadata, SectionError};
use provenant_records::warning::{Warning, warn};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
};

/// The configuration path every smoke setting key starts with.
pub const SECTION_KEY: &str = "domains.smoke";
/// The home-relative root under which nonce fixtures are built.
pub const FIXTURE_ROOT_KEY: &str = "domains.smoke.fixture_root";
/// Default home-relative fixture root. It stays relative so a peer resolves the
/// same path under its own home.
pub const DEFAULT_FIXTURE_ROOT: &str = ".local/state/provenant-smoke";
/// Hours after which a marked fixture is swept; zero turns the sweep off.
pub const SWEEP_AFTER_HOURS_KEY: &str = "domains.smoke.sweep_after_hours";
/// Prefix for the smoke graph and branch name.
pub const BRANCH_PREFIX_KEY: &str = "domains.smoke.branch_prefix";
/// Whether an unverified result exits nonzero.
pub const UNVERIFIED_KEY: &str = "domains.smoke.unverified";
/// The choices `domains.smoke.unverified` accepts.
pub const UNVERIFIED_CHOICES: &[&str] = &["warn", "fail"];

/// What an unverified smoke result does to the exit status. `Warn` reports the
/// verdict and exits zero; `Fail` reports the same verdict and exits nonzero.
/// The value is declared data so no host or project special case is needed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unverified {
    #[default]
    Warn,
    Fail,
}
impl Unverified {
    /// The config spelling of this choice.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Fail => "fail",
        }
    }
}

/// The closed `[domains.smoke]` section. A value no layer sets is its compiled
/// default; an out-of-range or unsafe value warns and falls back to that default.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Home-relative fixture root. Absolute paths and `..` are refused by
    /// [`validate_root`] because this is also the fixture removal path guard.
    pub fixture_root: String,
    /// Fixture age in hours at which the sweep removes a marked directory.
    pub sweep_after_hours: u64,
    /// Git ref prefix for the smoke graph name and branch.
    pub branch_prefix: String,
    /// Exit policy for an unverified result.
    pub unverified: Unverified,
    /// Template parameters supplied by configuration under `--set`. A command
    /// line `--set` of the same key wins over the configured value.
    pub set: BTreeMap<String, String>,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            fixture_root: DEFAULT_FIXTURE_ROOT.into(),
            sweep_after_hours: 24,
            branch_prefix: "smoke/".into(),
            unverified: Unverified::Warn,
            set: BTreeMap::new(),
        }
    }
}

/// Where each effective smoke setting came from. When the configuration could
/// not be read, every setting is its compiled default and `unavailable` says why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    profile: provenant_config::Profile,
    origins: BTreeMap<String, provenant_config::Origin>,
    unavailable: Option<String>,
}
impl Origin {
    /// Every setting from the compiled profile.
    pub fn compiled(profile: provenant_config::Profile) -> Self {
        Self {
            profile,
            origins: BTreeMap::new(),
            unavailable: None,
        }
    }

    /// The layer that set `key`; a key no layer names is compiled.
    pub fn of(&self, key: &str) -> provenant_config::Origin {
        self.origins
            .get(key)
            .cloned()
            .unwrap_or(provenant_config::Origin::Compiled {
                profile: self.profile,
            })
    }

    /// Why compiled defaults apply, when the configuration was unreadable.
    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    /// `KEY from SOURCE`, the form every smoke report names a setting in.
    pub fn describe(&self, key: &str) -> String {
        let source = match self.of(key) {
            provenant_config::Origin::Compiled { profile } => {
                format!("the compiled {} profile", profile.as_str())
            }
            provenant_config::Origin::User { path } => {
                path.as_path_text().unwrap_or("config.toml").to_owned()
            }
        };
        match &self.unavailable {
            Some(reason) => format!("{key} from {source}; configuration unavailable: {reason}"),
            None => format!("{key} from {source}"),
        }
    }
}

/// Validate a configured fixture root. It must be home-relative and made only
/// of normal components: absolute paths and `..` would escape the home and turn
/// a nonce guard into a deletion outside the fixture root. This is the single
/// validator the host and the peer share.
pub fn validate_root(value: &str) -> Result<PathBuf, String> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() {
        return Err("fixture root must be a non-empty home-relative path".into());
    }
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("fixture root must not contain '.' or '..' components".into());
    }
    Ok(path.to_path_buf())
}

/// Validate a Git ref prefix. It must be relative, made only of normal
/// components, and free of the characters Git refuses in a ref name.
pub fn validate_branch_prefix(value: &str) -> Result<(), String> {
    let path = Path::new(value);
    if value.is_empty() || path.is_absolute() {
        return Err("branch prefix must be a non-empty ref prefix".into());
    }
    if path
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("branch prefix must not contain '.' or '..' components".into());
    }
    if value
        .chars()
        .any(|c| c.is_control() || matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\'))
        || value.contains("..")
        || value.contains("@{")
    {
        return Err("branch prefix contains a character Git refuses in a ref".into());
    }
    Ok(())
}

/// The key of one `--set` item, the part before its first `=`.
fn set_key(item: &str) -> &str {
    item.split_once('=').map_or(item, |(key, _)| key)
}

impl Settings {
    /// The one resolution of `[domains.smoke]` for every lifecycle entry point.
    /// Bad configuration never stops smoke: it warns and uses compiled defaults.
    pub fn resolve(roots: &provenant_core::Roots) -> Result<(Self, Origin), ConfigError> {
        match provenant_config::read_with_domains(
            roots,
            &crate::domains::SECTIONS.scoped(&["smoke"]),
        ) {
            Ok(resolved) => Ok(Self::resolved(&resolved)),
            Err(error) => {
                let reason = error.to_string();
                warn(Warning::new(
                    provenant_records::warning::CONFIG_UNAVAILABLE,
                    format!(
                        "smoke settings use compiled defaults; configuration unavailable: {reason}"
                    ),
                ));
                let profile = provenant_config::Profile::default();
                Ok((
                    Self::default(),
                    Origin {
                        unavailable: Some(reason),
                        ..Origin::compiled(profile)
                    },
                ))
            }
        }
    }

    /// Smoke settings from a configuration a caller already resolved.
    pub fn resolved(
        resolved: &provenant_config::ResolvedConfig<crate::domains::Domains>,
    ) -> (Self, Origin) {
        let prefix = format!("{SECTION_KEY}.");
        (
            resolved.domains.smoke.clone(),
            Origin {
                profile: resolved.profile,
                origins: resolved
                    .origins
                    .iter()
                    .filter(|(key, _)| key.starts_with(&prefix))
                    .map(|(key, origin)| (key.clone(), origin.clone()))
                    .collect(),
                unavailable: None,
            },
        )
    }

    /// The effective fixture root. An unsafe configured value has already
    /// warned through [`Self::validate`]; fall back to the compiled relative root.
    pub fn root(&self) -> PathBuf {
        validate_root(&self.fixture_root).unwrap_or_else(|_| {
            validate_root(DEFAULT_FIXTURE_ROOT).expect("the default root is valid")
        })
    }

    /// `KEY = value (KEY from SOURCE)` for every effective value. Used by
    /// `run smoke --explain`, which prints these without creating a fixture.
    pub fn explain(&self, origin: &Origin) -> String {
        [
            (FIXTURE_ROOT_KEY.to_owned(), self.fixture_root.clone()),
            (
                SWEEP_AFTER_HOURS_KEY.to_owned(),
                self.sweep_after_hours.to_string(),
            ),
            (BRANCH_PREFIX_KEY.to_owned(), self.branch_prefix.clone()),
            (
                UNVERIFIED_KEY.to_owned(),
                self.unverified.as_str().to_owned(),
            ),
        ]
        .into_iter()
        .map(|(key, value)| format!("{key} = {value} ({})", origin.describe(&key)))
        .chain(self.set.iter().map(|(name, value)| {
            let key = format!("{SECTION_KEY}.set.{name}");
            format!("{key} = {value} ({})", origin.describe(&key))
        }))
        .collect::<Vec<_>>()
        .join("\n")
    }

    /// Configuration parameters under `--set`, with command line items winning.
    /// The merged list is what the lifecycle passes to the child.
    pub fn merged_set(&self, cli_set: &[String]) -> Vec<String> {
        let named: BTreeSet<&str> = cli_set.iter().map(|item| set_key(item)).collect();
        let mut merged: Vec<String> = self
            .set
            .iter()
            .filter(|(key, _)| !named.contains(key.as_str()))
            .map(|(key, value)| format!("{key}={value}"))
            .collect();
        merged.extend(cli_set.iter().cloned());
        merged
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        let mut errors = Vec::new();
        if !(0..=8760).contains(&self.sweep_after_hours) {
            errors.push(SectionError {
                detail: None,
                field: "sweep_after_hours".into(),
                code: "range",
            });
        }
        if validate_root(&self.fixture_root).is_err() {
            errors.push(SectionError {
                detail: Some("fixture root must be home-relative without '..'"),
                field: "fixture_root".into(),
                code: "value",
            });
        }
        if validate_branch_prefix(&self.branch_prefix).is_err() {
            errors.push(SectionError {
                detail: Some("branch prefix must be a valid Git ref prefix"),
                field: "branch_prefix".into(),
                code: "value",
            });
        }
        errors
    }
}

/// The declared `[domains.smoke]` section, registered with every other owner.
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "smoke",
    |_| {
        "fixture_root = \".local/state/provenant-smoke\"\nsweep_after_hours = 24\nbranch_prefix = \"smoke/\"\nunverified = \"warn\"\n"
    },
)
.with_fields(&[
    FieldMetadata::new("fixture_root", "string"),
    FieldMetadata::int("sweep_after_hours", 0, 8760),
    FieldMetadata::new("branch_prefix", "string"),
    FieldMetadata::enumeration("unverified", UNVERIFIED_CHOICES),
    FieldMetadata::new("set.{parameter}", "string"),
]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_validation_accepts_only_safe_home_relative_paths() {
        assert_eq!(
            validate_root(DEFAULT_FIXTURE_ROOT).unwrap(),
            PathBuf::from(DEFAULT_FIXTURE_ROOT)
        );
        assert_eq!(
            validate_root("fixtures").unwrap(),
            PathBuf::from("fixtures")
        );
        for bad in ["", "/tmp/x", "../x", "a/../b", "./x"] {
            assert!(validate_root(bad).is_err(), "{bad} must be refused");
        }
    }

    #[test]
    fn branch_prefix_validation_refuses_unsafe_ref_prefixes() {
        for good in ["smoke/", "verify", "ci/smoke/"] {
            assert!(validate_branch_prefix(good).is_ok(), "{good}");
        }
        for bad in ["", "../x", "/smoke", "smoke x", "smoke~/", "a..b/"] {
            assert!(
                validate_branch_prefix(bad).is_err(),
                "{bad} must be refused"
            );
        }
    }

    #[test]
    fn unverified_accepts_only_declared_choices() {
        assert_eq!(Settings::default().unverified, Unverified::Warn);
        assert_eq!(
            serde_json::from_value::<Settings>(serde_json::json!({"unverified": "fail"}))
                .unwrap()
                .unverified,
            Unverified::Fail
        );
        assert!(
            serde_json::from_value::<Settings>(serde_json::json!({"unverified": "other"})).is_err()
        );
    }

    #[test]
    fn configured_set_is_merged_under_the_command_line() {
        let settings = Settings {
            set: BTreeMap::from([
                ("write".into(), "false".into()),
                ("retry_probe".into(), "false".into()),
            ]),
            ..Settings::default()
        };
        let merged = settings.merged_set(&["write=true".into(), "diff_base=abc".into()]);
        assert_eq!(merged.len(), 3, "{merged:?}");
        assert!(merged.contains(&"write=true".into()), "{merged:?}");
        assert!(merged.contains(&"retry_probe=false".into()), "{merged:?}");
        assert!(merged.contains(&"diff_base=abc".into()), "{merged:?}");
        assert!(!merged.iter().any(|item| item == "write=false"));
    }

    #[test]
    fn explain_names_each_value_and_its_source() {
        let origin = Origin::compiled(provenant_config::Profile::default());
        let text = Settings::default().explain(&origin);
        for key in [
            FIXTURE_ROOT_KEY,
            SWEEP_AFTER_HOURS_KEY,
            BRANCH_PREFIX_KEY,
            UNVERIFIED_KEY,
        ] {
            assert!(text.contains(&format!("{key} =")), "{text}");
            assert!(text.contains(&format!("{key} from")), "{text}");
        }
        let settings = Settings {
            fixture_root: "../escape".into(),
            ..Settings::default()
        };
        assert!(!DomainConfig::validate(&settings).is_empty());
        assert_eq!(settings.root(), PathBuf::from(DEFAULT_FIXTURE_ROOT));
    }

    #[test]
    fn settings_resolve_through_one_failure_policy() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let mut input = provenant_core::RootInputs::new(
            temporary.path().join("home"),
            temporary.path().join("assets"),
        );
        input.state_dir = Some(temporary.path().join("state"));
        let roots = input
            .resolve(&provenant_os::fs::SystemPathResolver)
            .expect("roots");
        let state = roots.state.as_path();
        std::fs::create_dir_all(state).expect("state directory");
        std::fs::write(
            state.join("config.toml"),
            "schema_version=1\n[domains.smoke]\nsweep_after_hours=4\n[domains.smoke.set]\nwrite=\"false\"\n",
        )
        .expect("config");
        let (settings, origin) = Settings::resolve(&roots).expect("settings");
        assert_eq!(settings.sweep_after_hours, 4);
        assert_eq!(settings.set.get("write").map(String::as_str), Some("false"));
        assert!(
            origin
                .describe(SWEEP_AFTER_HOURS_KEY)
                .contains("config.toml")
        );
        assert!(
            settings
                .explain(&origin)
                .contains("domains.smoke.set.write")
        );
    }
}
