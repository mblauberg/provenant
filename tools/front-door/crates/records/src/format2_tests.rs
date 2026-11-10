//! Format-2 records: the stop table and the canonical encoding of each record.
use crate::handoff::{Handoff, HandoffStatus};
use crate::{
    CanonicalV1, Continuation, FindingDecision, FindingDisposition, Limit, LimitSource,
    LimitStrength, Limits, NextStep, Pause, PauseCause, STOP_TABLE, Stop, StopCause, WorkKey,
    decode,
};
use provenant_core::{GitOid, ProjectId};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[test]
fn every_stop_cause_has_exactly_one_default() {
    let causes = StopCause::ALL;
    for cause in causes {
        let name = cause.as_str();
        assert_eq!(
            serde_json::from_value::<StopCause>(json!(name)).unwrap(),
            cause
        );
        let rows = STOP_TABLE.iter().filter(|rule| rule.cause == cause).count();
        assert_eq!(rows, 1, "{name} has {rows} rows");
    }
    assert_eq!(STOP_TABLE.len(), causes.len());
}

#[test]
fn the_spec_table_matches_the_stop_table() {
    let spec = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../docs/specs/run.md"),
    )
    .unwrap();
    let (_, section) = spec.split_once("## What follows a stop").unwrap();
    let rows: Vec<(String, String, bool)> = section
        .lines()
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| {
            let cells: Vec<&str> = line
                .trim()
                .strip_prefix("| `")?
                .split('|')
                .map(|cell| cell.trim().trim_matches('`'))
                .collect();
            Some((cells[0].into(), cells[1].into(), cells[2] == "yes"))
        })
        .collect();
    let expected: Vec<(String, String, bool)> = STOP_TABLE
        .iter()
        .map(|rule| {
            let step = serde_json::to_value(rule.default).unwrap();
            (
                rule.cause.as_str().into(),
                step["step"].as_str().unwrap().into(),
                rule.pinned,
            )
        })
        .collect();
    assert_eq!(rows, expected);
}

#[test]
fn an_override_moves_only_an_unpinned_cause() {
    let resume = NextStep::SameRoute {
        continuation: Continuation::Resume,
    };
    assert_eq!(StopCause::StoppedEarly.next_step(None), NextStep::NewRoute);
    assert_eq!(StopCause::StoppedEarly.next_step(Some(resume)), resume);
    for pinned in [
        StopCause::Blocked,
        StopCause::InputRequired,
        StopCause::Cancelled,
    ] {
        assert!(pinned.rule().pinned);
        assert_eq!(pinned.next_step(Some(resume)), NextStep::Chair);
    }
    assert_eq!(NextStep::NewRoute.continuation(), Some(Continuation::Fresh));
    assert_eq!(resume.continuation(), Some(Continuation::Resume));
    assert_eq!(NextStep::Chair.continuation(), None);
}

fn handoff(status: HandoffStatus, note: &str) -> Handoff {
    Handoff {
        schema_version: 2,
        status,
        note: note.into(),
        remaining: vec!["Owner decision".into()],
        decisions: vec![],
        verify: vec![],
        spec_concerns: vec![],
        extra: Default::default(),
    }
}

#[test]
fn a_declared_stop_needs_the_handoff_that_declared_it() {
    let blocked = handoff(HandoffStatus::Blocked, "Needs the owner to pick a schema.");
    let stop = Stop::declared(blocked.clone()).unwrap().unwrap();
    assert_eq!(stop.cause(), StopCause::Blocked);
    assert_eq!(stop.note(), Some("Needs the owner to pick a schema."));
    assert_eq!(stop.next_step(None), NextStep::Chair);
    assert!(
        Stop::declared(handoff(HandoffStatus::Done, "Finished."))
            .unwrap()
            .is_none()
    );
    assert!(Stop::new(StopCause::Blocked, None).is_err());
    assert!(Stop::new(StopCause::StoppedEarly, Some(blocked.clone())).is_err());
    assert!(Stop::new(StopCause::TokenBudget, Some(blocked)).is_ok());
    let wire = json!({"cause": "blocked"});
    assert!(serde_json::from_value::<Stop>(wire).is_err());
    let mut empty = handoff(HandoffStatus::StoppedEarly, "x");
    empty.note = " ".into();
    assert!(Stop::new(StopCause::StoppedEarly, Some(empty)).is_err());
}

#[test]
fn records_refuse_values_outside_their_bounds() {
    let project = ProjectId::from_bytes([7; 16]);
    assert!(WorkKey::new(project, "g", "n", 0).is_err());
    assert!(WorkKey::new(project, "", "n", 1).is_err());
    assert!(WorkKey::new(project, "g", "a/b", 1).is_err());
    assert!(WorkKey::new(project, "g".repeat(65), "n", 1).is_err());
    let key = WorkKey::new(project, "w2a", "implement", 2).unwrap();
    assert_eq!(key.to_string(), "w2a/implement#2");
    assert_eq!(key.next_attempt().unwrap().attempt(), 3);
    let mut wire = serde_json::to_value(&key).unwrap();
    wire["attempt"] = json!(0);
    assert!(serde_json::from_value::<WorkKey>(wire).is_err());
    assert!(Pause::new(PauseCause::LoopPaused, None, "").is_err());
    assert!(Pause::new(PauseCause::LoopPaused, Some("a b".into()), "Paused.").is_err());
    let oid = GitOid::parse(&"a".repeat(40)).unwrap();
    assert!(FindingDecision::new("", FindingDisposition::Act, "Fix.", oid, Some(oid)).is_err());
    assert!(FindingDecision::new("f1", FindingDisposition::Act, "", oid, Some(oid)).is_err());
}

/// Encode canonically, decode, and compare; then add an unknown field and
/// decode again. `keeps` says whether the record retains that field.
fn round_trip<T>(value: &T, keeps: bool)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let bytes = CanonicalV1::bytes(value).unwrap();
    let decoded: T = decode(bytes.as_bytes(), Limits::default()).unwrap();
    assert_eq!(&decoded, value);
    assert_eq!(CanonicalV1::bytes(&decoded).unwrap(), bytes);

    let mut wire: Value = serde_json::from_slice(bytes.as_bytes()).unwrap();
    wire.as_object_mut()
        .unwrap()
        .insert("from_a_newer_writer".into(), json!({"kept": true}));
    let newer = serde_json::to_vec(&wire).unwrap();
    let decoded: T = decode(&newer[..], Limits::default()).unwrap();
    let again: Value =
        serde_json::from_slice(CanonicalV1::bytes(&decoded).unwrap().as_bytes()).unwrap();
    if keeps {
        assert_eq!(again, wire);
    } else {
        assert_eq!(&decoded, value);
    }
}

#[test]
fn each_record_round_trips_and_keeps_an_unknown_field() {
    let key = WorkKey::new(ProjectId::from_bytes([7; 16]), "w2a", "implement", 1).unwrap();
    round_trip(&key, false);
    for step in [
        NextStep::SameRoute {
            continuation: Continuation::Resume,
        },
        NextStep::SameRoute {
            continuation: Continuation::Fresh,
        },
        NextStep::NewRoute,
        NextStep::Chair,
    ] {
        round_trip(&step, false);
    }
    let mut blocked = handoff(HandoffStatus::Blocked, "Needs an owner decision.");
    blocked.extra.insert("future".into(), json!(1));
    round_trip(&Stop::declared(blocked).unwrap().unwrap(), true);
    round_trip(&Stop::new(StopCause::Lost, None).unwrap(), true);
    round_trip(
        &Limit::new(3, LimitSource::Launch, LimitStrength::Enforced),
        true,
    );
    let head = GitOid::parse(&"a".repeat(40)).unwrap();
    let base = GitOid::parse(&"b".repeat(64)).unwrap();
    round_trip(
        &FindingDecision::new(
            "lens-1/F3",
            FindingDisposition::Replace,
            "Replace the guard with one owner.",
            head,
            Some(base),
        )
        .unwrap(),
        true,
    );
    round_trip(
        &FindingDecision::new(
            "lens-1/F4",
            FindingDisposition::Act,
            "No base recorded.",
            head,
            None,
        )
        .unwrap(),
        true,
    );
    round_trip(
        &Pause::new(
            PauseCause::Stop(StopCause::Blocked),
            Some("implement".into()),
            "writer implement is blocked; inspect its handoff",
        )
        .unwrap(),
        true,
    );
    round_trip(
        &Pause::new(PauseCause::ContinuationBudget, None, "Budget spent.").unwrap(),
        true,
    );
}
