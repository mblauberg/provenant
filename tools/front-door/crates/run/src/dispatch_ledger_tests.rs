use super::*;

#[test]
fn write_grant_warning_threshold_does_not_refuse_large_terminal_ledger() -> TestResult {
    struct Raised(Confined);
    impl Providers for Raised {
        fn write_grant_limit(
            &self,
            _policy: &provenant_records::policy::Policy,
        ) -> WriteGrantLimit {
            WriteGrantLimit {
                bytes: 256 * 1024,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            }
        }
        fn prepare(
            &self,
            task: &TaskRequest,
            prompt: &str,
            paths: &LaunchPaths,
            resume: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, crate::PrepareRefusal> {
            self.0.prepare(task, prompt, paths, resume)
        }
        fn decode(
            &self,
            route: &RouteRecord,
            output: &[u8],
            exit: &ExitObservation,
            path: &std::path::Path,
        ) -> std::result::Result<Decoded, String> {
            self.0.decode(route, output, exit, path)
        }
    }
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let context = context(temp.path())?;
    let store = RunStore::open(&context, &call)?;
    let prepared = input();
    store.create(&prepared, &call)?;
    let id = prepared.attempt_id;
    let provider = Raised(Confined {
        writer: false,
        tool_writes: (0..6000)
            .map(|i| PathGrant {
                path: format!("/fixture/tools/{i}/{}", "x".repeat(3000)).into(),
                scope: PathScope::Subtree,
            })
            .collect(),
    });
    let started = store.start(
        prepared.attempt_id,
        &provider,
        &|record, _| {
            let route = record.applied_route.as_ref().expect("applied route");
            assert_eq!(route.extra["write_grant_limit"]["value"], 256 * 1024);
            assert_eq!(
                route.extra["write_grant_limit"]["origin"],
                "host configuration"
            );
            Err(RunError::Validation("fixture holder"))
        },
        &call,
    )?;
    assert!(matches!(
        started.as_slice(),
        [Started::Refused {
            kind: RefusalKind::Handoff,
            ..
        }]
    ));
    let record = store.record(id, &call)?;
    assert!(
        provenant_records::CanonicalV1::bytes(&record.write_grants)?
            .as_bytes()
            .len()
            > 16 * RECORD_LIMIT as usize
    );
    let attempt = store.attempt_dir(id, false)?;
    let terminal =
        crate::artifacts::read_sealed(&attempt, "terminal-pending.json", TERMINAL_RECORD_LIMIT)?
            .ok_or("terminal decision")?;
    assert!(terminal.len() > 16 * RECORD_LIMIT as usize);
    let terminal_record = crate::db::decode_attempt(&terminal)?;
    assert_eq!(terminal_record.write_grants, record.write_grants);
    // The explicit fixture handoff error survives; capacity never masks it.
    assert!(matches!(&record.state,
        AttemptState::Terminal { outcome: Outcome::Rejected, reason: Some(reason), .. }
        if reason.contains("fixture holder")));
    let expected_tools: Vec<_> = (0..6000)
        .map(|i| format!("/fixture/tools/{i}/{}", "x".repeat(3000)))
        .collect();
    let actual_tools = record
        .write_grants
        .iter()
        .filter(|grant| grant.source == WriteSource::ToolWrite)
        .map(|grant| Ok(grant.path.to_path_buf()?.to_string_lossy().into_owned()))
        .collect::<std::result::Result<Vec<_>, provenant_records::PathWireError>>()?;
    assert_eq!(actual_tools, expected_tools);
    drop(store);
    let reopened = RunStore::open(&context, &call)?;
    assert_eq!(
        reopened.record(id, &call)?.write_grants,
        record.write_grants
    );
    Ok(())
}
#[test]
fn admission_default_spacing_does_not_hold_a_second_writer() -> TestResult {
    let temp = tempfile::tempdir()?;
    let (clock, cancel) = (clock(), CancelToken::new());
    let call = call(&clock, &cancel);
    let store = RunStore::open(&context(temp.path())?, &call)?;
    let [first, second] = writers(&store, [80, 81], &call)?;
    assert!(store.claim(first, &clock, &call)?.is_some());
    assert!(store.claim(second, &clock, &call)?.is_some());
    Ok(())
}
