use super::*;

#[test]
fn remote_only_writer_branch_is_fetched_even_outside_fetch_refspec() -> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    source.run(&["branch", "remote-only"])?;
    let mut fixture = Fixture::new()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        source.root.to_str().ok_or("source")?,
    ])?;
    fixture.run(&[
        "config",
        "remote.origin.fetch",
        "+refs/heads/main:refs/remotes/origin/main",
    ])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let tip = fetch_remote_branch_tip(&context, "remote-only", &cancel())?
        .ok_or("remote branch tip was not fetched")?;
    assert_eq!(tip, source.head()?);
    let tracking = fixture.run(&[
        "for-each-ref",
        "--format=%(refname)",
        "refs/remotes/origin/remote-only",
    ])?;
    assert!(tracking.stdout.bytes.is_empty());
    let created = create(
        &context,
        WorktreeCreate {
            branch: "remote-only".into(),
            base: Some(tip.to_string()),
        },
        &cancel(),
    )?;
    assert_eq!(created.head, tip);
    source.run(&["commit", "--allow-empty", "-m", "move remote"])?;
    source.run(&["branch", "-f", "remote-only", "HEAD"])?;
    assert_ne!(source.head()?, tip);
    assert_eq!(
        fetch_remote_branch_tip(&context, "remote-only", &cancel())?,
        None
    );
    let local_tip = fixture.run(&["rev-parse", "refs/heads/remote-only"])?;
    assert_eq!(
        GitOid::parse(std::str::from_utf8(&local_tip.stdout.bytes)?.trim())?,
        tip
    );
    Ok(())
}

#[test]
fn create_defaults_to_fetched_remote_head_and_preserves_existing_and_explicit_bases()
-> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    source.run(&["branch", "-M", "trunk"])?;
    let mut fixture = Fixture::new()?;
    let clone = fixture._temp.path().join("clone-default");
    fixture.run(&[
        "clone",
        source.root.to_str().ok_or("source path")?,
        clone.to_str().ok_or("clone path")?,
    ])?;
    fixture.root = clone;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let old = fixture.head()?;
    fixture.run(&["branch", "existing"])?;
    source.run(&["commit", "--allow-empty", "-m", "new remote tip"])?;
    let latest = source.head()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let new = create(
        &context,
        WorktreeCreate {
            branch: "new-default".into(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(new.head, latest);
    assert!(new.warnings.is_empty());
    let existing = create(
        &context,
        WorktreeCreate {
            branch: "existing".into(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(existing.head, old);
    assert!(existing.warnings.is_empty());
    let explicit = create(
        &context,
        WorktreeCreate {
            branch: "explicit".into(),
            base: Some("HEAD".into()),
        },
        &cancel(),
    )?;
    assert_eq!(explicit.head, old);
    assert!(explicit.warnings.is_empty());
    Ok(())
}

#[test]
fn create_defaults_to_local_head_when_remote_unavailable_or_head_missing()
-> Result<(), Box<dyn Error>> {
    let source = Fixture::new()?;
    let mut fixture = Fixture::new()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        source.root.to_str().ok_or("source path")?,
    ])?;
    source.run(&["symbolic-ref", "HEAD", "refs/heads/missing"])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let head = fixture.head()?;
    let missing = create(
        &context,
        WorktreeCreate {
            branch: "missing-default".into(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(missing.head, head);
    assert_eq!(missing.warnings.len(), 1);
    assert!(missing.warnings[0].contains(&head.to_string()));
    let mut offline_fixture = Fixture::new()?;
    offline_fixture.run(&["remote", "add", "origin", "/nonexistent/provenant-2998"])?;
    offline_fixture.repo = offline_fixture.runtime.open_repo(
        &offline_fixture.executor,
        &offline_fixture.root,
        offline_fixture.budget()?,
        &cancel(),
    )?;
    let offline_head = offline_fixture.head()?;
    let test = TestContext::new(&offline_fixture)?;
    let context = test.context();
    let offline = create(
        &context,
        WorktreeCreate {
            branch: "offline-default".into(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(offline.head, offline_head);
    assert_eq!(offline.warnings.len(), 1);
    let receipt = serde_json::to_value(&offline)?;
    assert_eq!(receipt["warnings"][0], offline.warnings[0]);
    let mut warning = Vec::new();
    lifecycle::write_default_base_warning(&mut warning, head)?;
    let warning = String::from_utf8(warning)?;
    assert!(warning.contains("local HEAD"));
    assert!(warning.contains(&head.to_string()));
    Ok(())
}

#[test]
fn default_base_fetch_is_outside_registry_lock_and_failed_fetch_ignores_cached_tip()
-> Result<(), Box<dyn Error>> {
    struct FailedFetch<'a> {
        inner: &'a dyn Executor,
        project_state: &'a Directory,
        calls: std::sync::atomic::AtomicUsize,
        mutation_timeout: std::time::Duration,
    }
    impl Executor for FailedFetch<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options
                .argv
                .iter()
                .any(|word| word == "fetch" || word == "ls-remote")
            {
                let guard = self
                    .project_state
                    .try_lock_existing(OsStr::new(REPOSITORY_LOCK), LockMode::Exclusive)
                    .expect("remote discovery must not own the registry lock");
                drop(guard);
                assert_eq!(options.timeout, self.mutation_timeout);
            }
            if options.argv.iter().any(|word| word == "fetch") {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                return Ok(ExecOutcome {
                    termination: Termination::Exit(1),
                    stdout: Default::default(),
                    stderr: Default::default(),
                    timed_out: false,
                    drain_truncated: false,
                    overflow_stopped: false,
                    cancelled: false,
                    elapsed_micros: 0,
                });
            }
            self.inner.execute(options)
        }
    }
    let source = Fixture::new()?;
    let mut fixture = Fixture::new()?;
    fixture.run(&[
        "remote",
        "add",
        "origin",
        source.root.to_str().ok_or("source")?,
    ])?;
    fixture.run(&["fetch", "origin"])?;
    let source_branch = source.run(&["symbolic-ref", "--short", "HEAD"])?;
    let source_branch = std::str::from_utf8(&source_branch.stdout.bytes)?.trim();
    fixture.run(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        &format!("refs/remotes/origin/{source_branch}"),
    ])?;
    fixture.run(&["commit", "--allow-empty", "-m", "local-only work"])?;
    fixture.repo = fixture.runtime.open_repo(
        &fixture.executor,
        &fixture.root,
        fixture.budget()?,
        &cancel(),
    )?;
    let local = fixture.head()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let mutation_timeout = std::time::Duration::from_secs(120);
    context.mutation_budget = UtilityBudget::bounded(mutation_timeout)?;
    let failed = FailedFetch {
        inner: context.executor,
        project_state: context.project_state,
        calls: Default::default(),
        mutation_timeout,
    };
    context.executor = &failed;
    let created = create(
        &context,
        WorktreeCreate {
            branch: "offline-with-cache".into(),
            base: None,
        },
        &cancel(),
    )?;
    assert_eq!(
        created.head, local,
        "a stale cached remote tip cannot replace local HEAD after fetch failure"
    );
    assert_eq!(failed.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(created.warnings.len(), 1);
    assert!(created.warnings[0].contains(&local.to_string()));
    Ok(())
}

#[test]
fn gc_legacy_observer_gate_preview_is_read_only_and_apply_initialises_it()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let lane = gc_merged_worktree(&fixture, &context, "gc-legacy-gate")?;
    let slot = writer_directory(context.project_state, &lane.path)?;
    drop(slot.try_lock(OsStr::new(WRITER_LOCK), LockMode::Exclusive)?);
    let gate = slot.current_path()?.join("writer-probe.lock");
    fs::remove_file(&gate)?;
    let policy = WorktreeGcPolicy {
        upstream: Some(fixture.head()?),
        terminal: &|_| false,
        no_live_lanes: &|_| true,
        regenerable: &[],
        keep: &[],
        written_before_ns: i128::MAX,
        dry_run: true,
        foreign_merged_clean: true,
    };
    let preview = collect(&context, std::slice::from_ref(&lane), &policy, &cancel())?;
    assert!(preview.worktrees.removed.is_empty());
    assert_eq!(
        preview.worktrees.retained,
        vec![RetainedBranch {
            branch: "gc-legacy-gate".into(),
            reason: RetainReason::UnknownWriter,
            detail: None,
        }]
    );
    assert!(
        matches!(
            cache_writer_probe(context.project_state, &lane.path),
            Err(Occupancy::Unknown { scope, evidence, source: OccupancyUnknownSource::Custody })
                if scope == lane.path && evidence.contains("writer-probe.lock")
        ),
        "the missing observer gate must be the specific custody uncertainty"
    );
    assert!(!gate.exists(), "preview cannot upgrade custody state");
    assert!(lane.path.is_dir());
    let applied = collect(
        &context,
        std::slice::from_ref(&lane),
        &WorktreeGcPolicy {
            dry_run: false,
            ..policy
        },
        &cancel(),
    )?;
    assert_eq!(applied.worktrees.removed.len(), 1, "{applied:?}");
    assert!(gate.is_file(), "mutation upgrades legacy observation gates");
    assert!(!lane.path.exists());
    Ok(())
}

#[test]
fn streaming_inventory_reports_the_total_before_checked_rows() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let linked = create(
        &context,
        WorktreeCreate {
            branch: "streamed".into(),
            base: None,
        },
        &cancel(),
    )?;
    let mut events = Vec::new();
    let inventory = list_streaming(&context, &cancel(), |total, row| {
        events.push((total, row.map(|row| row.path.clone())));
    })?;
    assert_eq!(events.first(), Some(&(2, None)));
    assert_eq!(events.len(), 3);
    assert!(events.iter().skip(1).all(|(total, _)| *total == 2));
    assert!(
        events
            .iter()
            .any(|(_, path)| path.as_ref() == Some(&linked.path))
    );
    assert_eq!(inventory.len(), 2);
    Ok(())
}

#[test]
#[allow(clippy::disallowed_methods, clippy::print_stderr)] // Timing fixture around the real GC preview.
fn gc_preview_reuses_one_census_for_many_worktrees() -> Result<(), Box<dyn Error>> {
    use provenant_supervisor::census::{Census, CensusError, ProcessIdentity};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting {
        calls: AtomicUsize,
        current: ProcessFact,
    }
    impl ProcessObserver for Counting {
        fn boot_identity(&self) -> Result<String, CensusError> {
            Ok("fixture-boot".into())
        }
        fn observe(&self, pid: i64) -> ProcessObservation {
            if pid == self.current.identity.pid {
                ProcessObservation::Present(self.current.clone())
            } else {
                ProcessObservation::Absent
            }
        }
        fn census(&self, _: usize) -> Result<Census, CensusError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Census::from_observations([self.current.clone()], [], [], true, false, 32)
        }
    }
    impl CwdObserver for Counting {
        fn occupancy(&self, _: i64) -> provenant_os::pid::OccupancyObservation {
            provenant_os::pid::OccupancyObservation {
                paths: vec![],
                complete: true,
                gap: None,
            }
        }
        fn cwd(&self, _: i64) -> Result<CwdObservation, provenant_os::pid::PidError> {
            Ok(CwdObservation::Gone)
        }
    }
    impl WorkerObserver for Counting {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            super::census::observe_worker_roots(roots, self, self, &self.current, false).0
        }
    }
    let fixture = Fixture::new()?;
    fs::write(fixture.root.join(".git/info/exclude"), "target/\n")?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    for index in 0..40 {
        let path = fixture.root.join(format!("foreign-{index}"));
        fixture.run(&["worktree", "add", "--detach", path.to_str().ok_or("path")?])?;
        fs::create_dir(path.join("target"))?;
        fs::write(path.join("target/artifact"), "output")?;
    }
    let mut lanes = list(&context, &cancel())?;
    for lane in &mut lanes {
        lane.occupancy = Occupancy::custody_unknown(lane.path.clone(), "fixture unknown writer");
    }
    struct CountingGit<'a> {
        inner: &'a dyn Executor,
        listings: AtomicUsize,
    }
    impl Executor for CountingGit<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options
                .argv
                .windows(2)
                .any(|pair| pair[0] == "worktree" && pair[1] == "list")
            {
                self.listings.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let observer = Counting {
        calls: AtomicUsize::new(0),
        current: ProcessFact {
            identity: ProcessIdentity {
                boot: "fixture-boot".into(),
                pid: 100,
                native_start: 100,
                group_id: 100,
                session_id: 100,
            },
            parent_pid: 1,
            zombie: false,
            cpu_micros: None,
            command: None,
        },
    };
    let session = CensusSession::new(&observer, &test.clock);
    let git = CountingGit {
        inner: context.executor,
        listings: AtomicUsize::new(0),
    };
    context.workers = &session;
    context.executor = &git;
    let started = std::time::Instant::now();
    let report = collect(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &["target".into()],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    eprintln!(
        "GC preview collection for 40 worktrees: {:?}",
        started.elapsed()
    );
    assert_eq!(observer.calls.load(Ordering::SeqCst), 1);
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    // The shared 500 ms preview budget may expire on a busy CI host. Every
    // candidate must still receive a safe outcome, using the same census and
    // inventory even when the remaining work is deferred.
    let reported = report
        .idle_output
        .iter()
        .chain(&report.output)
        .map(|entry| {
            if entry.path.file_name() == Some(std::ffi::OsStr::new("target")) {
                entry.path.parent().unwrap_or(&entry.path).to_path_buf()
            } else {
                entry.path.clone()
            }
        })
        .collect::<std::collections::BTreeSet<_>>();
    let expected = (0..40)
        .map(|index| fixture.root.join(format!("foreign-{index}")))
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(reported, expected, "{report:?}");
    let entry_limit = provenant_os::fs::FsError::EntryLimit { limit: 20_000 }.to_string();
    assert!(
        report.idle_output.iter().all(|entry| {
            !entry.removed && (entry.reason == "would_remove" || entry.reason == entry_limit)
        }),
        "{report:?}"
    );
    assert!(
        report.output.iter().all(|entry| {
            !entry.removed && entry.reason == "preview traversal budget reached; retained"
        }),
        "{report:?}"
    );
    for index in 0..40 {
        assert_eq!(
            fs::read_to_string(
                fixture
                    .root
                    .join(format!("foreign-{index}/target/artifact"))
            )?,
            "output"
        );
    }
    Ok(())
}

#[test]
fn repo_facts_resolves_a_moved_ref_fresh() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let context = test.context();
    let facts = RepoFacts::default();
    fixture.run(&["branch", "facts-ref", "HEAD"])?;
    let name = GitRefName::branch("facts-ref")?;
    let initial = facts.resolve_commit(&context, name.clone(), &cancel())?;
    fixture.run(&["commit", "--allow-empty", "-m", "move facts ref"])?;
    fixture.run(&["branch", "-f", "facts-ref", "HEAD"])?;
    let moved = facts.resolve_commit(&context, name, &cancel())?;
    assert_ne!(moved, initial);
    assert_eq!(moved, fixture.head()?);
    assert_eq!(
        facts.resolve_revision(&context, GitRevision::new("facts-ref")?, &cancel())?,
        moved
    );
    assert_eq!(
        facts.resolve_revision(&context, GitRevision::new("facts-ref~1")?, &cancel())?,
        initial
    );
    assert_eq!(
        facts.symbolic_revision(&context, GitRevision::new("facts-ref")?, &cancel())?,
        Some("refs/heads/facts-ref".into())
    );
    context.facts.capture_inventory(&context, &cancel())?;
    context.facts.read_inventory(&context, &cancel())?;
    assert!(context.facts.take_inventory(&context).is_none());
    let inventory_limit = provenant_os::exec::DEFAULT_CAPTURE_LIMIT;
    assert!(RepoFacts::with_limits(inventory_limit, 0).is_err());
    assert!(RepoFacts::with_limits(0, provenant_os::git::REPO_FACT_SCALAR_LIMIT).is_err());
    let limited = RepoFacts::with_limits(inventory_limit, 1)?;
    assert!(matches!(
        limited.resolve_commit(&context, GitRefName::branch("facts-ref")?, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert_eq!(
        facts.bare_setting(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel(),
        )?,
        Some(false)
    );
    assert!(matches!(
        limited.bare_setting(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel(),
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    let read_only = context.utility.open_read_repo(
        context.executor,
        fixture.repo.root_path(),
        context.utility_budget,
        &cancel(),
    )?;
    let registrations = facts.read_only_inventory(
        &read_only,
        context.executor,
        context.utility,
        context.utility_budget,
        &cancel(),
    )?;
    assert!(registrations.starts_with(b"worktree "));
    assert!(registrations.ends_with(b"\0\0"));
    assert_eq!(
        facts.read_only_worktree_paths(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel(),
        )?,
        vec![fixture.repo.root_path().to_path_buf()]
    );
    let limited_inventory = RepoFacts::with_limits(1, provenant_os::git::REPO_FACT_SCALAR_LIMIT)?;
    assert!(matches!(
        limited_inventory.writer_inventory(
            context.repo,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel()
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    let git = provenant_os::git::git_executable().ok_or("Git executable unavailable")?;
    let environment = provenant_os::fixture::git_env();
    assert!(matches!(
        limited_inventory.discover_worktree_paths(
            fixture.repo.root_path(),
            &git,
            &environment,
            context.executor,
            std::time::Duration::from_secs(30),
            &cancel(),
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited_inventory.read_only_inventory(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            &cancel()
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    let tree = fixture.run(&["rev-parse", "facts-ref^{tree}"])?;
    let tree = GitOid::parse(std::str::from_utf8(&tree.stdout.bytes)?.trim())?;
    assert_eq!(
        facts.resolve_read_tree(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            moved,
            &cancel()
        )?,
        tree
    );
    assert!(matches!(
        limited.resolve_read_tree(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            moved,
            &cancel()
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    // An existing ref to a non-commit object remains distinct from a missing
    // ref, even when a caller separately asks for a peeled commit.
    fs::write(fixture.root.join("blob-fixture"), b"object")?;
    let blob = fixture.run(&["hash-object", "-w", "blob-fixture"])?;
    let blob = std::str::from_utf8(&blob.stdout.bytes)?.trim();
    fs::write(
        fixture.repo.common_git_dir().join("refs/heads/blob-ref"),
        format!("{blob}\n"),
    )?;
    assert_eq!(
        facts.resolve_read_ref(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            GitRefName::branch("blob-ref")?,
            &cancel()
        )?,
        GitOid::parse(blob)?
    );
    assert!(matches!(
        facts.resolve_read_commit(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            GitRefName::branch("blob-ref")?,
            &cancel()
        ),
        Err(WorktreeError::NotFound)
    ));
    assert_eq!(
        facts.resolve_read_ref(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            GitRefName::branch("facts-ref")?,
            &cancel()
        )?,
        moved
    );
    assert!(matches!(
        limited.resolve_read_ref(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            GitRefName::branch("facts-ref")?,
            &cancel()
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited.resolve_read_commit(
            &read_only,
            context.executor,
            context.utility,
            context.utility_budget,
            GitRefName::branch("facts-ref")?,
            &cancel(),
        ),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited.resolve_revision(&context, GitRevision::new("facts-ref")?, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited.symbolic_revision(&context, GitRevision::new("facts-ref")?, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited.checkout_head(&context, &fixture.repo, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    let limited_inventory = RepoFacts::with_limits(1, provenant_os::git::REPO_FACT_SCALAR_LIMIT)?;
    assert!(matches!(
        limited_inventory.read_inventory(&context, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert!(matches!(
        limited_inventory.capture_inventory(&context, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    Ok(())
}

#[test]
fn cached_inventory_is_reused_until_a_worktree_mutation() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountingGit<'a> {
        inner: &'a dyn Executor,
        listings: AtomicUsize,
    }
    impl Executor for CountingGit<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options
                .argv
                .windows(2)
                .any(|pair| pair[0] == "worktree" && pair[1] == "list")
            {
                self.listings.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let git = CountingGit {
        inner: context.executor,
        listings: AtomicUsize::new(0),
    };
    context.executor = &git;
    assert_eq!(list_cached(&context, &cancel())?.len(), 1);
    assert_eq!(list_cached(&context, &cancel())?.len(), 1);
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    let tight = WorktreeContext {
        utility_budget: context.utility_budget.with_stdout_cap(1)?,
        ..context
    };
    assert!(matches!(
        list_cached(&tight, &cancel()),
        Err(WorktreeError::Utility(UtilityError::Truncated {
            stream: "stdout"
        }))
    ));
    assert_eq!(git.listings.load(Ordering::SeqCst), 2);
    create(
        &context,
        WorktreeCreate {
            branch: "cache-mutation".into(),
            base: None,
        },
        &cancel(),
    )?;
    let after_mutation = git.listings.load(Ordering::SeqCst);
    assert_eq!(list_cached(&context, &cancel())?.len(), 2);
    assert_eq!(list_cached(&context, &cancel())?.len(), 2);
    assert_eq!(git.listings.load(Ordering::SeqCst), after_mutation + 1);
    Ok(())
}

#[test]
fn doctor_admission_and_list_share_one_inventory_with_100_worktrees() -> Result<(), Box<dyn Error>>
{
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountingGit<'a> {
        inner: &'a dyn Executor,
        listings: AtomicUsize,
    }
    impl Executor for CountingGit<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            if options
                .argv
                .windows(2)
                .any(|pair| pair[0] == "worktree" && pair[1] == "list")
            {
                self.listings.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    for index in 1..100 {
        let path = fixture._temp.path().join(format!("inventory-{index}"));
        fixture.run(&[
            "worktree",
            "add",
            "-b",
            &format!("inventory-{index}"),
            path.to_str().ok_or("path")?,
        ])?;
    }
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let git = CountingGit {
        inner: context.executor,
        listings: AtomicUsize::new(0),
    };
    context.executor = &git;
    let path = fixture._temp.path().join("inventory-1");
    assert_eq!(
        doctor(&context, &path, &cancel())?.state,
        WorktreeDiagnosisState::Clean
    );
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    assert_eq!(list(&context, &cancel())?.len(), 100);
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    let tree = verify_worktree(context.repo, &path)?;
    let branch = GitRefName::branch("inventory-1")?;
    let _custody = writer_custody(
        context.project_state,
        &tree,
        &branch,
        fixture.head()?,
        || {
            context.facts.writer_inventory(
                context.repo,
                context.executor,
                context.utility,
                context.utility_budget,
                &cancel(),
            )
        },
    )?;
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    Ok(())
}

#[test]
fn selected_inventory_observes_only_the_matching_checkout() -> Result<(), Box<dyn Error>> {
    use std::sync::Mutex;
    #[derive(Default)]
    struct Roots(Mutex<Vec<PathBuf>>);
    impl WorkerObserver for Roots {
        fn observe_roots(&self, roots: &[PathBuf]) -> BTreeMap<PathBuf, Occupancy> {
            *self.0.lock().unwrap() = roots.to_vec();
            roots
                .iter()
                .cloned()
                .map(|root| (root, Occupancy::Available))
                .collect()
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    let selected = create(
        &context,
        WorktreeCreate {
            branch: "selected".into(),
            base: None,
        },
        &cancel(),
    )?;
    create(
        &context,
        WorktreeCreate {
            branch: "sibling".into(),
            base: None,
        },
        &cancel(),
    )?;
    let roots = Roots::default();
    context.workers = &roots;
    let rows = list_for_gc_matching(&context, &cancel(), |path, _| path == selected.path)?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, selected.path);
    assert_eq!(*roots.0.lock().unwrap(), vec![selected.path]);
    Ok(())
}

#[test]
fn gc_does_not_inspect_status_of_out_of_scope_worktrees() -> Result<(), Box<dyn Error>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountingGit<'a> {
        inner: &'a dyn Executor,
        calls: AtomicUsize,
        listings: AtomicUsize,
    }
    impl Executor for CountingGit<'_> {
        fn execute(
            &self,
            options: provenant_os::exec::ExecOptions<'_>,
        ) -> Result<ExecOutcome, provenant_os::exec::ExecError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if options
                .argv
                .windows(2)
                .any(|pair| pair[0] == "worktree" && pair[1] == "list")
            {
                self.listings.fetch_add(1, Ordering::SeqCst);
            }
            self.inner.execute(options)
        }
    }
    let fixture = Fixture::new()?;
    let test = TestContext::new(&fixture)?;
    let mut context = test.context();
    // The primary plus 99 linked checkouts form a 100-worktree command.
    for index in 0..99 {
        let path = fixture.root.join(format!("foreign-{index}"));
        fixture.run(&["worktree", "add", "--detach", path.to_str().ok_or("path")?])?;
    }
    let git = CountingGit {
        inner: context.executor,
        calls: AtomicUsize::new(0),
        listings: AtomicUsize::new(0),
    };
    context.executor = &git;
    let lanes = list_for_gc(&context, &cancel())?;
    assert_eq!(lanes.len(), 100);
    let report = collect_lazy(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| true,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: true,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    assert!(report.worktrees.removed.is_empty());
    // A single command-scoped inventory serves both phases of a preview.
    assert_eq!(git.calls.load(Ordering::SeqCst), 1);
    assert_eq!(git.listings.load(Ordering::SeqCst), 1);
    let lanes = list_for_gc(&context, &cancel())?;
    collect_lazy(
        &context,
        &lanes,
        &WorktreeGcPolicy {
            upstream: None,
            terminal: &|_| false,
            no_live_lanes: &|_| false,
            regenerable: &[],
            keep: &[],
            written_before_ns: i128::MAX,
            dry_run: false,
            foreign_merged_clean: false,
        },
        &cancel(),
    )?;
    // Applying discards the cached snapshot and reads again under custody.
    assert_eq!(git.calls.load(Ordering::SeqCst), 3);
    Ok(())
}
