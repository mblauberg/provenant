//! Resolve CLI selections at the shared run owner boundary.
use super::{Edge, Fail, Session, Target, remote, run};

/// Resolution has already checked local and peer candidates for collisions.
pub(super) fn placement(
    edge: &Edge<'_>,
    target: &Target,
    allow_run: bool,
) -> Result<Option<remote::Placement>, Fail> {
    remote::find_target(edge.roots, target, allow_run)
}

pub(super) fn local(
    session: &Session,
    target: &Target,
    call: &run::Request<'_>,
) -> Result<run::LaneTarget, Fail> {
    let resolved;
    let target = if let Some(selection) = &target.selection {
        resolved = choose(
            selection,
            index(&session.context, selection, true)?
                .into_iter()
                .filter(|candidate| candidate.project == Some(session.context.project_id))
                .collect(),
            true,
        )?
        .target;
        &resolved
    } else {
        target
    };
    match target.run {
        Some(run) => {
            // `RUN/ATTEMPT`: the second part is an attempt of this run, not a task.
            if let (Some(task), None) = (target.task, target.attempt) {
                let candidate = provenant_core::AttemptId::from_bytes(*task.as_bytes());
                if session
                    .service()
                    .status(candidate, call)
                    .is_ok_and(|record| record.run_id == run)
                {
                    return Ok(run::LaneTarget {
                        run,
                        task: None,
                        attempt: Some(candidate),
                    });
                }
            }
            Ok(run::LaneTarget {
                run,
                task: target.task,
                attempt: target.attempt,
            })
        }
        None => Ok(session.service().resolve_target(
            target.attempt.ok_or(run::RunError::NotFound)?,
            true,
            call,
        )?),
    }
}

/// Index identities are hints only. The selected record is validated again by
/// RunStore after reopening in its recorded project, with the caller's actor.
#[derive(Clone, Debug)]
pub(super) struct Candidate {
    pub target: Target,
    pub project: Option<provenant_core::ProjectId>,
    pub host: Option<provenant_core::HostId>,
}
impl Candidate {
    fn name(&self) -> String {
        match (self.target.run, self.target.task, self.target.attempt) {
            (Some(run), Some(task), Some(attempt)) => format!("{run}/{task}/{attempt}"),
            (Some(run), Some(task), None) => format!("{run}/{task}"),
            (Some(run), None, Some(attempt)) => format!("{run}/{attempt}"),
            (Some(run), None, None) => run.to_string(),
            (_, _, Some(attempt)) => attempt.to_string(),
            _ => String::new(),
        }
    }
}
/// Callers only need to know whether a selection is unique, ambiguous or
/// missing, so the lane history is read no further once more than one
/// distinct target of the requested kind is present: an ambiguity keeps
/// holding over later pages, and the ambiguity report names a bounded prefix
/// of the matches instead of every prefix collision.
fn index(
    context: &run::ExecutionContext,
    selection: &str,
    allow_run: bool,
) -> Result<Vec<Candidate>, Fail> {
    let clock = super::SystemClock::new();
    let cancel = super::CancelToken::new();
    let call = super::request(&clock, &cancel)?;
    let Some(store) = run::RunStore::open_read_only(context, &call)? else {
        return Ok(Vec::new());
    };
    // Full bare IDs have direct indexed reads. Never page project history
    // just to locate one attempt or run.
    if selection.len() == 32 && !selection.contains('/') {
        let id: provenant_core::AttemptId =
            selection.parse().map_err(|_| run::RunError::NotFound)?;
        let mut candidates = Vec::new();
        match store.record(id, &call) {
            Ok(record) => candidates.push(Candidate {
                target: Target {
                    selection: None,
                    run: Some(record.run_id),
                    task: Some(record.task_id),
                    attempt: Some(id),
                },
                project: Some(context.project_id),
                host: None,
            }),
            Err(run::RunError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
        let run_id = provenant_core::RunId::from_bytes(*id.as_bytes());
        match store.run(run_id, &call) {
            Ok(_) => candidates.push(Candidate {
                target: Target {
                    selection: None,
                    run: Some(run_id),
                    task: None,
                    attempt: None,
                },
                project: Some(context.project_id),
                host: None,
            }),
            Err(run::RunError::NotFound) => {}
            Err(error) => return Err(error.into()),
        }
        return Ok(candidates);
    }
    let parts: Vec<_> = selection.split('/').collect();
    let mut query = run::LaneQuery {
        targets: vec![],
        all: true,
        limit: 100,
        after: 0,
        newest_first: false,
        owner: None,
        unseen_after: None,
    };
    let mut candidates = Vec::new();
    // Judge ambiguity through `choose` itself so the stop rule cannot drift
    // from the selection semantics it reports. The report renders five
    // matches and a remainder, so stop the scan once that much is already
    // on hand and `choose` proves the choice is ambiguous: no later page
    // can turn it back into a unique match or a miss.
    let ambiguous = |candidates: &[Candidate]| {
        matches!(
            choose(selection, candidates.to_owned(), allow_run),
            Err(fail) if fail.kind == super::ErrorKind::Conflict
        )
    };
    loop {
        let page = store.lanes(&query, &call)?;
        for lane in page.lanes {
            let run_matches = lane.run_id.to_string().starts_with(parts[0]);
            if parts.len() == 1 && run_matches {
                candidates.push(Candidate {
                    target: Target {
                        selection: None,
                        run: Some(lane.run_id),
                        task: None,
                        attempt: None,
                    },
                    project: Some(context.project_id),
                    host: None,
                });
            }
            if (parts.len() == 1 && lane.attempt_id.to_string().starts_with(parts[0]))
                || (parts.len() > 1 && run_matches)
            {
                candidates.push(Candidate {
                    target: Target {
                        selection: None,
                        run: Some(lane.run_id),
                        task: Some(lane.task_id),
                        attempt: Some(lane.attempt_id),
                    },
                    project: Some(context.project_id),
                    host: None,
                });
            }
            if candidates.len() >= 6 && ambiguous(&candidates) {
                return Ok(candidates);
            }
        }
        if ambiguous(&candidates) {
            break;
        }
        match page.next {
            Some(after) => query.after = after,
            None => break,
        }
    }
    Ok(candidates)
}
pub(super) fn choose(
    selection: &str,
    candidates: Vec<Candidate>,
    allow_run: bool,
) -> Result<Candidate, Fail> {
    let parts: Vec<_> = selection.split('/').collect();
    let prefix = |id: Option<String>, part: &str| id.is_some_and(|id| id.starts_with(part));
    let mut selected = Vec::new();
    for mut candidate in candidates {
        let target = &candidate.target;
        let run_matches = prefix(target.run.map(|id| id.to_string()), parts[0])
            || prefix(candidate.host.map(|id| id.to_string()), parts[0]);
        let matched = match parts.as_slice() {
            [id] => {
                if target.attempt.is_some() {
                    prefix(target.attempt.map(|id| id.to_string()), id)
                } else {
                    allow_run && prefix(target.run.map(|id| id.to_string()), id)
                }
            }
            [_, id] if run_matches => {
                if prefix(target.attempt.map(|id| id.to_string()), id) {
                    true
                } else if allow_run && prefix(target.task.map(|id| id.to_string()), id) {
                    candidate.target.attempt = None;
                    true
                } else {
                    false
                }
            }
            [_, task, attempt] if run_matches => {
                prefix(target.task.map(|id| id.to_string()), task)
                    && prefix(target.attempt.map(|id| id.to_string()), attempt)
            }
            _ => false,
        };
        if matched {
            selected.push(candidate);
        }
    }
    selected.sort_by_key(Candidate::name);
    selected.dedup_by(|left, right| {
        left.name() == right.name() && left.project == right.project && left.host == right.host
    });
    match selected.len() {
        0 => Err(Fail::new(
            super::Status::Invalid,
            super::ErrorKind::NotFound,
            format!(
                "no lane matches {selection}; use a unique prefix of 8 or more characters or the full ID, as `provenant lanes --all` lists them"
            ),
        )),
        1 => Ok(selected.remove(0)),
        _ => Err(Fail::new(
            super::Status::Invalid,
            super::ErrorKind::Conflict,
            format!(
                "ambiguous ID {selection}: {}{}; run `provenant help output` and retry with one full RUN/TASK/ATTEMPT ID",
                selected
                    .iter()
                    .take(5)
                    .map(Candidate::name)
                    .collect::<Vec<_>>()
                    .join(", "),
                if selected.len() > 5 {
                    format!(", and {} more", selected.len() - 5)
                } else {
                    String::new()
                }
            ),
        )),
    }
}
fn text(target: &Target) -> String {
    if let Some(selection) = &target.selection {
        return selection.clone();
    }
    Candidate {
        target: target.clone(),
        project: None,
        host: None,
    }
    .name()
}
/// Resolve across the local index and durable peer placements before choosing
/// either path, so a local/peer collision cannot select a different lane.
pub(super) fn resolve(
    edge: &Edge<'_>,
    target: &Target,
    allow_run: bool,
) -> Result<Candidate, Fail> {
    let mut selection = text(target);
    if let Some((qualifier, attempt)) = selection.split_once('/')
        && !attempt.contains('/')
        && let Ok(qualifier) = qualifier.parse()
        && remote::qualifier_site(edge, qualifier) == Some(remote::Site::Local)
    {
        selection = attempt.to_owned();
    }
    let context = super::execution_context(edge)?;
    let exact = selection.len() == 32 && !selection.contains('/');
    let peers = remote::target_candidates(edge.roots)?;
    let mut candidates = index(&context, &selection, allow_run)?;
    if exact
        && candidates
            .iter()
            .any(|candidate| allow_run || candidate.target.attempt.is_some())
    {
        candidates.extend(peers);
        return choose_for(edge, &selection, candidates, allow_run);
    }
    let mut projects: Vec<_> = provenant_config::registered_projects(edge.roots)
        .map_err(|error| {
            Fail::new(
                super::Status::Error,
                super::ErrorKind::Identity,
                error.to_string(),
            )
        })?
        .into_iter()
        .map(|binding| binding.project_id())
        .filter(|project| *project != context.project_id)
        .collect();
    projects.sort();
    projects.dedup();
    for project_id in projects {
        let scoped = run::ExecutionContext {
            project_id,
            ..context.clone()
        };
        candidates.extend(index(&scoped, &selection, allow_run)?);
        if exact
            && candidates
                .iter()
                .any(|candidate| allow_run || candidate.target.attempt.is_some())
        {
            break;
        }
    }
    candidates.extend(peers);
    choose_for(edge, &selection, candidates, allow_run)
}
/// `choose`, naming `--host` on a miss when a peer could hold the lane.
fn choose_for(
    edge: &Edge<'_>,
    selection: &str,
    candidates: Vec<Candidate>,
    allow_run: bool,
) -> Result<Candidate, Fail> {
    choose(selection, candidates, allow_run).map_err(|mut fail| {
        let peers = crate::fabric::Context::open_read_only(edge.roots.clone(), edge.home, edge.cwd)
            .is_ok_and(|context| !context.config.peers.is_empty());
        if fail.kind == super::ErrorKind::NotFound && peers {
            fail.message
                .push_str("; a configured peer's lane needs `--host HOST`");
        }
        fail
    })
}
/// Reopen the selected run's stored project without changing actor identity.
/// RunStore still checks custody and record identity on every operation.
pub(super) fn open(
    edge: &Edge<'_>,
    target: &Target,
    allow_run: bool,
    call: &run::Request<'_>,
) -> Result<(Session, Target), Fail> {
    open_resolved(edge, resolve(edge, target, allow_run)?, call)
}
pub(super) fn open_resolved(
    edge: &Edge<'_>,
    candidate: Candidate,
    call: &run::Request<'_>,
) -> Result<(Session, Target), Fail> {
    let mut context = super::execution_context(edge)?;
    context.project_id = candidate.project.ok_or_else(|| {
        Fail::new(
            super::Status::Refused,
            super::ErrorKind::Unsupported,
            "that lane ran on a peer; run `provenant status --all` on that host",
        )
    })?;
    let store =
        run::RunStore::open(&context, call)?.with_writer_registry(edge.roots.state.as_path());
    Ok((Session { store, context }, candidate.target))
}
/// A run selection also accepts an attempt and uses its owning run.
pub(super) fn run_id(edge: &Edge<'_>, target: &Target) -> Result<provenant_core::RunId, Fail> {
    resolve(edge, target, true)?
        .target
        .run
        .ok_or_else(|| run::RunError::NotFound.into())
}

pub(super) fn read_resolved(
    edge: &Edge<'_>,
    candidate: Candidate,
    call: &run::Request<'_>,
) -> Result<(Session, Target), Fail> {
    let mut context = super::execution_context(edge)?;
    context.project_id = candidate.project.ok_or_else(|| {
        Fail::new(
            super::Status::Refused,
            super::ErrorKind::Unsupported,
            "that lane ran on a peer; run `provenant status --all` on that host",
        )
    })?;
    let store = run::RunStore::open_read_only(&context, call)?
        .ok_or(run::RunError::NotFound)?
        .with_writer_registry(edge.roots.state.as_path());
    Ok((Session { store, context }, candidate.target))
}

/// A recovered queued lane needs its original project's location as well as
/// its ID. Existing writer requests retain their more specific worktree.
pub(super) fn cwd(
    edge: &Edge<'_>,
    project: provenant_core::ProjectId,
) -> Result<std::path::PathBuf, Fail> {
    if super::execution_context(edge)?.project_id == project {
        return Ok(edge.cwd.to_owned());
    }
    let binding = provenant_config::registered_projects(edge.roots)
        .map_err(|error| Fail::new(super::Status::Error, super::ErrorKind::Identity, format!("{error}; run `provenant status --all` from the lane's project")))?
        .into_iter().find(|binding| binding.project_id() == project)
        .ok_or_else(|| Fail::new(super::Status::Invalid, super::ErrorKind::NotFound, "project location is unavailable; run `provenant status --all` from the lane's project"))?;
    let location = binding.common_dir();
    Ok(if location.file_name().is_some_and(|name| name == ".git") {
        location.parent().unwrap_or(location).to_owned()
    } else {
        location.to_owned()
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use provenant_core::{AttemptId, Clock, ProjectId, RunId, TaskId};
    fn candidate(run: u8, attempt: u8, project: u8) -> Candidate {
        Candidate {
            target: Target {
                selection: None,
                run: Some(RunId::from_bytes([run; 16])),
                task: Some(TaskId::from_bytes([4; 16])),
                attempt: Some(AttemptId::from_bytes([attempt; 16])),
            },
            project: Some(ProjectId::from_bytes([project; 16])),
            host: None,
        }
    }
    #[test]
    fn prefixes_require_eight_hex_characters_and_keep_qualified_forms() {
        for selection in [
            "01010101",
            "01010101/02020202",
            "01010101/03030303/02020202",
        ] {
            let target: Target = selection.parse().expect("valid prefix");
            assert_eq!(target.selection.as_deref(), Some(selection));
            assert!(selection.parse::<crate::Attempt>().is_ok());
        }
        for selection in [
            "0101010",
            "0101010z",
            "01010101/",
            "/01010101",
            "01010101/02020202/03030303/04040404",
        ] {
            assert!(selection.parse::<Target>().is_err(), "{selection}");
        }
    }
    #[test]
    fn a_missed_lane_names_the_prefix_rule() {
        let error = choose("02020202", Vec::new(), false).expect_err("no lane");
        assert_eq!(error.kind, super::super::ErrorKind::NotFound);
        assert!(
            error.message.contains("unique prefix of 8 or more"),
            "{}",
            error.message
        );
    }
    #[test]
    fn unique_prefix_preserves_the_stored_project_and_full_identity() {
        let found = choose(
            "02020202",
            vec![candidate(1, 2, 8), candidate(3, 5, 9)],
            false,
        )
        .expect("unique attempt");
        assert_eq!(found.target.attempt, Some(AttemptId::from_bytes([2; 16])));
        assert_eq!(found.project, Some(ProjectId::from_bytes([8; 16])));
    }
    #[test]
    fn qualified_prefix_disambiguates_and_ambiguity_names_all_candidates() {
        let rows = vec![candidate(1, 2, 8), candidate(3, 2, 9)];
        let error = choose("02020202", rows.clone(), false).expect_err("ambiguous attempt");
        assert!(
            error
                .message
                .contains(&RunId::from_bytes([1; 16]).to_string())
        );
        assert!(
            error
                .message
                .contains(&RunId::from_bytes([3; 16]).to_string())
        );
        assert!(error.message.contains("provenant help output"));
        let selected = choose("03030303/02020202", rows, false).expect("qualified unique attempt");
        assert_eq!(selected.target.run, Some(RunId::from_bytes([3; 16])));
    }
    #[test]
    fn ambiguity_lists_at_most_five_candidates() {
        let error = choose(
            "02020202",
            (1..=8).map(|n| candidate(n, 2, n)).collect(),
            false,
        )
        .expect_err("ambiguous attempt");
        assert!(error.message.contains("and 3 more"), "{}", error.message);
        assert!(
            error
                .message
                .contains(&RunId::from_bytes([5; 16]).to_string())
        );
        assert!(
            !error
                .message
                .contains(&RunId::from_bytes([6; 16]).to_string())
        );
    }
    #[test]
    fn run_attempt_collision_is_ambiguous_only_when_runs_are_allowed() {
        let attempt = candidate(1, 2, 8);
        let run = Candidate {
            target: Target {
                selection: None,
                run: Some(RunId::from_bytes([2; 16])),
                task: None,
                attempt: None,
            },
            project: attempt.project,
            host: None,
        };
        assert!(choose("02020202", vec![attempt.clone(), run.clone()], true).is_err());
        assert!(choose("02020202", vec![attempt, run], false).is_ok());
    }
    #[test]
    fn multiple_attempts_of_one_task_resolve_to_one_task_selection() {
        let found = choose(
            "01010101/04040404",
            vec![candidate(1, 2, 8), candidate(1, 3, 8)],
            true,
        )
        .expect("one task");
        assert!(found.target.attempt.is_none());
        assert_eq!(found.target.task, Some(TaskId::from_bytes([4; 16])));
    }
    #[test]
    fn index_lookup_reads_prefixes_without_needing_current_project()
    -> Result<(), Box<dyn std::error::Error>> {
        use provenant_core::{ActorId, Clock, RootInputs};
        let temp = tempfile::tempdir()?;
        let root = std::fs::canonicalize(temp.path())?;
        let home = root.join("home");
        let checkout = root.join("checkout");
        std::fs::create_dir(&home)?;
        std::fs::create_dir(&checkout)?;
        let mut inputs = RootInputs::new(home, checkout);
        inputs.state_dir = Some(root.join("state"));
        let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let context = run::ExecutionContext {
            state_root: roots.state,
            project_id: ProjectId::from_bytes([8; 16]),
            actor_id: ActorId::from_bytes([9; 16]),
            read_prompts: true,
        };
        let clock = super::super::SystemClock::new();
        let cancel = super::super::CancelToken::new();
        let call = super::super::request(&clock, &cancel).map_err(|fail| fail.message)?;
        let store = run::RunStore::open(&context, &call)?;
        let prepared =
            super::super::tests::trace_run(1, AttemptId::from_bytes([9; 16]), clock.wall_now()?)?;
        store.create(&prepared, &call)?;
        let run_id = prepared.run_id.to_string();
        let attempt_id = prepared.attempts[0].attempt_id.to_string();
        let selection = format!("{}/{}", &run_id[..8], &attempt_id[..8]);
        let selected = choose(
            &selection,
            index(&context, &selection, false).map_err(|fail| fail.message)?,
            false,
        )
        .map_err(|fail| fail.message)?;
        assert_eq!(selected.target.run, Some(prepared.run_id));
        assert_eq!(
            selected.target.attempt,
            Some(prepared.attempts[0].attempt_id)
        );
        assert_eq!(selected.project, Some(context.project_id));
        Ok(())
    }
    /// Fixture for index scans: an isolated writer store plus the context the
    /// scan runs under. No worker is spawned and no detached process launched.
    struct ScanFixture {
        _temp: tempfile::TempDir,
        clock: super::super::SystemClock,
        cancel: super::super::CancelToken,
        context: run::ExecutionContext,
        store: run::RunStore,
    }
    impl ScanFixture {
        fn open() -> Result<Self, Box<dyn std::error::Error>> {
            use provenant_core::{ActorId, RootInputs};
            let temp = tempfile::tempdir()?;
            let root = std::fs::canonicalize(temp.path())?;
            let home = root.join("home");
            let checkout = root.join("checkout");
            std::fs::create_dir(&home)?;
            std::fs::create_dir(&checkout)?;
            let mut inputs = RootInputs::new(home, checkout);
            inputs.state_dir = Some(root.join("state"));
            let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
            let context = run::ExecutionContext {
                state_root: roots.state,
                project_id: ProjectId::from_bytes([8; 16]),
                actor_id: ActorId::from_bytes([9; 16]),
                read_prompts: true,
            };
            let clock = super::super::SystemClock::new();
            let cancel = super::super::CancelToken::new();
            let store = run::RunStore::open(
                &context,
                &run::Request {
                    clock: &clock,
                    cancel: &cancel,
                },
            )?;
            Ok(Self {
                _temp: temp,
                clock,
                cancel,
                context,
                store,
            })
        }
        fn call(&self) -> run::Request<'_> {
            run::Request {
                clock: &self.clock,
                cancel: &self.cancel,
            }
        }
    }
    /// A prepared run under the shared run prefix `11111111`, its siblings
    /// differing from it beyond the tenth hex character, with a unique attempt
    /// ID per lane.
    fn shared_prefix_run(
        tail: u8,
        attempt: u8,
        clock: &super::super::SystemClock,
    ) -> Result<run::PreparedRun, Box<dyn std::error::Error>> {
        let (task_id, run_id) = (
            TaskId::from_bytes([tail; 16]),
            RunId::from_bytes({
                let mut bytes = [0x11_u8; 16];
                bytes[4] = tail;
                bytes
            }),
        );
        Ok(run::PreparedRun {
            operation_id: provenant_core::OperationId::parse(&format!("prefix-scan-{tail}"))?,
            run_id,
            created_at: clock.wall_now()?,
            request: run::CanonicalRequest {
                extra: Default::default(),
                child_attempt_id_version: None,
                chair: None,
                fresh_continuation: None,
                schema_version: 1,
                tasks: vec![run::TaskRequest {
                    context: None,
                    extra: Default::default(),
                    build_heavy: false,
                    title: None,
                    base: None,
                    head: None,
                    base_ref: None,
                    task_id,
                    prompt_digest: provenant_records::CanonicalV1::file_digest(b"p"),
                    route: run::RequestedRoute::Model {
                        extra: Default::default(),
                        provider: "fixture".into(),
                        model: "fixture/model".into(),
                        effort: None,
                    },
                    account: None,
                    fallback: vec![],
                    profile: Default::default(),
                    authority_digest: provenant_core::Digest::from_bytes([5; 32]),
                    provider_timeout_seconds: 3600,
                    writer: None,
                    delivery_id: None,
                    deliver: None,
                    access: Default::default(),
                }],
                concurrency: 1,
                parent: None,
                lane_depth: Some(1),
                parent_attempt: Some(AttemptId::from_bytes([9; 16])),
            },
            attempts: vec![run::NewAttempt {
                task_id,
                attempt_id: AttemptId::from_bytes([attempt; 16]),
                prompt: "p".into(),
            }],
        })
    }
    fn shared_prefix_run_id(tail: u8) -> RunId {
        RunId::from_bytes({
            let mut bytes = [0x11_u8; 16];
            bytes[4] = tail;
            bytes
        })
    }
    #[test]
    fn ambiguous_scan_stops_on_the_bounded_report() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = ScanFixture::open()?;
        for tail in 1..=7 {
            fixture.store.create(
                &shared_prefix_run(tail, tail + 0x20, &fixture.clock)?,
                &fixture.call(),
            )?;
        }
        let selection = "11111111";
        let scanned = index(&fixture.context, selection, true).map_err(|f| f.message)?;
        let error = choose(selection, scanned, true).expect_err("ambiguous prefix");
        assert_eq!(error.kind, super::super::ErrorKind::Conflict);
        assert!(error.message.contains("ambiguous ID"), "{}", error.message);
        // Six of the seven sibling lanes share the prefix and the store holds
        // them all, but the scan stopped once the report carried five matches
        // plus a proved remainder: runs six and seven never render.
        let lanes = fixture
            .store
            .lanes(
                &run::LaneQuery {
                    targets: vec![],
                    all: true,
                    limit: 100,
                    after: 0,
                    newest_first: false,
                    owner: None,
                    unseen_after: None,
                },
                &fixture.call(),
            )?
            .lanes;
        assert_eq!(lanes.len(), 7);
        for tail in 1..=5 {
            assert!(
                error
                    .message
                    .contains(&shared_prefix_run_id(tail).to_string()),
                "{}",
                error.message
            );
        }
        assert!(error.message.contains("and 1 more"), "{}", error.message);
        assert!(
            !error.message.contains(&shared_prefix_run_id(6).to_string()),
            "{}",
            error.message
        );
        assert!(
            !error.message.contains(&shared_prefix_run_id(7).to_string()),
            "{}",
            error.message
        );
        Ok(())
    }
    #[test]
    fn run_free_scan_still_reads_the_whole_history() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = ScanFixture::open()?;
        for tail in 1..=7 {
            fixture.store.create(
                &shared_prefix_run(tail, tail + 0x20, &fixture.clock)?,
                &fixture.call(),
            )?;
        }
        let selection = "11111111";
        // With runs refused as kinds, a run-only prefix needs the whole history
        // to prove no lane attempt matches instead: the scan does not stop.
        let scanned = index(&fixture.context, selection, false).map_err(|f| f.message)?;
        assert_eq!(scanned.len(), 7);
        let error = choose(selection, scanned, false).expect_err("no lane attempt");
        assert_eq!(error.kind, super::super::ErrorKind::NotFound);
        assert!(
            error.message.contains("no lane matches"),
            "{}",
            error.message
        );
        Ok(())
    }
    #[test]
    fn unique_prefix_still_resolves_one_target() -> Result<(), Box<dyn std::error::Error>> {
        let fixture = ScanFixture::open()?;
        for tail in [1u8, 9] {
            fixture.store.create(
                &shared_prefix_run(tail, tail + 0x20, &fixture.clock)?,
                &fixture.call(),
            )?;
        }
        // `11111111` alone is ambiguous; naming the sibling byte resolves.
        let error = choose(
            "11111111",
            index(&fixture.context, "11111111", true).map_err(|f| f.message)?,
            true,
        )
        .expect_err("ambiguous prefix");
        assert_eq!(error.kind, super::super::ErrorKind::Conflict);
        let selection = "1111111101";
        let selected = choose(
            selection,
            index(&fixture.context, selection, true).map_err(|f| f.message)?,
            true,
        )
        .map_err(|f| f.message)?;
        assert_eq!(selected.target.run, Some(shared_prefix_run_id(1)));
        assert_eq!(selected.project, Some(fixture.context.project_id));
        Ok(())
    }
}
