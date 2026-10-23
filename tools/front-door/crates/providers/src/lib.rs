//! Sans-I/O provider mechanics. No processes, credential reads or mutable registry.
#![forbid(unsafe_code)]

pub mod agy;
pub mod claude;
pub mod codex;
mod contract;
pub mod cursor;
pub mod opencode;
mod stream;
pub use contract::*;
pub use stream::*;

use provenant_quota::QuotaSource;

pub struct Registration {
    pub adapter: &'static dyn Adapter,
    pub quota_source: Option<&'static dyn QuotaSource>,
}

/// One registry for execution and quota. OpenCode has no supported quota source.
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
        quota_source: None,
    },
];

// All invocation forms in step 0's activation.json are disabled. Keep shared
// refusal mechanics here, rather than speculative per-provider launch builders.
macro_rules! adapter {
    ($ty:ty, $id:literal, $argv:literal) => {
        impl crate::Adapter for $ty {
            fn id(&self) -> &'static str {
                $id
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
            ) -> Result<crate::AdapterLaunchSpec, crate::ProviderError> {
                if input.route.adapter != $id {
                    return Err(crate::ProviderError::IdentityMismatch);
                }
                if $argv {
                    input.prompt.check_argv()?;
                }
                if let Some(continuation) = input.continuation {
                    continuation.validate(input.route, input.grant_digest)?;
                }
                Err(crate::ProviderError::Disabled { adapter: $id })
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
                crate::Continuation::from_outcome($id, outcome, route, grant_digest)
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
                _: &provenant_quota::AccountScope,
                _: &provenant_quota::QuotaProbeInput,
            ) -> Result<provenant_quota::QuotaProbePlan, provenant_quota::QuotaError> {
                Err(provenant_quota::QuotaError::Disabled)
            }
            fn decode(
                &self,
                plan: &provenant_quota::QuotaProbePlan,
                _: &provenant_quota::ProbeCapture,
            ) -> Result<provenant_quota::QuotaSnapshot, provenant_quota::QuotaError> {
                match *plan {}
            }
        }
    };
}
pub(crate) use quota_source;

#[cfg(test)]
mod tests;
