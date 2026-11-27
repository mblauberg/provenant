#[cfg(test)]
mod tests {
    use super::*;

    #[allow(dead_code)] // The custom worktree driver compiles without a test harness.
    fn fixture() -> (tempfile::TempDir, Roots, ProjectId) {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().canonicalize().unwrap();
        let mut inputs = provenant_core::RootInputs::new(base.join("home"), base.join("checkout"));
        inputs.state_dir = Some(base.join("state"));
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        provenant_os::fixture::private_state_dir(roots.state.as_path()).unwrap();
        (temp, roots, ProjectId::from_bytes([34; 16]))
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn forge_tick_skips_a_binding_whose_common_directory_is_gone() -> Result<()> {
        if crate::fabric::peer_tests::host_test(
            "event_delivery::tests::forge_tick_skips_a_binding_whose_common_directory_is_gone",
        )? {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let project = base.join("project");
        let state = base.join("state");
        for path in [&home, &project, &state] {
            std::fs::create_dir(path)?;
        }
        provenant_os::fixture::private_state_dir(&state)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs
            .resolve(&provenant_os::fs::SystemPathResolver)
            .unwrap();
        provenant_os::fixture::run_git(&project, &["init", "-q"])?;
        provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
        provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::new(project.join(".git"))?,
        )?;
        std::fs::remove_dir_all(&project)?;
        let resolved = crate::roots::Resolved {
            assets: base.join("assets"),
            home,
            project,
            roots,
            claude_config: None,
            codex_home: None,
        };
        tick(
            &resolved,
            provenant_core::Deadline::at(provenant_core::MonoTime::from_micros(u64::MAX)),
        )?;
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn forge_tick_warns_once_and_skips_a_project_whose_checkout_vanished() -> Result<()> {
        if crate::fabric::peer_tests::host_test(
            "event_delivery::tests::forge_tick_warns_once_and_skips_a_project_whose_checkout_vanished",
        )? {
            return Ok(());
        }
        let temp = tempfile::tempdir()?;
        let base = temp.path().canonicalize()?;
        let home = base.join("home");
        let project = base.join("project");
        let state = base.join("state");
        let common = base.join("common.git");
        for path in [&home, &project, &state] {
            std::fs::create_dir(path)?;
        }
        provenant_os::fixture::private_state_dir(&state)?;
        let mut inputs = provenant_core::RootInputs::new(home.clone(), project.clone());
        inputs.state_dir = Some(state.clone());
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        provenant_os::fixture::run_git(
            &project,
            &[
                "init",
                "-q",
                "--separate-git-dir",
                common.to_str().ok_or("common path")?,
            ],
        )?;
        provenant_os::fixture::run_git(&project, &["commit", "--allow-empty", "-qm", "base"])?;
        let binding = provenant_config::bind_project(
            &roots,
            provenant_config::ProjectLocation::from_verified_primary_checkout(
                common.canonicalize()?,
                project.canonicalize()?,
            )?,
        )?;
        // Delete only the checkout: the separate Git common directory survives.
        std::fs::remove_dir_all(&project)?;
        assert!(binding.common_dir_present());
        assert!(!binding.primary_checkout_present());

        let resolved = crate::roots::Resolved {
            assets: base.join("assets"),
            home,
            project,
            roots,
            claude_config: None,
            codex_home: None,
        };
        let (result, warnings) = provenant_records::warning::capture(|| {
            tick(
                &resolved,
                provenant_core::Deadline::at(provenant_core::MonoTime::from_micros(u64::MAX)),
            )
        });
        result?;
        let warned = warnings
            .iter()
            .filter(|warning| {
                warning.code.as_ref() == provenant_records::warning::PROJECT_ROOT_VANISHED.0
            })
            .collect::<Vec<_>>();
        assert_eq!(warned.len(), 1, "reported once: {warnings:?}");
        assert!(
            warned[0]
                .next
                .iter()
                .any(|step| step.display_text().contains("provenant gc --apply")),
            "the warning names the unregister command: {:?}",
            warned[0]
        );
        Ok(())
    }

    #[test]
    fn failed_local_indexes_are_independent_and_nonfatal() {
        let project = ProjectId::from_bytes([36; 16]);
        let graph_calls = std::cell::Cell::new(0);
        let note_calls = std::cell::Cell::new(0);
        let forge_calls = std::cell::Cell::new(0);
        let tick = || {
            fetch_then_index(
                project,
                || {
                    forge_calls.set(forge_calls.get() + 1);
                    Ok(())
                },
                || {
                    graph_calls.set(graph_calls.get() + 1);
                    Err("corrupt graph checkpoint".into())
                },
                || {
                    note_calls.set(note_calls.get() + 1);
                    Err("corrupt note file".into())
                },
            )
        };
        tick().unwrap();
        assert_eq!(graph_calls.get(), 1);
        assert_eq!(note_calls.get(), 1);
        assert_eq!(forge_calls.get(), 1);
    }

    #[test]
    fn page_fetch_precedes_local_indexing() {
        let order = std::cell::RefCell::new(Vec::new());
        let value = fetch_then_index(
            "p",
            || {
                order.borrow_mut().push("fetch");
                Ok(7)
            },
            || {
                order.borrow_mut().push("graphs");
                Ok(())
            },
            || {
                order.borrow_mut().push("notes");
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(value, 7);
        assert_eq!(order.into_inner(), vec!["fetch", "graphs", "notes"]);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn note_and_correction_reconcile_once_across_consumer_reconnects() -> Result<()> {
        use provenant_fabric_runtime::notes::{Board, Host, Input, Scope};
        use provenant_records::lane_contract::ObservationKind;

        let (_temp, roots, project) = fixture();
        let clock = SystemClock::new();
        let cancel = CancelToken::new();
        let board = Board::open(
            roots.state.as_path(),
            project,
            provenant_fabric_runtime::notes::default_read_cap(),
            provenant_fabric_runtime::notes::default_byte_cap(),
        )?;
        let host = Host::chair(project, None);
        let add = |text: &str, resolves| Input::Add {
            text: text.into(),
            tags: vec![],
            scope: Scope::Project,
            resolves,
        };
        let first: provenant_fabric_runtime::notes::Note = serde_json::from_value(board.execute(
            &host,
            add("first note", None),
            &clock,
            &cancel,
        )?)?;
        index_notes(&roots, project)?;

        let request = Request {
            clock: &clock,
            cancel: &cancel,
        };
        let store = Store::open(&roots.state, &request)?;
        let consumer = provenant_core::ActorId::from_bytes([35; 16]);
        let first_page = store.observations(project, 0, 10, &request)?;
        assert_eq!(first_page.items.len(), 1);
        assert_eq!(first_page.items[0].id, first.id);
        assert_eq!(first_page.items[0].lineage, first.lineage);
        assert_eq!(first_page.items[0].kind, ObservationKind::Note);
        store.mark_events_read(project, consumer, first_page.items[0].cursor.seq, &request)?;
        let resumed = store.event_cursor(project, consumer, &request)?;
        assert_eq!(resumed, first_page.items[0].cursor.seq);

        let correction: provenant_fabric_runtime::notes::Note = serde_json::from_value(
            board.execute(&host, add("corrected note", Some(1)), &clock, &cancel)?,
        )?;
        index_notes(&roots, project)?;
        // Simulate retry after the source was indexed but before its cursor was
        // trusted by a restarted indexer.
        let dir = directory(&roots, project)?;
        save(&dir, "notes-cursor.json", &0_i64)?;
        index_notes(&roots, project)?;

        let after_reconnect = store.observations(project, resumed, 10, &request)?;
        assert_eq!(after_reconnect.items.len(), 1);
        assert_eq!(after_reconnect.items[0].id, correction.id);
        assert_eq!(after_reconnect.items[0].kind, ObservationKind::Note);
        assert_eq!(after_reconnect.items[0].detail["resolves"], 1);
        store.mark_events_read(
            project,
            consumer,
            after_reconnect.items[0].cursor.seq,
            &request,
        )?;
        assert_eq!(
            store
                .observations(
                    project,
                    store.event_cursor(project, consumer, &request)?,
                    10,
                    &request,
                )?
                .items
                .len(),
            0
        );
        let replay = store.observations(project, 0, 10, &request)?;
        assert_eq!(
            replay
                .items
                .iter()
                .map(|event| event.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str(), correction.id.as_str()]
        );
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn journal_subscriptions_register_projects_for_host_discovery() -> Result<()> {
        if crate::fabric::peer_tests::host_test(
            "event_delivery::tests::journal_subscriptions_register_projects_for_host_discovery",
        )? {
            return Ok(());
        }
        let rig = crate::worktree_tests::support::TestRig::new()?;
        let journal = Journal::open(&rig.state, &rig.project)?;
        journal.register_pr("owner/repo", 7, None)?;
        let bindings = provenant_config::registered_projects(&journal.roots)?;
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].project_id(), journal.project);
        let resumed = Journal::open(&rig.state, &rig.project)?;
        assert_eq!(resumed.project, journal.project);
        assert_eq!(
            provenant_config::registered_projects(&resumed.roots)?.len(),
            bindings.len()
        );
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn host_projection_deduplicates_reconnects_and_preserves_subscription_after_gap() {
        let (_temp, roots, project) = fixture();
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: None,
        };
        let mut journal = Journal::for_project(roots.clone(), project).unwrap();
        journal.register_pr("owner/repo", 7, None).unwrap();
        let cancel = CancelToken::new();
        assert!(
            journal
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap()
                .is_none()
        );
        let mut progress = PollProgress::default();
        let pending = json!({"repository":"owner/repo","number":7,"head":"abc","checks":[{"bucket":"pending"}],"status":{"state":"OPEN"}});
        project_poll(
            &roots,
            project,
            &subscription,
            &mut progress,
            pending.clone(),
        )
        .unwrap();
        // A crash before progress publication retries the same source identity.
        project_poll(
            &roots,
            project,
            &subscription,
            &mut PollProgress::default(),
            pending.clone(),
        )
        .unwrap();
        assert_eq!(
            journal
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap(),
            Some(pending)
        );
        let cursor = journal.cursor;
        assert!(
            journal
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap()
                .is_none()
        );
        drop(journal);
        let complete = json!({"repository":"owner/repo","number":7,"head":"abc","checks":[{"bucket":"pass"}],"status":{"state":"CLOSED"}});
        project_poll(
            &roots,
            project,
            &subscription,
            &mut progress,
            complete.clone(),
        )
        .unwrap();
        assert!(
            progress.closed,
            "closed outcomes stop active polling without deleting subscriptions"
        );
        let mut resumed = Journal::resume_graphs(roots.clone(), project, cursor).unwrap();
        assert_eq!(
            resumed
                .next_pr("owner/repo", 7, Duration::ZERO, &cancel)
                .unwrap(),
            Some(complete)
        );
        let dir = directory(&roots, project).unwrap();
        let kept: Subscription =
            read(&dir, &format!("pr-{}.json", key(&subscription).unwrap())).unwrap();
        assert_eq!(
            kept.number, subscription.number,
            "expiry keeps the host interest alive"
        );
    }

    #[test]
    fn webhook_pr_events_wake_waiters_with_identifier_only_markers() -> Result<()> {
        let (_temp, roots, project) = fixture();
        let mut journal = Journal::for_project(roots.clone(), project).unwrap();
        let payload = json!({"check_run":{"id":42,"head_sha":"abc",
            "status":"completed","conclusion":"success",
            "pull_requests":[{"number":7},{"number":9}]}});
        let (kind, detail) = normalise_webhook("owner/repo", payload).unwrap();
        append(&roots, project, "webhook:check-run:42".into(), kind, detail)?;

        let marker = journal
            .next_pr("owner/repo", 9, Duration::ZERO, &CancelToken::new())?
            .expect("webhook association wakes the matching PR waiter");
        assert_eq!(marker, json!({"repository":"owner/repo","number":9}));
        assert!(marker.get("status").is_none());
        assert!(marker.get("head").is_none());
        Ok(())
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn large_check_matrices_preserve_failure_and_the_pinned_head_in_a_bounded_projection() {
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: Some("old-head".into()),
        };
        let mut checks = vec![
            crate::pr::observer::Check {
                head: "old-head".into(),
                name: "x".repeat(700),
                job_id: None,
                bucket: "pass".into(),
                app: None,
                app_slug: None,
                conclusion: None,
                registered: None,
                url: None,
                excerpt: String::new(),
            };
            512
        ];
        checks.last_mut().unwrap().bucket = "fail".into();
        let status: crate::pr::status::Line = serde_json::from_value(json!({
            "number":7,"state":"OPEN","head":"new-head","draft":false,
            "checks_passed":511,"checks_total":512,"checks_failed":1,"checks_pending":0,
            "failing_checks":vec!["x".repeat(700);512],"merge":"UNKNOWN","review":"unknown","error":null
        })).unwrap();
        let detail = poll_detail(&subscription, &status, &checks).unwrap();
        assert_eq!(detail["head"], "old-head");
        assert_eq!(detail["pull_head"], "new-head");
        assert_eq!(detail["checks_truncated"], true);
        assert!(
            detail["checks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|check| check["bucket"] == "fail")
        );
        assert!(
            serde_json::to_vec(&detail).unwrap().len() < provenant_fabric_store::MAX_BODY_BYTES
        );
    }

    // Spec: docs/specs/cli.md#integration-batch-membership — an unsuccessful wait names the failed check.
    #[test]
    fn host_poller_failure_event_names_the_failed_check_in_an_incomplete_wait() {
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: None,
        };
        let names: Vec<String> = (0..64).map(|i| format!("check-{i}")).collect();
        let details: Vec<_> = names
            .iter()
            .map(|name| json!({"name":name,"url":"https://ci/run/1","bucket":"fail"}))
            .collect();
        let status: crate::pr::status::Line = serde_json::from_value(json!({
            "number":7,"state":"OPEN","head":"abc","draft":false,
            "checks_passed":0,"checks_total":64,"checks_failed":64,"checks_pending":0,
            "failing_checks":names.clone(),"failed_check_details":details,
            "merge":"UNKNOWN","review":"unknown","error":null
        }))
        .unwrap();
        let detail = poll_detail(&subscription, &status, &[]).unwrap();
        // A bounded projection must still carry failure details into the
        // journal, without letting a large matrix overflow the event body.
        let projected = detail["status"]["failed_check_details"].as_array().unwrap();
        assert_eq!(projected.len(), MAX_FAILURE_DETAILS);
        assert!(
            serde_json::to_vec(&detail).unwrap().len() < provenant_fabric_store::MAX_BODY_BYTES
        );
        // A default (incomplete) wait settles from the event directly, with no
        // fresh observer read, so the failure must still be named downstream.
        let mut subject = None;
        let mut seen = false;
        let reading = crate::status_wait::reading_pr_event(
            &mut subject,
            &mut seen,
            &crate::status_wait::PrSample {
                number: 7,
                repository: &Ok("owner/repo".into()),
                policy: provenant_records::subject::SubjectPolicy::Pinned,
                policy_source: &crate::status_wait::test_policy_source(),
                now: provenant_core::WallTime::UNIX_EPOCH,
            },
            false,
            detail,
        );
        assert_eq!(reading.settle, crate::status_wait::Settle::Unsuccessful);
        let recovered: crate::pr::status::Line =
            serde_json::from_value(reading.data.clone()).unwrap();
        let lines = recovered.failure_lines();
        assert_eq!(lines.len(), MAX_FAILURE_DETAILS);
        assert!(lines[0].contains("check-0"), "{lines:?}");
        assert!(lines[0].contains("https://ci/run/1"), "{lines:?}");
    }

    #[test]
    fn pr_status_journal_round_trip_settles_for_empty_and_large_matrices() {
        for count in [0, 512] {
            let (_temp, roots, project) = fixture();
            let mut journal = Journal::for_project(roots.clone(), project).unwrap();
            let subscription = Subscription {
                repository: "owner/repo".into(),
                number: 7,
                head: None,
            };
            let checks = vec![
                crate::pr::observer::Check {
                    head: "abc".into(),
                    name: "x".repeat(700),
                    job_id: None,
                    bucket: "fail".into(),
                    app: None,
                    app_slug: None,
                    conclusion: None,
                    registered: None,
                    url: None,
                    excerpt: String::new(),
                };
                count
            ];
            let status: crate::pr::status::Line = serde_json::from_value(json!({
                "number":7, "state":"OPEN", "head":"abc", "draft":false,
                "checks_passed":0, "checks_total":count, "checks_failed":count, "checks_pending":0,
                "failing_checks":vec!["x".repeat(700); count], "merge":"CLEAN", "review":"APPROVED"
            }))
            .unwrap();
            let detail = poll_detail(&subscription, &status, &checks).unwrap();
            let projected = detail["status"].clone();
            append(
                &roots,
                project,
                format!("matrix-{count}"),
                ObservationKind::CheckRun,
                detail,
            )
            .unwrap();
            let recovered = journal
                .next_pr("owner/repo", 7, Duration::ZERO, &CancelToken::new())
                .unwrap()
                .unwrap();
            let round_trip: crate::pr::status::Line =
                serde_json::from_value(recovered["status"].clone()).unwrap();
            assert_eq!(serde_json::to_value(round_trip).unwrap(), projected);
            let mut subject = None;
            let mut seen = false;
            let reading = crate::status_wait::reading_pr_event(
                &mut subject,
                &mut seen,
                &crate::status_wait::PrSample {
                    number: 7,
                    repository: &Ok("owner/repo".into()),
                    policy: provenant_records::subject::SubjectPolicy::Pinned,
                    policy_source: &crate::status_wait::test_policy_source(),
                    now: provenant_core::WallTime::UNIX_EPOCH,
                },
                false,
                recovered,
            );
            assert_eq!(
                reading.settle,
                if count == 0 {
                    crate::status_wait::Settle::Succeeded
                } else {
                    crate::status_wait::Settle::Unsuccessful
                }
            );
            assert!(
                reading
                    .line
                    .contains(if count == 0 { "ready" } else { "failing" })
            );
            assert!(!reading.line.contains("warning"));
        }
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn journal_drains_full_pages_before_waiting_for_another_hint() {
        let (_temp, roots, project) = fixture();
        let mut journal = Journal::for_project(roots.clone(), project).unwrap();
        for number in 1..=REPLAY_PAGE + 2 {
            append(
                &roots,
                project,
                format!("page-{number}"),
                ObservationKind::CheckRun,
                json!({"repository":"owner/repo","number":number,"status":{}}),
            )
            .unwrap();
        }
        assert!(
            journal
                .next_pr(
                    "owner/repo",
                    REPLAY_PAGE + 2,
                    Duration::ZERO,
                    &CancelToken::new()
                )
                .unwrap()
                .is_some()
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn unavailable_webhook_capabilities_warn_and_poll_without_refusal() {
        for requested in ["auto", "webhook", "poll"] {
            for capability in [
                Ok(()),
                Err("missing extension"),
                Err("missing admin rights"),
            ] {
                let probed = std::cell::Cell::new(false);
                let (source, warning) = choose_source(requested, || {
                    probed.set(true);
                    capability
                });
                assert_eq!(probed.get(), requested != "poll");
                assert_eq!(
                    source,
                    if requested != "poll" && capability.is_ok() {
                        "webhook"
                    } else {
                        "poll"
                    }
                );
                assert_eq!(
                    warning.is_some(),
                    requested != "poll" && capability.is_err()
                );
            }
        }
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn unchanged_host_reads_keep_waiters_local_until_a_real_source_gap() {
        let (_temp, roots, project) = fixture();
        let journal = Journal::for_project(roots.clone(), project).unwrap();
        let subscription = Subscription {
            repository: "owner/repo".into(),
            number: 7,
            head: None,
        };
        let dir = directory(&roots, project).unwrap();
        let name = format!("progress-pr-{}.json", key(&subscription).unwrap());
        let mut progress = PollProgress {
            last_success_micros: 100,
            ..Default::default()
        };
        save(&dir, &name, &progress).unwrap();
        assert!(
            journal
                .source_current_at("owner/repo", 7, None, 101)
                .unwrap()
        );
        let tick = i64::from(crate::tick_agent::INTERVAL) * 1_000_000;
        assert!(
            journal
                .source_current_at("owner/repo", 7, None, 100 + tick)
                .unwrap()
        );
        assert!(
            !journal
                .source_current_at("owner/repo", 7, None, 100 + 3 * tick)
                .unwrap()
        );
        progress.failures = 1;
        save(&dir, &name, &progress).unwrap();
        assert!(
            !journal
                .source_current_at("owner/repo", 7, None, 101)
                .unwrap()
        );
        progress.failures = 0;
        progress.closed = true;
        save(&dir, &name, &progress).unwrap();
        assert!(
            !journal
                .source_current_at("owner/repo", 7, None, 101)
                .unwrap()
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn forwarder_disconnect_switches_to_polling_and_preserves_the_config_source() {
        let (_temp, roots, project) = fixture();
        let dir = directory(&roots, project).unwrap();
        let repository = "owner/repo";
        let name = format!("source-{}.json", key(&repository).unwrap());
        save(
            &dir,
            &name,
            &Selection {
                requested: "auto".into(),
                selected: "webhook".into(),
                source: "configured fixture origin".into(),
                warning: None,
                retry_micros: i64::MAX,
            },
        )
        .unwrap();
        record_forwarder_exit(&dir, repository, 10, "account switch").unwrap();
        let selected: Selection = read(&dir, &name).unwrap();
        assert_eq!(selected.requested, "auto");
        assert_eq!(selected.source, "configured fixture origin");
        assert_eq!(selected.selected, "poll");
        assert!(selected.warning.is_some());
        assert!(selected.retry_micros > 10 && selected.retry_micros < i64::MAX);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn host_poll_failures_back_off_and_success_restores_cadence() {
        let mut progress = PollProgress::default();
        let interval = Duration::from_secs(5);
        progress.schedule(0, false, interval);
        let first = progress.next_micros;
        progress.schedule(0, false, interval);
        assert!(progress.next_micros > first);
        for _ in 0..100 {
            progress.schedule(0, false, interval);
        }
        assert!(progress.next_micros <= 900_000_000);
        progress.schedule(0, true, interval);
        assert_eq!(progress.failures, 0);
        assert_eq!(progress.next_micros, interval.as_micros() as i64);
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn oversized_forge_metadata_is_bounded_with_explicit_truncation() {
        let payload = json!({"check_run":{"id":1,"head_sha":"abc",
            "name":"n".repeat(provenant_fabric_store::MAX_BODY_BYTES),
            "pull_requests":(1..=MAX_PR_ASSOCIATIONS+1).map(|number| json!({"number":number})).collect::<Vec<_>>()}});
        let (_, detail) = normalise_webhook("owner/repo", payload).unwrap();
        assert_eq!(detail["name_truncated"], true);
        assert_eq!(detail["pull_requests_truncated"], true);
        assert_eq!(
            detail["pull_requests"].as_array().unwrap().len(),
            MAX_PR_ASSOCIATIONS
        );
        assert!(detail["name"].as_str().unwrap().len() <= LABEL_BYTES);
        assert_eq!(detail["number"], 1);
        assert_eq!(detail["id"], 1);
        assert!(
            serde_json::to_vec(&detail).unwrap().len() < provenant_fabric_store::MAX_BODY_BYTES
        );
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn webhook_normalisation_keeps_forge_kinds_and_discards_user_text() {
        for (field, kind) in [
            ("check_run", ObservationKind::CheckRun),
            ("check_suite", ObservationKind::CheckSuite),
            ("workflow_run", ObservationKind::WorkflowRun),
            ("pull_request", ObservationKind::PullRequest),
            ("comment", ObservationKind::IssueComment),
        ] {
            let payload = json!({field:{"id":1,"head_sha":"abc","body":"not indexed","status":"completed"},"repository":{"full_name":"owner/repo"},"number":7,"action":"completed"});
            let (actual, detail) = normalise_webhook("owner/repo", payload).unwrap();
            assert_eq!(actual, kind);
            assert_eq!(detail["repository"], "owner/repo");
            assert!(!detail.to_string().contains("not indexed"));
        }
        for field in ["check_run", "check_suite", "workflow_run"] {
            let name = (field != "check_suite").then_some("build");
            let payload = json!({field:{"id":1,"name":name,"head_sha":"abc",
                "pull_requests":[{"number":7},{"number":9}],"status":"completed"},"action":"completed"});
            let (_, detail) = normalise_webhook("owner/repo", payload).unwrap();
            assert_eq!(detail["number"], 7);
            assert_eq!(detail["pull_requests"], json!([7, 9]));
            assert_eq!(detail["name"], json!(name));
            assert_eq!(detail["head"], "abc");
        }
    }
}
