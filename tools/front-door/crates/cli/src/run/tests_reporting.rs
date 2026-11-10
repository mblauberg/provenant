use super::*;

#[test]
fn projected_cli_error_omits_unsafe_source_and_keeps_bounded_fields() {
    for source in [
        "TOKEN_SENTINEL\u{1b}".to_owned(),
        format!("TOKEN_SENTINEL{}", "é".repeat(2048)),
    ] {
        let failure = super::Fail::from(provenant_run::RunError::Io(std::io::Error::other(source)));
        let reply = super::reply("run", Err(failure));
        let wire = serde_json::to_value(&reply.result).unwrap();
        assert_eq!(wire["error"]["code"], "IO");
        assert!(wire["error"].get("detail").is_none());
        assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
        assert!(wire.to_string().len() < 4096);
        let mut human = Vec::new();
        crate::render::render(
            &reply.result,
            crate::render::RenderMode::Human {
                text: "",
                summary: None,
                read_only: false,
            },
            &mut human,
        )
        .unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(!human.contains("TOKEN_SENTINEL"));
        assert!(!human.chars().any(|c| c == '\u{1b}'));
        assert!(human.len() < 4096);
    }
    let failure = super::Fail::from(provenant_run::RunError::Io(std::io::Error::other(
        "TOKEN_SENTINEL\u{1b}",
    )));
    let reply = super::reply("run", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert_eq!(wire["error"]["code"], "IO");
    assert_eq!(wire["error"]["category"], "error");
    assert!(wire["error"].get("detail").is_none());
    assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    let mut rendered = Vec::new();
    crate::render::render(
        &reply.result,
        crate::render::RenderMode::Human {
            text: "",
            summary: None,
            read_only: false,
        },
        &mut rendered,
    )
    .unwrap();
    assert!(
        !String::from_utf8(rendered)
            .unwrap()
            .contains("TOKEN_SENTINEL")
    );
}
#[test]
fn non_refused_run_error_names_its_safe_cause_only() {
    let failure = super::Fail::from(provenant_run::RunError::OperationIo {
        operation: "read attempt",
        path: Some("/TOKEN_SENTINEL".into()),
        source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, "TOKEN_SENTINEL"),
    });
    let reply = super::reply("telemetry stats", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert_eq!(wire["error"]["category"], "error");
    let cause = wire["error"]["cause"].as_str().unwrap();
    assert!(cause.contains("read attempt"), "{cause}");
    assert!(cause.contains("PermissionDenied"), "{cause}");
    assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    let failure = super::Fail::from(provenant_run::RunError::Contention);
    let reply = super::reply("telemetry stats", Err(failure));
    let wire = serde_json::to_value(&reply.result).unwrap();
    assert!(
        wire["error"]["cause"]
            .as_str()
            .unwrap_or_else(|| panic!("{wire}"))
            .contains("busy timeout")
    );
}
#[test]
fn cli_safe_local_facts_are_bounded_and_escape_controls() {
    use provenant_confinement::worktrees::WorktreeError;
    use provenant_records::diagnostic::MAX_DIAGNOSTIC_BYTES;
    for evidence in [
        "é\u{1b}\nend".to_owned(),
        format!("é\u{1b}{}", "界".repeat(1024)),
    ] {
        let failure = super::Fail::from(provenant_run::RunError::Writer(
            WorktreeError::CensusUnknown {
                path: "/fixture/tree".into(),
                detail: evidence,
            },
        ));
        let reply = super::reply("run", Err(failure));
        let wire = serde_json::to_value(&reply.result).unwrap();
        let detail = wire["error"]["detail"].as_str().unwrap();
        assert!(
            detail.contains("é\\u{1b}\\n") || detail.contains("é\\u{1b}"),
            "{detail:?}"
        );
        assert!(detail.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(!detail.chars().any(char::is_control));
        assert!(wire["error"]["next"].is_string());
        let mut human = Vec::new();
        crate::render::render(
            &reply.result,
            crate::render::RenderMode::Human {
                text: "",
                summary: None,
                read_only: false,
            },
            &mut human,
        )
        .unwrap();
        let human = String::from_utf8(human).unwrap();
        assert!(!human.contains('\u{1b}'));
        assert!(human.contains("é\\u{1b}"));
    }
}
#[test]
fn cli_recorded_remedies_preserve_typed_causes() {
    use provenant_fabric_runtime::coordination::facade::{DispatchRefusalFact, FacadeError};
    use provenant_records::diagnostic::{ResourceFact, SettingSource};
    let cases = [
        (
            FacadeError::DispatchRefused {
                fact: Some(DispatchRefusalFact::DirtyWriter),
                disk: None,
                reason: "TOKEN_SENTINEL".into(),
            },
            "commit",
        ),
        (
            FacadeError::DispatchRefused {
                fact: Some(DispatchRefusalFact::Resource(ResourceFact::Disk {
                    free: 1,
                    floor: 2,
                    origin: SettingSource::Configuration,
                })),
                disk: None,
                reason: "TOKEN_SENTINEL".into(),
            },
            "free disk space",
        ),
        (
            FacadeError::LaunchUnknown {
                operation_id: OperationId::parse("dispatch-17").unwrap(),
            },
            "dispatch-17",
        ),
    ];
    for (error, condition) in cases {
        let projection = error.diagnostic();
        let failure = super::Fail::from(provenant_run::RunError::RecordedRefusal(Box::new(
            projection.clone(),
        )));
        let reply = super::reply("run", Err(failure));
        let wire = serde_json::to_value(&reply.result).unwrap();
        assert_eq!(wire["error"]["next"], projection.next.unwrap());
        let action = wire["error"]["next"].as_str().unwrap();
        assert!(action.contains(condition), "{wire}");
        if condition == "dispatch-17" {
            assert!(action.contains("inspect") && action.contains("same operation ID"));
        }
        assert!(!wire.to_string().contains("TOKEN_SENTINEL"));
    }
}
#[test]
fn malformed_claude_result_preserves_provider_error_in_settlement() {
    let route = run::RouteRecord {
        provider: Some("claude".into()),
        model: Some("claude/fixture".into()),
        ..Default::default()
    };
    let message = "Permission mode forced to default — CLAUDE_CODE_SUBPROCESS_ENV_SCRUB is set";
    let result =
        serde_json::json!({"type":"result", "is_error":true, "errors":[message]}).to_string();
    let earlier = r#"{"type":"result","session_id":"s","subtype":"success","is_error":false,"result":"earlier answer"}"#;
    for (result, reason) in [
        (
            result.as_str(),
            format!("provider protocol is malformed: {message}"),
        ),
        ("{", "provider protocol is malformed".into()),
    ] {
        for suffix in ["", "\n"] {
            let decoded = Registry::default()
                .decode(
                    &route,
                    format!("{earlier}\n{result}{suffix}").as_bytes(),
                    &clean_exit(),
                    Path::new("/unused"),
                )
                .unwrap();
            assert_eq!(decoded.outcome, run::Outcome::Failed);
            assert_eq!(decoded.reason.as_deref(), Some(reason.as_str()));
        }
    }
}
#[test]
fn lane_listing_omits_large_grants_but_id_retains_them() -> Result<(), Box<dyn std::error::Error>> {
    use std::ffi::OsString;
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let grants: Vec<run::WriteGrantRecord> = (0..600).map(|index| {
            serde_json::from_value(json!({
                "source": "tool_write",
                "path": {"relative_to_root": {"encoding":"utf8", "path": format!("home/fixture/.cache/build/{index}/{}output", "directory/".repeat(8))}},
                "scope":"subtree"
            }))
        }).collect::<Result<_, _>>()?;
            let database = rusqlite::Connection::open(
                session.context.state_root.as_path().join("run.sqlite3"),
            )?;
            let mut selected = None;
            for number in 1..=25 {
                let prepared =
                    trace_run(number, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
                session.store.create(&prepared, &call)?;
                let mut lane = session
                    .store
                    .record(prepared.attempts[0].attempt_id, &call)?;
                lane.write_grants = grants.clone();
                // Seed retained launch evidence without starting 25 native providers.
                // The command still opens the store and reads its ordinary lane query.
                assert_eq!(
                    database.execute(
                        "UPDATE attempts SET record=?1 WHERE id=?2",
                        rusqlite::params![serde_json::to_vec(&lane)?, lane.attempt_id.to_string()]
                    )?,
                    1
                );
                selected = Some(Target {
                    selection: None,
                    run: Some(lane.run_id),
                    task: None,
                    attempt: Some(lane.attempt_id),
                });
            }
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(
                ["status", "--all", "--limit", "25"]
                    .into_iter()
                    .map(OsString::from),
            ) else {
                return Err("parse lanes failed".into());
            };
            let crate::Command::Status(mut command) = invocation.command else {
                return Err("not lanes".into());
            };
            let reply = status::status(command.clone(), true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let listing = reply.result.data;
            let rows = listing["rows"].as_array().ok_or("listing rows")?;
            assert_eq!(rows.len(), 25);
            assert!(rows.iter().all(|row| row.get("write_grants").is_none()));
            assert!(serde_json::to_vec(&listing)?.len() < 1024 * 1024);

            command.id = Some(
                selected
                    .ok_or("selected lane")?
                    .attempt
                    .ok_or("attempt")?
                    .to_string(),
            );
            let reply = status::status(command, true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let detail = reply.result.data;
            let rows = detail["rows"].as_array().ok_or("detail rows")?;
            assert_eq!(rows.len(), 1);
            let retained = rows[0]["write_grants"].as_array().ok_or("detail grants")?;
            assert_eq!(retained.len(), grants.len());
            assert!(retained.iter().all(|grant| grant["path"].is_object()));
            assert!(
                serde_json::to_vec(retained)?.len() * 25 > 1024 * 1024,
                "grant evidence alone must reproduce the oversized page"
            );
            Ok(())
        },
    )?
}
#[test]
fn lane_detail_and_output_retain_safe_refusal_remedies() -> Result<(), Box<dyn std::error::Error>> {
    struct Refusing {
        note: String,
    }
    impl run::Providers for Refusing {
        fn prepare(
            &self,
            _: &run::TaskRequest,
            _: &str,
            _: &run::LaunchPaths,
            _: Option<&run::AttemptRecord>,
        ) -> Result<run::PreparedLaunch, run::PrepareRefusal> {
            Err(run::PrepareRefusal {
                boundary_class: provenant_records::diagnostic::BoundaryClass::None,
                resource: None,
                terminal_cause: Some(provenant_records::run::TerminalCause::AccessPolicyRefused),
                terminal_note: Some(self.note.clone()),
                message: "PROVIDER_SENTINEL must not become a public diagnostic".into(),
                skipped_routes: Vec::new(),
                fallback_outcome: None,
            })
        }
        fn decode(
            &self,
            _: &run::RouteRecord,
            _: &[u8],
            _: &ExitObservation,
            _: &Path,
        ) -> Result<run::Decoded, String> {
            unreachable!("a refused launch never reaches a provider")
        }
    }
    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            let prepared = trace_run(10, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
            session.store.create(&prepared, &call)?;
            let note = format!(
                "configured access boundary: {} END_SAFE_NOTE",
                "fixture setting ".repeat(24)
            );
            assert!(note.len() > 160);
            session.store.start(
                prepared.run_id,
                &Refusing { note: note.clone() },
                &|_, _| unreachable!(),
                &call,
            )?;
            let target = Target {
                selection: None,
                run: Some(prepared.run_id),
                task: None,
                attempt: Some(prepared.attempts[0].attempt_id),
            };
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(
                ["status", "--all"]
                    .into_iter()
                    .map(std::ffi::OsString::from),
            ) else {
                return Err("parse lanes failed".into());
            };
            let crate::Command::Status(mut command) = invocation.command else {
                return Err("not lanes".into());
            };
            let reply = status::status(command.clone(), true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (listing, human) = (reply.result.data, reply.human);
            let expected = format!(
                "{}; {note}",
                provenant_records::run::TerminalCause::AccessPolicyRefused.detail()
            );
            assert_eq!(listing["rows"][0]["reason"], expected);
            assert!(
                human.contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{human}"
            );
            assert!(!human.contains(&note), "{human}");
            for text in [serde_json::to_string(&listing)?, human] {
                assert!(!text.contains("PROVIDER_SENTINEL"), "{text}");
            }
            let listing_reply = status::status(command.clone(), false, edge);
            let listing_summary = listing_reply
                .summary
                .as_ref()
                .ok_or("listing summary")?
                .lines
                .join("\n");
            assert!(
                listing_summary
                    .contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{listing_summary}"
            );
            assert!(!listing_summary.contains(&note), "{listing_summary}");
            let crate::ParseOutcome::Invocation(invocation) =
                crate::parse(["status"].into_iter().map(std::ffi::OsString::from))
            else {
                return Err("parse status failed".into());
            };
            let crate::Command::Status(status_command) = invocation.command else {
                return Err("not status".into());
            };
            let status_reply = status::status(status_command, true, edge);
            assert_eq!(status_reply.result.data["rows"][0]["reason"], expected);
            let status_summary = status_reply
                .summary
                .as_ref()
                .ok_or("status summary")?
                .lines
                .join("\n");
            assert!(
                status_summary
                    .contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{status_summary}"
            );
            assert!(!status_summary.contains(&note), "{status_summary}");
            assert!(
                !serde_json::to_string(&status_reply.result.data)?.contains("PROVIDER_SENTINEL")
            );
            command.id = Some(target.attempt.ok_or("attempt")?.to_string());
            let reply = status::status(command.clone(), true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (detail, human) = (reply.result.data, reply.human);
            assert_eq!(detail["rows"][0]["reason"], expected);
            assert!(
                human.contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{human}"
            );
            assert!(!human.contains(&note), "{human}");
            let explicit = status::status(command, false, edge);
            let explicit_summary = explicit
                .summary
                .as_ref()
                .ok_or("explicit summary")?
                .lines
                .join("\n");
            assert!(
                explicit_summary
                    .contains(provenant_records::run::TerminalCause::AccessPolicyRefused.reason()),
                "{explicit_summary}"
            );
            assert!(!explicit_summary.contains(&note), "{explicit_summary}");
            let output_command = || Output {
                attempt: crate::Attempt(target.clone()),
                host: None,
                part: Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65_536,
                trace: false,
            };
            let (data, text) =
                output::output_inner(output_command(), edge, None).map_err(|fail| fail.message)?;
            assert_eq!(data["reason"], expected);
            assert!(text.contains(&expected));
            assert!(!serde_json::to_string(&data)?.contains("PROVIDER_SENTINEL"));
            let reply = output::output(output_command(), edge);
            let mut bytes = Vec::new();
            crate::render(
                &reply.result,
                crate::RenderMode::Compact {
                    text: &reply.human,
                    read_only: true,
                    summary: reply.summary.as_ref(),
                },
                &mut bytes,
            )?;
            let default = String::from_utf8(bytes)?;
            assert!(default.contains(&expected), "{default}");
            assert_eq!(default.matches("END_SAFE_NOTE").count(), 1, "{default}");
            assert!(!default.contains("PROVIDER_SENTINEL"));
            Ok(())
        },
    )?
}
#[test]
fn default_listing_shows_every_active_lane_and_counts_omitted_settled() {
    let mut rows: Vec<_> = (0..crate::limits::SUMMARY_ROWS + 5)
        .map(|n| serde_json::json!({"attempt_id": format!("{n:032x}"), "state": "running"}))
        .collect();
    rows.extend((0..3).map(
        |n| serde_json::json!({"attempt_id": format!("{:032x}", 1000 + n), "state": "succeeded"}),
    ));
    let data = serde_json::json!({ "lanes": rows });
    let summary = status::lane_summary(&status::lane_lines(&data), None);
    let listed = summary
        .lines
        .iter()
        .filter(|line| line.contains(" running"))
        .count();
    assert_eq!(
        listed,
        crate::limits::SUMMARY_ROWS + 5,
        "{:?}",
        summary.lines
    );
    assert!(
        summary
            .lines
            .iter()
            .any(|line| line.starts_with("3 settled lanes not listed")),
        "{:?}",
        summary.lines
    );
    assert_eq!(summary.omitted, 0);
}
#[test]
fn disk_wait_rows_stay_compact_and_explicit_output_keeps_the_sample()
-> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::OpenOptionsExt;
    let now = WallTime::from_micros(100)?;
    let reason = "host capacity: disk space: 23622320128 bytes available; gc.free_floor is 32212254720 bytes (source: host configuration)";
    let lanes: Vec<_> = (1..=20)
        .map(|number| {
            lane_fixture(
                number,
                number,
                run::AttemptState::Starting {
                    extra: Default::default(),
                },
            )
        })
        .collect::<Result<_, _>>()?;
    let waits: Vec<_> = lanes
        .iter()
        .map(|lane| run::LaneWait {
            extra: Default::default(),
            attempt_id: lane.attempt_id,
            lane_class: run::LaneClass::Writer,
            waiting_for: reason.into(),
        })
        .collect();
    let compact = status::compact_status(&lanes, &waits, now, Path::new("/state"));
    assert!(compact.len() <= 2048, "{compact}");
    assert!(compact.contains("disk below gc.free_floor; provenant output ID"));
    assert!(!compact.contains("23622320128"));
    let (data, _) = render_lanes(&lanes, &waits, None, Path::new("/state"), None);
    assert!(
        data["lanes"]
            .as_array()
            .ok_or("rows")?
            .iter()
            .all(|row| row["waiting_for"] == reason)
    );
    let summary = status::lane_summary(&status::lane_lines(&data), None);
    let mut bytes = Vec::new();
    crate::render(
        &CommandResult::new("status", Status::Ok, data),
        crate::RenderMode::Compact {
            text: &compact,
            read_only: true,
            summary: Some(&summary),
        },
        &mut bytes,
    )?;
    assert!(bytes.len() <= 2048, "{}", String::from_utf8_lossy(&bytes));

    let fixture = linked_fixture()?;
    with_edge(
        &fixture.root,
        &fixture.project,
        |edge| -> Result<(), Box<dyn std::error::Error>> {
            let clock = SystemClock::new();
            let cancel = CancelToken::new();
            let call = request(&clock, &cancel).map_err(|fail| fail.message)?;
            let session = open(edge, &call).map_err(|fail| fail.message)?;
            std::fs::write(
                edge.roots.state.as_path().join("config.toml"),
                "schema_version=1\n[gc]\nfree_floor='32212254720B'\n",
            )?;
            let mut prepared = trace_run(10, AttemptId::from_bytes([99; 16]), clock.wall_now()?)?;
            prepared.request.tasks[0].writer = Some(run::WriterRequest {
                extra: Default::default(),
                project: fixture.project.clone(),
                worktree: fixture.tree.clone(),
            });
            session.store.create(&prepared, &call)?;
            let mut lane = session
                .store
                .record(prepared.attempts[0].attempt_id, &call)?;
            lane.state = run::AttemptState::Starting {
                extra: Default::default(),
            };
            let database = rusqlite::Connection::open(
                session.context.state_root.as_path().join("run.sqlite3"),
            )?;
            assert_eq!(
                database.execute(
                    "UPDATE attempts SET record=?1 WHERE id=?2",
                    rusqlite::params![serde_json::to_vec(&lane)?, lane.attempt_id.to_string()]
                )?,
                1
            );
            // Admission reads private guardian checkpoints. Match their mode
            // rather than inheriting the test runner's umask.
            let mut sample = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(artifact(
                    session.context.state_root.as_path(),
                    &lane,
                    "admission-disk.json",
                ))?;
            std::io::Write::write_all(
                &mut sample,
                &serde_json::to_vec(&json!({"generation":lane.generation,"free":23622320128_u64}))?,
            )?;
            drop(sample);
            let target = Target {
                selection: None,
                run: Some(lane.run_id),
                task: None,
                attempt: Some(lane.attempt_id),
            };
            let command = || Output {
                attempt: crate::Attempt(target.clone()),
                host: None,
                part: Part::Result,
                to: None,
                path: false,
                offset: 0,
                tail: None,
                max_bytes: 65_536,
                trace: false,
            };
            let (data, text) =
                output::output_inner(command(), edge, None).map_err(|fail| fail.message)?;
            let expected = format!("{reason}; free disk space or run `provenant gc --apply`");
            assert_eq!(data["reason"], expected);
            assert!(text.contains(&expected), "{text}");
            let selection = lane.attempt_id.to_string();
            let crate::ParseOutcome::Invocation(invocation) = crate::parse(
                ["status", selection.as_str()]
                    .into_iter()
                    .map(std::ffi::OsString::from),
            ) else {
                return Err("parse lanes failed".into());
            };
            let crate::Command::Status(detail_command) = invocation.command else {
                return Err("not lanes".into());
            };
            let reply = status::status(detail_command, true, edge);
            assert!(reply.result.error.is_none(), "{:?}", reply.result.error);
            let (detail, human) = (reply.result.data, reply.human);
            assert_eq!(detail["rows"][0]["reason"], expected);
            assert!(
                human.contains("disk below gc.free_floor; provenant output ID"),
                "{human}"
            );
            assert!(!human.contains("23622320128"), "{human}");
            for field in ["length", "digest", "complete", "clipped"] {
                assert!(data[field].is_null(), "{field}: {data}");
            }
            assert_eq!(data["bytes"]["data"], "");
            let destination = fixture.root.join("preserved.txt");
            std::fs::write(&destination, "existing content")?;
            let mut save = command();
            save.to = Some(destination.clone());
            let reply = output::output(save, edge);
            assert!(reply.result.error.is_some(), "{:?}", reply.result);
            assert_eq!(std::fs::read_to_string(destination)?, "existing content");
            Ok(())
        },
    )?
}
