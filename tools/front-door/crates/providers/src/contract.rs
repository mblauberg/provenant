use crate::{
    ExitObservation, LaunchSpec, PrepareInput, ProviderEvent, ProviderOutcome, StreamState,
};
use provenant_records::ResolvedRoute;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("{adapter} invocation has no qualifying activation recording")]
    Disabled { adapter: &'static str },
    #[error("argv prompt exceeds 64 KiB")]
    PromptTooLarge,
    #[error("provider protocol is malformed")]
    Malformed,
    #[error("provider stream exceeds its size bound")]
    Overflow,
    #[error("provider/session/account/host/model/controls or grant changed")]
    IdentityMismatch,
    #[error("provider continuation identity is unknown")]
    UnknownIdentity,
    #[error(
        "claude reports no login for this config directory; run `claude auth login` there once \
         interactively, or over SSH unlock the login keychain first (`security unlock-keychain`)"
    )]
    LoginRequired,
    #[error("launch path is not absolute or lies outside the plan's roots")]
    InvalidPath,
    #[error("confinement plan asks for a policy this adapter cannot honour")]
    UnsupportedPolicy,
    #[error("provider login token is unavailable, unreadable or expired")]
    CredentialUnavailable,
}

/// No preflight probe form is recorded yet. An uninhabited plan cannot be
/// forged or accidentally executed; qualifying recordings must precede effects.
#[derive(Debug)]
pub enum PreflightPlan {}

pub struct PreflightInput {
    pub route: ResolvedRoute,
    pub observed_unix_seconds: i64,
}

/// Redacted cache projection; raw provider diagnostics are never persisted here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightFailure {
    Disabled,
    MissingExecutable,
    AuthRequired,
    UnsupportedVersion,
    MalformedProtocol,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AvailabilityRecord {
    pub schema_version: u64,
    pub adapter: String,
    pub executable: String,
    pub version: String,
    pub fixture_ref: String,
    pub catalogue_revision: String,
    pub account_key: Option<String>,
    pub observed_models: Vec<String>,
    pub last_preflight_error: Option<PreflightFailure>,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
}

pub struct Prompt(String);
impl Prompt {
    pub fn new(text: String) -> Self {
        Self(text)
    }
    pub fn text(&self) -> &str {
        &self.0
    }
    pub fn check_argv(&self) -> Result<(), ProviderError> {
        if self.0.len() > 65_536 {
            Err(ProviderError::PromptTooLarge)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountDetection {
    Unknown,
    AuthRequired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Continuation {
    adapter: String,
    session: String,
    account_key: String,
    host: String,
    model: String,
    effort: Option<String>,
    grant_digest: String,
}
impl Continuation {
    pub(crate) fn from_outcome(
        adapter: &str,
        outcome: &ProviderOutcome,
        route: &ResolvedRoute,
        grant_digest: &str,
        model_on_wire: bool,
    ) -> Result<Self, ProviderError> {
        if outcome.adapter() != adapter || route.adapter != adapter {
            return Err(ProviderError::IdentityMismatch);
        }
        let session = outcome.session().ok_or(ProviderError::UnknownIdentity)?;
        // Requested models cannot substitute for answering-model evidence.
        // Codex's exec stream has no model position; its resume argv pins the
        // same `-m`, and any model it does report must still match.
        match outcome.model() {
            Some(model) if model == route.model => {}
            None if !model_on_wire => {}
            _ => return Err(ProviderError::UnknownIdentity),
        }
        Self::retained(adapter, session, route, grant_digest)
    }
    /// A continuation of a session the run store recorded for a prior
    /// attempt. The caller has already matched the recorded route evidence
    /// to `route`, which supplies the account, host, model and effort.
    pub fn retained(
        adapter: &str,
        session: &str,
        route: &ResolvedRoute,
        grant_digest: &str,
    ) -> Result<Self, ProviderError> {
        if route.adapter != adapter || session.is_empty() {
            return Err(ProviderError::IdentityMismatch);
        }
        let account_key = route
            .account_key
            .clone()
            .ok_or(ProviderError::UnknownIdentity)?;
        Ok(Self {
            adapter: adapter.into(),
            session: session.into(),
            account_key,
            host: route.host.clone(),
            model: route.model.clone(),
            effort: route.effort.clone(),
            grant_digest: grant_digest.into(),
        })
    }
    pub(crate) fn bind_stream(
        &self,
        adapter: &str,
        state: &mut StreamState,
    ) -> Result<(), ProviderError> {
        if self.adapter != adapter {
            return Err(ProviderError::IdentityMismatch);
        }
        state.bind_identity(&self.session, &self.model);
        Ok(())
    }
    pub fn session(&self) -> &str {
        &self.session
    }
    pub fn validate(&self, route: &ResolvedRoute, grant_digest: &str) -> Result<(), ProviderError> {
        if self.adapter != route.adapter
            || Some(&self.account_key) != route.account_key.as_ref()
            || self.host != route.host
            || self.model != route.model
            || self.effort != route.effort
            || self.grant_digest != grant_digest
        {
            return Err(ProviderError::IdentityMismatch);
        }
        Ok(())
    }
}

pub struct FamilyEntry {
    pub adapter: String,
    pub model: String,
    pub family: String,
}
pub struct RouteProvenance<'a> {
    pub route: &'a ResolvedRoute,
    pub outcome: &'a ProviderOutcome,
    pub evidence_ref: &'a str,
    pub evidence_catalogue_revision: &'a str,
    pub catalogue: &'a [FamilyEntry],
}
/// Serialize only: stored claims cannot construct a verified fact.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct VerifiedLineage {
    adapter: String,
    observed_model: Option<String>,
    catalogue_revision: String,
    family: Option<String>,
    evidence_ref: String,
}
impl VerifiedLineage {
    pub fn adapter(&self) -> &str {
        &self.adapter
    }
    pub fn catalogue_revision(&self) -> &str {
        &self.catalogue_revision
    }
    pub fn evidence_ref(&self) -> &str {
        &self.evidence_ref
    }
    pub fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }
    pub fn observed_model(&self) -> Option<&str> {
        self.observed_model.as_deref()
    }
}
pub(crate) fn verify_lineage(
    adapter: &str,
    p: &RouteProvenance<'_>,
) -> Result<VerifiedLineage, ProviderError> {
    if adapter != p.route.adapter || adapter != p.outcome.adapter() {
        return Err(ProviderError::IdentityMismatch);
    }
    let observed = p.outcome.model();
    let family = if observed == Some(p.route.model.as_str())
        && p.route.catalogue_revision == p.evidence_catalogue_revision
        && !p.evidence_ref.is_empty()
    {
        p.catalogue
            .iter()
            .find(|entry| entry.adapter == adapter && Some(entry.model.as_str()) == observed)
            .map(|entry| entry.family.clone())
    } else {
        None
    };
    Ok(VerifiedLineage {
        adapter: adapter.into(),
        observed_model: observed.map(str::to_owned),
        catalogue_revision: p.route.catalogue_revision.clone(),
        family,
        evidence_ref: p.evidence_ref.into(),
    })
}

pub trait Adapter: Sync {
    fn id(&self) -> &'static str;
    /// Activation is a provider-owned recording gate, independent of cache/config:
    /// true when any mode of this adapter is enabled in the recorded matrix.
    fn execution_enabled(&self) -> bool {
        false
    }
    fn account(&self, state: &StreamState) -> Result<AccountDetection, ProviderError>;
    fn preflight(&self, input: &PreflightInput) -> Result<PreflightPlan, ProviderError>;
    fn decode_preflight(
        &self,
        plan: &PreflightPlan,
        capture: &[u8],
    ) -> Result<AvailabilityRecord, ProviderError>;
    fn prepare(&self, input: PrepareInput<'_>) -> Result<LaunchSpec, ProviderError>;
    fn stream(&self, continuation: Option<&Continuation>) -> Result<StreamState, ProviderError>;
    /// If a later line fails, events from earlier lines in that call are
    /// retained in `finish().final_events()` alongside the malformed outcome.
    fn observe(
        &self,
        state: &mut StreamState,
        bytes: &[u8],
    ) -> Result<Vec<ProviderEvent>, ProviderError>;
    fn finish(
        &self,
        state: StreamState,
        exit: ExitObservation,
    ) -> Result<ProviderOutcome, ProviderError>;
    fn continuation(
        &self,
        outcome: &ProviderOutcome,
        route: &ResolvedRoute,
        grant_digest: &str,
    ) -> Result<Continuation, ProviderError>;
    fn validate_provenance(
        &self,
        provenance: &RouteProvenance<'_>,
    ) -> Result<VerifiedLineage, ProviderError>;
}
