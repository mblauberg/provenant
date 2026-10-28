//! Pure explicit routing over immutable snapshots. No process or state effects.
#![forbid(unsafe_code)]
mod catalog;
mod facts;
mod request;
mod resolve;
mod review;
pub use catalog::*;
pub use facts::*;
pub use provenant_records::ResolvedRoute;
pub use request::*;
pub use resolve::*;
pub use review::*;

#[derive(Debug, thiserror::Error)]
pub enum RoutingError {
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
    #[error("invalid catalogue")]
    Catalogue,
    #[error("invalid availability record")]
    Availability,
    #[error("invalid review demand")]
    ReviewDemand,
}

#[cfg(test)]
mod tests;
