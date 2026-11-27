#[cfg(test)]
#[allow(dead_code, unused_imports, clippy::disallowed_methods)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{
        collections::{BTreeMap, VecDeque},
        hash::{Hash, Hasher},
        io,
        sync::{Arc, Condvar, Mutex},
    };

    const HEAD: &str = "1111111111111111111111111111111111111111";
    const OTHER: &str = "2222222222222222222222222222222222222222";
    /// 2026-01-01T00:00:00Z.
    const START: i64 = 1_767_225_600_000;

    /// One forge and one clock shared by every waiter in a test.
    #[derive(Default)]
    struct World {
        now_ms: i64,
        requests: Vec<(String, Option<String>)>,
        full_reads: usize,
        resources: BTreeMap<String, Value>,
        runs: BTreeMap<String, Value>,
        statuses: BTreeMap<String, Value>,
        /// Unix second until which every request is rate limited.
        limited_until: Option<i64>,
        /// The next request is limited with a reset already past.
        past_limit: bool,
        delay: Duration,
        /// Successive bodies served for a pull resource, so a test can model
        /// GitHub settling an UNKNOWN mergeability into a real reading.
        pull_replies: VecDeque<Value>,
    }
    /// Holds a forge read open until enough waiters have entered `checks`.
    #[derive(Default)]
    struct Gate {
        entered: Mutex<usize>,
        needed: Mutex<usize>,
        changed: Condvar,
    }
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<World>>, Arc<Gate>);
    impl Shared {
        fn hold_reads_until(&self, waiters: usize) {
            *self.1.needed.lock().expect("gate") = waiters;
        }
        fn enter(&self) {
            *self.1.entered.lock().expect("gate") += 1;
            self.1.changed.notify_all();
        }
        fn pass(&self) {
            let needed = *self.1.needed.lock().expect("gate");
            let entered = self.1.entered.lock().expect("gate");
            drop(
                self.1
                    .changed
                    .wait_while(entered, |entered| *entered < needed)
                    .expect("gate"),
            );
        }
        fn new() -> Self {
            let shared = Self::default();
            shared.with(|world| world.now_ms = START);
            shared
        }
        fn with<T>(&self, f: impl FnOnce(&mut World) -> T) -> T {
            f(&mut self.0.lock().expect("world"))
        }
        fn requests(&self) -> usize {
            self.with(|world| world.requests.len())
        }
        fn queue_pull(&self, replies: impl IntoIterator<Item = Value>) {
            self.with(|world| world.pull_replies.extend(replies));
        }
        fn observer(&self, state: &Path) -> Observer<Shared, Shared> {
            Observer::with(state, "Owner/Repo", self.clone(), self.clone()).expect("observer")
        }
    }
    fn forge_response(response: Response) -> provenant_config::ForgeResponse {
        provenant_config::ForgeResponse {
            status: Some(response.status),
            headers: provenant_config::HeadersObservation::Present {
                status: response.status,
                values: response.headers,
            },
            body: response.body,
            execution: provenant_config::PortExecution {
                disposition: provenant_config::PortCommandDisposition::Succeeded,
                outcome: None,
            },
        }
    }

    impl Forge for Shared {
        fn request(
            &mut self,
            operation: provenant_config::ForgeOperation,
            _budget: Duration,
        ) -> Result<provenant_config::ForgeResponse, provenant_config::PortError> {
            let provenant_config::ForgeOperation::Api { path, etag, .. } = operation else {
                return Err(provenant_config::PortError::Response(
                    "expected API request",
                ));
            };
            let path = path.as_str();
            let etag = etag.as_deref();
            let respond = || -> io::Result<Response> {
                self.pass();
                let delay = self.with(|world| {
                    world.requests.push((path.into(), etag.map(str::to_owned)));
                    world.delay
                });
                self.sleep(delay);
                self.with(|world| {
                    let mut headers = BTreeMap::new();
                    let now = world.now_ms / 1000;
                    if std::mem::take(&mut world.past_limit) {
                        headers.insert("retry-after".into(), "0".into());
                        return Ok(Response {
                            status: 429,
                            headers,
                            body: Vec::new(),
                        });
                    }
                    if let Some(until) = world.limited_until.filter(|until| *until > now) {
                        headers.insert("x-ratelimit-remaining".into(), "0".into());
                        headers.insert("x-ratelimit-reset".into(), until.to_string());
                        return Ok(Response {
                            status: 403,
                            headers,
                            body: br#"{"message":"API rate limit exceeded"}"#.to_vec(),
                        });
                    }
                    let commit = path
                        .split("/commits/")
                        .nth(1)
                        .and_then(|rest| rest.split('/').next())
                        .unwrap_or_default();
                    let body = if path.contains("/pulls/") && !world.pull_replies.is_empty() {
                        world.pull_replies.pop_front()
                    } else if !path.contains("/commits/") {
                        world.resources.get(path).cloned()
                    } else if path.contains("/check-runs?") {
                        world.runs.get(commit).cloned()
                    } else {
                        world.statuses.get(commit).cloned()
                    }
                    .unwrap_or_else(|| json!([]));
                    let mut hasher = std::collections::hash_map::DefaultHasher::new();
                    body.to_string().hash(&mut hasher);
                    let tag = format!("W/\"{:x}\"", hasher.finish());
                    headers.insert("etag".into(), tag.clone());
                    if etag == Some(tag.as_str()) {
                        return Ok(Response {
                            status: 304,
                            headers,
                            body: Vec::new(),
                        });
                    }
                    world.full_reads += 1;
                    Ok(Response {
                        status: 200,
                        headers,
                        body: serde_json::to_vec(&body)?,
                    })
                })
            };
            let response = respond().map_err(provenant_config::PortError::Executor)?;
            Ok(forge_response(response))
        }
    }
    impl Clock for Shared {
        fn now(&self) -> jiff::Timestamp {
            self.with(|world| jiff::Timestamp::from_millisecond(world.now_ms).expect("time"))
        }
        fn sleep(&self, duration: Duration) {
            let millis = i64::try_from(duration.as_millis()).expect("sleep");
            self.with(|world| world.now_ms += millis);
        }
    }

    fn run(name: &str, status: &str, conclusion: Option<&str>) -> Value {
        json!({"name":name,"head_sha":HEAD,"status":status,"conclusion":conclusion,
            "started_at":"2020-01-01T00:00:00Z","app":{"id":15368,"slug":"github-actions"}})
    }
    fn page(runs: Vec<Value>) -> Value {
        json!({"total_count":runs.len(),"check_runs":runs})
    }
    fn state() -> tempfile::TempDir {
        tempfile::tempdir().expect("state")
    }

    #[test]
    fn actions_runs_are_head_filtered_and_all_pages_contribute() {
        let shared = Shared::new();
        shared.with(|world| {
            world.resources.insert(
                format!("repos/Owner/Repo/actions/runs?head_sha={HEAD}&per_page=100&page=1"),
                json!({"workflow_runs": vec![json!({"status":"completed"}); 100]}),
            );
            world.resources.insert(
                format!("repos/Owner/Repo/actions/runs?head_sha={HEAD}&per_page=100&page=2"),
                json!({"workflow_runs": [{"status":"queued"}]}),
            );
        });
        let dir = state();
        let mut observer = shared.observer(dir.path());
        let request = Request::within(Duration::from_secs(60));
        assert_eq!(
            observer
                .active_workflows(HEAD, Duration::ZERO, &request)
                .unwrap(),
            Some(true)
        );
        assert_eq!(shared.requests(), 2);
        shared.with(|world| {
            world.resources.insert(
                format!("repos/Owner/Repo/actions/runs?head_sha={HEAD}&per_page=100&page=2"),
                json!({"workflow_runs": [{"status":"completed"}]}),
            );
        });
        assert_eq!(
            observer
                .active_workflows(HEAD, Duration::ZERO, &request)
                .unwrap(),
            Some(false)
        );
    }

    #[test]
    fn stored_cache_shapes_preserve_additive_fields_through_bounded_decode() {
        macro_rules! row {
            ($ty:ty, $payload:expr) => {{
                let mut payload = $payload;
                payload["future_field"] = json!({"nested": [1, true]});
                let bytes = serde_json::to_vec(&payload).unwrap();
                let limits = provenant_records::Limits {
                    max_bytes: CACHE_LIMIT,
                    ..Default::default()
                };
                let decoded: $ty = provenant_records::decode(bytes.as_slice(), limits).unwrap();
                let encoded = provenant_records::encode(&decoded).unwrap();
                let actual: Value = provenant_records::decode(encoded.as_slice(), limits).unwrap();
                assert_eq!(actual, payload, "{}", stringify!($ty));
            }};
        }
        row!(Page, json!({"etag":"stable","body":{"number":12}}));
        row!(
            ResourcePage,
            json!({"page":{"etag":"stable","body":{}},"fetched_at":0})
        );
        row!(Cached, json!({"fetched_at":0,"runs":[],"statuses":[]}));
        row!(RateLimit, json!({"reset":100}));
    }

    #[derive(Clone, Default)]
    struct ResourceForge {
        revalidated: Arc<Mutex<bool>>,
    }
    impl Forge for ResourceForge {
        fn request(
            &mut self,
            operation: provenant_config::ForgeOperation,
            _budget: Duration,
        ) -> Result<provenant_config::ForgeResponse, provenant_config::PortError> {
            let provenant_config::ForgeOperation::Api { path, etag, .. } = operation else {
                return Err(provenant_config::PortError::Response(
                    "expected API request",
                ));
            };
            let path = path.as_str();
            let etag = etag.as_deref();
            let respond = || -> io::Result<Response> {
                if path != "repos/Owner/Repo/pulls/12" {
                    return Err(io::Error::other("unknown repository resource"));
                }
                let headers = BTreeMap::from([("etag".into(), "stable".into())]);
                if etag == Some("stable") {
                    *self.revalidated.lock().expect("revalidated") = true;
                    Ok(Response {
                        status: 304,
                        headers,
                        body: Vec::new(),
                    })
                } else {
                    Ok(Response {
                        status: 200,
                        headers,
                        body: br#"{"number":12,"state":"open"}"#.to_vec(),
                    })
                }
            };
            let response = respond().map_err(provenant_config::PortError::Executor)?;
            Ok(forge_response(response))
        }
    }

    #[test]
    fn repository_resource_revalidates_through_the_shared_forge_cache() {
        let state = state();
        let forge = ResourceForge::default();
        let mut observer = Observer::with(state.path(), "Owner/Repo", forge.clone(), Shared::new())
            .expect("observer");
        let first = observer
            .resource(
                "pulls/12",
                Duration::ZERO,
                &Request::within(Duration::from_secs(30)),
            )
            .expect("first resource");
        let second = observer
            .resource(
                "pulls/12",
                Duration::ZERO,
                &Request::within(Duration::from_secs(30)),
            )
            .expect("conditional resource");
        assert_eq!(first, second);
        assert!(
            *forge.revalidated.lock().expect("revalidated"),
            "the cached resource must be conditionally revalidated"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repository_context_uses_the_selected_transport_and_none_does_not_fall_back() {
        use std::os::unix::fs::PermissionsExt;

        let project = tempfile::tempdir().expect("project");
        let state = tempfile::tempdir().expect("state");
        let config = project.path().join(".provenant");
        std::fs::create_dir(&config).expect("port config directory");
        let command = project.path().join("ci-context-command");
        std::fs::write(
            &command,
            r#"#!/bin/sh
[ "$PROVENANT_PORT_REQUEST_PATH" = 'repos/{owner}/{repo}' ] || exit 71
[ -z "$PROVENANT_PORT_IF_NONE_MATCH" ] || exit 72
printf '%s\n' '{"status":200,"headers":{},"body":"{\"full_name\":\"owner/repo\"}"}'
"#,
        )
        .expect("command fixture");
        let mut permissions = std::fs::metadata(&command)
            .expect("command metadata")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&command, permissions).expect("executable command");

        let mut ports = provenant_config::PortTable::builtin();
        ports.ci_observation.backend = provenant_config::PortBackend::Command {
            command: provenant_records::PortCommand {
                argv: vec![command.to_string_lossy().into_owned()],
                exit_codes: provenant_records::CommandExitCodes {
                    success: vec![0],
                    unknown: Vec::new(),
                    other: provenant_records::ExitDisposition::Refused,
                },
            },
        };
        let path = config.join("ports.json");
        std::fs::write(&path, serde_json::to_vec(&ports).expect("encode ports"))
            .expect("write command port");
        assert_eq!(
            repository_for_project(
                state.path(),
                project.path(),
                &Request::within(Duration::from_secs(30)),
            )
            .expect("selected command repository"),
            "owner/repo"
        );

        ports.ci_observation.backend = provenant_config::PortBackend::None;
        std::fs::write(&path, serde_json::to_vec(&ports).expect("encode ports"))
            .expect("write disabled port");
        let error = repository_for_project(
            state.path(),
            project.path(),
            &Request::within(Duration::from_secs(30)),
        )
        .expect_err("none must fail before any request");
        assert!(error.to_string().contains("disabled"), "{error}");
    }

    #[test]
    fn concurrent_waiters_never_queue_and_share_the_cached_read() {
        let shared = Shared::new();
        shared.with(|world| {
            world
                .runs
                .insert(HEAD.into(), page(vec![run("build", "in_progress", None)]));
            world.delay = Duration::from_millis(100);
        });
        // The first read stays open until both waiters are inside `checks`;
        // the second never queues behind it, so it may read too.
        shared.hold_reads_until(2);
        let dir = state();
        let poll = |shared: &Shared| {
            std::thread::scope(|scope| {
                let waiters: Vec<_> = (0..2)
                    .map(|_| {
                        let mut observer = shared.observer(dir.path());
                        let shared = shared.clone();
                        scope.spawn(move || {
                            shared.enter();
                            observer.checks(HEAD, POLL, &Request::within(Duration::from_secs(60)))
                        })
                    })
                    .collect();
                waiters
                    .into_iter()
                    .map(|waiter| waiter.join().expect("waiter").expect("checks"))
                    .collect::<Vec<_>>()
            })
        };
        let first = poll(&shared);
        assert_eq!(first[0], first[1]);
        assert_eq!(first[0][0].bucket, "pending");
        // One read of the head is one check-run request and one status
        // request; a waiter that found the read busy made its own.
        let read = shared.requests();
        assert!(matches!(read, 2 | 4), "{read}");
        // Within the interval, neither waiter reaches the forge.
        shared.with(|world| world.now_ms += 1_000);
        poll(&shared);
        assert_eq!(shared.requests(), read);
    }

    fn pull_resource(mergeable: &str, status: &str, base: &str) -> Value {
        json!({
            "number": 7,
            "state": "open",
            "head": {"sha": HEAD},
            "base": {"ref": base},
            "mergeable": mergeable,
            "mergeStateStatus": status,
            "mergeable_state": match status {
                "DIRTY" => "dirty",
                "UNKNOWN" => "unknown",
                _ => "clean",
            },
            "html_url": "https://github.com/Owner/Repo/pull/7"
        })
    }

    #[test]
    fn mergeability_unknown_is_reread_only_a_bounded_number_of_times() {
        let shared = Shared::new();
        shared.queue_pull([
            json!({"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN"}),
            json!({"mergeable":"UNKNOWN","mergeStateStatus":"UNKNOWN"}),
            json!({"mergeable":"CONFLICTING","mergeStateStatus":"DIRTY"}),
        ]);
        let dir = state();
        let mut observer = shared.observer(dir.path());
        let request = Request::within(Duration::from_secs(60));
        let first = observer
            .resource("pulls/7", Duration::ZERO, &request)
            .unwrap();
        let (state, _) = observer.pull_merge_state(7, first, &request).unwrap();
        assert_eq!(state, MergeState::Conflicting);
        assert_eq!(shared.requests(), 3, "first read plus two rereads");
        assert_eq!(state.no_checks_cause(Some("main")).code, "merge_conflict");

        let shared = Shared::new();
        shared.queue_pull(vec![json!({"mergeable":"UNKNOWN"}); 8]);
        let mut observer = shared.observer(dir.path());
        let first = observer
            .resource("pulls/7", Duration::ZERO, &request)
            .unwrap();
        let (state, _) = observer.pull_merge_state(7, first, &request).unwrap();
        assert_eq!(state, MergeState::Unknown);
        assert_eq!(
            shared.requests(),
            usize::from(retry::Retry::default().mergeable_attempts.value) + 1,
            "UNKNOWN stops at the configured bound"
        );
    }

    fn no_checks_diagnosis(mergeable: &str, status: &str) -> (String, String) {
        let shared = Shared::new();
        shared.with(|world| {
            world.resources.insert(
                "repos/Owner/Repo/pulls/7".into(),
                pull_resource(mergeable, status, "main"),
            );
            // One readable check-run page that reports no runs.
            world.runs.insert(HEAD.into(), page(vec![]));
        });
        let dir = state();
        let mut observer = shared.observer(dir.path());
        let request = Request::within(Duration::from_secs(60));
        let (line, checks) =
            super::super::status::poll_line(&mut observer, 7, None, &request, Duration::ZERO, &[])
                .unwrap();
        assert!(checks.is_empty(), "fake forge reports no checks");
        assert!(line.cause_suffix().contains(line.cause.as_deref().unwrap()));
        (line.cause.expect("cause"), line.next.expect("next step"))
    }

    #[test]
    fn conflicting_head_with_no_checks_diagnoses_a_merge() {
        let (cause, next) = no_checks_diagnosis("CONFLICTING", "DIRTY");
        assert_eq!(cause, "merge_conflict");
        assert!(next.contains("git merge origin/main"), "{next}");
        assert!(next.contains("push"), "{next}");
        assert!(!next.contains("empty commit"), "{next}");
    }

    #[test]
    fn mergeable_head_with_no_checks_keeps_the_trigger_diagnosis() {
        for (mergeable, status) in [("MERGEABLE", "CLEAN"), ("UNKNOWN", "UNKNOWN")] {
            let (cause, next) = no_checks_diagnosis(mergeable, status);
            assert_eq!(cause, "not_scheduled", "{mergeable}");
            assert!(next.contains("push an empty commit"), "{next}");
            assert!(!next.contains("merge_conflict"), "{next}");
        }
    }

    // Spec: docs/specs/cli.md#waitpr-causes
    #[test]
    fn a_wait_reads_the_ruleset_required_check_instead_of_the_observed_set() {
        let read_line = |selector: Value, aggregate: Option<Value>| {
            let shared = Shared::new();
            shared.with(|world| {
                world.resources.insert(
                    "repos/Owner/Repo/pulls/7".into(),
                    pull_resource("MERGEABLE", "CLEAN", "main"),
                );
                // The shape GitHub returns for a branch protected only by a
                // ruleset: protected, classic protection disabled (its own
                // endpoint answers 404), the required check in the ruleset.
                world.resources.insert(
                    "repos/Owner/Repo/branches/main".into(),
                    json!({"protected":true,"protection":{"enabled":false,
                        "required_status_checks":{"enforcement_level":"off","contexts":[],"checks":[]}}}),
                );
                world.resources.insert(
                    "repos/Owner/Repo/rules/branches/main?per_page=100".into(),
                    json!([{"type":"required_status_checks","parameters":{
                        "required_status_checks":[{"context":"ci-status","integration_id":null}]
                    }}]),
                );
                let mut runs = vec![selector];
                runs.extend(aggregate);
                world.runs.insert(HEAD.into(), page(runs));
            });
            let dir = state();
            let mut observer = shared.observer(dir.path());
            let request = Request::within(Duration::from_secs(60));
            super::super::status::poll_line(&mut observer, 7, None, &request, Duration::ZERO, &[])
                .unwrap()
                .0
        };

        let pending = read_line(run("Select CI suites", "in_progress", None), None);
        assert_eq!(pending.class(), "pending");
        assert_eq!(pending.verdict(), None);
        assert!(pending.text().contains("required_pending=ci-status"));

        let green = read_line(
            run("Select CI suites", "completed", Some("success")),
            Some(run("ci-status", "completed", Some("success"))),
        );
        assert_eq!(green.verdict(), Some(true));
        assert!(!green.text().contains("required_pending="));
    }

    // Spec: docs/specs/lane-contracts.md#observation-events
    #[test]
    fn host_poller_revalidates_metadata_and_checks_with_304() {
        let shared = Shared::new();
        shared.with(|world| {
            world.resources.insert("repos/Owner/Repo/pulls/7".into(), json!({
                "state":"open", "head":{"sha":HEAD}, "base":{"ref":"main"}, "mergeable":true, "html_url":"https://github.com/Owner/Repo/pull/7"
            }));
            world.resources.insert(
                "repos/Owner/Repo/branches/main".into(),
                json!({"protected":false}),
            );
            world.resources.insert(
                "repos/Owner/Repo/rules/branches/main?per_page=100".into(),
                json!([{"type":"required_status_checks","parameters":{
                    "required_status_checks":[{"context":"build","integration_id":null}]
                }}]),
            );
            world.runs.insert(HEAD.into(), page(vec![run("build", "completed", Some("success"))]));
        });
        let dir = state();
        let mut observer = shared.observer(dir.path());
        let request = Request::within(Duration::from_secs(60));
        let first =
            super::super::status::poll_line(&mut observer, 7, None, &request, Duration::ZERO, &[])
                .unwrap();
        let full = shared.with(|world| world.full_reads);
        let calls = shared.requests();
        let again =
            super::super::status::poll_line(&mut observer, 7, None, &request, Duration::ZERO, &[])
                .unwrap();
        assert_eq!(first, again);
        assert_eq!(
            calls, 5,
            "PR metadata, branch policy, ruleset, check runs, legacy statuses"
        );
        assert_eq!(shared.requests(), 2 * calls);
        assert_eq!(first.0.verdict(), Some(true));
        assert_eq!(
            shared.with(|world| world.full_reads),
            full,
            "all second responses were 304"
        );
        assert!(shared.with(|world| world.requests[calls..].iter().all(|(_, tag)| tag.is_some())));
    }

    #[test]
    fn an_unchanged_answer_is_revalidated_by_a_304() {
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
        });
        let dir = state();
        let first = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("first");
        assert_eq!(shared.with(|world| world.full_reads), 2);
        // A one-shot reader in another process accepts no age, yet an unchanged
        // head costs only conditional requests.
        let again = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("again");
        assert_eq!(again, first);
        assert_eq!(shared.with(|world| world.full_reads), 2);
        let conditional = shared.with(|world| world.requests[2..].to_vec());
        assert_eq!(conditional.len(), 2);
        assert!(
            conditional.iter().all(|(_, etag)| etag.is_some()),
            "{conditional:?}"
        );
        // A changed answer is read in full.
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("failure"))]),
            );
        });
        let changed = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("changed");
        assert_eq!(changed[0].bucket, "fail");
        assert_eq!(shared.with(|world| world.full_reads), 3);
    }

    #[test]
    fn a_rate_limit_stops_every_waiter_until_its_reset() {
        let shared = Shared::new();
        let reset = START / 1000 + 120;
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
            world.limited_until = Some(reset);
        });
        let dir = state();
        let error = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect_err("limited");
        let at = jiff::Timestamp::from_second(reset).expect("reset");
        assert!(
            matches!(error, ObserveError::RateLimited { reset } if reset == at),
            "{error}"
        );
        assert!(error.to_string().contains(&at.to_string()), "{error}");
        assert_eq!(shared.requests(), 1);
        // Another waiter, on another head, holds off without asking again.
        let error = shared
            .observer(dir.path())
            .checks(OTHER, POLL, &Request::within(Duration::from_secs(60)))
            .expect_err("still limited");
        assert!(matches!(error, ObserveError::RateLimited { .. }), "{error}");
        assert_eq!(shared.requests(), 1);
        // A budget that outlasts the reset still skips this poll at once.
        let before = shared.with(|world| world.now_ms);
        let error = shared
            .observer(dir.path())
            .checks(HEAD, POLL, &Request::within(Duration::from_secs(300)))
            .expect_err("skips the poll");
        assert!(matches!(error, ObserveError::RateLimited { .. }), "{error}");
        assert_eq!(shared.with(|world| world.now_ms), before);
        assert_eq!(shared.requests(), 1);
        // The first poll after the reset reads again.
        shared.with(|world| {
            world.now_ms = reset * 1000;
            world.limited_until = None;
        });
        let checks = shared
            .observer(dir.path())
            .checks(HEAD, POLL, &Request::within(Duration::from_secs(300)))
            .expect("after reset");
        assert_eq!(checks[0].bucket, "pass");
    }

    #[test]
    fn a_rate_limit_whose_reset_is_past_still_skips_one_poll() {
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
            world.past_limit = true;
        });
        let dir = state();
        let error = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect_err("skips the poll");
        assert!(matches!(error, ObserveError::RateLimited { .. }), "{error}");
        shared.with(|world| world.now_ms += 1000);
        let checks = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("next poll");
        assert_eq!(checks[0].bucket, "pass");
    }

    #[test]
    fn trimming_keeps_the_ids_a_rerun_needs() {
        let shared = Shared::new();
        let mut kept = run("build", "completed", Some("failure"));
        kept["id"] = json!(42);
        kept["check_suite"] = json!({"id": 7, "app": {"id": 1}});
        shared.with(|world| {
            world.runs.insert(HEAD.into(), page(vec![kept]));
        });
        let dir = state();
        shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("checks");
        let repository = std::fs::read_dir(dir.path().join("ci-observer"))
            .expect("root")
            .next()
            .expect("repository")
            .expect("entry")
            .path();
        let text = std::fs::read_to_string(repository.join(format!("{HEAD}.json"))).expect("cache");
        let cached: Cached = serde_json::from_str(&text).expect("cached");
        let run = &cached.runs[0].body["check_runs"][0];
        assert_eq!(run["id"], 42);
        assert_eq!(run["check_suite"], json!({"id": 7}));
    }

    #[test]
    fn pr_status_and_the_landing_gate_agree_on_one_observation() {
        let shared = Shared::new();
        let dir = state();
        for (runs, expected) in [
            (
                vec![run("build", "in_progress", None)],
                ("pending", "pending"),
            ),
            (
                vec![run("build", "completed", Some("failure"))],
                ("failing", "failed"),
            ),
            (
                vec![run("build", "completed", Some("success"))],
                ("ready", "passed"),
            ),
            (
                vec![
                    run("lint", "in_progress", None),
                    run("test", "completed", Some("failure")),
                    run("build", "completed", Some("success")),
                ],
                ("failing", "failed"),
            ),
        ] {
            shared.with(|world| {
                world.runs.insert(HEAD.into(), page(runs));
            });
            let checks = shared
                .observer(dir.path())
                .checks(
                    HEAD,
                    Duration::ZERO,
                    &Request::within(Duration::from_secs(60)),
                )
                .expect("checks");
            let status = crate::pr::status::class_of(HEAD, &checks);
            let gate = match crate::pr::landing::adapters::gate_checks(checks).1 {
                crate::pr::landing::model::GateResult::Passed => "passed",
                crate::pr::landing::model::GateResult::Pending => "pending",
                crate::pr::landing::model::GateResult::Failed => "failed",
                other => panic!("unexpected gate result {other:?}"),
            };
            assert_eq!((status, gate), expected);
        }
    }

    /// Spec: docs/specs/cli.md#ci-observations
    #[test]
    fn one_fake_forge_drives_pending_to_green_and_red_through_both_waiters() {
        let dir = state();
        for (final_run, expected) in [
            (
                run("build", "completed", Some("success")),
                ("ready", "passed"),
            ),
            (
                run("build", "completed", Some("failure")),
                ("failing", "failed"),
            ),
        ] {
            let shared = Shared::new();
            let mut observer = shared.observer(dir.path());
            let request = Request::within(Duration::from_secs(60));
            let reads = std::cell::Cell::new(0_usize);
            let pauses: std::cell::RefCell<Vec<Duration>> = std::cell::RefCell::new(Vec::new());
            let outcome = super::await_ci(
                || {
                    let index = reads.get();
                    reads.set(index + 1);
                    let run = if index == 0 {
                        run("build", "in_progress", None)
                    } else {
                        final_run.clone()
                    };
                    shared.with(|world| {
                        world.runs.insert(HEAD.into(), page(vec![run]));
                    });
                    let checks = observer
                        .checks(HEAD, Duration::ZERO, &request)
                        .unwrap_or_default();
                    let status = crate::pr::status::class_of(HEAD, &checks);
                    let gate = match crate::pr::landing::adapters::gate_checks(checks).1 {
                        crate::pr::landing::model::GateResult::Passed => "passed",
                        crate::pr::landing::model::GateResult::Failed => "failed",
                        crate::pr::landing::model::GateResult::Pending => "pending",
                        other => panic!("unexpected gate result {other:?}"),
                    };
                    // Read 0 deliberately injects an in-progress run, so both
                    // waiters must agree on `pending` there; the final state
                    // is checked on the settled read.
                    let read_expected = if index == 0 {
                        ("pending", "pending")
                    } else {
                        expected
                    };
                    assert_eq!(
                        (status, gate),
                        read_expected,
                        "the landing gate and wait --pr must agree on read {index}"
                    );
                    Ok::<_, ()>(if index == 0 {
                        Observation::Pending(Duration::from_millis(1))
                    } else {
                        Observation::Settled
                    })
                },
                Duration::from_secs(5),
                &|duration| pauses.borrow_mut().push(duration),
            )
            .unwrap();
            assert_eq!(outcome, AwaitOutcome::Settled);
            assert_eq!(reads.get(), 2);
            assert_eq!(pauses.borrow().as_slice(), &[Duration::from_millis(1)]);
        }
    }

    #[test]
    fn every_page_is_read_and_a_lost_one_is_refused() {
        let shared = Shared::new();
        let dir = state();
        let runs: Vec<_> = (0..PAGE + 1)
            .map(|index| run(&format!("job-{index}"), "completed", Some("success")))
            .collect();
        let mut first = page(runs[..PAGE].to_vec());
        first["total_count"] = json!(PAGE + 1);
        let mut second = page(runs[PAGE..].to_vec());
        second["total_count"] = json!(PAGE + 1);
        let mut world_pages = vec![first, second];
        struct Paged(Shared, Vec<Value>);
        impl Forge for Paged {
            fn request(
                &mut self,
                operation: provenant_config::ForgeOperation,
                _budget: Duration,
            ) -> Result<provenant_config::ForgeResponse, provenant_config::PortError> {
                let provenant_config::ForgeOperation::Api { path, etag, .. } = operation else {
                    return Err(provenant_config::PortError::Response(
                        "expected API request",
                    ));
                };
                let path = path.as_str();
                let etag = etag.as_deref();
                let mut respond = || -> io::Result<Response> {
                    let index: usize = path
                        .rsplit("&page=")
                        .next()
                        .and_then(|n| n.parse().ok())
                        .unwrap_or(1);
                    if path.contains("/check-runs?") {
                        let body = self
                            .1
                            .get(index - 1)
                            .cloned()
                            .unwrap_or_else(|| json!({"check_runs":[]}));
                        self.0.with(|world| world.runs.insert(HEAD.into(), body));
                    }
                    port::get(&mut self.0, path, etag)
                };
                let response = respond().map_err(provenant_config::PortError::Executor)?;
                Ok(forge_response(response))
            }
        }
        let mut paged = Observer::with(
            dir.path(),
            "owner/repo",
            Paged(shared.clone(), world_pages.clone()),
            shared.clone(),
        )
        .expect("observer");
        let checks = paged
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("pages");
        assert_eq!(checks.len(), PAGE + 1);
        world_pages.truncate(1);
        let mut lost = Observer::with(
            dir.path(),
            "owner/repo",
            Paged(shared.clone(), world_pages),
            shared.clone(),
        )
        .expect("observer");
        let error = lost
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect_err("lost page");
        assert!(matches!(error, ObserveError::Unavailable(_)), "{error}");
    }

    #[test]
    fn evidence_for_another_head_or_an_unknown_state_is_refused() {
        let ok = parse_pages(
            &[page(vec![run("build", "in_progress", None)])],
            &[json!([])],
            HEAD,
        )
        .expect("valid");
        assert_eq!(ok[0].bucket, "pending");
        assert_eq!(ok[0].app, Some(15368));
        assert_eq!(ok[0].app_slug.as_deref(), Some("github-actions"));
        let mut moved = run("build", "completed", Some("success"));
        moved["head_sha"] = json!(OTHER);
        assert!(parse_pages(&[page(vec![moved])], &[json!([])], HEAD).is_none());
        assert!(
            parse_pages(
                &[page(vec![run("build", "completed", None)])],
                &[json!([])],
                HEAD
            )
            .is_none()
        );
        let mut bad_time = run("build", "completed", Some("success"));
        bad_time["started_at"] = json!("yesterday");
        assert!(parse_pages(&[page(vec![bad_time])], &[json!([])], HEAD).is_none());
    }

    #[test]
    fn only_the_latest_state_of_a_status_context_counts() {
        let statuses = json!([
            {"context":"legacy","state":"success","target_url":"https://ci.example/1"},
            {"context":"legacy","state":"failure","description":"obsolete failure"},
            {"context":"external","state":"error","description":"broken"},
        ]);
        let checks = parse_pages(&[page(Vec::new())], &[statuses], HEAD).expect("valid");
        let by_context: BTreeMap<_, _> = checks
            .iter()
            .map(|check| (check.name.as_str(), check))
            .collect();
        assert_eq!(checks.len(), by_context.len(), "each context appears once");
        assert_eq!(
            by_context.keys().copied().collect::<Vec<_>>(),
            ["external", "legacy"]
        );
        assert_eq!(by_context["legacy"].bucket, "pass");
        assert_eq!(
            by_context["legacy"].url.as_deref(),
            Some("https://ci.example/1")
        );
        assert_eq!(by_context["external"].bucket, "fail");
        assert_eq!(by_context["external"].excerpt, "broken");
    }

    #[test]
    fn a_failed_check_keeps_its_link_and_output_excerpt() {
        let mut failed = run("build", "completed", Some("failure"));
        failed["details_url"] = json!("https://github.com/owner/repo/actions/runs/1");
        failed["output"] = json!({"title":"Build failed","summary":"","text":"src/main.rs:12","annotations_count":3});
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(HEAD.into(), page(vec![failed]));
        });
        let dir = state();
        let checks = shared
            .observer(dir.path())
            .checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60)),
            )
            .expect("checks");
        assert_eq!(checks[0].excerpt, "Build failed\nsrc/main.rs:12");
        assert!(
            checks[0]
                .url
                .as_deref()
                .is_some_and(|url| url.ends_with("/runs/1"))
        );
    }

    #[test]
    fn pending_cache_cadence_speeds_up_near_the_recent_check_duration() {
        let cached = Cached {
            runs: vec![Page {
                etag: None,
                body: json!({"check_runs":[{
                    "name":"build", "status":"in_progress", "started_at":"1970-01-01T00:00:00Z"
                }]}),
                extra: Default::default(),
            }],
            ..Cached::default()
        };
        let history = [RunDuration {
            id: 1,
            name: "build".into(),
            duration_ms: 300_000,
            completed_at_ms: 300_000,
        }];
        let cadence = Cadence {
            fast: Duration::from_secs(5),
            slow: Duration::from_secs(15),
            near_end: Duration::from_secs(30),
            conditional_requests: true,
        };
        assert_eq!(
            adaptive_freshness(&cached, &history, Duration::from_secs(5), cadence, 100_000),
            Duration::from_secs(15)
        );
        assert_eq!(
            adaptive_freshness(&cached, &history, Duration::from_secs(5), cadence, 280_000),
            Duration::from_secs(5)
        );
        assert_eq!(
            adaptive_freshness(&cached, &[], Duration::from_secs(5), cadence, 280_000),
            Duration::from_secs(15)
        );
    }

    #[test]
    fn configured_fast_cadence_drives_cache_age_and_zero_forces_refresh() {
        let cadence = Cadence {
            fast: Duration::from_secs(2),
            slow: Duration::from_secs(15),
            near_end: Duration::from_secs(30),
            conditional_requests: true,
        };
        assert_eq!(
            adaptive_freshness(&Cached::default(), &[], Duration::from_secs(30), cadence, 0),
            Duration::from_secs(2),
        );
        assert_eq!(
            adaptive_freshness(&Cached::default(), &[], Duration::ZERO, cadence, 0),
            Duration::ZERO,
        );
        assert_eq!(
            adaptive_interval(&Cached::default(), &[], cadence, 0),
            Duration::from_secs(2),
            "forced reads still publish the configured next cadence",
        );
    }

    #[test]
    fn duration_history_deduplicates_run_identity_not_equal_measurements() {
        let sample = |id| RunDuration {
            id,
            name: "build".into(),
            duration_ms: 300_000,
            completed_at_ms: 600_000,
        };
        let history = unique_run_history([sample(1), sample(2), sample(1)]);
        assert_eq!(history.iter().filter(|run| run.id == 1).count(), 1);
        assert_eq!(history.iter().filter(|run| run.id == 2).count(), 1);
    }

    #[test]
    fn etag_requests_follow_the_sourced_conditional_setting() {
        let previous = Page {
            etag: Some("validator".into()),
            body: Value::Null,
            extra: Default::default(),
        };
        let mut cadence = Cadence {
            fast: Duration::from_secs(5),
            slow: Duration::from_secs(15),
            near_end: Duration::from_secs(30),
            conditional_requests: true,
        };
        assert_eq!(request_etag(cadence, Some(&previous)), Some("validator"));
        cadence.conditional_requests = false;
        assert_eq!(request_etag(cadence, Some(&previous)), None);
    }

    #[test]
    fn a_server_error_is_retried_and_a_refusal_is_not() {
        struct Failing(Shared, Vec<u16>);
        impl Forge for Failing {
            fn request(
                &mut self,
                operation: provenant_config::ForgeOperation,
                _budget: Duration,
            ) -> Result<provenant_config::ForgeResponse, provenant_config::PortError> {
                let provenant_config::ForgeOperation::Api { path, etag, .. } = operation else {
                    return Err(provenant_config::PortError::Response(
                        "expected API request",
                    ));
                };
                let path = path.as_str();
                let etag = etag.as_deref();
                let mut respond = || -> io::Result<Response> {
                    if let Some(status) = self.1.pop() {
                        self.0
                            .with(|world| world.requests.push((path.into(), None)));
                        return Ok(Response {
                            status,
                            headers: BTreeMap::new(),
                            body: b"{}".to_vec(),
                        });
                    }
                    port::get(&mut self.0, path, etag)
                };
                let response = respond().map_err(provenant_config::PortError::Executor)?;
                Ok(forge_response(response))
            }
        }
        let shared = Shared::new();
        shared.with(|world| {
            world.runs.insert(
                HEAD.into(),
                page(vec![run("build", "completed", Some("success"))]),
            );
        });
        let dir = state();
        let mut retried = Observer::with(
            dir.path(),
            "owner/repo",
            Failing(shared.clone(), vec![502]),
            shared.clone(),
        )
        .expect("observer");
        assert_eq!(
            retried
                .checks(
                    HEAD,
                    Duration::ZERO,
                    &Request::within(Duration::from_secs(60))
                )
                .expect("retried")
                .len(),
            1
        );
        let mut refused = Observer::with(
            dir.path(),
            "owner/repo",
            Failing(shared.clone(), vec![404]),
            shared.clone(),
        )
        .expect("observer");
        assert!(matches!(
            refused.checks(
                HEAD,
                Duration::ZERO,
                &Request::within(Duration::from_secs(60))
            ),
            Err(ObserveError::Unavailable(_))
        ));
    }

    /// Seed one pull, its branch protection and its exact-head check pages.
    fn seed_head(shared: &Shared, head_sha: &str, check: serde_json::Value) {
        shared.with(|world| {
            world.resources.insert(
                "repos/Owner/Repo/pulls/7".into(),
                json!({
                    "number": 7,
                    "head": {"sha": head_sha, "ref": "work"},
                    "base": {"ref": "main"},
                    "mergeable": "MERGEABLE",
                    "mergeStateStatus": "CLEAN"
                }),
            );
            world.resources.insert(
                "repos/Owner/Repo/branches/main/protection".into(),
                json!({"required_status_checks": {"contexts": ["build"]}}),
            );
            world.runs.insert(head_sha.into(), page(vec![check]));
        });
    }

    fn read_head(
        shared: &Shared,
        dir: &std::path::Path,
        fallback: &dyn Fn() -> Option<String>,
    ) -> super::HeadReading {
        let mut observer = shared.observer(dir);
        let request = Request::within(Duration::from_secs(60));
        observer.observe_head(
            7,
            GitOid::parse(HEAD).expect("expected head"),
            &request,
            Duration::ZERO,
            fallback,
        )
    }

    #[test]
    fn ci_verdict_binds_to_exact_head_complete_reading() {
        let shared = Shared::new();
        seed_head(&shared, HEAD, run("build", "completed", Some("success")));
        let dir = state();
        let reading = read_head(&shared, dir.path(), &|| None);
        assert!(reading.complete, "{:?}", reading.pending_reason);
        assert_eq!(reading.pr_head, Some(GitOid::parse(HEAD).expect("head")));
        assert_eq!(
            reading.branch_ref.as_deref(),
            Some("work"),
            "the reading names the branch it bound"
        );
        assert_eq!(reading.mergeable, MergeState::Mergeable);
        assert_eq!(reading.required.len(), 1);
        assert_eq!(reading.required[0].name, "build");
        assert!(reading.quota_error().is_none());
        assert!(reading.fresh_at(reading.observed_at, HEAD_MAX_AGE));
    }

    #[test]
    fn a_queued_required_check_keeps_the_head_pending() {
        let shared = Shared::new();
        seed_head(&shared, HEAD, run("build", "in_progress", None));
        let dir = state();
        let reading = read_head(&shared, dir.path(), &|| None);
        assert!(!reading.complete);
        assert_eq!(reading.required.len(), 1);
        assert_eq!(reading.required[0].bucket, "pending");
        assert!(
            reading
                .pending_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("queued or in progress"))
        );
    }

    #[test]
    fn a_head_that_moved_since_the_read_is_not_a_verdict() {
        let shared = Shared::new();
        seed_head(&shared, OTHER, run("build", "completed", Some("success")));
        let dir = state();
        let reading = read_head(&shared, dir.path(), &|| None);
        assert!(!reading.complete);
        assert_eq!(reading.pr_head, Some(GitOid::parse(OTHER).expect("head")));
        assert_eq!(reading.branch_ref.as_deref(), Some("work"));
        assert!(reading.required.is_empty());
        assert!(
            reading
                .pending_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("not the expected"))
        );
    }

    #[test]
    fn an_empty_required_set_cannot_complete_a_reading() {
        let shared = Shared::new();
        seed_head(&shared, HEAD, run("build", "completed", Some("success")));
        shared.with(|world| {
            world
                .resources
                .remove("repos/Owner/Repo/branches/main/protection");
        });
        let dir = state();
        let reading = read_head(&shared, dir.path(), &|| None);
        assert!(!reading.complete);
        assert!(reading.required.is_empty());
        assert!(
            reading
                .pending_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("no required checks"))
        );
    }

    /// Spec: docs/specs/agent-native.md#33-error-and-warning-contract-3777
    #[test]
    fn forge_rate_limit_is_transient_with_reset() {
        let shared = Shared::new();
        let reset = START / 1000 + 60;
        shared.with(|world| world.limited_until = Some(reset));
        let dir = state();
        let reading = read_head(&shared, dir.path(), &|| None);
        assert!(!reading.complete);
        assert_eq!(reading.retry_at, Some(timestamp(reset)));
        let error = reading.quota_error().expect("quota error");
        assert_eq!(error.code, "FORGE_QUOTA_EXHAUSTED");
        assert_eq!(
            error.class,
            ErrorClass::Transient {
                retry_at: reading.retry_at
            }
        );
        assert_eq!(
            error.subject.as_ref().and_then(|subject| subject.head),
            Some(GitOid::parse(HEAD).expect("head"))
        );
        assert_eq!(error.next.len(), 1, "the catalogue requires a next step");
        assert!(
            error.next[0].display_text().contains(&reset.to_string()),
            "the next step names the reset"
        );
    }

    /// Spec: docs/specs/agent-native.md#39-delivery-queue-absorbs-the-fleet-3972-residue-3510
    #[test]
    fn graphql_rest_head_mismatch_is_pending() {
        let shared = Shared::new();
        shared.with(|world| world.limited_until = Some(START / 1000 + 60));
        let dir = state();
        let reading = read_head(&shared, dir.path(), &|| Some(OTHER.to_owned()));
        assert!(!reading.complete);
        assert_eq!(reading.retry_at, None, "the fallback answered, so no hold");
        assert!(
            reading
                .pending_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("mismatch"))
        );
    }

    #[test]
    fn a_stale_reading_cannot_settle_a_verdict() {
        let shared = Shared::new();
        seed_head(&shared, HEAD, run("build", "completed", Some("success")));
        let dir = state();
        let mut reading = read_head(&shared, dir.path(), &|| None);
        reading.observed_at = timestamp(START / 1000 - 120);
        assert!(!reading.fresh_at(timestamp(START / 1000), HEAD_MAX_AGE));
        assert!(
            reading.fresh_at(timestamp(START / 1000), Duration::from_secs(300)),
            "a wider bound admits the same sample"
        );
    }
}
