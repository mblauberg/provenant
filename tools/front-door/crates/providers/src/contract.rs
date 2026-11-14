use crate::{
    ExitObservation, LaunchSpec, PrepareInput, ProviderEvent, ProviderOutcome, StreamState,
};
use provenant_records::ResolvedRoute;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("argv prompt exceeds domains.run.argv_prompt_limit_bytes ({limit} bytes; source: {})", .origin.description())]
    PromptTooLarge {
        limit: usize,
        origin: provenant_records::diagnostic::SettingSource,
    },
    #[error("provider protocol is malformed")]
    Malformed,
    #[error("provider protocol is malformed: {0}")]
    MalformedResult(String),
    #[error("provider stream exceeds its size bound")]
    Overflow,
    #[error("provider/session/account/host/model/controls or grant changed")]
    IdentityMismatch,
    #[error("provider continuation identity is unknown")]
    UnknownIdentity,
    #[error("provider probe could not prepare its private directory: {0}")]
    ProbeDirectory(String),
    #[error(
        "claude reports no login for this config directory; run `claude auth login` there once \
         interactively, or over SSH unlock the login keychain first (`security unlock-keychain`); \
         on a macOS peer, `provenant install` starts the dev.provenant.peer LaunchAgent so peer \
         requests run in the login session (check with `provenant hosts doctor`)"
    )]
    LoginRequired,
    #[error("launch path is not absolute or lies outside the plan's roots")]
    InvalidPath,
    #[error("confinement plan asks for a policy this adapter cannot honour")]
    UnsupportedPolicy,
    #[error(
        "agy isolated keychain {0} is missing or not a regular file; re-register with \
         `provenant accounts register --adapter agy --directory ~ <label>` if this is the \
         login-keychain account, or create the isolated keychain per docs/runbooks/providers.md"
    )]
    IsolatedKeychainUnavailable(String),
    #[error("provider login token is unavailable")]
    CredentialUnavailable,
    #[error("provider login token was not found")]
    CredentialNotFound,
    #[error("provider login token has expired")]
    CredentialExpired,
    // Reasons must never include subprocess output or credential contents.
    #[error("provider credential read failed: {0}")]
    CredentialReadFailed(String),
}

impl ProviderError {
    pub fn resource_fact(&self) -> Option<provenant_records::diagnostic::ResourceFact> {
        match self {
            Self::PromptTooLarge { limit, origin } => Some(
                provenant_records::diagnostic::ResourceFact::ArgvPromptLimit {
                    limit: u64::try_from(*limit).ok()?,
                    origin: *origin,
                },
            ),
            _ => None,
        }
    }
    /// Classify only failures whose typed cause establishes a hard boundary.
    pub fn boundary_class(&self) -> provenant_records::diagnostic::BoundaryClass {
        use provenant_records::diagnostic::BoundaryClass as B;
        match self {
            Self::IdentityMismatch
            | Self::UnknownIdentity
            | Self::InvalidPath
            | Self::UnsupportedPolicy => B::Authority,
            Self::IsolatedKeychainUnavailable(_) | Self::CredentialReadFailed(_) => B::Credentials,
            Self::PromptTooLarge { .. }
            | Self::Malformed
            | Self::MalformedResult(_)
            | Self::Overflow
            | Self::ProbeDirectory(_)
            | Self::LoginRequired
            | Self::CredentialUnavailable
            | Self::CredentialNotFound
            | Self::CredentialExpired => B::None,
        }
    }
}

#[cfg(test)]
#[test]
fn provider_failure_boundary_depends_on_typed_cause() {
    use provenant_records::diagnostic::BoundaryClass as B;
    assert_eq!(
        ProviderError::IdentityMismatch.boundary_class(),
        B::Authority
    );
    assert_eq!(
        ProviderError::UnsupportedPolicy.boundary_class(),
        B::Authority
    );
    assert_eq!(
        ProviderError::CredentialReadFailed("sentinel".into()).boundary_class(),
        B::Credentials
    );
    assert_eq!(ProviderError::LoginRequired.boundary_class(), B::None);
    assert_eq!(
        ProviderError::PromptTooLarge {
            limit: 1,
            origin: provenant_records::diagnostic::SettingSource::ShippedDefault
        }
        .boundary_class(),
        B::None
    );
}

/// Published per attempt as `availability.json`, which the depletion ladder
/// reads back as the stopped attempt's observed readiness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityRecord {
    pub schema_version: u64,
    pub adapter: String,
    pub executable: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_update: Option<crate::update::UpdateRecord>,
    pub fixture_ref: String,
    pub catalogue_revision: String,
    pub account_key: Option<String>,
    pub observed_models: Vec<String>,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
}

/// Match a requested model against a list of offered models reported by an account.
/// Empty offered models means no restriction is known.
pub fn model_matches_offered(requested: &str, offered_models: &[String]) -> bool {
    if offered_models.is_empty() {
        return true;
    }
    offered_models
        .iter()
        .any(|offered| model_matches_single(requested, offered))
}

fn model_matches_single(requested: &str, offered: &str) -> bool {
    fn tokens(name: &str) -> Vec<String> {
        name.strip_prefix("agy/")
            .unwrap_or(name)
            .to_ascii_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
    let (req, off) = (tokens(requested), tokens(offered));
    if req.is_empty() || off.is_empty() {
        return false;
    }
    // Whole tokens only: "mini" never matches inside "gemini". An offered
    // display name ("Gemini Flash") matches a technical id that contains all of
    // its tokens ("gemini-3.8-flash").
    off.iter().all(|t| req.contains(t)) || req.iter().all(|t| off.contains(t))
}

pub struct Prompt {
    text: String,
    file: Option<std::path::PathBuf>,
    argv_limit: usize,
    argv_origin: provenant_records::diagnostic::SettingSource,
}
impl Prompt {
    pub fn new(text: String) -> Self {
        Self {
            text,
            file: None,
            argv_limit: provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES,
            argv_origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
        }
    }
    pub fn from_file(path: std::path::PathBuf) -> Self {
        Self {
            text: format!(
                "Read the file at {} first and treat its full contents as your task instructions.",
                path.display()
            ),
            file: Some(path),
            argv_limit: provenant_confinement::run_config::DEFAULT_ARGV_PROMPT_LIMIT_BYTES,
            argv_origin: provenant_records::diagnostic::SettingSource::ShippedDefault,
        }
    }
    /// Override the compiled argv prompt threshold for this launch.
    pub fn with_argv_limit(
        mut self,
        limit: usize,
        origin: provenant_records::diagnostic::SettingSource,
    ) -> Self {
        self.argv_limit = limit;
        self.argv_origin = origin;
        self
    }
    pub fn file(&self) -> Option<&std::path::Path> {
        self.file.as_deref()
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    pub fn check_argv(&self) -> Result<(), ProviderError> {
        if self.text.len() > self.argv_limit {
            Err(ProviderError::PromptTooLarge {
                limit: self.argv_limit,
                origin: self.argv_origin,
            })
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
    fn authority_error(message: &str, error: ProviderError) -> ProviderError {
        provenant_records::policy::Policy::default().boundary_error(
            "CONTRACT_CONTINUATION_VALIDATE_AUTHORITY",
            provenant_records::policy::Facts::violation(message),
            error,
        )
    }
    pub(crate) fn from_outcome(
        adapter: &str,
        outcome: &ProviderOutcome,
        route: &ResolvedRoute,
        grant_digest: &str,
        model_on_wire: bool,
    ) -> Result<Self, ProviderError> {
        if outcome.adapter() != adapter || route.adapter != adapter {
            return Err(Self::authority_error(
                "provider outcome does not match the applied route adapter",
                ProviderError::IdentityMismatch,
            ));
        }
        let session = outcome.session().ok_or_else(|| {
            Self::authority_error(
                "provider outcome did not identify its session",
                ProviderError::UnknownIdentity,
            )
        })?;
        // Requested models cannot substitute for answering-model evidence.
        // Codex's exec stream has no model position; its resume argv pins the
        // same `-m`, and any model it does report must still match.
        match outcome.model() {
            Some(model) if model == route.model => {}
            None if !model_on_wire => {}
            _ => {
                return Err(Self::authority_error(
                    "provider outcome did not identify its applied model",
                    ProviderError::UnknownIdentity,
                ));
            }
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
            return Err(Self::authority_error(
                "retained session does not match its applied route",
                ProviderError::IdentityMismatch,
            ));
        }
        let account_key = route.account_key.clone().ok_or_else(|| {
            Self::authority_error(
                "applied route has no account identity",
                ProviderError::UnknownIdentity,
            )
        })?;
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
            return Err(Self::authority_error(
                "provider stream does not match the continuation adapter",
                ProviderError::IdentityMismatch,
            ));
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
            return Err(Self::authority_error(
                "continuation identity differs from the resolved route or write grant",
                ProviderError::IdentityMismatch,
            ));
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

/// A recognised provider failure, kept separate from human diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    AuthRequired,
    Provider,
}

pub trait Adapter: Sync {
    fn failure_cause(&self, stderr: &[u8]) -> FailureCause {
        let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
        if provenant_records::AdapterSpec::get(self.id())
            .is_some_and(|spec| spec.auth_markers.iter().any(|marker| text.contains(marker)))
        {
            FailureCause::AuthRequired
        } else {
            FailureCause::Provider
        }
    }
    /// A recognised failure diagnostic, without untrusted stderr prose.
    fn failure_diagnostic(&self, _stderr: &[u8]) -> Option<String> {
        None
    }
    fn id(&self) -> &'static str;
    fn account(&self, state: &StreamState) -> Result<AccountDetection, ProviderError>;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_matches_offered() {
        let flash_only = vec!["Gemini Flash".to_string()];
        assert!(!model_matches_offered("gemini-3.8-pro", &flash_only));
        assert!(!model_matches_offered("agy/gemini-3.8-pro", &flash_only));
        assert!(model_matches_offered("gemini-3.8-flash", &flash_only));
        assert!(model_matches_offered("agy/gemini-3.8-flash", &flash_only));

        let both = vec!["Gemini Flash".to_string(), "Gemini Pro".to_string()];
        assert!(model_matches_offered("gemini-3.8-pro", &both));
        assert!(model_matches_offered("gemini-3.8-flash", &both));

        let empty: Vec<String> = Vec::new();
        assert!(model_matches_offered("gemini-3.8-pro", &empty));

        let technical = vec!["gemini-2.5-flash".to_string()];
        assert!(!model_matches_offered("gemini-3.8-pro", &technical));
        assert!(model_matches_offered("agy/Gemini-2.5-Flash", &technical));
    }
}
