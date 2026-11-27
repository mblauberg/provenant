#[cfg(test)]
#[allow(dead_code, unused_imports)]
mod tests {
    use super::*;
    use provenant_records::Outcome;
    use provenant_records::warning::{NAME_CASE_FOLDED, WarningCode, capture};
    use provenant_sqlite::{ensure_schema, open_connection};
    use std::ffi::OsString;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn with_root(f: impl FnOnce(&Path)) {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dir = tempfile::tempdir().expect("tempdir");
        f(dir.path());
    }

    // Spec: docs/specs/cli.md#output-references-and-bounded-tails
    #[test]
    fn piped_stdout_has_no_ansi_escapes_but_a_terminal_keeps_them() {
        let result = CommandResult::new("output", Outcome::Ok, json!({"state": "succeeded"}));
        let globals = provenant_cli::Globals {
            full: true,
            ..Default::default()
        };
        let human = "settlement: succeeded\n\u{1b}[31mred findings\u{1b}[0m \u{1b}]8;;https://example.invalid\u{7}link\u{1b}\\ tail\n";
        let mut piped = Vec::new();
        let mut err = Vec::new();
        assert_eq!(
            emit_with_writers_and_tty(
                &result, &globals, &mut piped, &mut err, human, true, None, false, false,
            ),
            (0, true)
        );
        let printed = String::from_utf8(piped).expect("utf8");
        assert!(!printed.contains('\u{1b}'), "{printed:?}");
        assert!(printed.contains("red findings"), "{printed:?}");
        assert!(printed.contains("link tail"), "{printed:?}");
        let mut terminal = Vec::new();
        let mut err = Vec::new();
        emit_with_writers_and_tty(
            &result,
            &globals,
            &mut terminal,
            &mut err,
            human,
            true,
            None,
            true,
            false,
        );
        assert!(
            String::from_utf8(terminal)
                .expect("utf8")
                .contains("\u{1b}[31m"),
            "a terminal keeps the escapes"
        );

        // The compact view prints summary rows, so those must be clean too.
        let compact_globals = provenant_cli::Globals::default();
        let summary = Summary {
            lines: vec!["\u{1b}[1msettlement: succeeded\u{1b}[0m".into()],
            omitted: 0,
        };
        let mut compact = Vec::new();
        emit_with_writers_and_tty(
            &result,
            &compact_globals,
            &mut compact,
            &mut Vec::new(),
            "",
            true,
            Some(&summary),
            false,
            false,
        );
        let compact = String::from_utf8(compact).expect("utf8");
        assert!(!compact.contains('\u{1b}'), "{compact:?}");
        assert!(compact.contains("settlement: succeeded"), "{compact:?}");
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn json_mode_carries_warnings_in_the_envelope_and_not_on_stderr() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        warning::warn(Warning::new(
            WarningCode("TEST"),
            "a typed warning",
            provenant_records::warning::detected_source(WarningCode("TEST")),
        ));
        let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert_eq!(envelope["warnings"][0]["code"], "TEST");
        assert_eq!(envelope["warnings"][0]["message"], "a typed warning");
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn json_mode_carries_next_steps_as_argv_arrays() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        let result =
            CommandResult::new("fixture", Outcome::Ok, json!({"ok": true})).with_next(vec![
                NextStep::new(
                    vec![
                        "provenant".into(),
                        "pr".into(),
                        "status".into(),
                        "12".into(),
                    ],
                    "check CI",
                ),
            ]);
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert_eq!(
            envelope["next"][0]["argv"],
            json!(["provenant", "pr", "status", "12"])
        );
        assert_eq!(envelope["next"][0]["why"], "check CI");
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    }

    /// The envelope's `next` array and the one human line the edge prints are
    /// two views of the same typed step. Every owner family pins this.
    fn assert_next_agrees(command: &str, steps: Vec<NextStep>) {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = CommandResult::new(command, Outcome::Ok, json!({})).with_next(steps.clone());

        let json_globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let sink = install_command_sink(true);
        let mut json_out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(
            &result,
            &json_globals,
            &mut json_out,
            &mut err,
            "",
            false,
            None,
        );
        let envelope: Value = serde_json::from_slice(&json_out).expect("envelope");
        let on_wire: Vec<NextStep> =
            serde_json::from_value(envelope["next"].clone()).expect("next steps");
        assert_eq!(on_wire, steps, "JSON must carry the exact argv and why");
        drop(sink);

        let human_globals = provenant_cli::Globals {
            json: false,
            full: true,
            ..Default::default()
        };
        let _sink = install_command_sink(false);
        let mut human_out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(
            &result,
            &human_globals,
            &mut human_out,
            &mut err,
            "",
            false,
            None,
        );
        let text = String::from_utf8(human_out).expect("human text");
        let prefix = provenant_workflow::session::CommandPrefix::default();
        assert_eq!(
            text,
            provenant_cli::next_steps_text(&steps, &prefix),
            "the human line must be rendered from the same array"
        );
        assert_eq!(text.lines().count(), steps.len());
        for step in &steps {
            let rendered = prefix.render_argv(&step.argv).expect("renderable argv");
            assert!(
                text.lines()
                    .any(|line| line.ends_with(&format!("; {rendered}"))
                        || line == format!("next: {rendered}")),
                "no human line matches argv {:?}: {text}",
                step.argv
            );
        }
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_launch() {
        assert_next_agrees(
            "run",
            vec![NextStep::from_args(
                &["provenant", "wait", "review-1"],
                "the run is still running",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_wait() {
        assert_next_agrees(
            "wait",
            vec![NextStep::from_command(
                "provenant status mixed/fast mixed/slow",
                "mixed/fast settled ok, mixed/slow failed",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_status() {
        assert_next_agrees(
            "status",
            vec![NextStep::from_args(
                &[
                    "provenant",
                    "status",
                    "--all",
                    "--limit",
                    "50",
                    "--cursor",
                    "7",
                ],
                "read the next page of graphs",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_steer() {
        assert_next_agrees(
            "steer",
            vec![NextStep::from_command(
                "provenant status fix-2",
                "re-read the graph",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_pr() {
        assert_next_agrees(
            "pr status",
            vec![NextStep::from_command(
                "provenant ci failures pr 1669",
                "inspect the failing checks",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_worktree() {
        assert_next_agrees(
            "worktree gc",
            vec![NextStep::from_command(
                "provenant gc --apply",
                "apply the pending retention",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn next_agrees_for_ci() {
        assert_next_agrees(
            "ci failures",
            vec![NextStep::from_command(
                "gh run rerun 42 --failed",
                "rerun the failed jobs",
            )],
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn a_real_command_owner_carries_a_non_empty_next_array() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A real graph-status projection, not a hand-built fixture: the owner's
        // own builder produces the step from the row data a failing graph emits.
        let rows = vec![json!({
            "kind": "graph",
            "id": "g",
            "state": "failed",
            "next": "provenant retry g",
        })];
        let steps: Vec<NextStep> = crate::fabric::unified::graph_status_next(&rows)
            .into_iter()
            .collect();
        assert!(
            !steps.is_empty(),
            "the real owner must suggest a next command"
        );
        assert_eq!(steps[0].argv, ["provenant", "retry", "g"]);
        let result = CommandResult::new("status", Outcome::Ok, json!({})).with_next(steps);
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let sink = install_command_sink(true);
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        drop(sink);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert!(envelope["next"].as_array().is_some_and(|a| !a.is_empty()));
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn an_owner_capture_keeps_warnings_out_of_the_command_sink() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ((), captured) = capture(|| {
            warning::warn(Warning::new(
                WarningCode("TEST"),
                "a configured key was ignored",
                provenant_records::warning::detected_source(WarningCode("TEST")),
            ));
        });
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].code.as_ref(), "TEST");
        // No sink was installed: nothing was taken for an envelope.
        assert!(take_warnings().is_empty());
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_per_change_key_is_remembered_but_a_changed_key_is_shown() {
        with_root(|root| {
            set_state_root(root);
            assert!(dedup::remember(root, "sha256:a", "A", 30));
            assert!(!dedup::remember(root, "sha256:a", "A", 30));
            assert!(dedup::remember(root, "sha256:b", "A", 30));
            assert!(root.join("warnings.sqlite3").exists());
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn pruning_removes_only_stale_rows() {
        with_root(|root| {
            let Ok(db) = open_connection(&root.join("warnings.sqlite3"), false) else {
                return;
            };
            assert!(ensure_schema(&db, dedup::WARNINGS_SCHEMA).is_ok());
            use provenant_core::Clock as _;
            let now = provenant_os::clock::SystemClock.unix_seconds();
            let stale = now.saturating_sub(400 * 86_400);
            assert!(db
                .execute(
                    "INSERT INTO seen (key, code, first_seen, last_seen, count) VALUES ('old', 'A', ?1, ?1, 1)",
                    rusqlite::params![stale],
                )
                .is_ok());
            assert!(dedup::remember(root, "sha256:fresh", "A", 30));
            let remaining: Vec<String> = db
                .prepare("SELECT key FROM seen ORDER BY key")
                .expect("prepare")
                .query_map([], |row| row.get(0))
                .expect("query")
                .map_while(Result::ok)
                .collect();
            assert_eq!(remaining, ["sha256:fresh"]);
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn parse_warnings_replayed_before_emit_reach_the_json_envelope() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A capital target is folded by clap's value parser, which runs before
        // dispatch installs the command sink.
        let (parsed, parse_warnings) =
            capture(|| provenant_cli::parse(["stop", "Fix-Login", "--json"].map(OsString::from)));
        assert!(
            matches!(parsed, provenant_cli::ParseOutcome::Invocation(_)),
            "the command must parse"
        );
        assert!(
            parse_warnings
                .iter()
                .any(|warning| warning.code.as_ref() == NAME_CASE_FOLDED.as_str()),
            "{parse_warnings:?}"
        );
        let _sink = install_command_sink(true);
        replay_warnings(parse_warnings);
        let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
        let envelope: Value = serde_json::from_slice(&out).expect("envelope");
        assert!(
            envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning["code"] == NAME_CASE_FOLDED.as_str()),
            "{envelope}"
        );
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn an_oversized_json_result_carries_warnings_on_the_bounded_marker() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        let result = CommandResult::new(
            "fixture",
            Outcome::Ok,
            "x".repeat(provenant_cli::MAX_RESULT_BYTES + 128),
        );
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status =
            emit_unpublished_with_writers(&result, &globals, "", false, None, &mut out, &mut err);
        assert_eq!(status, 0);
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        let envelope: Value = serde_json::from_slice(&out).expect("bounded marker envelope");
        assert_eq!(envelope["data"]["truncated"], true);
        assert!(envelope["data"]["omitted_bytes"].as_u64().unwrap() > 0);
        assert!(
            envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| warning["code"] == RESULT_TOO_LARGE.as_str()),
            "{envelope}"
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#one-bounded-envelope-clipped-with-a-warning
    fn omitted_bytes_measure_the_envelope_not_the_bare_result() {
        let _lock = TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _sink = install_command_sink(true);
        // Size the bare result just under the cap so a modest held warning is
        // what pushes the envelope over it. The marker and its warning must
        // report that overflow, not "0 bytes omitted".
        let empty = CommandResult::new("fixture", Outcome::Ok, String::new());
        let overhead = serde_json::to_vec(&empty).expect("encode").len();
        let data = "x".repeat(provenant_cli::MAX_RESULT_BYTES - overhead - 256);
        let result = CommandResult::new("fixture", Outcome::Ok, data);
        assert!(
            serde_json::to_vec(&result).expect("encode").len() < provenant_cli::MAX_RESULT_BYTES,
            "the bare result fits"
        );
        warning::warn(Warning::new(
            WarningCode("TEST"),
            "w".repeat(512),
            provenant_records::warning::detected_source(WarningCode("TEST")),
        ));
        let globals = provenant_cli::Globals {
            json: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let status =
            emit_unpublished_with_writers(&result, &globals, "", false, None, &mut out, &mut err);
        assert_eq!(status, 0);
        assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        let envelope: Value = serde_json::from_slice(&out).expect("bounded marker envelope");
        let omitted = envelope["data"]["omitted_bytes"]
            .as_u64()
            .expect("omitted_bytes") as usize;
        assert!(omitted > 0, "the envelope overflowed: {envelope}");
        assert!(
            envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|warning| {
                    warning["code"] == RESULT_TOO_LARGE.as_str()
                        && warning["message"].as_str().is_some_and(|message| {
                            message.contains(&format!("{omitted} bytes omitted"))
                        })
                }),
            "the warning reports the same honest count as the marker: {envelope}"
        );
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_json_delivered_warning_is_kept_but_not_remembered() {
        with_root(|root| {
            set_state_root(root);
            let shown = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Reader` to `reader`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("reader-shown")
            .once_per_change();
            let shown_key = shown.key();
            let fresh = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("writer-fresh")
            .once_per_change();
            let fresh_key = fresh.key();
            // A prior human display must not hide the warning from a consumer.
            assert!(dedup::remember(
                root,
                &shown_key,
                NAME_CASE_FOLDED.as_str(),
                30
            ));
            let sink = install_command_sink(true);
            warning::warn(shown);
            warning::warn(fresh);
            let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
            let globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None),
                0
            );
            let envelope: Value = serde_json::from_slice(&out).expect("envelope");
            let codes: Vec<_> = envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .map(|warning| warning["code"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(codes, [NAME_CASE_FOLDED.as_str(); 2], "{envelope}");
            assert!(
                dedup::was_seen(root, &shown_key, 30),
                "a JSON delivery does not clear prior human memory"
            );
            assert!(
                !dedup::was_seen(root, &fresh_key, 30),
                "a JSON delivery never marks a key as shown; a later person still sees it"
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            drop(sink);
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_failed_write_does_not_remember_an_undelivered_warning() {
        with_root(|root| {
            set_state_root(root);
            let warning = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("writer-bad")
            .once_per_change();
            let key = warning.key();
            let sink = install_command_sink(true);
            warning::warn(warning);
            let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
            let globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };
            let mut out = BrokenWriter;
            let mut err = Vec::new();
            assert_eq!(
                emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None),
                3
            );
            assert!(
                !dedup::was_seen(root, &key, 30),
                "a warning no envelope delivered is not remembered on an IO failure"
            );
            // The guard shows the returned warning on exit and remembers it then.
            drop(sink);
        });
    }

    /// A writer that always fails, driving `emit_with_writers` into its exit-3
    /// arm while keeping the test inside the in-process emission seam.
    struct BrokenWriter;
    impl Write for BrokenWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn an_exit_without_emitting_shows_and_remembers_held_warnings() {
        with_root(|root| {
            set_state_root(root);
            let warning = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Writer` to `writer`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("writer")
            .once_per_change();
            let key = warning.key();
            {
                let _sink = install_command_sink(true);
                warning::warn(warning);
                assert!(!dedup::was_seen(root, &key, 30));
            }
            assert!(
                dedup::was_seen(root, &key, 30),
                "the guard drains held warnings on exit so they are shown once"
            );
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_fresh_per_change_key_starts_at_one() {
        with_root(|root| {
            assert!(dedup::remember(root, "sha256:fresh", "A", 30));
            let db = open_connection(&root.join("warnings.sqlite3"), true).expect("db");
            let count: i64 = db
                .query_row(
                    "SELECT count FROM seen WHERE key = 'sha256:fresh'",
                    [],
                    |row| row.get(0),
                )
                .expect("count");
            assert_eq!(count, 1, "a new row is shown once and counted once");
            drop(db);
            assert!(!dedup::remember(root, "sha256:fresh", "A", 30));
            let db = open_connection(&root.join("warnings.sqlite3"), true).expect("db");
            let count: i64 = db
                .query_row(
                    "SELECT count FROM seen WHERE key = 'sha256:fresh'",
                    [],
                    |row| row.get(0),
                )
                .expect("count");
            assert_eq!(count, 2, "only a repeat advances the count");
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_malformed_warnings_setting_does_not_reenter_the_sink_under_its_lock() {
        with_root(|root| {
            std::fs::write(
                root.join("config.toml"),
                "schema_version = 1\n[warnings]\ndedup_retention_days = 0\n",
            )
            .expect("config");
            set_state_root(root);
            let _sink = install_command_sink(true);
            // Resolving the malformed setting raises a per-change config warning.
            // If the sink re-entered while holding its mutex on this path the
            // command would hang; it must instead reach the same envelope.
            warning::warn(
                Warning::new(
                    NAME_CASE_FOLDED,
                    "folded `Sink` to `sink`",
                    provenant_records::warning::detected_source(NAME_CASE_FOLDED),
                )
                .subject("sink")
                .once_per_change(),
            );
            let result = CommandResult::new("fixture", Outcome::Ok, json!({"ok": true}));
            let globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None);
            let envelope: Value = serde_json::from_slice(&out).expect("envelope");
            assert!(
                envelope["warnings"]
                    .as_array()
                    .expect("warnings")
                    .iter()
                    .any(|warning| warning["code"] == NAME_CASE_FOLDED.as_str()),
                "{envelope}"
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
        });
    }

    #[test]
    // Spec: docs/specs/cli.md#warnings-are-typed-values
    fn a_failed_stderr_write_does_not_remember_a_per_change_warning() {
        with_root(|root| {
            set_state_root(root);
            let first = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Sink` to `sink`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("sink-write")
            .once_per_change();
            let key = first.key();
            let _sink = install_command_sink(false);
            let sink = COMMAND_SINK
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
                .expect("sink");
            let mut broken = BrokenWriter;
            sink.display_with(first, &mut broken);
            assert!(
                !dedup::was_seen(root, &key, 30),
                "an undelivered warning must not be remembered as shown"
            );

            let second = Warning::new(
                NAME_CASE_FOLDED,
                "folded `Sink` to `sink`",
                provenant_records::warning::detected_source(NAME_CASE_FOLDED),
            )
            .subject("sink-write")
            .once_per_change();
            let mut delivered = Vec::new();
            sink.display_with(second, &mut delivered);
            assert_eq!(
                String::from_utf8_lossy(&delivered),
                "warning: folded `Sink` to `sink`\n"
            );
            assert!(
                dedup::was_seen(root, &key, 30),
                "a warning the reader received is remembered"
            );
        });
    }

    /// Spec: docs/specs/cli.md#warnings-are-typed-values
    /// Two launches in one state root show an observational policy warning once
    /// to a person, while a later JSON envelope still carries it with its code
    /// and source. The warning is the real one `Policy::check` produces.
    #[test]
    fn writer_census_is_shown_once_but_every_envelope_carries_it() {
        use provenant_records::policy::{Effect, Facts, Policy};
        with_root(|root| {
            set_state_root(root);
            let Effect::Warn(warning) = Policy::default().check(
                "WRITER_CENSUS",
                Facts::violation("process census reports an occupant"),
            ) else {
                panic!("WRITER_CENSUS is an observational rule")
            };
            let result = CommandResult::new("run", Outcome::Ok, json!({}));
            let human = provenant_cli::Globals {
                json: false,
                ..Default::default()
            };
            let json_globals = provenant_cli::Globals {
                json: true,
                ..Default::default()
            };

            let key = warning.key();
            for _ in 0..2 {
                let _sink = install_command_sink(false);
                warning::warn(warning.clone());
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    emit_with_writers(&result, &human, &mut out, &mut err, "", false, None),
                    0
                );
            }
            // A human warning prints to the process stderr as it arrives, so it
            // never appears in the emitter's writer. Its per-change row is
            // advanced only after a successful display, which makes the row's
            // count the number of times a person actually saw the notice.
            let db = open_connection(&root.join("warnings.sqlite3"), true).expect("db");
            let shown: i64 = db
                .query_row(
                    "SELECT count FROM seen WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get(0),
                )
                .expect("the census notice was shown");
            assert_eq!(shown, 1, "a person sees the census notice once");
            drop(db);

            let _sink = install_command_sink(true);
            warning::warn(warning);
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                emit_with_writers(&result, &json_globals, &mut out, &mut err, "", false, None),
                0
            );
            assert!(err.is_empty(), "{}", String::from_utf8_lossy(&err));
            let envelope: Value = serde_json::from_slice(&out).expect("envelope");
            let carried = envelope["warnings"]
                .as_array()
                .expect("warnings")
                .iter()
                .any(|entry| {
                    entry["code"] == "WRITER_CENSUS"
                        && entry["source"]["layer"] == "default"
                        && entry["source"]["origin"] == "compiled policy table"
                });
            assert!(
                carried,
                "the second envelope still lists the warning: {envelope}"
            );
        });
    }

    fn shared_warning(subject: &str) -> Warning {
        let setting = provenant_records::command_result::SettingRef {
            key: "domains.cli.collapse_warnings".into(),
            value: "on".into(),
            layer: provenant_records::Layer::Host,
            origin: "config.toml:12".into(),
        };
        let code = provenant_records::WarningCode("SHARED");
        Warning::new(
            code,
            "7 lanes share a tree",
            provenant_records::warning::detected_source(code),
        )
        .subject(subject)
        .setting(setting)
    }

    #[test]
    // Spec: docs/specs/agent-native.md#35-short-output
    fn same_template_warnings_render_once_json_keeps_all() {
        with_root(|root| {
            set_state_root(root);
            let warnings: Vec<Warning> = (0..7)
                .map(|index| shared_warning(&format!("lane-{index}")))
                .collect();
            let result = CommandResult::new("fixture", Outcome::Ok, json!({}));
            let human_globals = provenant_cli::Globals {
                json: false,
                ..Default::default()
            };
            {
                let _sink = install_command_sink_with(false, false);
                for warning in warnings.clone() {
                    warning::warn(warning);
                }
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    emit_with_writers(&result, &human_globals, &mut out, &mut err, "", false, None,),
                    0
                );
                let text = String::from_utf8(err).expect("utf8");
                assert!(text.contains("warning [SHARED] ×7:"), "{text}");
                assert!(text.contains("(first: lane-0)"), "{text}");
                assert!(text.contains("--full lists all"), "{text}");
                assert_eq!(
                    text.lines()
                        .filter(|line| line.starts_with("warning [SHARED]"))
                        .count(),
                    1,
                    "{text}"
                );
            }
            {
                let _sink = install_command_sink(true);
                for warning in warnings {
                    warning::warn(warning);
                }
                let globals = provenant_cli::Globals {
                    json: true,
                    ..Default::default()
                };
                let mut out = Vec::new();
                let mut err = Vec::new();
                assert_eq!(
                    emit_with_writers(&result, &globals, &mut out, &mut err, "", false, None),
                    0
                );
                let envelope: Value = serde_json::from_slice(&out).expect("envelope");
                assert_eq!(envelope["warnings"].as_array().map(Vec::len), Some(7));
            }
        });
    }

    #[test]
    // Spec: docs/specs/agent-native.md#35-short-output
    fn large_result_keeps_outcome_and_writes_private_report() {
        with_root(|root| {
            set_state_root(root);
            let _sink = install_command_sink(false);
            let result = CommandResult::new(
                "status",
                Outcome::Ok,
                json!({"blob": "x".repeat(provenant_cli::MAX_RESULT_BYTES)}),
            );
            let globals = provenant_cli::Globals {
                json: false,
                ..Default::default()
            };
            let mut out = Vec::new();
            let mut err = Vec::new();
            assert_eq!(
                emit_unpublished_with_writers(
                    &result,
                    &globals,
                    "status: ok\n",
                    false,
                    None,
                    &mut out,
                    &mut err,
                ),
                0
            );
            let entries: Vec<_> = std::fs::read_dir(root.join("reports"))
                .expect("reports directory")
                .flatten()
                .collect();
            assert_eq!(entries.len(), 1, "one private report for the result");
            let path = entries[0].path();
            assert!(path.to_string_lossy().contains("status-"), "{path:?}");
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("report metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
            let text = String::from_utf8(err).expect("utf8");
            assert!(text.contains("RESULT_TOO_LARGE"), "{text}");
            assert!(text.contains(&path.to_string_lossy().to_string()), "{text}");
            let saved: Value =
                serde_json::from_slice(&std::fs::read(&path).expect("report bytes")).expect("json");
            assert_eq!(saved["command"], "status");
            assert_eq!(saved["status"], "ok");
        });
    }
}
