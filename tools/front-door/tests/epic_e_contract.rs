//! Cross-owner fixture for the agent-native slice 0 contracts.
//! Spec: docs/specs/agent-native.md#2-unifying-model-five-primitives

use crate::fabric::progress::{
    WakeClaim, WakeOutcome, WakeReason, WakeSource, WakeSubjectRef, wake_subject,
};
use provenant_cli::{RenderMode, render};
use provenant_core::{AttemptId, Deadline, DurationMicros, MonoTime, ProjectId, WallTime};
use provenant_records::command_result::{ErrorClass, SettingRef};
use provenant_records::custody::{
    Admit, Handover, LeaseObservation, LeaseState, Sharing, Subject, WorktreeLease, admit_writer,
    default_sharing,
};
use provenant_records::{
    CommandError, CommandResult, Layer, Outcome, Source, Warning, WarningCode,
};

// All exported setting-family precedences remain subsequences of the canonical order.
const _: () = {
    use provenant_records::provenance::assert_canonical_subsequence as check;
    check(provenant_config::ADMISSION_PRECEDENCE.0);
    check(provenant_config::OVERLAY_PRECEDENCE.0);
    check(provenant_config::KNOB_PRECEDENCE.0);
    check(provenant_config::SCHEMA_PRECEDENCE.0);
    check(provenant_fabric_runtime::config::LAND_PRECEDENCE.0);
    check(provenant_fabric_runtime::config::ADMIN_PUBLICATION_PRECEDENCE.0);
    check(provenant_fabric_runtime::graph::template::SEALED_SETTINGS_PRECEDENCE.0);
    check(provenant_fabric_runtime::graph::template::TEMPLATE_PRECEDENCE.0);
};

#[test]
fn fake_wake_handover_and_error_render_share_the_contract() -> Result<(), Box<dyn std::error::Error>>
{
    let now = MonoTime::from_micros(1_000);
    let deadline = Deadline::after(now, DurationMicros::from_micros(10_000))?;
    let child = deadline.child(now, DurationMicros::from_micros(2_000));
    assert_eq!(child.remaining(now).as_micros(), 2_000);
    let project = ProjectId::from_bytes([1; 16]);
    struct FakeWake {
        now: MonoTime,
    }
    impl WakeSource for FakeWake {
        fn reconcile(&mut self, _: &WakeSubjectRef, _: WakeReason) -> Result<(), WakeOutcome> {
            Ok(())
        }
        fn claim(&mut self, _: &WakeSubjectRef, _: Deadline) -> WakeClaim {
            WakeClaim::Ready { revision: 1 }
        }
        fn live_revision(&self, _: &WakeSubjectRef) -> u64 {
            1
        }
        fn advance(
            &mut self,
            subject: &WakeSubjectRef,
            reason: WakeReason,
            budget: Deadline,
        ) -> WakeOutcome {
            assert!(matches!(subject, WakeSubjectRef::Graph { id, .. } if id == "fake"));
            assert_eq!(reason, WakeReason::Wait);
            assert_eq!(budget.remaining(self.now).as_micros(), 2_000);
            WakeOutcome::Advanced { transitions: 1 }
        }
    }
    let outcome = wake_subject(
        WakeSubjectRef::Graph {
            project,
            id: "fake".into(),
        },
        WakeReason::Wait,
        child,
        &mut FakeWake { now },
    );
    assert_eq!(outcome, WakeOutcome::Advanced { transitions: 1 });

    let old = AttemptId::from_bytes([2; 16]);
    let new = AttemptId::from_bytes([3; 16]);
    let at = WallTime::from_micros(1)?;
    let observation = LeaseObservation {
        lease: WorktreeLease {
            holder: old,
            since: at,
            sharing: Sharing::Inherit,
        },
        state: LeaseState::Lapsed {
            attempt_state: "stopped".into(),
        },
    };
    let tree = Subject::worktree("/example", "generation");
    assert!(matches!(
        admit_writer(&tree, Some(&observation), &default_sharing()),
        Admit::Proceed { .. }
    ));
    let handover = Handover {
        from: old,
        to: new,
        at,
        head: None,
        uncommitted: vec!["src/file.rs".into()],
    };
    assert_eq!(handover.uncommitted, ["src/file.rs"]);

    let setting = SettingRef {
        key: "lanes.worktree.sharing".into(),
        value: "refuse".into(),
        layer: Layer::Launch,
        origin: "--worktree-sharing".into(),
    };
    let warning = Warning::new(
        WarningCode("WRITER_HANDOVER"),
        "Previous work was retained",
        Source {
            layer: Layer::Launch,
            origin: Some("fixture".into()),
        },
    )
    .setting(setting.clone());
    let error = CommandError::new(
        "WORKTREE_SHARING_REFUSED",
        "Worktree is held",
        None,
        "provenant wait lane-1",
    )
    .with_class(ErrorClass::Configured)
    .with_setting(setting);
    let mut result = CommandResult::new("run", Outcome::Error, ());
    result.warnings.push(warning);
    result.error = Some(error);
    let mut output = Vec::new();
    render(&result, RenderMode::Json, &mut output)?;
    let envelope: serde_json::Value = serde_json::from_slice(&output)?;
    assert_eq!(envelope["warnings"][0]["code"], "WRITER_HANDOVER");
    assert_eq!(
        envelope["error"]["setting"]["key"],
        "lanes.worktree.sharing"
    );
    assert_eq!(envelope["next"][0]["argv"][0], "provenant");
    Ok(())
}
