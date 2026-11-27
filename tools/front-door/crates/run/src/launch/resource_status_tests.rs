use super::*;
use crate::tests::{call, clock, context, input};
use provenant_core::CancelToken;
use provenant_records::diagnostic::{
    LaneLimit, Layer, PressureLevel, PressureThreshold, ResourceFact,
};
use provenant_records::wire::LaneRow;

struct Dependencies;
impl Providers for Dependencies {
    fn prepare(
        &self,
        _: &TaskRequest,
        _: &str,
        _: &LaunchPaths,
        _: Option<&AttemptRecord>,
    ) -> std::result::Result<PreparedLaunch, Box<PrepareRefusal>> {
        let mut refusal = PrepareRefusal::boxed(
            CauseKind::RouteResolutionRefused,
            "DEPENDENCY_OUTPUT_SECRET",
        );
        refusal.resource = Some(ResourceFact::DependencyPreparation { configured: true });
        Err(refusal)
    }
    fn decode(
        &self,
        _: &RouteRecord,
        _: &[u8],
        _: &ExitObservation,
        _: &std::path::Path,
    ) -> std::result::Result<Decoded, String> {
        unreachable!()
    }
}

#[test]
fn launch_resource_refusals_survive_withdrawal_in_retained_status()
-> std::result::Result<(), Box<dyn std::error::Error>> {
    for handoff in [None, Some(false), Some(true)] {
        let temp = tempfile::tempdir()?;
        let (clock, cancel) = (clock(), CancelToken::new());
        let call = call(&clock, &cancel);
        let context = context(temp.path())?;
        let store = RunStore::open(&context, &call)?;
        let run = input();
        let id = run.attempt_id;
        store.create(&run, &call)?;
        let started = store.start_inner(
            run.attempt_id,
            if handoff.is_some() {
                &crate::dispatch_tests::Ready
            } else {
                &Dependencies
            },
            &|_, _| Ok(Vec::new()),
            &|_, _, _| {
                Err(custody::CustodyError::Admission(if handoff == Some(true) {
                    AdmissionError::LaneCap {
                        active: 2,
                        limit: 2,
                        kind: LaneLimit::HostTotal,
                        origin: Layer::Host,
                    }
                } else {
                    AdmissionError::MemoryPressure {
                        level: PressureLevel::Critical,
                        threshold: PressureThreshold::Warning,
                        origin: Layer::Host,
                    }
                }))
            },
            None,
            &call,
        )?;
        let [Started::Refused { kind, .. }] = started.as_slice() else {
            return Err("launch did not refuse".into());
        };
        let expected = kind.diagnostic().ok_or("missing resource diagnostic")?;
        assert_eq!(store.record(id, &call)?.provider_launched, Some(false));
        drop(store);
        let reopened = RunStore::open(&context, &call)?;
        let record = reopened.record(id, &call)?;
        let (state, reason) = LaneRow::state_and_reason(&record.state);
        assert_eq!(state, "rejected");
        let reason = reason.ok_or("missing retained status")?;
        assert!(
            reason.contains(expected.detail.as_deref().unwrap()),
            "{reason}"
        );
        assert!(
            reason.contains(expected.next.as_deref().unwrap()),
            "{reason}"
        );
        assert!(!reason.contains("DEPENDENCY_OUTPUT_SECRET"));
    }
    Ok(())
}
