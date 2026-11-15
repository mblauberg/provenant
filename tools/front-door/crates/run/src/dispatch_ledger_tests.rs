use super::*;

#[test]
fn write_grant_warning_threshold_preserves_a_large_terminal_record() -> TestResult {
    struct Raised(Confined);
    impl Providers for Raised {
        fn write_grant_limit(
            &self,
            _policy: &provenant_records::policy::Policy,
        ) -> WriteGrantLimit {
            WriteGrantLimit {
                bytes: provenant_records::run::MAX_WRITE_GRANT_BYTES,
                origin: provenant_records::diagnostic::SettingSource::Configuration,
            }
        }
        fn prepare(
            &self,
            task: &TaskRequest,
            prompt: &str,
            paths: &LaunchPaths,
            resume: Option<&AttemptRecord>,
        ) -> std::result::Result<PreparedLaunch, Box<crate::PrepareRefusal>> {
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
    let sample_grant = WriteGrantRecord {
        extra: Default::default(),
        source: WriteSource::ToolWrite,
        path: provenant_records::AbsolutePath::new(std::path::Path::new("/fixture/tools/0"))?,
        scope: PathScope::Subtree,
    };
    let sample_grant_bytes = provenant_records::CanonicalV1::bytes(&[sample_grant])?
        .as_bytes()
        .len();
    let tool_count =
        provenant_records::run::MAX_WRITE_GRANT_BYTES as usize / sample_grant_bytes + 1;
    let provider = Raised(Confined {
        writer: false,
        tool_writes: (0..tool_count)
            .map(|index| PathGrant {
                path: format!("/fixture/tools/{index}").into(),
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
            > provenant_records::run::MAX_WRITE_GRANT_BYTES as usize
    );
    let attempt = store.attempt_dir(id, false)?;
    let terminal =
        crate::artifacts::read_sealed(&attempt, "terminal-pending.json", TERMINAL_RECORD_LIMIT)?
            .ok_or("terminal decision")?;
    assert!(terminal.len() > provenant_records::run::MAX_WRITE_GRANT_BYTES as usize);
    let terminal_record = crate::db::decode_attempt(&terminal)?;
    assert_eq!(terminal_record.write_grants, record.write_grants);
    // The explicit fixture handoff error survives; capacity never masks it.
    assert_eq!(
        record.state.terminal_cause(),
        Some(TerminalCause::HandoffRefused)
    );
    let expected_tools: std::collections::BTreeSet<_> = (0..tool_count)
        .map(|index| format!("/fixture/tools/{index}"))
        .collect();
    let actual_tools = record
        .write_grants
        .iter()
        .filter(|grant| grant.source == WriteSource::ToolWrite)
        .map(|grant| Ok(grant.path.to_path_buf()?.to_string_lossy().into_owned()))
        .collect::<std::result::Result<
            std::collections::BTreeSet<_>,
            provenant_records::PathWireError,
        >>()?;
    assert_eq!(actual_tools, expected_tools);
    drop(store);
    let reopened = RunStore::open(&context, &call)?;
    assert_eq!(
        reopened.record(id, &call)?.write_grants,
        record.write_grants
    );
    Ok(())
}
