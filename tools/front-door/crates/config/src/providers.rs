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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderSettings {
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
            update: ProviderUpdate::default(),
            update_interval_seconds: 24 * 60 * 60,
            update_timeout_seconds: 300,
            runtime_deadline_seconds: None,
        }
    }
}

/// The longest runtime deadline a dispatch can request.
const MAX_RUNTIME_DEADLINE_SECONDS: u64 = 14_400;

pub(crate) fn defaults() -> BTreeMap<String, ProviderSettings> {
    provenant_records::ADAPTER_SPECS
        .iter()
        .map(|provider| (provider.name.to_owned(), ProviderSettings::default()))
        .collect()
}

pub(crate) fn validate(providers: &BTreeMap<String, ProviderSettings>) -> Result<(), &'static str> {
    for (id, settings) in providers {
        if provenant_records::AdapterSpec::get(id).is_none() {
            return Err("provider_id");
        }
        if settings.update_interval_seconds == 0 {
            return Err("update_interval_seconds_range");
        }
        if settings.update_timeout_seconds == 0 {
            return Err("update_timeout_seconds_range");
        }
        if settings
            .runtime_deadline_seconds
            .is_some_and(|seconds| !(1..=MAX_RUNTIME_DEADLINE_SECONDS).contains(&seconds))
        {
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
