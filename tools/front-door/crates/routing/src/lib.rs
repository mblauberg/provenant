//! Pure explicit routing over immutable snapshots. No process or state effects.
#![forbid(unsafe_code)]
mod catalog;
mod facts;
mod policy;
mod request;
mod resolve;
mod review;
pub use catalog::*;
pub use facts::*;
pub use policy::*;
pub use provenant_records::ResolvedRoute;
pub use request::*;
pub use resolve::*;
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
    #[error("unknown adapter")]
    UnknownAdapter,
    #[error("unknown named route")]
    UnknownRoute,
    #[error("route is not permitted for confidential input")]
    ConfidentialModel,
    #[error("no eligible pool member: {exclusions:?}")]
    NoEligiblePoolMember { exclusions: Vec<String> },
    #[error("{model} does not support the requested effort {effort}; {support}")]
    UnsupportedEffort {
        model: String,
        effort: String,
        support: String,
    },
    #[error("{model} is not an admitted OpenCode route; choose one of: {admitted}")]
    UnlistedOpenCodeModel { model: String, admitted: String },
    #[error("invalid catalogue")]
    Catalogue,
    #[error("invalid availability record")]
    Availability,
    #[error("invalid review demand")]
    ReviewDemand,
}

impl RoutingError {
    /// Stable identifier for the refusal, independent of its display text.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ReadOnlyPolicy => "read_only_policy",
            Self::InvalidModel => "invalid_model",
            Self::InvalidValue => "invalid_value",
            Self::Alternatives => "alternatives",
            Self::UnknownAdapter => "unknown_adapter",
            Self::UnknownRoute => "unknown_route",
            Self::ConfidentialModel => "confidential_model",
            Self::NoEligiblePoolMember { .. } => "no_eligible_pool_member",
            Self::UnsupportedEffort { .. } => "unsupported_effort",
            Self::UnlistedOpenCodeModel { .. } => "unlisted_opencode_model",
            Self::Catalogue => "catalogue",
            Self::Availability => "availability",
            Self::ReviewDemand => "review_demand",
        }
    }
}

#[cfg(test)]
mod tests;
