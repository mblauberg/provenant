//! Pure explicit routing over immutable snapshots. No process or state effects.
#![forbid(unsafe_code)]
mod catalog;
mod discovery;
mod monthly;
pub use monthly::*;
mod overlay;
pub use overlay::CatalogueSources;
mod exclusion;
mod facts;
mod fallback;
mod ladder;
mod policy;
mod quality;
mod request;
mod resolve;
mod retry;
mod review;
pub use catalog::*;
pub use discovery::*;
pub use exclusion::*;
pub use facts::*;
pub use fallback::*;
pub use ladder::*;
pub use policy::*;
pub use provenant_records::ResolvedRoute;
pub use quality::*;
pub use request::*;
pub use resolve::*;
pub use retry::*;
pub use review::*;

#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
    #[error("model policy restricts this route to read-only and review work")]
    ReadOnlyPolicy,
    #[error("model must be a bounded exact ADAPTER/ID selector")]
    InvalidModel,
    #[error("invalid route name or effort")]
    InvalidValue,
    #[error("at most two distinct alternatives, different from the primary, are allowed")]
    Alternatives,
    #[error("unknown adapter{}", close(.candidates))]
    UnknownAdapter { candidates: Vec<String> },
    #[error("{0}")]
    UnsupportedLaunch(String),
    #[error("unknown named route")]
    UnknownRoute,
    #[error("route is not permitted for confidential input")]
    ConfidentialModel,
    #[error("no eligible pool member: {exclusions:?}")]
    NoEligiblePoolMember {
        exclusions: Vec<String>,
        /// A uniform account refusal, retained for diagnostics.
        cause: Option<provenant_records::RouteExclusionReason>,
        /// Aggregate local exclusion, even when candidates have different reasons.
        preparation_failure: provenant_records::run::PreparationFailure,
    },
    #[error("{model} does not support the requested effort {effort}; {support}")]
    UnsupportedEffort {
        model: String,
        effort: String,
        support: String,
    },
    #[error("unknown exclusion `{selector}`: {detail}{}", close(.candidates))]
    UnknownExclusion {
        selector: String,
        detail: String,
        candidates: Vec<String>,
    },
    #[error("invalid catalogue")]
    Catalogue,
    #[error("invalid availability record")]
    Availability,
    #[error("invalid review demand")]
    ReviewDemand,
}

fn close(matches: &[String]) -> String {
    close_matches_hint(matches)
}

/// The `; close matches: ...` suffix for an error that cannot carry a typed
/// candidate list, built from the one shared ranker's output.
pub fn close_matches_hint(matches: &[String]) -> String {
    if matches.is_empty() {
        String::new()
    } else {
        format!("; close matches: {}", matches.join(", "))
    }
}

/// Adapter names resolved through the shared name machinery.
struct AdapterCatalogue;

impl provenant_core::Catalogue for AdapterCatalogue {
    type Item = &'static str;
    const KIND: provenant_core::NameKind = provenant_core::NameKind("adapter");

    fn names(&self) -> Vec<String> {
        provenant_records::ADAPTER_SPECS
            .iter()
            .map(|spec| spec.name.to_owned())
            .collect()
    }

    fn lookup(&self, canonical: &str) -> Option<Self::Item> {
        provenant_records::ADAPTER_SPECS
            .iter()
            .find(|spec| spec.name == canonical)
            .map(|spec| spec.name)
    }
}

/// The close adapter names for `name`, ranked by the shared ranker. Callers
/// whose own error type cannot carry a `RoutingError` use this to render the
/// same suggestion without a second ranker.
pub fn adapter_candidates(name: &str) -> Vec<String> {
    match provenant_core::resolve(&AdapterCatalogue, name) {
        Ok(_) => Vec::new(),
        Err(refusal) => refusal.candidates,
    }
}

impl RoutingError {
    /// Refuse an unknown adapter, listing the close adapter names.
    pub fn unknown_adapter(name: &str) -> Self {
        Self::UnknownAdapter {
            candidates: adapter_candidates(name),
        }
    }

    /// Refuse an unknown exclusion, ranking the close selectors from the
    /// providers and models the caller knows.
    pub fn unknown_exclusion(
        selector: &str,
        detail: impl Into<String>,
        universe: impl IntoIterator<Item = &str>,
    ) -> Self {
        let candidates = provenant_core::close_matches(selector, universe)
            .into_iter()
            .map(str::to_owned)
            .collect();
        Self::UnknownExclusion {
            selector: selector.to_owned(),
            detail: detail.into(),
            candidates,
        }
    }

    /// Stable identifier for the refusal, independent of its display text.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ReadOnlyPolicy => "read_only_policy",
            Self::InvalidModel => "invalid_model",
            Self::InvalidValue => "invalid_value",
            Self::Alternatives => "alternatives",
            Self::UnknownAdapter { .. } => "unknown_adapter",
            Self::UnsupportedLaunch(_) => "unsupported_launch",
            Self::UnknownRoute => "unknown_route",
            Self::ConfidentialModel => "confidential_model",
            Self::NoEligiblePoolMember { .. } => "no_eligible_pool_member",
            Self::UnsupportedEffort { .. } => "unsupported_effort",
            Self::UnknownExclusion { .. } => "unknown_exclusion",
            Self::Catalogue => "catalogue",
            Self::Availability => "availability",
            Self::ReviewDemand => "review_demand",
        }
    }
}

#[cfg(test)]
mod tests;
