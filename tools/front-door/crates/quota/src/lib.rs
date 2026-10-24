//! Quota policy is pure: immutable observations in, bounded update intents out.
#![forbid(unsafe_code)]
pub mod chair;
mod source;
pub use source::*;
mod settings;
mod state;
pub use settings::*;
pub use state::*;
mod admission;
pub mod schedule;
pub use admission::*;

#[derive(Debug, thiserror::Error)]
pub enum QuotaError {
    #[error("quota source has no qualifying recording")]
    Disabled,
    #[error("quota number is out of range")]
    NumberOutOfRange,
    #[error("invalid quota record")]
    Record(#[from] provenant_records::DecodeError),
    #[error("invalid quota state: {0}")]
    Invalid(&'static str),
    #[error("quota timestamp is out of range")]
    TimeRange,
    #[error("invalid or ambiguous quota reset")]
    InvalidTimestamp,
    #[error("quota revision or refresh claim changed")]
    Conflict,
    #[error("quota admission refused: {0}")]
    Refused(&'static str),
}

/// Provider-local reset times must have one interpretation, including at DST.
pub fn local_reset(
    local: jiff::civil::DateTime,
    zone: jiff::tz::TimeZone,
) -> Result<i64, QuotaError> {
    zone.to_ambiguous_zoned(local)
        .unambiguous()
        .map(|z| z.timestamp().as_second())
        .map_err(|_| QuotaError::InvalidTimestamp)
}
