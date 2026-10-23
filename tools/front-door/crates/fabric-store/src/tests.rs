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

