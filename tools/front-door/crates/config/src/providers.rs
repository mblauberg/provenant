//! Host provider updates, separate from self-update inside confined lanes.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    pub out_of_scope_pin: OutOfScopePin,
    pub update: ProviderUpdate,
    pub update_interval_seconds: u64,
    pub update_timeout_seconds: u64,
    /// A cap on any lane's runtime deadline on this provider, in seconds. A
    /// dispatch's own `--provider-timeout` applies when it is shorter.
    pub runtime_deadline_seconds: Option<u64>,
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            models: None,
            out_of_scope_pin: OutOfScopePin::default(),
            update: ProviderUpdate::default(),
            update_interval_seconds: 24 * 60 * 60,
            update_timeout_seconds: 300,
            runtime_deadline_seconds: None,
        }
    }
}

pub fn defaults() -> BTreeMap<String, ProviderSettings> {
    provenant_records::ADAPTER_SPECS
        .iter()
        .map(|provider| {
            let mut settings = ProviderSettings::default();
            if provider.name == "cursor" {
                settings.models =
                    Some(["auto", "composer-*", "grok-*"].map(str::to_owned).to_vec());
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
