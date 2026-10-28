//! Host provider updates, separate from self-update inside confined lanes.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProviderUpdate {
    Enabled(bool),
    Command(String),
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
}

impl Default for ProviderSettings {
    fn default() -> Self {
        Self {
            update: ProviderUpdate::default(),
            update_interval_seconds: 24 * 60 * 60,
        }
    }
}

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
        if let ProviderUpdate::Command(command) = &settings.update
            && (command.trim().is_empty() || command.len() > 4096 || command.contains('\0'))
        {
            return Err("provider_update_command");
        }
    }
    Ok(())
}
