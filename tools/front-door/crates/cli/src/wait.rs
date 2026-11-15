//! Settings for the `wait` verb. Source cadence remains overridable.
use provenant_config::{ConfigError, DomainConfig, DomainSection, FieldMetadata, SectionError};
use provenant_core::DurationMicros;
use provenant_fabric_runtime::coordination::{LOCAL_OBSERVATION_INTERVAL_KEY, ObservationSettings};
use serde::{Deserialize, Serialize};

/// The configuration path every wait setting key starts with.
pub const SECTION_KEY: &str = "domains.wait";
/// The key that bounds every wait budget.
pub const MAX_TIMEOUT_KEY: &str = "domains.wait.max_timeout_seconds";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct Settings {
    pub timeout_seconds: u64,
    pub max_timeout_seconds: Option<u64>,
    pub local_interval_ms: u64,
    pub pr_interval_ms: u64,
    pub pr_slow_interval_ms: u64,
    pub pr_near_end_window_ms: u64,
    pub pr_conditional_requests: bool,
    pub max_targets: u64,
    pub pr_read_timeout_seconds: u64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            timeout_seconds: 300,
            max_timeout_seconds: None,
            local_interval_ms: 500,
            pr_interval_ms: 5000,
            pr_slow_interval_ms: 15000,
            pr_near_end_window_ms: 30000,
            pr_conditional_requests: true,
            max_targets: 64,
            pr_read_timeout_seconds: 60,
        }
    }
}
/// Where each effective wait setting came from. When the configuration file
/// could not be read, every setting is its compiled default and `unavailable`
/// says why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    profile: provenant_config::Profile,
    origins: std::collections::BTreeMap<String, provenant_config::Origin>,
    unavailable: Option<String>,
}
impl Origin {
    /// Every setting from the compiled profile.
    pub fn compiled(profile: provenant_config::Profile) -> Self {
        Self {
            profile,
            origins: Default::default(),
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

    /// `KEY from SOURCE`, the form every wait report names a setting in.
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

    /// The local paced-reread cadence these settings select.
    pub fn local_cadence(&self, settings: &Settings) -> LocalCadence {
        LocalCadence {
            interval: std::time::Duration::from_millis(settings.local_interval_ms),
            source: self.describe(LOCAL_OBSERVATION_INTERVAL_KEY),
        }
    }

    /// The coordination service's view of the same cadence.
    pub fn observation(&self, settings: &Settings) -> ObservationSettings {
        ObservationSettings {
            local_interval: DurationMicros::from_micros(
                settings.local_interval_ms.saturating_mul(1_000),
            ),
            source: self.of(LOCAL_OBSERVATION_INTERVAL_KEY),
        }
    }
}

/// The paced-reread cadence for local observation and the setting that chose
/// it. Every report of a paced reread renders it through [`Self::every`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalCadence {
    pub interval: std::time::Duration,
    source: String,
}
impl LocalCadence {
    /// A cadence from the compiled default profile, for callers without a
    /// resolved configuration.
    pub fn compiled(interval: std::time::Duration) -> Self {
        Origin::compiled(provenant_config::Profile::default()).local_cadence(&Settings {
            local_interval_ms: u64::try_from(interval.as_millis()).unwrap_or(u64::MAX),
            ..Settings::default()
        })
    }

    /// `every N ms (KEY from SOURCE)`.
    pub fn every(&self) -> String {
        format!("every {} ms ({})", self.interval.as_millis(), self.source)
    }
}

pub(crate) fn non_wait_observation_settings() -> ObservationSettings {
    Origin::compiled(provenant_config::Profile::default()).observation(&Settings::default())
}

pub(crate) fn observation_settings(
    resolved: &provenant_config::ResolvedConfig<crate::domains::Domains>,
) -> ObservationSettings {
    let (settings, origin) = Settings::resolved(resolved);
    origin.observation(&settings)
}

/// The declared failure policy for reading wait settings. A configuration file
/// that cannot be read leaves the wait on its compiled defaults, with a warning
/// and the reason in every reported source. A file that is present but
/// malformed is a typed error. A single out-of-range value is not a read
/// failure: the loader recovers it with a warning and its origin is compiled.
fn read_failure(error: ConfigError) -> Result<String, ConfigError> {
    match error {
        ConfigError::Read { .. } | ConfigError::PathEncoding { .. } => Ok(error.to_string()),
        error => Err(error),
    }
}

impl Settings {
    /// The one resolution of `[domains.wait]` for every waiter.
    pub fn resolve(roots: &provenant_core::Roots) -> Result<(Self, Origin), ConfigError> {
        match crate::config_reads::read_for(roots, "wait") {
            Ok(resolved) => Ok(Self::resolved(&resolved)),
            Err(error) => {
                let reason = read_failure(error)?;
                use std::io::Write;
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "warning: wait settings use compiled defaults; configuration unavailable: {reason}"
                );
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

    /// Wait settings from a configuration a caller has already resolved.
    pub fn resolved(
        resolved: &provenant_config::ResolvedConfig<crate::domains::Domains>,
    ) -> (Self, Origin) {
        let prefix = format!("{}.", SECTION_KEY);
        (
            resolved.domains.wait.clone(),
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

    /// The effective wait budget in seconds: the request, else the configured
    /// default, bounded by the configured maximum with a warning naming it.
    pub fn timeout(&self, override_seconds: Option<u64>, origin: &Origin) -> u64 {
        let requested = override_seconds
            .filter(|seconds| *seconds != 0)
            .unwrap_or(self.timeout_seconds);
        let maximum = self
            .max_timeout_seconds
            .unwrap_or(provenant_core::MAX_LANE_RUNTIME_SECONDS);
        if requested > maximum {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "warning: wait budget clamped to {maximum}s; {}",
                origin.describe(MAX_TIMEOUT_KEY)
            );
        }
        requested.min(maximum)
    }
}
impl DomainConfig for Settings {
    fn validate(&self) -> Vec<SectionError> {
        [
            ("timeout_seconds", self.timeout_seconds, 1, u32::MAX as u64),
            (
                "max_timeout_seconds",
                self.max_timeout_seconds
                    .unwrap_or(provenant_core::MAX_LANE_RUNTIME_SECONDS),
                1,
                u32::MAX as u64,
            ),
            ("local_interval_ms", self.local_interval_ms, 10, 60000),
            ("pr_interval_ms", self.pr_interval_ms, 10, 60000),
            ("pr_slow_interval_ms", self.pr_slow_interval_ms, 10, 60000),
            (
                "pr_near_end_window_ms",
                self.pr_near_end_window_ms,
                0,
                60000,
            ),
            ("max_targets", self.max_targets, 1, 1024),
            (
                "pr_read_timeout_seconds",
                self.pr_read_timeout_seconds,
                1,
                600,
            ),
        ]
        .into_iter()
        .filter(|(_, value, min, max)| !(min..=max).contains(&value))
        .map(|(field, ..)| SectionError {
            detail: None,
            field: field.into(),
            code: "range",
        })
        .collect()
    }
}
pub const SECTION: DomainSection = DomainSection::new::<Settings>(
    "wait",
    |_| {
        "timeout_seconds = 300\nmax_timeout_seconds = 14400\nlocal_interval_ms = 500\npr_interval_ms = 5000\npr_slow_interval_ms = 15000\npr_near_end_window_ms = 30000\npr_conditional_requests = true\nmax_targets = 64\npr_read_timeout_seconds = 60\n"
    },
)
.with_fields(&[
    FieldMetadata::int("timeout_seconds", 1, u32::MAX as i64),
    FieldMetadata::int("max_timeout_seconds", 1, u32::MAX as i64),
    FieldMetadata::int("local_interval_ms", 10, 60000),
    FieldMetadata::int("pr_interval_ms", 10, 60000),
    FieldMetadata::int("pr_slow_interval_ms", 10, 60000),
    FieldMetadata::int("pr_near_end_window_ms", 0, 60000),
    FieldMetadata::new("pr_conditional_requests", "bool"),
    FieldMetadata::int("max_targets", 1, 1024),
    FieldMetadata::int("pr_read_timeout_seconds", 1, 600),
]);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_settings_resolve_through_one_failure_policy() {
        #[derive(Debug, PartialEq)]
        enum Expect {
            Configured,
            Default { unavailable: bool },
            Refused,
        }
        let configured = "schema_version=1\n[domains.wait]\nlocal_interval_ms=750\n";
        for (shape, expect) in [
            ("absent", Expect::Default { unavailable: false }),
            ("configured", Expect::Configured),
            ("unknown key", Expect::Configured),
            ("out of range", Expect::Default { unavailable: false }),
            ("malformed", Expect::Refused),
            ("unreadable", Expect::Default { unavailable: true }),
        ] {
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
            let config = state.join("config.toml");
            match shape {
                "configured" => std::fs::write(&config, configured).expect("config"),
                "unknown key" => std::fs::write(
                    &config,
                    "schema_version=1\n[domains.wait]\nlocal_interval_ms=750\nfuture_wait_option=true\n",
                )
                .expect("config"),
                "out of range" => std::fs::write(
                    &config,
                    "schema_version=1\n[domains.wait]\nlocal_interval_ms=0\n",
                )
                .expect("config"),
                "malformed" => {
                    std::fs::write(&config, "schema_version=1\n[domains.wait\n").expect("config")
                }
                "unreadable" => {
                    // A link is never followed, so the read fails rather than
                    // finding the file absent.
                    let target = temporary.path().join("elsewhere.toml");
                    std::fs::write(&target, configured).expect("link target");
                    std::os::unix::fs::symlink(&target, &config).expect("link");
                }
                _ => {}
            }
            let observed = match Settings::resolve(&roots) {
                Err(_) => Expect::Refused,
                Ok((settings, origin)) => {
                    let user = matches!(
                        origin.of(LOCAL_OBSERVATION_INTERVAL_KEY),
                        provenant_config::Origin::User { .. }
                    );
                    if user {
                        assert_eq!(settings.local_interval_ms, 750, "{shape}");
                        Expect::Configured
                    } else {
                        assert_eq!(
                            settings.local_interval_ms,
                            Settings::default().local_interval_ms,
                            "{shape}"
                        );
                        let cadence = origin.local_cadence(&settings);
                        assert!(cadence.every().contains(LOCAL_OBSERVATION_INTERVAL_KEY));
                        Expect::Default {
                            unavailable: origin.unavailable().is_some(),
                        }
                    }
                }
            };
            assert_eq!(observed, expect, "{shape}");
        }
    }

    #[test]
    fn requested_wait_is_bounded_by_the_effective_maximum() {
        for configured in [None, Some(1), Some(90), Some(86400)] {
            let settings = Settings {
                max_timeout_seconds: configured,
                ..Settings::default()
            };
            let maximum = configured.unwrap_or(provenant_core::MAX_LANE_RUNTIME_SECONDS);
            for requested in [
                None,
                Some(0),
                Some(1),
                Some(60),
                Some(21600),
                Some(u64::MAX),
            ] {
                let effective = settings.timeout(requested, &Origin::compiled(Default::default()));
                assert!(effective <= maximum);
                if let Some(requested) = requested.filter(|seconds| *seconds != 0)
                    && requested <= maximum
                {
                    assert_eq!(effective, requested);
                }
            }
        }
    }
}
