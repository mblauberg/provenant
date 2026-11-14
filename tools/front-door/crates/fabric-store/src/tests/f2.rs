use super::*;
use provenant_core::{AttemptId, HostId};
use provenant_records::custody::*;
fn holder(who: u8) -> Holder {
    Holder::Chair {
        chair: Chair {
            extra: Default::default(),
            host: HostId::from_bytes([1; 16]),
            actor: actor(who),
            session: Some(format!("session-{who}")),
            process: None,
        },
    }
}
#[test]
fn custody_is_durable_advisory_and_queryable_by_declared_kinds() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    let original = holder(1);
    let adopter = holder(2);
    for &kind in SubjectKind::ALL {
        let id = if kind.is_path() {
            f.temp
                .path()
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{kind}-item")
        };
        let subject = Subject::new(kind, id);
        ledger
            .acquire_custody(
                project(),
                &subject,
                &original,
                Origin::Provenant,
                &f.request(),
            )
            .unwrap();
        let changed = ledger
            .acquire_custody(project(), &subject, &adopter, Origin::Adopted, &f.request())
            .unwrap();
        assert_eq!(changed.previous_holder, Some(original.clone()));
        assert_eq!(changed.record.holder, adopter);
        assert_eq!(changed.record.state, CustodyState::Active);
        ledger
            .change_custody_state(
                project(),
                &subject,
                CustodyState::Released,
                adopter.actor(),
                &f.request(),
            )
            .unwrap();
        assert_eq!(
            ledger
                .custody_item(project(), &subject, &f.request())
                .unwrap()
                .unwrap()
                .state,
            CustodyState::Released
        );
    }
    let active = ledger
        .custody(
            &CustodyQuery {
                state: Some(StateKind::Active),
                ..Default::default()
            },
            &f.request(),
        )
        .unwrap();
    assert!(active.items.is_empty());
    let released = ledger
        .custody(
            &CustodyQuery {
                project: Some(project()),
                state: Some(StateKind::Released),
                holder: Some(adopter),
                ..Default::default()
            },
            &f.request(),
        )
        .unwrap();
    for &kind in SubjectKind::ALL {
        assert!(
            released
                .items
                .iter()
                .any(|record| record.subject.kind == kind)
        );
    }
}
#[test]
fn custody_queries_page_and_include_nested_lanes_without_crossing_chairs() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    let Holder::Chair { chair } = holder(1) else {
        unreachable!()
    };
    let nested = Holder::Attempt {
        attempt: AttemptId::from_bytes([3; 16]),
        chair: Some(chair.clone()),
    };
    for n in 1..=4 {
        ledger
            .acquire_custody(
                ProjectId::from_bytes([n; 16]),
                &Subject::new(SubjectKind::Branch, format!("branch-{n}")),
                if n % 2 == 0 { &nested } else { &holder(2) },
                Origin::Provenant,
                &f.request(),
            )
            .unwrap();
    }
    let mut query = CustodyQuery {
        chair: Some(chair),
        limit: 1,
        ..Default::default()
    };
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let page = ledger.custody(&query, &f.request()).unwrap();
        for record in page.items {
            assert_eq!(record.holder, nested);
            assert!(seen.insert(record.project.to_string()));
        }
        match page.next {
            Some(next) => {
                assert!(next > query.after);
                query.after = next;
            }
            None => break,
        }
    }
    assert!(!seen.is_empty());
}
#[test]
fn custody_event_failure_rolls_back_the_record() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    ledger.0.db.execute_batch("CREATE TRIGGER reject_event BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT,'fault'); END;").unwrap();
    let subject = Subject::new(SubjectKind::Issue, "3078");
    assert!(
        ledger
            .acquire_custody(
                project(),
                &subject,
                &holder(1),
                Origin::Provenant,
                &f.request()
            )
            .is_err()
    );
    assert!(
        ledger
            .custody_item(project(), &subject, &f.request())
            .unwrap()
            .is_none()
    );
}
#[test]
fn custody_retains_evidence_links_and_extensions_after_settlement() {
    let f = Fixture::new();
    let ledger = HostStore::open(&f.root, &f.request()).unwrap();
    let subjects = [
        Subject::new(SubjectKind::Branch, "feature"),
        Subject::new(SubjectKind::Pr, "42"),
    ];
    for subject in &subjects {
        ledger
            .acquire_custody(
                project(),
                subject,
                &holder(1),
                Origin::Provenant,
                &f.request(),
            )
            .unwrap();
    }
    ledger
        .link_custody(project(), &subjects, &f.request())
        .unwrap();
    ledger
        .0
        .db
        .execute(
            "UPDATE custody SET record=CAST(json_set(CAST(record AS TEXT),'$.future',42) AS BLOB)",
            [],
        )
        .unwrap();
    let evidence = LandedEvidence {
        predicate: LandingPredicate::Landed,
        source: LandingSource::ForgeMerged {
            pr: "42".into(),
            head: "0123456789abcdef0123456789abcdef01234567".parse().unwrap(),
            merge_commit: None,
        },
    };
    for state in [
        CustodyState::Landed {
            evidence: evidence.clone(),
        },
        CustodyState::Retired {
            retirement: RetirementRecord {
                reason: "landed".into(),
                evidence,
                retired_at: f.now(),
            },
        },
    ] {
        let record = ledger
            .change_custody_state(
                project(),
                &subjects[0],
                state.clone(),
                holder(1).actor(),
                &f.request(),
            )
            .unwrap();
        assert_eq!(record.state, state);
        assert!(record.links.contains(&subjects[1]));
        assert_eq!(record.extra["future"], serde_json::json!(42));
    }
}
