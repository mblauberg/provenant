//! Typed readiness observations and their freshness boundary.
use serde::{Deserialize, Serialize};

/// Identity of the route whose readiness was observed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessScope {
    pub host: String,
    pub adapter: String,
    pub account_key: String,
    /// Canonical model or verified pool identifier.
    pub route: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessReason {
    NotProbed,
    Stale,
    ProbeTimeout,
    CommandFailure,
    MissingExecutable,
    SignedOut,
    AccountMismatch,
    Exhausted,
    ProviderRefusal,
    UncalibratedPartialCoverage,
    InvalidEvidence,
    UnreachablePeer,
    UnsupportedPeerVersion,
}

/// An observation is valid only for its exact scope and before its expiry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteReadiness {
    pub schema_version: u32,
    pub scope: ReadinessScope,
    pub state: ReadinessState,
    pub reason: Option<ReadinessReason>,
    pub observed_unix_seconds: i64,
    pub expires_unix_seconds: i64,
    pub reset_unix_seconds: Option<i64>,
    pub evidence_ref: Option<String>,
}

impl RouteReadiness {
    /// Read an observation; absence, invalid scope and stale data are unknown.
    /// A reset does not change the observation's state.
    pub fn read(
        record: Option<&Self>,
        scope: &ReadinessScope,
        now: i64,
    ) -> (ReadinessState, Option<ReadinessReason>) {
        let Some(record) = record else {
            return (ReadinessState::Unknown, Some(ReadinessReason::NotProbed));
        };
        if record.schema_version != 1
            || record.scope != *scope
            || record.observed_unix_seconds > now
            || record.expires_unix_seconds <= record.observed_unix_seconds
            || record.evidence_ref.as_ref().is_some_and(|r| {
                r.is_empty()
                    || r.len() > 256
                    || !r
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            })
        {
            return (
                ReadinessState::Unknown,
                Some(ReadinessReason::InvalidEvidence),
            );
        }
        if now >= record.expires_unix_seconds {
            return (ReadinessState::Unknown, Some(ReadinessReason::Stale));
        }
        (record.state, record.reason)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_round_trip_and_boundaries() {
        let scope = ReadinessScope {
            host: "host".into(),
            adapter: "agy".into(),
            account_key: "account".into(),
            route: "model".into(),
        };
        let record = RouteReadiness {
            schema_version: 1,
            scope: scope.clone(),
            state: ReadinessState::Unavailable,
            reason: Some(ReadinessReason::ProviderRefusal),
            observed_unix_seconds: 10,
            expires_unix_seconds: 20,
            reset_unix_seconds: Some(15),
            evidence_ref: Some("probe:1".into()),
        };
        let decoded: RouteReadiness =
            serde_json::from_slice(&serde_json::to_vec(&record).unwrap()).unwrap();
        assert_eq!(record, decoded);
        assert_eq!(
            RouteReadiness::read(None, &scope, 10).0,
            ReadinessState::Unknown
        );
        assert_eq!(
            RouteReadiness::read(Some(&record), &scope, 9).0,
            ReadinessState::Unknown
        );
        assert_eq!(
            RouteReadiness::read(Some(&record), &scope, 15).0,
            ReadinessState::Unavailable
        );
        assert_eq!(
            RouteReadiness::read(Some(&record), &scope, 20).0,
            ReadinessState::Unknown
        );
        let mut other = scope.clone();
        other.route = "other".into();
        assert_eq!(
            RouteReadiness::read(Some(&record), &other, 15).0,
            ReadinessState::Unknown
        );
    }
}
