use super::*;
struct Fixed(i64);
impl Clock for Fixed {
    fn now_ms(&self) -> i64 {
        self.0
    }
}
#[test]
fn landing_generation_and_expiry_fence() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let who = Identity {
        project: "project".into(),
        agent_id: "alice".into(),
        provider: "codex".into(),
    };
    let sha = "a".repeat(40);
    let first = store.acquire_landing(&who, "s", &sha, 1, 1000).unwrap();
    assert!(matches!(
        store.acquire_landing(&who, "b", &sha, 1, 1000),
        Err(Error::LandingHeld { .. })
    ));
    let next = store.acquire_landing(&who, "b", &sha, 60, 2000).unwrap();
    assert_eq!(next.generation, first.generation + 1);
    assert_eq!(next.taken_over_from.as_deref(), Some("alice/s"));
    assert!(matches!(
        store.verify_landing(&who, "s", first.generation, &sha, 2000),
        Err(Error::StaleLanding)
    ));
}
#[test]
fn sessions_fence_active_turns_and_keep_last_clean_pointer() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let who = Identity {
        project: "p".into(),
        agent_id: "a".into(),
        provider: "codex".into(),
    };
    assert!(matches!(
        store.claim_session_turn(&who, "Review", None, SessionTurnKind::Start, 123, "claim-a"),
        Ok(SessionClaim::Claim { .. })
    ));
    assert!(matches!(
        store.claim_session_turn(&who, "Review", None, SessionTurnKind::Start, 124, "claim-b"),
        Ok(SessionClaim::Conflict { .. })
    ));
    store
        .bind_session_turn(
            &who,
            "Review",
            "claim-a",
            &SessionLaunch {
                run_id: "r".into(),
                task_id: "t".into(),
                attempt: 1,
            },
        )
        .unwrap();
    store
        .settle_session_turn(&who, "Review", "wrong", "failed", None)
        .unwrap();
    assert!(store
        .session("p", "Review")
        .unwrap()
        .unwrap()
        .turn_status
        .is_none());
    let pointer = SessionPointer {
        adapter: Some("codex".into()),
        provider_session_id: Some("native".into()),
        run_id: "r".into(),
        task_id: "t".into(),
        attempt: 1,
        result_path: None,
    };
    store
        .settle_session_turn(&who, "Review", "claim-a", "ok", Some(&pointer))
        .unwrap();
    store
        .claim_session_turn(
            &who,
            "Review",
            Some("claim-a"),
            SessionTurnKind::Resume,
            125,
            "claim-b",
        )
        .unwrap();
    store
        .settle_session_turn(&who, "Review", "claim-b", "failed", None)
        .unwrap();
    assert_eq!(
        store
            .session("p", "Review")
            .unwrap()
            .unwrap()
            .provider_session_id
            .as_deref(),
        Some("native")
    );
    assert!(store.session("p", "review").unwrap().is_none());
    assert!(store.forget_session(&who, "Review").unwrap());
}

#[test]
fn wait_ignores_push_start_and_observes_release_without_a_transaction() {
    use std::{cell::Cell, rc::Rc};
    #[derive(Clone)]
    struct Time(Rc<Cell<i64>>);
    impl Clock for Time {
        fn now_ms(&self) -> i64 {
            self.0.get()
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let time = Time(Rc::new(Cell::new(1000)));
    let store = Store::open(&path, 5000, time.clone()).unwrap();
    let raw = Connection::open(&path).unwrap();
    let who = Identity {
        project: "p".into(),
        agent_id: "a".into(),
        provider: "codex".into(),
    };
    store
        .acquire_landing(&who, "s", &"a".repeat(40), 60, 1000)
        .unwrap();
    let mut polls = 0;
    let result = store
        .wait_landing(
            "p",
            1.0,
            100,
            || false,
            |duration| {
                assert!(store.db.is_autocommit()); // The callback can perform another writer's work.
                time.0.set(time.0.get() + duration.as_millis() as i64);
                polls += 1;
                if polls == 1 {
                    raw.execute(
                        "UPDATE landing_leases SET pushing_until=151000 WHERE project='p'",
                        [],
                    )
                    .unwrap();
                } else {
                    raw.execute(
                        "UPDATE landing_leases SET released_at=1200 WHERE project='p'",
                        [],
                    )
                    .unwrap();
                }
            },
        )
        .unwrap();
    assert!(result.changed);
    assert!(result.lease.is_none());
    assert_eq!(polls, 2);
    let idle = store
        .wait_landing(
            "p",
            1.0,
            100,
            || true,
            |_| panic!("cancelled wait must not poll"),
        )
        .unwrap();
    assert!(!idle.changed);
}

#[test]
fn push_hold_survives_expiry_and_takeover_waits_for_both_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let who = Identity {
        project: "p".into(),
        agent_id: "a".into(),
        provider: "codex".into(),
    };
    let sha = "a".repeat(40);
    store.acquire_landing(&who, "s", &sha, 1, 1000).unwrap();
    store
        .db
        .execute("UPDATE landing_leases SET pushing_until=151000", [])
        .unwrap();
    assert!(store.landing_lease("p", 2000).unwrap().is_some());
    assert!(matches!(
        store.try_release_landing(&who, "s", 1, Some(2000)).unwrap(),
        LandingRelease::Refused {
            reason: ReleaseReason::Pushing,
            ..
        }
    ));
    assert!(store.renew_landing(&who, "s", 1, 60, 2000).is_err());
    assert!(matches!(
        store
            .acquire_landing(&who, "b", &sha, 60, 151000)
            .unwrap()
            .taken_over_from
            .as_deref(),
        Some("a/s")
    ));
}

#[test]
fn message_log_failure_rolls_back_message_and_all_deliveries() {
    struct NoEntropy;
    impl pending::IdSource for NoEntropy {
        fn uuid(&self) -> Result<String> {
            Err(invalid("entropy failed"))
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let a = Identity {
        project: "p".into(),
        agent_id: "alice".into(),
        provider: "codex".into(),
    };
    let b = Identity {
        agent_id: "bob".into(),
        ..a.clone()
    };
    store.announce(&a).unwrap();
    store.announce(&b).unwrap();
    store.db.execute_batch("CREATE TRIGGER fail_log BEFORE INSERT ON activity BEGIN SELECT RAISE(ABORT,'log blocked'); END").unwrap();
    assert!(store
        .send(
            &a,
            "bob",
            "hello",
            SendOptions::default(),
            RecipientContext::default(),
            "m"
        )
        .is_err());
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM messages", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    store.db.execute_batch("DROP TRIGGER fail_log").unwrap();
    store
        .send(
            &a,
            "bob",
            "hello",
            SendOptions::default(),
            RecipientContext::default(),
            "m",
        )
        .unwrap();
    assert!(store
        .inbox(
            &b,
            InboxOptions {
                busy_timeout_ms: 1,
                ..Default::default()
            },
            &NoEntropy
        )
        .is_err());
    assert!(store.db.is_autocommit());
    assert_eq!(
        store
            .db
            .query_row("SELECT count(*) FROM delivery_claims", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .db
            .pragma_query_value(None, "busy_timeout", |r| r.get::<_, i64>(0))
            .unwrap(),
        5000
    );
}

#[test]
fn digest_truncates_groups_preserves_null_tasks_and_ignores_expired_messages() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1_300_000_000)).unwrap();
    let a = Identity {
        project: "p".into(),
        agent_id: "a".into(),
        provider: "codex".into(),
    };
    let b = Identity {
        agent_id: "b".into(),
        ..a.clone()
    };
    store.announce(&a).unwrap();
    store.announce(&b).unwrap();
    for n in 0..21 {
        let sender = Identity {
            agent_id: format!("sender{n:02}"),
            ..a.clone()
        };
        store
            .send(
                &sender,
                "b",
                &format!("line{n}\r\n{}", "🙂".repeat(100)),
                SendOptions::default(),
                RecipientContext::default(),
                &format!("m{n:02}"),
            )
            .unwrap();
    }
    store
        .db
        .execute(
            "UPDATE messages SET created_at=0 WHERE message_id='m00'",
            [],
        )
        .unwrap();
    let digest = store.inbox_digest(&b, None).unwrap();
    assert_eq!(digest.total, 20);
    assert!(!digest.truncated);
    assert!(digest
        .groups
        .iter()
        .all(|g| g.task_id.is_none() && g.summary.chars().count() == 80));
    store
        .db
        .execute(
            "UPDATE messages SET created_at=1300000000 WHERE message_id='m00'",
            [],
        )
        .unwrap();
    let digest = store.inbox_digest(&b, None).unwrap();
    assert_eq!(digest.total, 21);
    assert_eq!(digest.groups.len(), 20);
    assert!(digest.truncated);
    assert_eq!(store.inbox_digest(&b, Some("missing")).unwrap().total, 0);
}

#[test]
fn alias_routing_uses_explicit_chair_parent_and_excludes_sender_from_teams() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let a = Identity {
        project: "p".into(),
        agent_id: "alice".into(),
        provider: "codex".into(),
    };
    for seat in ["alice", "chair", "leader", "parent-seat"] {
        store
            .announce(&Identity {
                agent_id: seat.into(),
                ..a.clone()
            })
            .unwrap();
    }
    let context = || RecipientContext {
        chair: Some("leader"),
        parent: Some("parent-seat"),
    };
    assert_eq!(
        store
            .send(&a, "chair", "hi", SendOptions::default(), context(), "m1")
            .unwrap()
            .recipients,
        ["leader"]
    );
    assert_eq!(
        store
            .send(&a, "/root", "hi", SendOptions::default(), context(), "m2")
            .unwrap()
            .recipients,
        ["parent-seat"]
    );
    assert_eq!(
        store
            .send(
                &a,
                "parent",
                "hi",
                SendOptions::default(),
                RecipientContext {
                    chair: Some("missing"),
                    parent: Some("missing")
                },
                "m3"
            )
            .unwrap()
            .recipients,
        ["chair"]
    );
    store.create_team(&a, "self", &["alice".into()]).unwrap();
    assert!(store
        .send(&a, "self", "hi", SendOptions::default(), context(), "m4")
        .is_err());
}

#[test]
fn diagnostic_incomplete_claim_schema_and_foreign_keys_are_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db");
    let store = Store::open(&path, 5000, Fixed(1000)).unwrap();
    store.db.execute_batch("DROP TABLE delivery_claims; CREATE TABLE delivery_claims(message_id TEXT, project TEXT);").unwrap();
    let report = inspect_snapshot(&store.db, "fixture", "p", DiagnosticMode::Doctor);
    assert_eq!(report.status, "error");
    let claim = report
        .checks
        .unwrap()
        .into_iter()
        .find(|c| c.name == "delivery-claim-schema")
        .unwrap();
    assert!(!claim.ok);
    assert_eq!(
        claim.detail,
        "missing columns: recipient_id, claim_id, claimed_at, expires_at"
    );
}

#[test]
fn only_plain_busy_and_locked_are_retryable() {
    for (code, expected) in [(5, true), (6, true), (517, false), (262, false)] {
        let error = Error::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(code), None));
        assert_eq!(is_sqlite_contention(&error), expected);
    }
}

#[test]
fn landing_release_refusals_round_trip_without_losing_reason() {
    for reason in ["pushing", "expired", "not_held"] {
        let wire = format!(r#"{{"released":false,"reason":"{reason}"}}"#);
        let release: LandingRelease = serde_json::from_str(&wire).unwrap();
        assert!(matches!(release, LandingRelease::Refused { .. }));
        assert_eq!(serde_json::to_string(&release).unwrap(), wire);
    }
}

#[test]
fn panicking_push_clears_its_hold_and_preserves_the_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let who = Identity { project: "p".into(), agent_id: "a".into(), provider: "codex".into() };
    let sha = "a".repeat(40);
    store.acquire_landing(&who, "s", &sha, 60, 1000).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.with_landing_push(&who, "s", 1, &sha, || -> Result<()> { panic!("push panicked") })
    }));
    assert!(panic.is_err());
    assert!(store.landing_lease("p", 1000).unwrap().unwrap().pushing_until_ms.is_none());
    assert!(store.try_release_landing(&who, "s", 1, Some(1000)).unwrap().released());
}

#[test]
fn landing_wait_is_bounded_when_the_clock_is_frozen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db"), 5000, Fixed(1000)).unwrap();
    let mut polls = 0;
    let result = store.wait_landing("p", 0.005, 1, || false, |_| { polls += 1; }).unwrap();
    assert!(!result.changed);
    assert!(polls <= 5);
    for seconds in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, f64::MAX] {
        assert!(matches!(store.wait_landing("p", seconds, 1, || false, |_| {}), Err(Error::NumberOutOfRange)));
    }
}
