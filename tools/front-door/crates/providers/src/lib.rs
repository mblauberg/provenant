//! Sans-I/O provider mechanics. No processes, credential reads or mutable registry.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod agy;
pub mod claude;
pub mod codex;
pub mod codex_rollout;
mod contract;
pub mod cursor;
pub mod identity;
mod lane;
mod launch;
pub use lane::LaneRuntime;
pub mod opencode;
pub mod quota;
mod stream;
pub use contract::*;
pub use launch::{Access, LaunchSpec, PrepareInput, Session, SetupStep, Stdin, mode_enabled};
pub use stream::*;

use provenant_quota::QuotaSource;

pub struct Registration {
    pub adapter: &'static dyn Adapter,
    pub quota_source: Option<&'static dyn QuotaSource>,
}

/// One registry for execution and quota.
pub static REGISTRY: &[Registration] = &[
    Registration {
        adapter: &codex::Codex,
        quota_source: Some(&codex::Codex),
    },
    Registration {
        adapter: &claude::Claude,
        quota_source: Some(&claude::Claude),
    },
    Registration {
        adapter: &cursor::Cursor,
        quota_source: Some(&cursor::Cursor),
    },
    Registration {
        adapter: &agy::Agy,
        quota_source: Some(&agy::Agy),
    },
    Registration {
        adapter: &opencode::OpenCode,
        quota_source: Some(&opencode::OpenCode),
    },
];

// Shared mechanics; each adapter supplies its own `prepare` builder.
macro_rules! adapter {
    ($ty:ty, $id:literal, $prepare:path) => {
        impl crate::Adapter for $ty {
            fn id(&self) -> &'static str {
                $id
            }
            fn execution_enabled(&self) -> bool {
                crate::launch::any_mode_enabled($id)
            }
            fn account(
                &self,
                state: &crate::StreamState,
            ) -> Result<crate::AccountDetection, crate::ProviderError> {
                state.check_adapter($id)?;
                Ok(if state.authentication_required() {
                    crate::AccountDetection::AuthRequired
                } else {
                    crate::AccountDetection::Unknown
                })
            }
            fn preflight(
                &self,
                _: &crate::PreflightInput,
            ) -> Result<crate::PreflightPlan, crate::ProviderError> {
                Err(crate::ProviderError::Disabled { adapter: $id })
            }
            fn decode_preflight(
                &self,
                plan: &crate::PreflightPlan,
                _: &[u8],
            ) -> Result<crate::AvailabilityRecord, crate::ProviderError> {
                match *plan {}
            }
            fn prepare(
                &self,
                input: crate::PrepareInput<'_>,
            ) -> Result<crate::LaunchSpec, crate::ProviderError> {
                let (access, session) = crate::launch::admit($id, &input)?;
                let mut stream = crate::StreamState::new($id, decode);
                if let Some(continuation) = input.continuation {
                    continuation.bind_stream($id, &mut stream)?;
                }
                $prepare(&input, access, session, stream)
            }
            fn stream(
                &self,
                continuation: Option<&crate::Continuation>,
            ) -> Result<crate::StreamState, crate::ProviderError> {
                let mut state = crate::StreamState::new($id, decode);
                if let Some(continuation) = continuation {
                    continuation.bind_stream($id, &mut state)?;
                }
                Ok(state)
            }
            fn observe(
                &self,
                state: &mut crate::StreamState,
                bytes: &[u8],
            ) -> Result<Vec<crate::ProviderEvent>, crate::ProviderError> {
                state.check_adapter($id)?;
                state.observe(bytes)
            }
            fn finish(
                &self,
                state: crate::StreamState,
                exit: crate::ExitObservation,
            ) -> Result<crate::ProviderOutcome, crate::ProviderError> {
                state.check_adapter($id)?;
                state.finish(exit)
            }
            fn continuation(
                &self,
                outcome: &crate::ProviderOutcome,
                route: &provenant_records::ResolvedRoute,
                grant_digest: &str,
            ) -> Result<crate::Continuation, crate::ProviderError> {
                crate::Continuation::from_outcome($id, outcome, route, grant_digest, MODEL_ON_WIRE)
            }
            fn validate_provenance(
                &self,
                provenance: &crate::RouteProvenance<'_>,
            ) -> Result<crate::VerifiedLineage, crate::ProviderError> {
                crate::verify_lineage($id, provenance)
            }
        }
    };
}
pub(crate) use adapter;

macro_rules! quota_source {
    ($ty:ty, $id:literal) => {
        impl provenant_quota::QuotaSource for $ty {
            fn adapter_id(&self) -> &'static str {
                $id
            }
            fn probe(
                &self,
                account: &provenant_quota::AccountScope,
                input: &provenant_quota::QuotaProbeInput,
            ) -> Result<provenant_quota::QuotaProbePlan, provenant_quota::QuotaError> {
                crate::quota::plan($id, account, input)
            }
            fn decode(
                &self,
                plan: &provenant_quota::QuotaProbePlan,
                capture: &provenant_quota::ProbeCapture,
            ) -> Result<provenant_quota::QuotaSnapshot, provenant_quota::QuotaError> {
                crate::quota::decode(plan, capture)
            }
        }
    };
}
pub(crate) use quota_source;

#[cfg(test)]
mod tests;
