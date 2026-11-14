//! Sans-I/O provider mechanics, apart from `quota_live`: the live quota edge
//! that runs probes and reads credentials. No mutable registry.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod agy;
pub mod claude;
pub mod codex;
pub mod codex_rollout;
pub mod command;
mod contract;
pub mod cursor;
pub mod duration;
mod dylib;
pub use dylib::{ClosureOutcome, DependencyClosure, IncompleteReason};
pub mod host;
pub mod identity;
mod lane;
mod launch;
mod skill_stage;
pub use lane::{BuildSlotGate, BuildSlotSetupOutcome, GhShim, LaneRuntime};
pub use skill_stage::{SkillWarning, StagedUserSkills};
pub mod opencode;
pub mod quota;
pub mod quota_live;
mod stream;
mod swift;
pub use swift::toolchain_available as swift_toolchain_available;
pub mod update;
pub use contract::*;
pub use launch::{
    Access, AppliedAccess, LaunchSpec, OmissionSource, OmittedGrant, PrepareInput,
    PreparedToolRequirement, Session, SetupStep, Stdin,
};
pub use stream::*;

pub trait QuotaSource: Sync {
    fn adapter_id(&self) -> &'static str;
    fn probe(
        &self,
        account: &provenant_quota::AccountScope,
        input: &provenant_quota::QuotaProbeInput,
    ) -> Result<provenant_quota::QuotaProbePlan, provenant_quota::QuotaError>;
    fn capture(
        &self,
        live: &quota_live::LiveQuota,
        plan: &provenant_quota::QuotaProbePlan,
        previous: Option<&provenant_quota::AccountRecord>,
    ) -> Result<provenant_quota::ProbeCapture, provenant_quota::ProbeFailure>;
    fn decode(
        &self,
        plan: &provenant_quota::QuotaProbePlan,
        capture: &provenant_quota::ProbeCapture,
    ) -> Result<provenant_quota::QuotaSnapshot, provenant_quota::QuotaError>;
}

#[derive(Clone, Copy)]
pub struct Registration {
    pub adapter: &'static dyn Adapter,
    pub quota_source: Option<&'static dyn QuotaSource>,
    pub stdout_decoder: Option<&'static dyn provenant_os::capture::FrameDecoder>,
    pub stderr_decoder: Option<&'static dyn provenant_os::capture::FrameDecoder>,
    pub display_name: &'static str,
    pub quota: provenant_quota::QuotaPolicy,
}

// Preserve dispatch order independently of the order used to install harnesses.
const fn ordered_registrations<const N: usize>(
    mut rows: [(u8, Registration); N],
) -> [Registration; N] {
    let mut i = 1;
    while i < N {
        let mut j = i;
        while j > 0 && rows[j].0 < rows[j - 1].0 {
            rows.swap(j, j - 1);
            j -= 1;
        }
        i += 1;
    }
    let mut result = [rows[0].1; N];
    let mut i = 0;
    while i < N {
        result[i] = rows[i].1;
        i += 1;
    }
    result
}

macro_rules! registrations {
    ($( $(#[$cfg:meta])* $module:ident : $order:literal,)*) => {
        /// One registry for execution and quota.
        pub static REGISTRY: &[Registration] = &ordered_registrations([
            $( $(#[$cfg])* ($order, $module::REGISTRATION),)*
        ]);
    };
}
provenant_records::adapter_implementation_bindings!(registrations);

pub fn quota_policies(registry: &[Registration]) -> Vec<provenant_quota::QuotaPolicy> {
    registry
        .iter()
        .filter(|registration| registration.quota_source.is_some())
        .map(|registration| registration.quota)
        .collect()
}

/// Registry policies with `[domains.quota.polling_intervals]` applied. Scheduling and staleness checks use this, not `quota_policies`.
pub fn scheduling_policies(
    settings: &provenant_quota::Settings,
    registry: &[Registration],
) -> Vec<provenant_quota::QuotaPolicy> {
    let mut policies = quota_policies(registry);
    settings.apply_polling_intervals(&mut policies);
    policies
}

// Shared mechanics; each adapter supplies its own `prepare` builder.
macro_rules! adapter {
    ($ty:ty, $id:literal, $prepare:path $(, $diagnostic:path)?) => {
        impl crate::Adapter for $ty {
            $(fn failure_diagnostic(&self, stderr: &[u8]) -> Option<String> {
                $diagnostic(stderr)
            })?
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
            fn prepare(
                &self,
                input: crate::PrepareInput<'_>,
            ) -> Result<crate::LaunchSpec, crate::ProviderError> {
                let (access, session) = crate::launch::admit($id, &input)?;
                let mut stream = crate::StreamState::new($id, decode);
                if let Some(continuation) = input.continuation {
                    continuation.bind_stream($id, &mut stream)?;
                }
                let mut spec = $prepare(&input, access, session, stream)?;
                crate::launch::prepare_tool_requirements(&input, &mut spec)?;
                crate::launch::private_git_objects(&input, &mut spec)?;
                crate::launch::prepare_tool_requirements(&input, &mut spec)?;
                crate::launch::finalise_provider_env(&input, &mut spec);
                Ok(spec)
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
    ($ty:ty, $id:literal, $capture:path, $decode:path) => {
        impl crate::QuotaSource for $ty {
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
            fn capture(
                &self,
                live: &crate::quota_live::LiveQuota,
                plan: &provenant_quota::QuotaProbePlan,
                previous: Option<&provenant_quota::AccountRecord>,
            ) -> Result<provenant_quota::ProbeCapture, provenant_quota::ProbeFailure> {
                $capture(live, plan, previous)
            }
            fn decode(
                &self,
                plan: &provenant_quota::QuotaProbePlan,
                capture: &provenant_quota::ProbeCapture,
            ) -> Result<provenant_quota::QuotaSnapshot, provenant_quota::QuotaError> {
                $decode(plan, capture)
            }
        }
    };
}
pub(crate) use quota_source;

#[cfg(test)]
mod seatbelt_snapshots;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod readonly_tests;
