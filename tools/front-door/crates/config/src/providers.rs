//! Host provider updates, separate from self-update inside confined lanes.
use provenant_records::warning::{PROVIDER_SETTING_OUT_OF_RANGE, Warning, warn};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProviderUpdate {
    Enabled(bool),
    Command { command: Vec<String> },
}

impl Default for ProviderUpdate {
    fn default() -> Self {
        Self::Enabled(true)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutOfScopePin {
    #[default]
    Warn,
    Refuse,
    Allow,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSettings {
    /// Allowed model name patterns; None permits every model.
    pub models: Option<Vec<String>>,
    /// Codex's optional suppression of inherited configuration for writers.
    /// Read-only authority still requires a clean configuration.
    pub clean_config: bool,
    pub out_of_scope_pin: OutOfScopePin,
    pub update: ProviderUpdate,
    pub update_interval_seconds: u64,
    pub update_timeout_seconds: u64,
    /// A cap on any lane's runtime deadline on this provider, in seconds. A
    /// dispatch's own `--provider-timeout` applies when it is shorter.
    pub runtime_deadline_seconds: Option<u64>,
    /// Seconds without provider output after which a running attempt is
    /// reported as stalled in status, wait and the digest. 0 disables the
    /// report. This never signals the provider; see `stall_policy` for the
    /// opt-in response.
    pub stall_after_seconds: u64,
    /// Opt-in action once an attempt is stalled. The default only warns.
    pub stall_policy: crate::StallPolicy,
    /// Whether a harness `--version` probe runs. false reports an unreadable
    /// version with a reason naming this key instead of running the binary.
    pub version_probe: bool,
    /// Bounds one `--version` probe. An out-of-range value warns and uses the
    /// compiled default; it never refuses the configuration.
    pub version_probe_timeout_seconds: u64,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            models: None,
            clean_config: false,
            out_of_scope_pin: OutOfScopePin::default(),
            update: ProviderUpdate::default(),
            update_interval_seconds: 24 * 60 * 60,
            update_timeout_seconds: 300,
            runtime_deadline_seconds: None,
            stall_after_seconds: provenant_records::DEFAULT_STALL_AFTER_SECONDS,
            stall_policy: crate::StallPolicy::default(),
            version_probe: true,
            version_probe_timeout_seconds: 4,
        }
    }
}

pub fn defaults() -> BTreeMap<String, ProviderSettings> {
    provenant_records::ADAPTER_SPECS
        .iter()
        .map(|provider| {
            let mut settings = ProviderSettings::default();
            if let Some(models) = provider.default_models {
                settings.models = Some(models.iter().map(|model| (*model).to_owned()).collect());
            }
            (provider.name.to_owned(), settings)
        })
        .collect()
}

pub(crate) fn validate(providers: &BTreeMap<String, ProviderSettings>) -> Result<(), &'static str> {
    for settings in providers.values() {
        if settings.models.as_ref().is_some_and(|patterns| {
            patterns.len() > 128
                || patterns.iter().any(|pattern| {
                    pattern.is_empty()
                        || pattern.len() > 128
                        || !pattern.bytes().all(|b| {
                            b.is_ascii_alphanumeric()
                                || matches!(b, b'-' | b'_' | b'.' | b'/' | b'*')
                        })
                })
        }) {
            return Err("provider_models_pattern");
        }
        if settings.update_interval_seconds == 0 {
            return Err("update_interval_seconds_range");
        }
        if settings.update_timeout_seconds == 0 {
            return Err("update_timeout_seconds_range");
        }
        if settings.runtime_deadline_seconds.is_some_and(|seconds| {
            !(1..=provenant_core::MAX_LANE_RUNTIME_SECONDS).contains(&seconds)
        }) {
            return Err("runtime_deadline_seconds_range");
        }
        if let ProviderUpdate::Command { command } = &settings.update
            && (command.is_empty()
                || command[0].trim().is_empty()
                || command
                    .iter()
                    .any(|arg| arg.len() > 4096 || arg.contains('\0')))
        {
            return Err("provider_update_command");
        }
    }
    Ok(())
}

impl ProviderSettings {
    /// The bounded `--version` timeout for one adapter. A value outside 1-60
    /// seconds warns with the setting and uses the compiled four seconds; this
    /// is advisory, so `validate` does not refuse it.
    pub fn version_probe_timeout(&self, provider: &str) -> Duration {
        const DEFAULT_SECONDS: u64 = 4;
        let seconds = self.version_probe_timeout_seconds;
        if (1..=60).contains(&seconds) {
            return Duration::from_secs(seconds);
        }
        let key = format!("providers.{provider}.version_probe_timeout_seconds");
        warn(
            Warning::new(
                PROVIDER_SETTING_OUT_OF_RANGE,
                format!("{key} = {seconds} is outside 1-60; using {DEFAULT_SECONDS}"),
            )
            .subject(key),
        );
        Duration::from_secs(DEFAULT_SECONDS)
    }

    pub fn pin_warning(&self, provider: &str, model: &str) -> Option<String> {
        (!self.permits_model(model) && self.out_of_scope_pin == OutOfScopePin::Warn).then(|| {
            format!("warning: {provider}/{model} is outside providers.{provider}.models; providers.{provider}.out_of_scope_pin = warn")
        })
    }

    /// Match literal bytes and `*` (zero or more bytes) against a provider model name.
    pub fn permits_model(&self, model: &str) -> bool {
        fn matches(pattern: &[u8], text: &[u8]) -> bool {
            let (mut p, mut t, mut star, mut retry) = (0, 0, None, 0);
            while t < text.len() {
                if p < pattern.len() && pattern[p] == text[t] {
                    p += 1;
                    t += 1;
                } else if p < pattern.len() && pattern[p] == b'*' {
                    star = Some(p);
                    p += 1;
                    retry = t;
                } else if let Some(i) = star {
                    retry += 1;
                    t = retry;
                    p = i + 1;
                } else {
                    return false;
                }
            }
            while p < pattern.len() && pattern[p] == b'*' {
                p += 1;
            }
            p == pattern.len()
        }
        self.models.as_ref().is_none_or(|patterns| {
            patterns
                .iter()
                .any(|p| matches(p.as_bytes(), model.as_bytes()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn version_probe_defaults_are_on_with_a_four_second_bound() {
        let settings = ProviderSettings::default();
        assert!(settings.version_probe);
        assert_eq!(settings.version_probe_timeout_seconds, 4);
        assert_eq!(
            settings.version_probe_timeout("codex"),
            Duration::from_secs(4)
        );
    }

    #[test]
    fn an_out_of_range_version_probe_timeout_warns_and_uses_the_default() {
        for seconds in [0, 61, 500] {
            let settings = ProviderSettings {
                version_probe_timeout_seconds: seconds,
                ..ProviderSettings::default()
            };
            let (timeout, warnings) =
                provenant_records::warning::capture(|| settings.version_probe_timeout("codex"));
            assert_eq!(timeout, Duration::from_secs(4));
            assert_eq!(warnings.len(), 1);
            assert_eq!(
                warnings[0].code.as_ref(),
                PROVIDER_SETTING_OUT_OF_RANGE.as_str()
            );
            assert!(
                warnings[0]
                    .message
                    .contains("providers.codex.version_probe_timeout_seconds")
            );
            assert!(warnings[0].message.contains(&seconds.to_string()));
        }
    }

    #[test]
    fn a_configured_version_probe_timeout_in_range_is_honoured() {
        let settings = ProviderSettings {
            version_probe_timeout_seconds: 9,
            ..ProviderSettings::default()
        };
        let (timeout, warnings) =
            provenant_records::warning::capture(|| settings.version_probe_timeout("codex"));
        assert_eq!(timeout, Duration::from_secs(9));
        assert!(warnings.is_empty());
    }
}
