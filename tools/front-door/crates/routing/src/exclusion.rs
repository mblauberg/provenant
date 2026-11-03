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
    /// One model within one named pool, without excluding it elsewhere.
    PoolMember { pool: String, model: ModelId },
    /// A selector supported by a cause and optionally bounded in time.
    /// Wrap one of the scope variants; expiry is exclusive.
    Evidenced {
        scope: Box<ExclusionSelector>,
        cause: String,
        expires_at: Option<provenant_core::WallTime>,
    },
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
            Self::PoolMember { pool, model } => write!(f, "pool {pool} member {}", model.as_str()),
            Self::Evidenced {
                scope,
                cause,
                expires_at,
            } => {
                write!(f, "{scope} ({cause}")?;
                if let Some(expiry) = expires_at {
                    write!(f, ", until {expiry}")?;
                }
                write!(f, ")")
            }
        }
    }
}
impl fmt::Display for ExclusionRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "excluded by {}: {}", self.source.as_str(), self.selector)
    }
}
impl ExclusionSelector {
    /// Whether this evidence is active at the supplied snapshot time.
    pub fn active_at(&self, now: provenant_core::WallTime) -> bool {
        match self {
            Self::Evidenced {
                scope, expires_at, ..
            } => expires_at.is_none_or(|expiry| now < expiry) && scope.active_at(now),
            _ => true,
        }
    }

    fn scope(&self) -> &Self {
        match self {
            Self::Evidenced { scope, .. } => scope.scope(),
            _ => self,
        }
    }
}

impl ExclusionRule {
    /// Whether the rule removes a member after excluded accounts are withheld.
    pub(crate) fn removes(
        &self,
        model: &ModelId,
        pool: Option<&str>,
        serving_accounts_remain: bool,
        now: provenant_core::WallTime,
    ) -> bool {
        if !self.selector.active_at(now) {
            return false;
        }
        match self.selector.scope() {
            ExclusionSelector::Provider(provider) => model.adapter() == provider,
            ExclusionSelector::Model(excluded) => excluded == model,
            ExclusionSelector::PoolMember {
                pool: excluded_pool,
                model: excluded,
            } => pool == Some(excluded_pool.as_str()) && excluded == model,
            ExclusionSelector::Account { adapter, .. } => {
                model.adapter() == adapter && !serving_accounts_remain
            }
            ExclusionSelector::Evidenced { .. } => unreachable!("scope unwraps evidence"),
        }
    }
    pub(crate) fn account(&self, now: provenant_core::WallTime) -> Option<(&str, &str)> {
        if !self.selector.active_at(now) {
            return None;
        }
        match self.selector.scope() {
            ExclusionSelector::Account { adapter, key } => Some((adapter, key)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    use provenant_core::WallTime;
    #[test]
    fn evidence_respects_scope_and_expiry() {
        let model = ModelId::parse("codex/alpha").unwrap();
        let other = ModelId::parse("codex/beta").unwrap();
        let now = WallTime::from_unix_seconds(10).unwrap();
        let expiry = WallTime::from_unix_seconds(11).unwrap();
        let rule = |scope| ExclusionRule {
            selector: ExclusionSelector::Evidenced {
                scope: Box::new(scope),
                cause: "refused".into(),
                expires_at: Some(expiry),
            },
            source: ExclusionSource::Graph,
        };
        let provider = rule(ExclusionSelector::Provider("codex".into()));
        assert!(provider.removes(&model, Some("pool"), true, now));
        assert!(!provider.removes(&model, Some("pool"), true, expiry));
        let account = rule(ExclusionSelector::Account {
            adapter: "codex".into(),
            key: "a".into(),
        });
        assert_eq!(account.account(now), Some(("codex", "a")));
        assert!(account.removes(&model, None, false, now));
        assert!(!account.removes(&model, None, true, now));
        assert_eq!(account.account(expiry), None);
        let exact = rule(ExclusionSelector::Model(model.clone()));
        assert!(exact.removes(&model, None, true, now));
        assert!(!exact.removes(&other, None, true, now));
        let member = rule(ExclusionSelector::PoolMember {
            pool: "pool".into(),
            model: model.clone(),
        });
        assert!(member.removes(&model, Some("pool"), true, now));
        assert!(!member.removes(&model, Some("other"), true, now));
        assert!(!member.removes(&other, Some("pool"), true, now));
        assert!(!member.removes(&model, Some("pool"), true, expiry));
    }
}
