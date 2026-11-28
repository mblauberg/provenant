use super::*;

#[test]
fn inventory_pages_use_ordered_index_scans_without_project_sort() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(include_str!("schema.sql")).unwrap();
    for all in [false, true] {
        for newest in [false, true] {
            for cursor in [false, true] {
                let sql = format!(
                    "EXPLAIN QUERY PLAN {}",
                    inventory_page_sql(all, newest, cursor)
                );
                let mut stmt = db.prepare(&sql).unwrap();
                let plan: Vec<String> = stmt
                    .query_map(
                        rusqlite::params![
                            "project",
                            if cursor { 42 } else { 0 },
                            all,
                            101,
                            Option::<String>::None,
                            Option::<i64>::None
                        ],
                        |row| row.get(3),
                    )
                    .unwrap()
                    .map(|row| row.unwrap())
                    .collect();
                assert!(
                    !plan.iter().any(|line| line.contains("TEMP B-TREE")),
                    "{plan:?}"
                );
                assert!(
                    plan.iter().any(|line| line.contains("terminal_attempts")),
                    "{plan:?}"
                );
            }
        }
    }
}

// Spec: docs/specs/lane-contracts.md#result-projection
#[test]
fn observed_commands_are_unknown_when_no_adapter_exposes_them() {
    use provenant_records::lane_contract::Coverage;

    let commands = super::observed_commands();
    assert_eq!(commands.completeness, Coverage::Unknown);
    assert!(commands.value.is_none());
    assert_eq!(commands.source, "unavailable");
}

#[test]
fn usage_identity_requires_a_verified_registered_applied_and_observed_key() {
    use provenant_records::{AccountIdentity, run::RouteRecord};

    let route = RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/sonnet".into()),
        account_key: Some("verified-key".into()),
        ..Default::default()
    };
    assert_eq!(
        usage_identity(Some(&route), Some(&route), Some(&route)),
        AccountIdentity::provider_observation("claude", "verified-key")
    );

    let requested_only = usage_identity(Some(&route), None, Some(&route));
    assert_eq!(requested_only, AccountIdentity::unknown());

    let mut mismatch = route.clone();
    mismatch.account_key = Some("other-key".into());
    assert_eq!(
        usage_identity(Some(&route), Some(&route), Some(&mismatch)),
        AccountIdentity::unknown()
    );

    let local = RouteRecord {
        provider: Some("agy".into()),
        model: Some("agy/gemini".into()),
        account_key: Some("local".into()),
        ..Default::default()
    };
    assert_eq!(
        usage_identity(Some(&local), Some(&local), Some(&local)),
        AccountIdentity::unknown()
    );
}

#[test]
fn slot_wait_is_queue_to_start_and_absent_until_started() {
    let queued = WallTime::from_micros(1_000_000).unwrap();
    let started = WallTime::from_micros(2_500_000).unwrap();
    assert_eq!(slot_wait_ms(queued, Some(started)), Some(1_500));
    assert_eq!(slot_wait_ms(queued, None), None);
    assert_eq!(slot_wait_ms(started, Some(queued)), None);
}

#[test]
fn graph_labels_come_from_the_work_context() {
    let context = provenant_records::run::WorkContext {
        tier: Some("senior".into()),
        template: Some("review.security".into()),
        parent_node: Some("map".into()),
        ..Default::default()
    };
    assert_eq!(
        graph_labels(Some(&context)),
        (
            Some("senior".into()),
            Some("review.security".into()),
            Some("map".into())
        )
    );
    assert_eq!(graph_labels(None), (None, None, None));
}

// Spec: docs/specs/cli.md#output-references-and-bounded-tails
#[test]
fn a_nonterminal_attempt_reads_as_an_unsealed_page() {
    use provenant_core::CancelToken;

    let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let context = crate::tests::context(temp.path())?;
        let clock = crate::tests::clock();
        let cancel = CancelToken::new();
        let call = crate::tests::call(&clock, &cancel);
        let store = RunStore::open(&context, &call)?;
        let input = crate::tests::input();
        let id = input.attempt_id;
        store.create(&input, &call)?;
        assert!(!matches!(
            store.record(id, &call)?.state,
            AttemptState::Terminal { .. }
        ));

        // A live attempt has no sealed result. The reader must not invent
        // bytes, a length or completeness; the CLI labels the empty page as
        // no-result-yet rather than a partial result.
        for part in [ArtifactPart::Result, ArtifactPart::Stderr] {
            let chunk = store.read_output(id, part, ByteRange::default(), &context, &call)?;
            assert!(chunk.bytes.is_empty(), "{part:?}");
            assert_eq!(chunk.len, 0, "{part:?}");
            assert!(!chunk.complete, "{part:?}");
        }
        Ok(())
    })();
    assert!(result.is_ok(), "{result:?}");
}
