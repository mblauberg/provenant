//! Format-2 records: the stop table and the canonical encoding of each record.
use crate::handoff::{Handoff, HandoffStatus};
use crate::{
    CanonicalV1, Continuation, FindingDecision, FindingDisposition, Limit, LimitSource,
    LimitStrength, Limits, NextStep, OnStop, Pause, PauseCause, STOP_TABLE, Stop, StopCause,
    StopFacts, WorkKey, decode,
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
    let rows: Vec<(String, String, bool, u8)> = section
        .lines()
        .take_while(|line| !line.starts_with("## "))
        .filter_map(|line| {
            let cells: Vec<&str> = line
                .trim()
                .strip_prefix("| `")?
                .split('|')
                .map(|cell| cell.trim().trim_matches('`'))
                .collect();
            Some((
                cells[0].into(),
                cells[1].into(),
                cells[2] == "yes",
                cells[3].parse().unwrap(),
            ))
        })
        .collect();
    let expected: Vec<(String, String, bool, u8)> = STOP_TABLE
        .iter()
        .map(|rule| {
            let step = serde_json::to_value(rule.default).unwrap();
            (
                rule.cause.as_str().into(),
                step["step"].as_str().unwrap().into(),
                rule.pinned,
                rule.retries,
            )
        })
        .collect();
    assert_eq!(rows, expected);
}

const RELEASED: StopFacts = StopFacts {
    retries: 0,
    custody_released: true,
};

fn override_of(cause: StopCause, step: NextStep) -> OnStop {
    OnStop::new([(cause, step)].into())
}

#[test]
fn an_override_moves_only_an_unpinned_cause() {
    let resume = NextStep::SameRoute {
        continuation: Continuation::Resume,
    };
    let none = OnStop::default();
    assert_eq!(
        StopCause::StoppedEarly.next_step(&none, RELEASED),
        NextStep::NewRoute
    );
    assert_eq!(
        StopCause::StoppedEarly.next_step(&override_of(StopCause::StoppedEarly, resume), RELEASED),
        resume
    );
    for pinned in STOP_TABLE
        .iter()
        .filter(|rule| rule.pinned)
        .map(|rule| rule.cause)
    {
        assert_eq!(
            pinned.next_step(&override_of(pinned, resume), RELEASED),
            NextStep::Chair
        );
    }
    assert_eq!(NextStep::NewRoute.continuation(), Some(Continuation::Fresh));
    assert_eq!(resume.continuation(), Some(Continuation::Resume));
    assert_eq!(NextStep::Chair.continuation(), None);
}

/// Every cause, derived from the enum: its default after its same-route
/// retries, one override, and the chair while custody is unproved.
#[test]
fn every_cause_takes_its_default_its_override_and_the_chair_without_custody() {
    let fresh = NextStep::SameRoute {
        continuation: Continuation::Fresh,
    };
    let none = OnStop::default();
    for cause in StopCause::ALL {
        let rule = cause.rule();
        for retries in 0..rule.retries {
            let facts = StopFacts {
                retries,
                custody_released: true,
            };
            assert_eq!(
                cause.next_step(&none, facts),
                fresh,
                "{cause} retry {retries}"
            );
        }
        let spent = StopFacts {
            retries: rule.retries,
            custody_released: true,
        };
        assert_eq!(
            cause.next_step(&none, spent),
            rule.default,
            "{cause} default"
        );
        let other = if rule.default == NextStep::Chair {
            NextStep::NewRoute
        } else {
            NextStep::Chair
        };
        let overridden = cause.next_step(&override_of(cause, other), RELEASED);
        assert_eq!(
            overridden,
            if rule.pinned { rule.default } else { other },
            "{cause} override"
        );
        // An override of another cause leaves this one alone.
        let elsewhere = StopCause::ALL
            .into_iter()
            .find(|candidate| *candidate != cause)
            .unwrap();
        assert_eq!(
            cause.next_step(&override_of(elsewhere, other), spent),
            rule.default
        );
        for on_stop in [none.clone(), override_of(cause, fresh)] {
            assert_eq!(
                cause.next_step(&on_stop, StopFacts::default()),
                NextStep::Chair,
                "{cause} without released custody"
            );
        }
    }
}

#[test]
fn on_stop_reads_a_step_object_or_its_name() {
    let on_stop: OnStop = serde_json::from_value(json!({
        "usage_limited": "chair",
        "process_crash": "same_route",
        "provider_stop": {"step": "new_route"},
        "lost": {"step": "same_route", "continuation": "fresh"},
    }))
    .unwrap();
    assert_eq!(on_stop.get(StopCause::UsageLimited), Some(NextStep::Chair));
    let fresh = NextStep::SameRoute {
        continuation: Continuation::Fresh,
    };
    assert_eq!(on_stop.get(StopCause::ProcessCrash), Some(fresh));
    assert_eq!(
        on_stop.get(StopCause::ProviderStop),
        Some(NextStep::NewRoute)
    );
    assert_eq!(on_stop.get(StopCause::Lost), Some(fresh));
    assert_eq!(on_stop.get(StopCause::Deadline), None);
    let encoded = serde_json::to_value(&on_stop).unwrap();
    assert_eq!(serde_json::from_value::<OnStop>(encoded).unwrap(), on_stop);
    assert!(serde_json::from_value::<OnStop>(json!({"not_a_cause": "chair"})).is_err());
    assert!(serde_json::from_value::<OnStop>(json!({"lost": "park"})).is_err());
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
    assert_eq!(
        stop.next_step(&OnStop::default(), RELEASED),
        NextStep::Chair
    );
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
        &Limit::new(3, LimitSource::Launch, LimitStrength::Default),
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

/// A declared writer stop restarts with its edits whether or not it committed
/// them, while a host custody fact still outranks the writer's account.
#[test]
fn a_declared_stop_survives_uncommitted_edits_but_not_uncertain_custody() {
    use crate::run::Outcome;
    let stopped = Handoff::from_json(
        br#"{"schema_version":2,"status":"stopped_early","note":"n","remaining":["r"],"decisions":[],"verify":[]}"#,
    )
    .unwrap();
    let cause = |outcome, handoff: Option<&Handoff>| {
        Stop::sealed(None, outcome, handoff.cloned())
            .unwrap()
            .map(|stop| stop.cause())
    };
    for outcome in [Outcome::ReviewRequired, Outcome::Uncommitted] {
        assert_eq!(
            cause(outcome, Some(&stopped)),
            Some(StopCause::StoppedEarly),
            "{outcome:?}"
        );
    }
    assert_eq!(
        cause(Outcome::Uncommitted, None),
        Some(StopCause::Uncommitted)
    );
    for (outcome, host) in [
        (Outcome::Lost, StopCause::Lost),
        (Outcome::Unsettled, StopCause::Unsettled),
        (Outcome::Quarantined, StopCause::Quarantined),
    ] {
        assert_eq!(cause(outcome, Some(&stopped)), Some(host), "{outcome:?}");
    }
}
