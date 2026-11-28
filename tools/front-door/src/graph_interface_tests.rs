#[cfg(test)]
#[allow(
    unused_imports,
    dead_code,
    reason = "the custom worktree driver includes this module without libtest"
)]
mod tests {
    use super::*;

    #[test]
    fn smoke_uses_one_named_host_and_rejects_aggregate_targets() {
        use provenant_cli::{CommonArgs, HostArg};
        let mut common = CommonArgs::default();
        assert_eq!(smoke_host(&common), Ok(None));
        common.host = vec![HostArg::Named("workshop".into())];
        assert_eq!(smoke_host(&common), Ok(Some("workshop".into())));
        common.host.push(HostArg::Named("laptop".into()));
        assert!(smoke_host(&common).is_err());
        common.host = vec![HostArg::All];
        assert!(smoke_host(&common).is_err());
    }

    fn run_input_fixture(
        request: GraphRunArgs,
    ) -> Result<Value, Box<provenant_records::diagnostic::Diagnostic>> {
        run_input(
            request,
            &provenant_routing::Catalogue::bundled().unwrap(),
            &Globals::default(),
        )
    }

    #[test]
    fn run_launch_error_skips_upstream_blocked_dependant() {
        // `run` names the pre-attempt launch failure, never the dependant that
        // the failed node blocked. This uses the shared cause predicate.
        let data = json!({"status":"failed", "checkpoint":{"nodes":[
            {"id":"dependant", "state":"blocked", "attempts":[],
             "advance_code":"upstream_blocked",
             "advance_note":"blocked by writer (Failed)"},
            {"id":"writer", "state":"failed", "attempts":[],
             "advance_code":"launch_refused",
             "advance_owner_code":"command_unavailable",
             "advance_note":"configure the fixture provider command"}
        ]}});
        let node = launch_error_node(&data, Outcome::Failed).expect("a launch error remains");
        assert_eq!(node["id"], "writer");
        assert_eq!(node["advance_owner_code"], "command_unavailable");
    }

    #[test]
    fn configuration_cause_and_selected_state_survive_projection() {
        let error = provenant_cli::fabric::EdgeError::Config(
            provenant_config::ConfigError::Refused("typed cause".into()),
        );
        assert!(config_cause(&error).is_some());
        let globals = Globals {
            state_dir: Some("/tmp/state with 'quote".into()),
            ..Globals::default()
        };
        let repair = config_repair(&globals);
        assert!(repair.contains("--state-dir"));
        assert!(repair.ends_with(" check config --json"));
        assert!(repair.contains("'\\''"));
    }

    // Spec: docs/specs/state.md#config-never-widens-authority
    #[test]
    fn unsafe_project_binding_state_has_local_mode_and_owner_repairs() {
        use std::path::PathBuf;

        let path = PathBuf::from("/private/state with 'quote");
        let mode_error = provenant_config::ConfigError::ProjectBindingDirectoryUnsafe {
            path: path.clone(),
            mode: 0o755,
            owner_matches: true,
        };
        let mode = config_projection(&mode_error, &Globals::default());
        assert!(mode.detail.as_deref().is_some_and(|detail| {
            detail.contains("mode 0755") && detail.contains("other users can")
        }));
        let mode_repair = mode.next.unwrap();
        assert!(mode_repair.contains("chmod 700 '/private/state with '\\''quote'"));
        assert!(!mode_repair.contains("chown"));
        assert!(!mode_error.to_string().contains("/private"));

        let owner_error = provenant_config::ConfigError::ProjectBindingDirectoryUnsafe {
            path,
            mode: 0o700,
            owner_matches: false,
        };
        let owner = config_projection(&owner_error, &Globals::default());
        assert!(
            owner
                .detail
                .as_deref()
                .is_some_and(|detail| { detail.contains("not owned by the current user") })
        );
        let owner_repair = owner.next.unwrap();
        assert!(owner_repair.contains("sudo chown \"$(id -u)\""));
        assert!(owner_repair.contains("&& chmod 700 '/private/state with '\\''quote'"));
    }

    #[allow(
        dead_code,
        reason = "the custom worktree driver omits libtest functions"
    )]
    fn run_request() -> GraphRunArgs {
        GraphRunArgs {
            common: Default::default(),
            model: None,
            effort: None,
            account: None,
            name: None,
            brief: None,
            brief_text: Vec::new(),
            brief_file: None,
            preamble_file: None,
            template: None,
            route: None,
            mode: None,
            source: None,
            worktree: None,
            branch: None,
            base: None,
            pr: None,
            head: None,
            write_scope: Vec::new(),
            over: Vec::new(),
            adopt: None,
            priority: None,
            batch: None,
            set: Vec::new(),
            plan: false,
            file: None,
            detached: false,
            wait: false,
            no_wait: false,
            context: Vec::new(),
            notes: Vec::new(),
            strict: false,
            checks: None,
            isolated: false,
            destination: None,
            commit: None,
            worktree_sharing: None,
            sandbox: false,
            no_sandbox: false,
            explain: false,
        }
    }

    #[test]
    fn review_pr_reaches_host_launch_input() {
        let mut request = run_request();
        request.template = Some("review.correctness".into());
        request.pr = Some(7);
        let input = run_input_fixture(request).unwrap();
        assert_eq!(input["pr"], 7);
    }

    #[test]
    fn review_head_reaches_host_launch_input() {
        let mut request = run_request();
        request.template = Some("review.correctness".into());
        request.pr = Some(7);
        request.head = Some("aabbccddeeff00112233445566778899aabbccdd".into());
        let input = run_input_fixture(request).unwrap();
        assert_eq!(input["pr"], 7);
        assert_eq!(input["head"], "aabbccddeeff00112233445566778899aabbccdd");
    }

    #[test]
    fn plan_sets_parameter_for_implementation_templates() {
        let mut request = run_request();
        request.template = Some("implement".into());
        request.plan = true;
        let input = run_input_fixture(request).unwrap();
        assert_eq!(input["set"]["plan"], true);

        let mut task = run_request();
        task.template = Some("task".into());
        task.plan = true;
        let input = run_input_fixture(task).unwrap();
        assert!(input["set"].get("plan").is_none());
    }

    #[test]
    fn review_settings_set_at_launch_are_forwarded_as_typed_policy() {
        let mut request = run_request();
        request.set.extend([
            "domains.review.later_rounds='full'".into(),
            "domains.review.accept_below='blocking'".into(),
            "domains.review.evidence_retries=3".into(),
        ]);
        let input = run_input_fixture(request).unwrap();
        assert_eq!(input["review_policy"]["later_rounds"], "full");
        assert_eq!(input["review_policy"]["accept_below"], "blocking");
        assert_eq!(input["review_policy"]["evidence_retries"], 3);
        assert!(input["set"].as_object().unwrap().is_empty());
    }

    #[test]
    fn malformed_set_and_missing_file_preserve_input_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        for (setting, detail) in [
            ("broken", "--set expects NAME=VALUE"),
            ("=1", "--set parameter name is empty"),
            ("attempt=", "TOML"),
        ] {
            let mut request = run_request();
            request.set.push(setting.into());
            let error = run_input_fixture(request)
                .err()
                .ok_or("invalid set was accepted")?;
            let diagnostic = error;
            assert_eq!(diagnostic.code, "invalid_input");
            assert!(
                diagnostic
                    .detail
                    .as_deref()
                    .is_some_and(|text| text.contains(detail))
            );
        }
        let mut request = run_request();
        request.set.push("attempt='PROMPT_SENTINEL".into());
        let diagnostic = run_input_fixture(request).unwrap_err();
        assert!(!serde_json::to_string(&diagnostic)?.contains("PROMPT_SENTINEL"));
        let directory = tempfile::tempdir()?;
        let mut request = run_request();
        request.file = Some(directory.path().join("missing.json"));
        let error = run_input_fixture(request)
            .err()
            .ok_or("missing file was accepted")?;
        let diagnostic = error;
        assert_eq!(diagnostic.code, "invalid_input");
        assert!(
            diagnostic
                .detail
                .as_deref()
                .is_some_and(|text| text.contains("--file") && text.contains("missing.json"))
        );
        Ok(())
    }

    #[test]
    fn run_flags_and_attempt_settings_reach_the_graph_request() {
        let provenant_cli::ParseOutcome::Invocation(invocation) = provenant_cli::parse(
            [
                "run",
                "Inspect",
                "--host",
                "workshop",
                "--model",
                "codex/gpt-6-luna",
                "--effort",
                "high",
                "--account",
                "work",
                "--set",
                "attempt.build_heavy=true",
                "--set",
                "attempt.confidential=true",
                "--set",
                "attempt.provider_timeout=42",
                "--set",
                "attempt.host='other'",
            ]
            .map(std::ffi::OsString::from),
        ) else {
            panic!("run should parse");
        };
        let provenant_cli::Command::Run(request) = invocation.command else {
            panic!("run command");
        };
        let input = run_input_fixture(*request).unwrap();
        assert_eq!(input["host"], "workshop");
        assert_eq!(input["model"], "codex/gpt-6-luna");
        assert_eq!(input["effort"], "high");
        assert_eq!(input["account"], "work");
        // Values travel as written; the launch owner types them by declaration.
        assert!(input.get("operation_id").is_none());
        for (key, text) in [
            ("attempt.build_heavy", "true"),
            ("attempt.confidential", "true"),
            ("attempt.provider_timeout", "42"),
            ("attempt.host", "'other'"),
        ] {
            assert_eq!(input["set"][key], text);
        }
    }

    #[test]
    fn sandbox_flags_set_the_attempt_sandbox_and_conflict() {
        let parse = |flags: &[&str]| {
            provenant_cli::parse(
                ["run", "Inspect"]
                    .iter()
                    .chain(flags)
                    .map(std::ffi::OsString::from),
            )
        };
        for (flag, value) in [("--sandbox", "on"), ("--no-sandbox", "off")] {
            let provenant_cli::ParseOutcome::Invocation(invocation) = parse(&[flag]) else {
                panic!("{flag} should parse");
            };
            let provenant_cli::Command::Run(request) = invocation.command else {
                panic!("run command");
            };
            let input = run_input_fixture(*request).unwrap();
            assert_eq!(input["set"]["attempt.sandbox"], value);
        }
        assert!(!matches!(
            parse(&["--sandbox", "--no-sandbox"]),
            provenant_cli::ParseOutcome::Invocation(_)
        ));
    }

    #[test]
    fn bare_model_resolves_to_its_provider_and_refuses_otherwise() {
        let with_model = |model: &str| {
            let mut request = run_request();
            request.model = Some(model.into());
            run_input_fixture(request)
        };
        // One provider offers it: the bare name becomes ADAPTER/ID.
        let input = with_model("claude-haiku-5-5").unwrap();
        assert_eq!(input["model"], "claude/claude-haiku-5-5");
        // Several providers offer it: refused as ambiguous, naming each ADAPTER/ID.
        for (bare, expected) in [
            (
                "claude-sonnet-5-5",
                ["claude/claude-sonnet-5-5", "agy/claude-sonnet-5-5"],
            ),
            (
                "claude-opus-5-5",
                ["claude/claude-opus-5-5", "agy/claude-opus-5-5"],
            ),
        ] {
            let ambiguous = with_model(bare).unwrap_err();
            assert_eq!(ambiguous.code, "invalid_input");
            let detail = ambiguous.detail.unwrap_or_default();
            for candidate in expected {
                assert!(detail.contains(candidate), "{detail}");
            }
        }
        // The provider-qualified form is passed through unchanged.
        let input = with_model("codex/gpt-6.1-terra").unwrap();
        assert_eq!(input["model"], "codex/gpt-6.1-terra");
        // No provider offers it: refused as invalid input, naming the form to use.
        let refusal = with_model("no-such-model-9").unwrap_err();
        assert_eq!(refusal.code, "invalid_input");
        let detail = refusal.detail.unwrap_or_default();
        assert!(
            detail.contains("unknown model `no-such-model-9`"),
            "{detail}"
        );
        assert!(detail.contains("provider/model form"), "{detail}");
    }

    // Spec: docs/specs/routing.md#provider-model-discovery
    #[test]
    fn bare_model_uses_the_effective_discovery_snapshot() {
        let snapshot = provenant_routing::parse_codex_models(
            br#"{"models":[{"slug":"gpt-9-sol","supported_reasoning_levels":[]}]}"#,
            1,
        )
        .unwrap();
        let catalogue = provenant_routing::Catalogue::bundled()
            .unwrap()
            .with_discovery(&[snapshot])
            .unwrap();
        let mut request = run_request();
        request.model = Some("gpt-9-sol".into());
        assert_eq!(
            run_input(request, &catalogue, &Globals::default()).unwrap()["model"],
            "codex/gpt-9-sol"
        );
    }

    #[test]
    fn ambiguous_bare_model_names_each_provider_form() {
        let detail = bare_model_detail(
            "gpt-6.1-terra",
            &BareModelError::Ambiguous {
                candidates: vec!["codex/gpt-6.1-terra".into(), "cursor/gpt-6.1-terra".into()],
            },
        );
        assert_eq!(
            detail,
            "model `gpt-6.1-terra` is offered by several providers: codex/gpt-6.1-terra, cursor/gpt-6.1-terra; name one as provider/model"
        );
        // A name that is not identifier-shaped is never echoed, even when the
        // refusal carries close matches.
        let secret = bare_model_detail(
            "sk-x y",
            &BareModelError::Unknown {
                candidates: vec!["gpt-6.1-terra".into()],
            },
        );
        assert!(!secret.contains("sk-x y"), "{secret}");
        assert!(secret.contains("close matches: gpt-6.1-terra"), "{secret}");
    }

    #[test]
    fn attempt_access_paths_refuse_and_name_the_launch_flag() {
        for (setting, flag) in [
            ("attempt.mode='worktree-write'", "--mode"),
            ("attempt.worktree='W'", "--worktree"),
            ("attempt.branch='B'", "--branch B --base REF"),
            ("attempt={mode='worktree-write'}", "--mode"),
            ("attempt.unapplied=true", "declared template parameter"),
        ] {
            let mut request = run_request();
            request.set.push(setting.into());
            let error =
                run_input_fixture(request).expect_err("unsupported launch path must refuse");
            assert!(
                error
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains(flag)),
                "{setting}: {error:?}"
            );
        }
    }

    #[test]
    fn attempt_cloud_placement_is_refused_in_both_set_forms() {
        let cloud = "{target='t',provider='p'}";
        for setting in [
            format!("attempt.cloud={cloud}"),
            format!("attempt={{cloud={cloud}}}"),
            format!("attempt.placement={cloud}"),
        ] {
            let mut request = run_request();
            request.set.push(setting.clone());
            let error = run_input_fixture(request).expect_err("cloud placement must refuse");
            assert!(
                error
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("cloud launch")),
                "{setting}: {error:?}"
            );
        }
    }

    #[test]
    fn pending_graph_projects_failed_pre_attempt_node() {
        let data = json!({"status":"pending", "checkpoint":{"nodes":[
            {"id":"retry", "state":"blocked", "attempts":[], "advance_note":"retrying"},
            {"id":"writer", "state":"failed", "attempts":[], "advance_note":"binding failed"},
            {"id":"other", "state":"running", "attempts":[{}]}
        ]}});
        assert_eq!(response_outcome("run", &data), Outcome::Waiting);
        assert_eq!(
            launch_error_node(&data, Outcome::Waiting).map(|node| &node["id"]),
            Some(&json!("writer"))
        );
        let mut timed_out = data.clone();
        timed_out["timed_out"] = json!(true);
        assert_eq!(timed_out["status"], "pending");
        let timed_out_outcome = response_outcome("run", &timed_out);
        assert_eq!(timed_out_outcome, Outcome::Timeout);
        assert_eq!(
            launch_error_node(&timed_out, timed_out_outcome).map(|node| &node["id"]),
            Some(&json!("writer"))
        );
    }

    #[test]
    fn pending_graph_projects_definitive_blocked_launch_errors() {
        for code in ["launch_refused", "launch_retry_exhausted"] {
            let data = json!({"status":"pending", "timed_out":true, "checkpoint":{"nodes":[
                {"id":"slow", "state":"running", "attempts":[{}]},
                {"id":"writer", "state":"blocked", "attempts":[],
                 "advance_note":"bounded launch refused", "advance_code":code,
                 "advance_owner_code":"writer_lock_busy"}
            ]}});
            assert_eq!(data["status"], "pending");
            let outcome = response_outcome("run", &data);
            assert_eq!(outcome, Outcome::Timeout);
            let node = launch_error_node(&data, outcome).unwrap();
            assert_eq!(node["id"], "writer");
            assert_eq!(node["advance_owner_code"], "writer_lock_busy");
            let mut retrying = data.clone();
            retrying["checkpoint"]["nodes"][1]["advance_code"] = json!("retrying");
            assert!(launch_error_node(&retrying, outcome).is_none());
            let mut launched = data.clone();
            launched["checkpoint"]["nodes"][1]["attempts"] = json!([{}]);
            assert!(launch_error_node(&launched, outcome).is_none());
        }
    }

    #[test]
    fn refusal_lines_use_latest_attempt_outcome_not_advance_code() {
        let checkpoint = json!({"nodes":[
            {"id":"refused", "advance_code":"route_refused", "advance_note":"route unavailable",
             "attempts":[{"outcome":{"preparation_refused":"route_resolution_refused"}}]},
            {"id":"launched", "advance_code":"route_refused", "advance_note":"other failure",
             "attempts":[{"outcome":{"preparation_refused":"route_resolution_refused"}},
                         {"outcome":"failed"}]}
        ]});
        assert_eq!(
            refusal_lines(&checkpoint),
            "refused refused: route unavailable\n"
        );
    }

    #[test]
    fn run_receipt_is_bounded_and_ends_in_one_next_command() {
        let nodes: Vec<_> = (0..40)
            .map(|n| {
                json!({"id":format!("n{n}"), "state":"failed", "advance_code":"route_refused",
                "advance_note":"x".repeat(1000), "attempts":[{"outcome":{"preparation_refused":"route_resolution_refused"}}]})
            })
            .collect();
        for (status, command, why) in [
            (
                "pending",
                "provenant wait review-1",
                "the run is still running",
            ),
            (
                "complete",
                "provenant output review-1",
                "the run is complete",
            ),
            (
                "cancelled",
                "provenant output review-1",
                "the run was cancelled",
            ),
            ("paused", "provenant status review-1", "the run is paused"),
        ] {
            let data =
                json!({"status":status, "checkpoint":{"graph_id":"review-1", "nodes":nodes}});
            for announced in [false, true] {
                let (human, step) = run_human(&data, &json!({"op":"run"}), announced);
                assert!(
                    human.starts_with(if announced { "refused" } else { "review-1" }),
                    "{human}"
                );
                // The suggested command is a typed step; the edge renders the
                // human line from the same argv and state-derived why it puts
                // on the JSON envelope.
                assert_eq!(step.argv, command.split_whitespace().collect::<Vec<_>>());
                assert_eq!(step.why, why);
                assert_eq!(
                    provenant_cli::next_steps_text(
                        &[step],
                        &provenant_workflow::session::CommandPrefix::default()
                    ),
                    format!("next: {why}; {command}\n")
                );
                assert!(!human.lines().any(|line| line.starts_with("next:")));
                assert!(
                    human
                        .lines()
                        .all(|line| line.len() <= provenant_cli::limits::SUMMARY_ROW_BYTES)
                );
                assert!(human.lines().count() <= provenant_cli::limits::SUMMARY_ROWS + 4);
            }
        }
    }

    #[test]
    fn compact_run_reports_resolved_lane_access() {
        let data = json!({"status":"pending", "checkpoint":{"graph_id":"work", "nodes":[]},
            "lane_accesses":[{"access":"isolated","source":"route","key":"rules.policy.lane.access"}]});
        let (human, _) = run_human(&data, &json!({}), true);
        assert!(human.contains("access: isolated (rules.policy.lane.access route)\n"));
    }

    // Spec: docs/specs/fabric.md#graph-admission-and-refusal-causes
    #[test]
    fn compact_run_prints_field_only_policy_warnings() {
        let input = json!({"brief":"ghp_abcdefghijklmnopqrstuvwxyz0123456789"});
        {
            let _mode = provenant_redact::scoped(provenant_redact::Mode::Off);
            assert!(
                provenant_fabric_runtime::graph::refusal::input_warnings(
                    &input,
                    &Default::default()
                )
                .is_empty()
            );
        }
        let _mode = provenant_redact::scoped(provenant_redact::Mode::Secrets);
        let warnings =
            provenant_fabric_runtime::graph::refusal::input_warnings(&input, &Default::default());
        let warning = warnings.first().expect("a field-only policy warning");
        assert_eq!(warning.code.as_ref(), "SECRET_SHAPED_INPUT");
        assert!(warning.message.contains("brief looks like a credential"));
        assert!(
            !warning
                .message
                .contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
        );
        // The facade carries typed warnings to the compact surface as their
        // messages; the code stays with the typed value in the v2 envelope.
        let messages: Vec<String> = warnings
            .into_iter()
            .map(|warning| warning.message)
            .collect();
        let data = json!({"checkpoint":{"graph_id":"example","nodes":[]},"status":"pending","warnings":messages});
        for announced in [false, true] {
            let (output, _) = run_human(&data, &json!({"op":"run"}), announced);
            assert!(output.contains("warning: [SECRET_SHAPED_INPUT]"));
            assert!(output.contains("brief looks like a credential"));
            assert!(!output.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        }
    }

    #[test]
    fn a_suffixed_name_is_announced_and_a_replay_is_not_a_failure() {
        let data = json!({"status":"pending", "named":{"requested":"fix", "graph":"fix-2", "superseded":"fix"},
            "checkpoint":{"graph_id":"fix-2", "nodes":[]}});
        let (human, _) = run_human(&data, &json!({}), false);
        assert!(
            human.starts_with("started fix-2 (fix exists with a different input)\n"),
            "{human}"
        );
        // An already-announced launch carries the next step on the envelope;
        // the notice was the first line written at launch time and is never
        // duplicated in the result.
        let (announced, step) = run_human(&data, &json!({}), true);
        assert!(
            !announced.contains("exists with a different input"),
            "{announced}"
        );
        assert_eq!(step.argv, ["provenant", "wait", "fix-2"]);
        assert_eq!(
            announced,
            "— fix-2 · - · - · read-only, checkout · 0 warnings\n"
        );
        let replay = json!({"status":"complete", "outcome":"failed", "named":{"graph":"fix", "replayed":true}});
        assert_eq!(response_outcome("run", &replay), Outcome::Ok);
        let fresh = json!({"status":"complete", "outcome":"failed"});
        assert_eq!(response_outcome("run", &fresh), Outcome::Failed);
    }

    #[test]
    fn announced_headline_reads_the_document_before_any_attempt() {
        let checkpoint = json!({"graph_id":"g-2", "nodes":[
            {"id":"one", "state":"pending", "attempts":[]},
            {"id":"two", "state":"pending", "attempts":[]}
        ]});
        let document = json!({"graph_id":"g-2", "nodes":[
            {"id":"one", "route":{"alias":"senior"},
             "attempt":{"new_worktree":{"branch":"batch/work", "base":"main"}}},
            {"id":"two", "routes":[{"adapter":"codex", "model":"gpt"}]}
        ]});
        assert_eq!(
            run_headline(&checkpoint, "pending", Some(&document), &json!({})),
            "g-2 pending senior new:batch/work"
        );
        assert_eq!(
            run_headline(&checkpoint, "pending", None, &json!({})),
            "g-2 pending - -"
        );
    }

    #[test]
    fn failed_attempt_does_not_project_launch_error() {
        let data = json!({"checkpoint":{"nodes":[
            {"id":"task", "state":"failed", "attempts":[{}], "advance_note":"fallback_route_pinned"}
        ]}});
        assert!(launch_error_node(&data, Outcome::Failed).is_none());
    }

    #[test]
    fn run_reports_the_graph_outcome_and_status_reads_remain_successful() {
        for (status, outcome, expected) in [
            ("complete", "failed", Outcome::Failed),
            ("complete", "succeeded", Outcome::Ok),
            ("complete", "partial", Outcome::Ok),
            ("cancelled", "failed", Outcome::Cancelled),
            ("paused", "failed", Outcome::Refused),
            ("refused", "failed", Outcome::Refused),
            ("pending", "partial", Outcome::Waiting),
        ] {
            let data = json!({"status":status,"outcome":outcome});
            assert_eq!(response_outcome("run", &data), expected);
            assert_eq!(response_outcome("status", &data), Outcome::Ok);
        }
        assert_eq!(
            response_outcome("run", &json!({"document":{}})),
            Outcome::Ok
        );
        // An inline run past --timeout is a 124 with the lane still running;
        // other commands keep the bounded-wait outcome.
        assert_eq!(
            response_outcome("run", &json!({"timed_out":true})),
            Outcome::Timeout
        );
        assert_eq!(
            response_outcome("status", &json!({"timed_out":true})),
            Outcome::Waiting
        );
    }

    #[test]
    fn launch_mode_from_template_shape() {
        // A leaf is inline, a composite or queue is a handle, and an explicit
        // flag beats both the shape and the knob.
        assert_eq!(launch_mode(false, false, None, false), LaunchMode::Inline);
        assert_eq!(launch_mode(false, false, None, true), LaunchMode::Handle);
        assert_eq!(
            launch_mode(false, false, Some("handle"), false),
            LaunchMode::Handle
        );
        assert_eq!(
            launch_mode(false, false, Some("inline"), true),
            LaunchMode::Inline
        );
        assert_eq!(
            launch_mode(true, false, Some("handle"), true),
            LaunchMode::Inline
        );
        assert_eq!(
            launch_mode(false, true, Some("inline"), false),
            LaunchMode::Handle
        );
    }

    #[test]
    fn context_and_notes_are_bounded_and_brief_stays_first() {
        let sections = vec![
            ("context: a.md".to_owned(), "A".repeat(60)),
            ("notes: n".to_owned(), "N".repeat(60)),
        ];
        let (full, cut) = bounded_context("do the work", &sections, 4096);
        assert!(!cut);
        assert!(full.starts_with("do the work"));
        assert!(full.contains("context: a.md"));
        assert!(full.contains("notes: n"));

        let (short, cut) = bounded_context("do the work", &sections, 40);
        assert!(cut);
        assert!(short.starts_with("do the work"));
        assert!(short.len() <= 40);
        assert!(std::str::from_utf8(short.as_bytes()).is_ok());

        // An empty opt-in section never appears.
        let empty = vec![("empty".to_owned(), " \n".to_owned())];
        let (text, _) = bounded_context("brief", &empty, 100);
        assert_eq!(text, "brief");
    }

    #[test]
    fn inline_task_prints_result_and_footer() {
        let data = json!({
            "status":"complete",
            "checkpoint":{"graph_id":"task-3f2","nodes":[]},
            "route":{"model":"luna"},
            "elapsed_ms":134000,
            "warnings":["a","b"],
        });
        let inline = json!({"launch_mode":"inline","access":"read_only","source":"checkout"});
        let (human, _) = run_human(&data, &inline, false);
        assert!(
            human.contains("— task-3f2 · luna · 2m14s · read_only, checkout · 2 warnings"),
            "{human}"
        );
        // A handle launch reports only the handle; no inline footer.
        let handle = json!({"launch_mode":"handle"});
        let (human, _) = run_human(&data, &handle, false);
        assert!(!human.contains("warnings\n"));
        assert!(!human.contains("2m14s"));
    }

    #[test]
    fn carried_paths_are_listed_in_inline_and_handle_launches() {
        let data = json!({
            "status": "complete",
            "checkpoint": {"graph_id": "task", "nodes": [
                {"writer_binding": {"carried_paths": ["src/a.rs", "odd\nname"]}}
            ]}
        });
        for mode in ["inline", "handle"] {
            let (human, _) = run_human(&data, &json!({"launch_mode":mode}), false);
            assert!(human.contains("carried: \"src/a.rs\""), "{human}");
            assert!(human.contains("carried: \"odd\\nname\""), "{human}");
        }
    }

    #[test]
    fn inline_timeout_is_124_still_running() {
        let mut data = json!({
            "status":"pending",
            "checkpoint":{"graph_id":"task-9","nodes":[]},
            "timed_out":true,
        });
        assert_eq!(response_outcome("run", &data), Outcome::Timeout);
        assert_eq!(Outcome::Timeout.exit_code(), 124);
        // The project edge marks the lane as still running on this outcome.
        if data["timed_out"] == true {
            data["still_running"] = json!(true);
        }
        assert_eq!(data["still_running"], true);
        let (_, step) = run_human(&data, &json!({"launch_mode":"inline"}), true);
        assert_eq!(
            step.argv,
            vec!["provenant", "wait", "task-9"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        );
        assert_eq!(step.why, "the inline run is still running");
    }

    #[test]
    fn native_lane_flags_reach_the_graph_input() {
        let mut request = run_request();
        request.brief = Some("Review the tree".into());
        request.strict = true;
        request.checks = Some("off".into());
        request.isolated = true;
        request.destination = Some("parent".into());
        request.commit = Some("own".into());
        request.worktree_sharing = Some("refuse".into());
        let input = run_input_fixture(request).unwrap();
        // --isolated sets its bundle; the explicit destination and sharing
        // flags then win for this launch.
        assert_eq!(input["isolated"], true);
        assert_eq!(input["checks_mode"], "strict");
        assert_eq!(input["destination"], "parent");
        assert_eq!(input["commit"], "own");
        assert_eq!(input["worktree_sharing"], "refuse");
        assert_eq!(input["launch_mode"], "inline");
        assert!(
            input["timeout_seconds"]
                .as_u64()
                .is_some_and(|seconds| seconds > 0)
        );
        assert_eq!(input["context_inputs"]["files"], json!([]));
        assert_eq!(input["context_inputs"]["notes"], json!([]));
    }
}
