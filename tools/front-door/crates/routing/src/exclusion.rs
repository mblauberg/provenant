//! Caller-chosen exclusions: members a dispatch or graph keeps out of a pool.
use crate::{ModelId, RoutingError};
use std::fmt;

/// What one exclusion removes. A pinned `--model` is never affected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExclusionSelector {
    /// Every member served by this provider (adapter ID).
    Provider(String),
    /// One registered account, as `adapter/key`. Members it alone serves go;
    /// members another account serves stay.
    Account { adapter: String, key: String },
    /// One exact `adapter/id` model.
    Model(ModelId),
}
/// Where an exclusion came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionSource {
    Flag,
    Graph,
    Config,
}
impl ExclusionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Graph => "graph",
            Self::Config => "config",
        }
    }
}
/// One selector and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExclusionRule {
    pub selector: ExclusionSelector,
    pub source: ExclusionSource,
}

/// Valid selector forms, quoted by every refusal.
pub const EXCLUSION_FORMS: &str =
    "a provider (claude), an account (label, entry or adapter/key), or an exact model (adapter/id)";

impl ExclusionSelector {
    /// Parse a selector against the known providers, models and accounts.
    /// `accounts` lists the `adapter/key` of each registered account and
    /// `resolve_account` maps a label, entry or key to one of them.
    pub fn parse(
        text: &str,
        providers: &[&str],
        models: &[ModelId],
        resolve_account: impl Fn(&str) -> Result<Option<(String, String)>, String>,
    ) -> Result<Self, RoutingError> {
        let refuse = |detail: String| RoutingError::UnknownExclusion {
            selector: text.to_owned(),
            detail,
        };
        if providers.contains(&text) {
            return Ok(Self::Provider(text.to_owned()));
        }
        if let Ok(model) = ModelId::parse(text)
            && models.contains(&model)
        {
            return Ok(Self::Model(model));
        }
        match resolve_account(text).map_err(refuse)? {
            Some((adapter, key)) => Ok(Self::Account { adapter, key }),
            None => Err(refuse(format!(
                "valid forms: {EXCLUSION_FORMS}; providers: {}",
                providers.join(", ")
            ))),
        }
    }
}
impl fmt::Display for ExclusionSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Provider(provider) => write!(f, "provider {provider}"),
            Self::Account { adapter, key } => write!(f, "account {adapter}/{key}"),
            Self::Model(model) => write!(f, "model {}", model.as_str()),
        }
    }
}
impl fmt::Display for ExclusionRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "excluded by {}: {}", self.source.as_str(), self.selector)
    }
}
impl ExclusionRule {
    /// Whether the rule removes a member whose account choices are `remaining`
    /// after the excluded accounts have already been withheld.
    pub(crate) fn removes(&self, model: &ModelId, serving_accounts_remain: bool) -> bool {
        match &self.selector {
            ExclusionSelector::Provider(provider) => model.adapter() == provider,
            ExclusionSelector::Model(excluded) => excluded == model,
            ExclusionSelector::Account { adapter, .. } => {
                model.adapter() == adapter && !serving_accounts_remain
            }
        }
    }
    pub(crate) fn account(&self) -> Option<(&str, &str)> {
        match &self.selector {
            ExclusionSelector::Account { adapter, key } => Some((adapter, key)),
            _ => None,
        }
    }
}
