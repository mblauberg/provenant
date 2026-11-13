
#[test]
fn retained_state_copy_leaves_nothing_when_refused() -> Result<(), Box<dyn std::error::Error>> {
    const LIMIT: u64 = 1 << 20;
    let limits = StateLimits {
        bytes: LIMIT,
        ..RETAINED_STATE
    };
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(source.join("sessions.lock"))?;
    std::fs::write(source.join("sessions.lock/rollout.jsonl"), "turn")?;
    std::fs::write(source.join("keep"), "lane")?;
    // A lock-named partial, which ordinary removal refuses.
    std::fs::File::create(source.join("Cargo.lock"))?.set_len(64 * LIMIT)?;
    // Like a launch, the destination is an empty candidate directory.
    let state = base.join("provider-state");
    let copied = state.join("candidate-0");
    std::fs::create_dir_all(&copied)?;
    let error = copy_state_within(&source, &copied, limits, false).unwrap_err();
    assert!(error.contains("domains.run.continuation_copy_limit_bytes"));
    assert!(error.contains(&LIMIT.to_string()));
    let names = |path: &Path| -> std::io::Result<Vec<_>> {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.file_name()))
            .collect()
    };
    assert_eq!(names(&state)?, ["candidate-0"], "no staging survives");
    assert!(names(&copied)?.is_empty(), "no partial output survives");
    std::fs::remove_file(source.join("Cargo.lock"))?;
    copy_state_within(&source, &copied, limits, false)?;
    assert_eq!(names(&state)?, ["candidate-0"]);
    assert_eq!(
        std::fs::read_to_string(copied.join("sessions.lock/rollout.jsonl"))?,
        "turn"
    );
    Ok(())
}

#[test]
fn session_search_reports_its_spent_entry_budget() -> Result<(), Box<dyn std::error::Error>> {
    let limits = StateLimits {
        entries: 4,
        ..RETAINED_STATE
    };
    let temp = tempfile::tempdir()?;
    let state = std::fs::canonicalize(temp.path())?;
    std::fs::create_dir_all(state.join("candidate-0/sessions"))?;
    for name in ["1", "2", "3"] {
        std::fs::write(state.join("candidate-0/sessions").join(name), "lane")?;
    }
    // An unfinished search is not a missing session.
    assert_eq!(
        find_session_state_within(&state, "fixture-session", limits),
        Err(SearchExhausted::Entries)
    );
    std::fs::remove_file(state.join("candidate-0/sessions/3"))?;
    assert_eq!(
        find_session_state_within(&state, "fixture-session", limits),
        Ok(None)
    );
    Ok(())
}

// No unprivileged test can mount a filesystem inside a temporary tree, so
// this runs the walk as if its anchor sat on another device.
#[test]
fn retained_state_copy_refuses_a_filesystem_boundary() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = std::fs::canonicalize(temp.path())?;
    let source = base.join("candidate-0");
    std::fs::create_dir_all(source.join("sessions"))?;
    std::fs::create_dir(base.join("copy"))?;
    let (from, to) = (
        Directory::open_anchor(&source)?,
        Directory::open_anchor(&base.join("copy"))?,
    );
    let mut budget = Budget {
        entries: RETAINED_STATE.entries,
        bytes: RETAINED_STATE.bytes,
        device: from.metadata()?.device ^ 1,
    };
    let opened = std::rc::Rc::new(std::cell::Cell::new(false));
    let seen = std::rc::Rc::clone(&opened);
    BEFORE_OPEN.set(Some(Box::new(move |_| seen.set(true))));
    let result = copy_tree(&from, &to, RETAINED_STATE.depth, &mut budget, true);
    BEFORE_OPEN.set(None);
    assert!(matches!(result, Err(CopyRefusal::CrossDevice)));
    assert!(!opened.get(), "a mount point is refused before its open");
    assert!(!base.join("copy/sessions").exists());
    Ok(())
}

#[test]
fn codex_rollout_is_found_in_the_launched_candidate() -> Result<(), Box<dyn std::error::Error>> {
    for native in [false, true] {
        let temp = tempfile::tempdir()?;
        let base = temp.path().join("provider-state");
        let candidate = base.join("candidate-1");
        let thread = "01234567-89ab-cdef-0123-456789abcdef";
        let home = if native {
            temp.path().join("selected-account")
        } else {
            candidate.clone()
        };
        let day = home.join("sessions/2026/10/24");
        std::fs::create_dir_all(base.join("candidate-0"))?;
        std::fs::create_dir_all(&candidate)?;
        std::fs::create_dir_all(&day)?;
        if native {
            std::fs::write(
                base.join("codex-observation-candidate-1.json"),
                serde_json::to_vec(&serde_json::json!({
                    "schema_version": 1,
                    "home": provenant_records::StructuredPathWire::from_path(&home)?,
                }))?,
            )?;
        }
        // Candidate-local bytes cannot redirect host observation.
        std::fs::write(
            candidate.join("codex-observation-candidate-1.json"),
            br#"{"schema_version":1,"home":"/unrelated-home"}"#,
        )?;
        std::fs::write(
            day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session_meta","payload":{"id":thread}}),
                serde_json::json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
            ),
        )?;
        assert_eq!(observe_codex_rollout(&base, thread)?.model, "gpt-6-luna");
        assert!(observe_codex_rollout(&base, "another-thread").is_err());
        if native {
            // Two fallback candidates can launch against the same login home.
            std::fs::copy(
                base.join("codex-observation-candidate-1.json"),
                base.join("codex-observation-candidate-0.json"),
            )?;
            assert_eq!(observe_codex_rollout(&base, thread)?.model, "gpt-6-luna");
        }
        // Independent homes that claim the same thread remain ambiguous.
        let other = base.join("candidate-0");
        let other_day = other.join("sessions/2026/10/24");
        std::fs::create_dir_all(&other_day)?;
        std::fs::copy(
            day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
            other_day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
        )?;
        if native {
            std::fs::remove_file(base.join("codex-observation-candidate-0.json"))?;
        }
        assert!(matches!(
            observe_codex_rollout(&base, thread),
            Err(provenant_providers::ProviderError::IdentityMismatch)
        ));
    }
    Ok(())
}

#[test]
fn explicit_write_root_keeps_read_only_git_authority() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let source = root.join("source");
    std::fs::create_dir(&source)?;
    let write = root.join("write");
    std::fs::create_dir(&write)?;
    let access = run::LaneAccess {
        write_roots: vec![write.clone()],
        ..Default::default()
    };
    let request = confinement_request(
        ProjectId::parse("0123456789abcdef0123456789abcdef")?,
        ConfinementRoots {
            host_roots: None,
            cwd: &source,
            home: &root,
            state: Path::new("/fixture-host-state"),
            credential_roots: &[],
            github_roots: &[],
            protected_worktrees: &[],
            scratch: &root,
            private_state: &root,
            assets: &root,
            prompt_file: None,
        },
        None,
        &access,
        &Default::default(),
        "codex",
        false,
        InventoryContext::Sandboxed,
    )?;
    assert!(matches!(request.permissions.mode, AccessMode::ReadOnly));
    assert_eq!(request.permissions.write_roots[0].path, write);
    Ok(())
}

#[test]
fn launch_policy_binds_mandatory_aliases_and_all_network_ceilings()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let source = root.join("source");
    let state = root.join("state");
    let relocated = root.join("relocated-identity");
    let gh = root.join("relocated-gh");
    let credential_target = root.join("credential-target");
    for path in [&home, &source, &state, &relocated, &gh, &credential_target] {
        std::fs::create_dir(path)?;
    }
    std::fs::set_permissions(
        &state,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    std::os::unix::fs::symlink(&credential_target, home.join(".ssh"))?;
    let subtree = |path: &Path| PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    };
    for ceiling in [
        NetworkAccess::Provider,
        NetworkAccess::Local,
        NetworkAccess::Open,
    ] {
        let config = provenant_confinement::run_config::RunConfig {
            host_policy: Some(serde_json::from_value(
                serde_json::json!({"network": ceiling, "deny_read": ["/Users"], "deny_write": ["/home"]}),
            )?),
            ..Default::default()
        };
        for requested in [
            NetworkAccess::Provider,
            NetworkAccess::Local,
            NetworkAccess::Open,
        ] {
            let access = run::LaneAccess {
                network: Some(requested),
                ..Default::default()
            };
            let request = confinement_request(
                ProjectId::from_bytes([1; 16]),
                ConfinementRoots {
                    host_roots: None,
                    cwd: &source,
                    home: &home,
                    state: &state,
                    credential_roots: &[subtree(&relocated)],
                    github_roots: &[subtree(&gh)],
                    protected_worktrees: &[],
                    scratch: &root.join("scratch"),
                    private_state: &root.join("private"),
                    assets: &root.join("assets"),
                    prompt_file: None,
                },
                None,
                &access,
                &config,
                "codex",
                false,
                InventoryContext::Sandboxed,
            )?;
            let policy = request
                .permissions
                .policy
                .as_ref()
                .ok_or("missing launch policy")?;
            let expected = match (ceiling, requested) {
                (NetworkAccess::Provider, _) | (_, NetworkAccess::Provider) => {
                    NetworkAccess::Provider
                }
                (NetworkAccess::Local, _) | (_, NetworkAccess::Local) => NetworkAccess::Local,
                _ => NetworkAccess::Open,
            };
            assert_eq!(request.permissions.network, expected);
            assert_eq!(policy.mandatory().state().path, state);
            for credential in [
                &relocated,
                &credential_target,
                &home.join(".ssh"),
                &home.join(".claude.json"),
            ] {
                assert!(
                    policy
                        .mandatory()
                        .credentials()
                        .iter()
                        .any(|root| &root.path == credential)
                );
            }
            assert!(policy.mandatory().github().contains(&subtree(&gh)));
            let plan = ConfinedPlan::new(request)?;
            let mut profile = provenant_confinement::permissions::ProviderProfileSpec {
                deny_credential_access: true,
                ..Default::default()
            };
            assert!(!github_auth_permitted(&plan, &profile));
            profile.deny_credential_access = false;
            profile.deny_network = true;
            assert!(!github_auth_permitted(&plan, &profile));
        }
    }
    Ok(())
}

#[test]
fn launch_credential_inventory_includes_unselected_and_host_sealed_stores()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(home.join(".agy-unselected"))?;
    let settings: provenant_providers::accounts::Settings = serde_json::from_value(
        serde_json::json!({
            "entries": {
                "unselected": {"adapter":"codex", "label":"unselected", "key":"unselected", "directory":root.join("registered"), "directories":[root.join("other-registration")]},
                "claude": {"adapter":"claude", "label":"claude", "key":"claude", "directory":root.join("claude"), "directories":[root.join("other-claude")]}
            }
        }),
    )?;
    let mut host = run::HostLaunchContext::from_environment(
        &root.join("state"),
        &root.join("source"),
        "/usr/bin/git".into(),
    );
    host.credential_roots = vec![
        PathGrant { path: root.join("relocated-xdg/opencode"), scope: PathScope::Subtree },
        PathGrant { path: root.join("agent/socket"), scope: PathScope::Exact },
    ];
    host.default_account_directories = [("codex".into(), root.join("default-codex"))].into();
    host.provider_path_environment = [(
        "XDG_DATA_HOME".into(),
        root.join("relocated-xdg").into_os_string(),
    )]
    .into();
    // Custody encoding retains these host inputs across a cleared guardian
    // environment and through another generation of child launches.
    let host: run::HostLaunchContext = serde_json::from_slice(&serde_json::to_vec(&host)?)?;
    let registry = Registry {
        host: Some(&host),
        ..Default::default()
    };
    let inventory = credential_roots(&registry, &home, &settings)?;
    assert!(inventory.contains(&PathGrant { path: root.join("agent/socket"), scope: PathScope::Exact }));
    assert!(!inventory.iter().any(|entry| entry.path == root.join("agent")));
    for path in [
        root.join("registered"),
        root.join("other-registration"),
        root.join("default-codex"),
        root.join("relocated-xdg/opencode"),
    ] {
        assert!(inventory.iter().any(|root| root.path == path));
    }
    assert_eq!(
        host_provider_var(&registry, "XDG_DATA_HOME"),
        Some(root.join("relocated-xdg").into_os_string())
    );
    assert_eq!(host_provider_var(&registry, "GH_CONFIG_DIR"), None);
    assert_eq!(host_provider_var(&registry, "SSH_AUTH_SOCK"), None);
    let mut inputs = provenant_core::RootInputs::new(home.clone(), root.join("source"));
    inputs.state_dir = Some(root.join("state"));
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let sibling = root.join("claude.json");
    let sibling_target = root.join("outside-claude-profile");
    std::fs::File::create(&sibling_target)?;
    std::os::unix::fs::symlink(&sibling_target, &sibling)?;
    let mandatory = provenant_confinement::permissions::MandatoryRoots::for_host(&roots, &home, &root.join("source"), &inventory, &[], &[])?;
    for path in [sibling, sibling_target, root.join("other-claude.json")] {
        assert!(mandatory.credentials().contains(&PathGrant { path, scope: PathScope::Exact }));
    }
    assert!(mandatory.credentials().iter().any(|entry| entry.path == home.join(".agy-unselected")));

    Ok(())
}

#[test]
fn launch_inventory_protects_owned_worktrees_in_another_registered_project()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::worktrees::{WorktreeOrigin, WorktreeOwner};
    let temp = tempfile::tempdir()?;
    let root = std::fs::canonicalize(temp.path())?;
    let home = root.join("home");
    let source = root.join("source");
    let state = root.join("state");
    let foreign_common = root.join("other-project/.git");
    let foreign = root.join("external-owned-checkout");
    for path in [&home, &source, &state, &foreign, &foreign_common.join("objects")] {
        std::fs::create_dir_all(path)?;
    }
    std::fs::set_permissions(
        &state,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    std::fs::write(foreign_common.join("HEAD"), "ref: refs/heads/main\n")?;
    let mut inputs = provenant_core::RootInputs::new(home.clone(), source.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    provenant_config::bind_project(&roots, provenant_config::ProjectLocation::new(&foreign_common)?)?;
    let registration = foreign_common.join("worktrees/external");
    std::fs::create_dir_all(&registration)?;
    let owner = WorktreeOwner {
        attempt_id: None, operation_id: None, branch: "other-lane".into(),
        base: "1".repeat(40), host: provenant_core::HostId::from_bytes([7; 16]),
        created_at: provenant_core::WallTime::from_micros(1)?,
        origin: WorktreeOrigin::Adopted, label: None,
        reference: None,
    };
    std::fs::write(registration.join("provenant-owner.json"), serde_json::to_vec(&owner)?)?;
    std::fs::write(registration.join("gitdir"), format!("{}\n", foreign.join(".git").display()))?;
    // An unowned sibling's name is not an extra mandatory source boundary.
    let unowned = foreign_common.join("worktrees/unowned");
    std::fs::create_dir(&unowned)?;
    std::fs::write(unowned.join("gitdir"), format!("{}\n", root.join("unowned/.git").display()))?;
    let registry = Registry { roots: Some(&roots), ..Default::default() };
    let git = provenant_os::git::git_executable().ok_or("Git unavailable")?;
    let selected = protected_worktrees(&registry, &source, &git)?;
    let inventory_for = |source: &Path| provenant_confinement::permissions::MandatoryRoots::for_host(&roots, &home, source, &[], &[], &selected);
    let inventory = inventory_for(&source)?;
    let protected = inventory.protected().to_vec();
    assert!(protected.iter().any(|root| root.path == foreign));
    assert!(protected.iter().any(|root| root.path == foreign_common));
    assert!(!protected.iter().any(|entry| entry.path == root.join("unowned")));
    // A human checkout with active/retained writer custody is protected even
    // when it has no lane-provenance marker. Empty retired slots grant none.
    let custody_slot = PathBuf::from(provenant_confinement::worktrees::state_dir_name(&root.join("unowned")));
    assert!(provenant_confinement::worktrees::owned_worktree_paths(&foreign_common, &[custody_slot])?.contains(&root.join("unowned")));
    assert!(!inventory_for(&foreign)?.protected().iter().any(|entry| entry.path == foreign));
    let request = confinement_request(
        ProjectId::from_bytes([8; 16]),
        ConfinementRoots {
            host_roots: None,
            cwd: &source, home: &home, state: &state, credential_roots: &[], github_roots: &[],
            protected_worktrees: &protected, scratch: &root.join("scratch"),
            private_state: &root.join("private"), assets: &root.join("assets"), prompt_file: None,
        },
        None, &run::LaneAccess::default(), &Default::default(), "command", false,
        InventoryContext::Sandboxed,
    )?;
    let mandatory = request.permissions.policy.as_ref().ok_or("policy")?.mandatory();
    for path in [&foreign, &foreign_common, &foreign.join("nested"), &foreign_common.join("refs")] {
        assert!(matches!(mandatory.validate_extra_write(&PathGrant { path: path.clone(), scope: PathScope::Subtree }), Err(provenant_confinement::permissions::PermissionError::ProtectedOverlap)));
    }
    assert!(mandatory.validate_extra_write(&PathGrant { path: root.join("external-owned-checkout-other"), scope: PathScope::Subtree }).is_ok());
    // A checkout pruned after ownership was recorded is still denied by name.
    std::fs::remove_dir(&foreign)?;
    assert!(inventory_for(&source)?.protected().iter().any(|root| root.path == foreign));
    Ok(())
}

#[test]
fn codex_attachment_argv_contains_image_flag() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::Adapter;
    let image = PathBuf::from("/outside/shot.png");
    let access = run::LaneAccess {
        attachments: vec![image.clone()],
        ..Default::default()
    };
    let plan = ConfinedPlan::new(confinement_request(
        ProjectId::parse("0123456789abcdef0123456789abcdef")?,
        ConfinementRoots {
            host_roots: None,
            cwd: Path::new("/project"),
            home: Path::new("/home/user"),
            state: Path::new("/fixture-host-state"),
            credential_roots: &[],
            github_roots: &[],
            protected_worktrees: &[],
            scratch: Path::new("/tmp/scratch"),
            private_state: Path::new("/state/private"),
            assets: Path::new("/release"),
            prompt_file: None,
        },
        None,
        &access,
        &Default::default(),
        "codex",
        false,
        InventoryContext::Sandboxed,
    )?)?;
    let route = provenant_records::ResolvedRoute {
        extra: Default::default(),
        adapter: "codex".into(),
        model: "fixture".into(),
        effort: None,
        catalogue_revision: "fixture".into(),
        account_key: None,
        host: "local".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Ready {
            extra: Default::default(),
            availability_ref: "fixture".into(),
            expires_unix_seconds: 100,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "fixture".into(),
        },
        pool_selection: None,
    };
    let prompt = Prompt::new("describe".into());
    let launch = provenant_providers::codex::Codex.prepare(PrepareInput {
        images: &access.attachments,
        command: None,
        route: &route,
        confined: &plan,
        prompt: &prompt,
        continuation: None,
        grant_digest: "fixture",
        executable: Path::new("/opt/homebrew/Cellar/codex/fixture/bin/codex"),
        home: Path::new("/home/user"),
        user: "fixture".as_ref(),
        host_var: &|_| None,
        skill_stage_max_bytes: 0,
        account: None,
        token: None,
        cwd: Path::new("/project"),
        git_context: false,
        nonce: [0; 16],
        provenant: LaneRuntime {
            executable: Path::new("/fixture/provenant/bin/provenant"),
            git: Path::new("/usr/bin/git"),
            state: Path::new("/state"),
            home: Path::new("/home/user"),
            depth: 1,
            attempt: provenant_core::AttemptId::from_bytes([7; 16]),
            assets: Path::new("/release"),
            fabric_mcp: true,
        },
    })?;
    assert!(
        launch
            .argv
            .windows(2)
            .any(|pair| pair == ["--image", "/outside/shot.png"])
    );
    Ok(())
}

#[test]
fn attachment_read_grant_is_exact() -> Result<(), Box<dyn std::error::Error>> {
    let image = PathBuf::from("/outside/image.png");
    let access = run::LaneAccess {
        attachments: vec![image.clone()],
        ..Default::default()
    };
    let request = confinement_request(
        ProjectId::parse("0123456789abcdef0123456789abcdef")?,
        ConfinementRoots {
            host_roots: None,
            cwd: Path::new("/project"),
            home: Path::new("/home/user"),
            state: Path::new("/fixture-host-state"),
            credential_roots: &[],
            github_roots: &[],
            protected_worktrees: &[],
            scratch: Path::new("/tmp/scratch"),
            private_state: Path::new("/state/private"),
            assets: Path::new("/release"),
            prompt_file: None,
        },
        None,
        &access,
        &Default::default(),
        "cursor",
        false,
        InventoryContext::Sandboxed,
    )?;
    assert!(request.permissions.read_roots.contains(&PathGrant {
        path: image,
        scope: PathScope::Exact
    }));
    assert!(
        !request
            .permissions
            .read_roots
            .iter()
            .any(|grant| grant.path == Path::new("/outside"))
    );
    Ok(())
}

#[test]
fn lane_plan_reads_the_active_release_root() -> Result<(), Box<dyn std::error::Error>> {
    let cwd = Path::new("/project");
    let assets = Path::new("/release/sha256:active");
    let home = Path::new("/home/user");
    let scratch = Path::new("/tmp/lane-scratch");
    let private_state = Path::new("/state/attempt/provider-state");
    let prompt = Path::new("/state/attempt/prompt.md");
    for entry in REGISTRY {
        let request = confinement_request(
            ProjectId::parse("0123456789abcdef0123456789abcdef")?,
            ConfinementRoots {
                host_roots: None,
                cwd,
                home,
                state: Path::new("/fixture-host-state"),
                credential_roots: &[],
                github_roots: &[],
                protected_worktrees: &[],
                scratch,
                private_state,
                assets,
                prompt_file: Some(prompt),
            },
            None,
            &run::LaneAccess::default(),
            &Default::default(),
            entry.adapter.id(),
            true,
            InventoryContext::Sandboxed,
        )?;
        assert!(request.permissions.read_roots.contains(&PathGrant {
            path: assets.to_owned(),
            scope: PathScope::Subtree,
        }));
        let plan = ConfinedPlan::new(request)?;
        assert!(plan.permissions().read_roots.contains(&PathGrant {
            path: prompt.to_owned(),
            scope: PathScope::Exact,
        }));
        assert!(
            !plan
                .permissions()
                .read_roots
                .iter()
                .any(|grant| { grant.path == prompt.parent().expect("attempt") })
        );
    }
    Ok(())
}

#[test]
fn writers_read_the_primary_checkouts_untracked_scratch() -> Result<(), Box<dyn std::error::Error>>
{
    let project = tempfile::tempdir()?;
    let canonical = std::fs::canonicalize(project.path())?;
    let root = canonical.as_path();
    assert!(project_scratch_reads(root).is_empty());
    std::fs::create_dir_all(root.join(".provenant/scratch"))?;
    std::fs::create_dir(root.join(".agent-run"))?;
    std::fs::create_dir(root.join(".provenant/other"))?;
    let paths = |grants: Vec<PathGrant>| -> Vec<PathBuf> {
        assert!(grants.iter().all(|grant| grant.scope == PathScope::Subtree));
        grants.into_iter().map(|grant| grant.path).collect()
    };
    assert_eq!(
        paths(project_scratch_reads(root)),
        [root.join(".provenant/scratch"), root.join(".agent-run")]
    );
    // A link could point the grant anywhere.
    std::fs::remove_dir(root.join(".agent-run"))?;
    std::os::unix::fs::symlink("/", root.join(".agent-run"))?;
    assert_eq!(
        paths(project_scratch_reads(root)),
        [root.join(".provenant/scratch")]
    );
    Ok(())
}

#[test]
fn project_scratch_reads_rejects_a_symlinked_parent_outside_the_checkout()
-> Result<(), Box<dyn std::error::Error>> {
    let project = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::fs::create_dir(outside.path().join("scratch"))?;
    std::os::unix::fs::symlink(outside.path(), project.path().join(".provenant"))?;
    assert!(project_scratch_reads(project.path()).is_empty());
    // An in-checkout parent link is safe, and grants its canonical target.
    std::fs::remove_file(project.path().join(".provenant"))?;
    std::fs::create_dir_all(project.path().join("owned/scratch"))?;
    std::os::unix::fs::symlink("owned", project.path().join(".provenant"))?;
    let canonical = std::fs::canonicalize(project.path())?;
    assert_eq!(
        project_scratch_reads(project.path()),
        vec![PathGrant {
            path: canonical.join("owned/scratch"),
            scope: PathScope::Subtree,
        }]
    );
    Ok(())
}

#[test]
fn host_write_paths_reach_writers_only() {
    let host = provenant_confinement::run_config::RunConfig {
        writer_writes: vec!["/p/.agent-run/locks".into()],
        ..Default::default()
    };
    let access = run::LaneAccess {
        write_roots: vec!["/p/extra".into()],
        ..Default::default()
    };
    let paths = |writer| -> Vec<PathBuf> {
        write_roots(&access, &host, writer, "project")
            .into_iter()
            .map(|grant| grant.path)
            .collect()
    };
    assert_eq!(
        paths(true),
        [PathBuf::from("/p/extra"), "/p/.agent-run/locks".into()]
    );
    assert_eq!(paths(false), [PathBuf::from("/p/extra")]);
}

#[test]
fn writer_plan_roots_are_resolved_for_the_selected_project() {
    use provenant_confinement::run_config::{ProjectRunConfig, RunConfig};
    let config = RunConfig {
        writer_writes: vec!["/host/locks".into()],
        projects: [
            (
                "a".into(),
                ProjectRunConfig {
                    writer_writes: Some(vec!["/a/locks".into()]),
                    ..Default::default()
                },
            ),
            (
                "b".into(),
                ProjectRunConfig {
                    writer_writes: Some(vec!["/b/locks".into()]),
                    ..Default::default()
                },
            ),
        ]
        .into(),
        ..Default::default()
    };
    let roots = write_roots(&run::LaneAccess::default(), &config, true, "a");
    assert_eq!(
        roots,
        vec![PathGrant {
            path: "/a/locks".into(),
            scope: PathScope::Subtree
        }]
    );
    assert!(
        !roots
            .iter()
            .any(|grant| grant.path == Path::new("/b/locks"))
    );
}

#[test]
fn setup_stays_below_run_owned_roots_without_following_links()
-> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::temp_dir().join(format!("provenant-setup-{}", std::process::id()));
    std::fs::create_dir(&base)?;
    let base = canonical(&base)?;
    let (scratch, state, outside) = (base.join("tmp"), base.join("state"), base.join("out"));
    for path in [&scratch, &state, &outside] {
        std::fs::create_dir(path)?;
    }
    let roots = [scratch.as_path(), state.as_path()];
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        apply_setup(
            &[
                SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                SetupStep::CreateDir(scratch.join("cargo-home/nested")),
                SetupStep::Symlink {
                    link: scratch.join("cargo-home/registry"),
                    target: outside.clone(),
                },
            ],
            &roots,
            None,
        )?;
        assert!(scratch.join("cargo-home/nested").is_dir());
        apply_setup(
            &[SetupStep::CreateDir(outside.join("projects/session"))],
            &roots,
            Some(&outside),
        )?;
        assert!(outside.join("projects/session").is_dir());
        assert!(
            apply_setup(
                &[SetupStep::CreateDir(outside.join("credentials/new"))],
                &roots,
                Some(&outside)
            )
            .is_err()
        );
        assert!(
            apply_setup(
                &[SetupStep::Symlink {
                    link: outside.join("projects/link"),
                    target: state.clone()
                }],
                &roots,
                Some(&outside)
            )
            .is_err()
        );
        std::os::unix::fs::symlink(&outside, scratch.join("escape"))?;
        for step in [
            SetupStep::CreateDir(outside.join("x")),
            SetupStep::CreateDir(scratch.join("../out/x")),
            SetupStep::CreateDir(scratch.clone()),
            SetupStep::CreateDir(scratch.join("escape/x")),
            SetupStep::Symlink {
                link: scratch.join("cargo-home/registry/x"),
                target: outside.clone(),
            },
        ] {
            assert!(apply_setup(&[step], &roots, None).is_err());
        }
        assert!(!outside.join("x").exists());
        Ok(())
    })();
    std::fs::remove_dir_all(&base)?;
    result
}

#[test]
fn provider_setup_recovers_after_interrupted_preparation_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let base =
        std::env::temp_dir().join(format!("provenant-setup-recovery-{}", std::process::id()));
    std::fs::create_dir(&base)?;
    let base = canonical(&base)?;
    let (scratch, state) = (base.join("tmp"), base.join("state"));
    for path in [&scratch, &state] {
        std::fs::create_dir(path)?;
    }
    let roots = [scratch.as_path(), state.as_path()];
    let file = scratch.join("settings.json");
    let setup = |contents: &[u8]| {
        apply_setup(
            &[SetupStep::WriteFile {
                path: file.clone(),
                bytes: contents.to_vec(),
            }],
            &roots,
            None,
        )
    };
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        setup(b"first")?;
        // A crash after setup but before the launch claim leaves these
        // entries in place for the next preparation of the same attempt.
        setup(b"first")?;
        assert_eq!(std::fs::read(&file)?, b"first");

        setup(b"second")?;
        assert_eq!(std::fs::read(&file)?, b"second");
        Ok(())
    })();
    std::fs::remove_dir_all(&base)?;
    result
}

#[test]
fn a_task_may_read_only_its_own_nodes_graph_inputs() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let state = super::canonical(temp.path())?;
    let project = "p1";
    let context = |graph: &str, node: &str| provenant_records::run::WorkContext {
        graph_id: Some(graph.into()),
        node_id: Some(node.into()),
        ..Default::default()
    };
    let inputs = |graph: &str, node: &str| {
        super::graph_node_inputs_directory(&state, project, graph, node).expect("path-safe")
    };
    for (graph, node) in [("g", "mine"), ("g", "sibling"), ("other", "mine")] {
        std::fs::create_dir_all(inputs(graph, node))?;
    }
    let check = |context: Option<&provenant_records::run::WorkContext>, root: &Path| {
        super::check_graph_inputs_root(&state, project, context, Some(root))
    };
    let mine = context("g", "mine");
    // No grant requested, and its own directory, are accepted.
    assert!(super::check_graph_inputs_root(&state, project, None, None).is_ok());
    assert!(check(Some(&mine), &inputs("g", "mine")).is_ok());
    // The shared root, every ancestor, other nodes and other graphs are not.
    let shared = super::graph_inputs_directory(&state);
    for refused in [
        shared.clone(),
        shared.join(project),
        shared.join(project).join("g"),
        inputs("g", "sibling"),
        inputs("other", "mine"),
        inputs("g", "mine").join("nested"),
        inputs("g", "mine").join(".."),
        state.clone(),
    ] {
        assert!(
            check(Some(&mine), &refused).is_err(),
            "{} must be refused",
            refused.display()
        );
    }
    // A task naming no graph node has no directory to be granted.
    assert!(check(None, &inputs("g", "mine")).is_err());
    // Names a case-insensitive filesystem would merge, and names that are not
    // path-safe, still get separate directories, and one is refused the other.
    assert_ne!(inputs("g", "Review"), inputs("g", "review"));
    assert_ne!(inputs("G", "review"), inputs("g", "review"));
    assert_ne!(inputs("g", ".."), inputs("g", "."));
    for (graph, node) in [("g", "Review"), ("g", "review"), ("g", "..")] {
        std::fs::create_dir_all(inputs(graph, node))?;
    }
    let upper = context("g", "Review");
    let lower = context("g", "review");
    assert!(check(Some(&upper), &inputs("g", "Review")).is_ok());
    assert!(check(Some(&lower), &inputs("g", "review")).is_ok());
    assert!(check(Some(&upper), &inputs("g", "review")).is_err());
    assert!(check(Some(&lower), &inputs("g", "Review")).is_err());
    assert!(check(Some(&context("g", "..")), &shared.join(project).join("g")).is_err());
    // A link standing in for the node's directory is not followed.
    std::fs::remove_dir(inputs("g", "mine"))?;
    std::os::unix::fs::symlink(inputs("g", "sibling"), inputs("g", "mine"))?;
    assert!(check(Some(&mine), &inputs("g", "mine")).is_err());
    Ok(())
}

#[test]
fn prelaunch_login_failures_keep_auth_when_availability_is_absent()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_providers::{ProviderError, accounts::Registration};
    assert!(!violates_account_pin(None, None, Some("other"), "codex"));
    assert!(!violates_account_pin(
        Some("key"),
        Some("codex"),
        Some("key"),
        "codex"
    ));
    assert!(violates_account_pin(
        Some("key"),
        Some("codex"),
        Some("other"),
        "codex"
    ));
    assert!(violates_account_pin(
        Some("key"),
        Some("codex"),
        Some("key"),
        "claude"
    ));
    let entries: Vec<Registration> = ["work", "personal"].into_iter().map(|label| {
        serde_json::from_value(serde_json::json!({"adapter": "codex", "label": label, "key": label, "directory": format!("/fixture/{label}")}))
    }).collect::<Result<_, _>>()?;
    let route: ResolvedRoute = serde_json::from_value(serde_json::json!({
        "adapter": "codex", "model": "example", "catalogue_revision": "fixture",
        "host": "fixture", "alternatives": [], "exclusions": [],
        "confidentiality": {"evidence_ref": "fixture"}, "eligibility": "eligible",
        "readiness": {"status": "unavailable", "reason": "unknown_availability"}
    }))?;
    let mut failures = std::collections::BTreeMap::new();
    for (entry, error) in entries.iter().zip([
        ProviderError::LoginRequired,
        ProviderError::CredentialNotFound,
    ]) {
        let AccountObservationError::AuthRequired(detail) =
            account_observation_error(entry, &error)
        else {
            panic!("login failure lost its type")
        };
        assert!(detail.contains("codex login"));
        assert!(detail.contains(&format!("CODEX_HOME='/fixture/{}'", entry.label)));
        failures.insert((entry.adapter.clone(), entry.key.clone()), detail);
    }
    assert!(unavailable_auth_detail(&route, None, &entries, &failures).is_some());
    failures.remove(&("codex".into(), "personal".into()));
    assert!(unavailable_auth_detail(&route, None, &entries, &failures).is_none());
    assert!(unavailable_auth_detail(&route, Some("work"), &entries, &failures).is_some());
    assert!(matches!(
        account_observation_error(&entries[0], &ProviderError::IdentityMismatch),
        AccountObservationError::Other(_)
    ));
    assert!(matches!(
        account_observation_error(&entries[0], &ProviderError::CredentialUnavailable),
        AccountObservationError::AuthRequired(_)
    ));
    assert!(matches!(
        account_observation_error(&entries[0], &ProviderError::UnknownIdentity),
        AccountObservationError::Other(_)
    ));
    assert!(matches!(
        account_observation_error(
            &entries[0],
            &ProviderError::ProbeDirectory("fixture".into())
        ),
        AccountObservationError::Other(_)
    ));
    Ok(())
}

#[test]
fn absent_default_login_differs_from_directory_integrity_errors()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let mut spec = *AdapterSpec::get("codex").unwrap();
    spec.name = "fixture";
    spec.login_hint = "fixture login";
    let missing = temp.path().join("missing");
    assert!(
        default_directory_error(&spec, &missing, "not found".into())
            .to_string()
            .contains("fixture")
    );
    assert_eq!(
        default_directory_error(&spec, &missing, "not found".into()).skip_kind(),
        SkipKind::AuthRequired
    );
    assert_eq!(
        default_directory_error(&spec, temp.path(), "permission denied".into()).skip_kind(),
        SkipKind::Account
    );
    #[cfg(unix)]
    {
        let broken = temp.path().join("broken-link");
        std::os::unix::fs::symlink(&missing, &broken)?;
        assert_eq!(
            default_directory_error(&spec, &broken, "broken link".into()).skip_kind(),
            SkipKind::Account
        );
    }
    assert_eq!(
        AccountObservationError::VersionUnavailable("codex executable missing".into()).skip_kind(),
        SkipKind::Unavailable
    );
    Ok(())
}

#[test]
fn staged_skill_files_are_written_outside_writable_scratch()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let temporary = temp.path().join("tmp");
    std::fs::create_dir_all(&temporary)?;
    let scratch = temporary.join("candidate-0");
    let stage = temporary.join("candidate-0-skills");
    apply_setup_directory(&[&temporary], &scratch)?;
    apply_setup_directory(&[&temporary], &stage)?;
    apply_setup(
        &[
            SetupStep::CreateDir(stage.join("example")),
            SetupStep::WriteFile {
                path: stage.join("example/SKILL.md"),
                bytes: b"staged".to_vec(),
            },
        ],
        &[&stage],
        None,
    )?;
    assert_eq!(std::fs::read(stage.join("example/SKILL.md"))?, b"staged");
    assert!(!stage.starts_with(&scratch));
    assert!(!scratch.starts_with(&stage));
    Ok(())
}

#[test]
fn live_child_source_uses_its_selected_directory_and_rechecks_authority()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, WallTime};
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("parent");
    let scratch = base.join("scratch");
    for path in [&home, &state, &project, &scratch] { std::fs::create_dir(path)?; }
    let registry = Registry {
        roots: None, home: Some(&home), assets: None, cwd: Some(&project),
        state_root: Some(&state), project_id: None, host: None, cancel: None,
        repo_facts: Default::default(),
    };
    let mut task = super::super::tests::trace_run(50, AttemptId::from_bytes([51; 16]), WallTime::from_micros(100)?)?
        .request.tasks.remove(0);
    task.writer = None;
    task.head = None;
    task.access.source_root = Some(scratch.clone());
    task.access.source_identity = Some(source_identity(&scratch)?);
    task.access.source_live = true;
    assert_eq!(source_cwd(&registry, &task)?, scratch);
    let mut changed_identity = task.clone();
    changed_identity.access.source_identity.as_mut().ok_or("identity")?.inode ^= 1;
    assert!(source_cwd(&registry, &changed_identity).is_err());
    for root in [&state, &home.join(".codex")] {
        std::fs::create_dir_all(root)?;
        let mut denied = task.clone();
        denied.access.source_root = Some(root.clone());
        denied.access.source_identity = Some(source_identity(root)?);
        assert!(source_cwd(&registry, &denied).is_err());
    }
    Ok(())
}

#[test]
fn graph_read_only_snapshot_survives_moving_and_replaced_checkout()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{AttemptId, OperationId, RunId, WallTime};
    use provenant_os::exec::{ExecOptions, Executor, Termination};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("repo");
    let assets = base.join("assets");
    let git_home = base.join("git-home");
    for path in [&home, &state, &project, &assets, &git_home] {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    let executor = provenant_os::exec::SystemExecutor::new(1)?;
    let git = init_fixture_repository(&executor, &project, &git_home)?;
    std::fs::write(project.join("subject"), "verified checkout")?;
    for args in [vec!["add", "subject"], vec!["commit", "-qm", "subject"]] {
        let argv: Vec<std::ffi::OsString> = std::iter::once(git.clone().into_os_string())
            .chain(args.into_iter().map(Into::into))
            .collect();
        let env = provenant_os::fixture::git_env();
        let mut options = ExecOptions::new(&argv, &project, Duration::from_secs(5));
        options.env = Some(&env);
        let result = executor.execute(options)?;
        assert_eq!(result.termination, Termination::Exit(0));
    }
    let head = run::resolve_git_base(&git, &project, "HEAD")?;
    let source = base.join("source");
    let added_args = vec![
        git.clone().into_os_string(),
        "worktree".into(),
        "add".into(),
        "--detach".into(),
        "-q".into(),
        source.clone().into_os_string(),
        head.clone().into(),
    ];
    let added = executor.execute(ExecOptions::new(
        &added_args,
        &project,
        Duration::from_secs(5),
    ))?;
    assert_eq!(added.termination, Termination::Exit(0));
    std::fs::write(project.join("subject"), "wrong checkout")?;
    std::fs::write(source.join("subject"), "uncommitted source edit")?;
    let fake = assets.join("fake-provider");
    std::fs::write(
        &fake,
        "#!/bin/sh\ncat subject; : > \"$SOURCE_READY\"; while [ ! -f \"$SOURCE_GO\" ]; do sleep 0.05; done; cat subject\n",
    )?;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        state.join("config.toml"),
        format!(
            "schema_version = 1\n[domains.fabric.commands.fake]\nargv = ['{}']\nruntimes = ['/bin/sh']\n",
            fake.display()
        ),
    )?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home.clone(), project.clone())
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let utility = provenant_confinement::UtilityRuntime::new(&git, &git_home)?;
    let budget = provenant_confinement::UtilityBudget::bounded(std::time::Duration::from_secs(5))?;
    let cancel = provenant_core::CancelToken::new();
    let repo = utility.open_repo(&executor, &project, budget, &cancel)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(repo.common_git_dir())?,
    )?;
    let registry = Registry {
        roots: Some(&roots),
        home: Some(&home),
        assets: Some(&assets),
        cwd: Some(&project),
        state_root: Some(&state),
        project_id: Some(binding.project_id()),
        host: None,
        cancel: Some(&cancel),
        repo_facts: Default::default(),
    };
    let mut task = super::super::tests::trace_run(
        50,
        AttemptId::from_bytes([51; 16]),
        WallTime::from_micros(100)?,
    )?
    .request
    .tasks
    .remove(0);
    task.head = Some(head.clone());
    task.base = Some(head.clone());
    task.access.source_root = Some(source.clone());
    task.access.source_identity = Some(source_identity(&source)?);
    task.route = run::RequestedRoute::Adapter {
        extra: Default::default(),
        provider: "command".into(),
        name: "fake".into(),
    };
    let paths = run::LaunchPaths {
        attempt_id: AttemptId::from_bytes([52; 16]),
        operation_id: OperationId::parse("source-test")?,
        task_index: 0,
        lane_depth: 1,
        run_id: RunId::from_bytes([53; 16]),
        deadline_unix_ms: u64::MAX,
        scratch: base.join("scratch"),
        provider_state: base.join("provider-state"),
        temporary: base.join("tmp"),
        temporary_alias: None,
    };
    for path in [&paths.scratch, &paths.provider_state, &paths.temporary] {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    // A separate Git directory's parent is not the checkout and must never
    // become an implicit source grant.
    let separate = base.join("separate-checkout");
    std::fs::create_dir(&separate)?;
    let separate_git = base.join(".git");
    let init_args = vec![
        git.clone().into_os_string(),
        "init".into(),
        "-q".into(),
        "--separate-git-dir".into(),
        separate_git.clone().into_os_string(),
        separate.clone().into_os_string(),
    ];
    let init = executor.execute(ExecOptions::new(&init_args, &base, Duration::from_secs(5)))?;
    assert_eq!(init.termination, Termination::Exit(0));
    let separate_repo = utility.open_repo(&executor, &separate, budget, &cancel)?;
    assert_eq!(separate_repo.common_git_dir(), separate_git);
    assert_eq!(
        run::resolve_reader_source(&state, &separate, &separate)?,
        separate
    );
    assert!(run::resolve_reader_source(&state, &separate, &base).is_err());
    let subdir = project.join("nested");
    std::fs::create_dir(&subdir)?;
    assert_eq!(
        run::resolve_reader_source(&state, &subdir, &subdir)?,
        project
    );
    let mut primary = task.clone();
    primary.access.source_root = Some(project.clone());
    primary.access.source_identity = Some(source_identity(&project)?);
    assert_eq!(source_cwd(&registry, &primary)?, project);
    let mut missing_identity = task.clone();
    missing_identity.access.source_identity = None;
    assert!(
        source_cwd(&registry, &missing_identity)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    let mut replaced = task.clone();
    replaced
        .access
        .source_identity
        .as_mut()
        .ok_or("source identity")?
        .inode ^= 1;
    assert!(
        source_cwd(&registry, &replaced)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    let mut protected = task.clone();
    protected.access.source_root = Some(state.clone());
    assert!(
        source_cwd(&registry, &protected)
            .unwrap_err()
            .to_string()
            .contains("not an allowed read root")
    );
    let foreign = base.join("foreign");
    std::fs::create_dir(&foreign)?;
    init_fixture_repository(&executor, &foreign, &git_home)?;
    protected.access.source_root = Some(foreign.clone());
    let error = source_cwd(&registry, &protected).unwrap_err().to_string();
    assert!(error.contains("no longer registered"));
    assert!(!error.contains(&foreign.to_string_lossy().into_owned()));
    // A live child uses the selected scratch checkout, even when it was not
    // registered and belongs to another project. Immutable snapshots above
    // retain their stricter source binding.
    let mut live = task.clone();
    live.head = None;
    live.base = None;
    live.access.source_root = Some(foreign.clone());
    live.access.source_identity = Some(source_identity(&foreign)?);
    live.access.source_live = true;
    assert_eq!(source_cwd(&registry, &live)?, foreign);
    let prepared_live = prepare_command(&registry, &live, &paths, None,
        &|_| Ok(fake.clone()), "command", "fake")?;
    assert_eq!(prepared_live.cwd, foreign);
    assert!(prepared_live.confinement.as_ref().ok_or("live confinement")?.plan
        .permissions().protected_roots.iter().any(|root| root.path == foreign));
    let launch = prepare_command(
        &registry,
        &task,
        &paths,
        None,
        &|_| Ok(fake.clone()),
        "command",
        "fake",
    )?;
    assert!(launch.cwd.starts_with(&paths.temporary));
    assert_ne!(launch.cwd, source);
    let plan = &launch
        .confinement
        .as_ref()
        .ok_or("missing confinement")?
        .plan;
    assert!(
        plan.permissions()
            .read_roots
            .iter()
            .any(|grant| grant.path == launch.cwd)
    );
    assert!(
        !plan
            .permissions()
            .read_roots
            .iter()
            .any(|grant| grant.path == source)
    );
    assert!(
        !plan
            .permissions()
            .read_roots
            .iter()
            .any(|grant| grant.path == project)
    );
    let git_reads: Vec<_> = plan
        .permissions()
        .read_only
        .as_ref()
        .ok_or("Git authority")?
        .git_read_paths()
        .collect();
    assert_eq!(
        git_reads,
        vec![repo.common_git_dir().join("objects").as_path()]
    );
    let live_source = source.clone();
    let ready = base.join("provider-ready");
    let proceed = base.join("provider-proceed");
    let thread_ready = ready.clone();
    let thread_proceed = proceed.clone();
    let thread_git = git.clone();
    let thread_home = git_home.clone();
    let move_while_running = std::thread::spawn(move || -> std::io::Result<()> {
        use provenant_core::Clock;
        let clock = provenant_os::clock::SystemClock::new();
        let deadline =
            provenant_core::WaitBudget::cli(clock.mono_now(), 5).map_err(std::io::Error::other)?;
        let (_sender, wait) = std::sync::mpsc::channel::<()>();
        while !thread_ready.exists() {
            if deadline.remaining(clock.mono_now()).as_micros() == 0 {
                return Err(std::io::Error::other("provider did not start"));
            }
            let _ = wait.recv_timeout(Duration::from_millis(10));
        }
        std::fs::write(live_source.join("subject"), "changed while provider ran")?;
        let args: Vec<std::ffi::OsString> = [
            thread_git.as_os_str(),
            "-c".as_ref(),
            "core.hooksPath=/dev/null".as_ref(),
            "commit".as_ref(),
            "--allow-empty".as_ref(),
            "-qm".as_ref(),
            "concurrent move".as_ref(),
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
        let mut env = vec![
            ("HOME".into(), thread_home.into_os_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ];
        env.extend(provenant_os::fixture::fixture_git_settings());
        let mut options = ExecOptions::new(&args, &live_source, Duration::from_secs(5));
        options.env = Some(&env);
        let result = provenant_os::exec::SystemExecutor::new(1)
            .map_err(std::io::Error::other)?
            .execute(options)
            .map_err(std::io::Error::other)?;
        if result.termination != Termination::Exit(0) {
            return Err(std::io::Error::other("source checkout did not advance"));
        }
        std::fs::write(thread_proceed, "continue")
    });
    let env = vec![
        ("SOURCE_READY".into(), ready.into_os_string()),
        ("SOURCE_GO".into(), proceed.into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let mut options = ExecOptions::new(&launch.argv, &launch.cwd, Duration::from_secs(10));
    options.env = Some(&env);
    let output = executor.execute(options)?;
    move_while_running
        .join()
        .map_err(|_| "source mutation thread panicked")??;
    assert_eq!(output.termination, Termination::Exit(0));
    assert_eq!(output.stdout.bytes, b"verified checkoutverified checkout");
    let status_args: Vec<std::ffi::OsString> =
        [git.as_os_str(), "status".as_ref(), "--porcelain".as_ref()]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect();
    let status = executor.execute(ExecOptions::new(
        &status_args,
        &launch.cwd,
        Duration::from_secs(5),
    ))?;
    assert_eq!(status.termination, Termination::Exit(0));
    assert!(
        status.stdout.bytes.is_empty(),
        "snapshot status: {:?}",
        status.stdout.bytes
    );
    let moved_args: Vec<std::ffi::OsString> = [
        git.as_os_str(),
        "commit".as_ref(),
        "--allow-empty".as_ref(),
        "-qm".as_ref(),
        "move".as_ref(),
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect();
    let env = provenant_os::fixture::git_env();
    let mut options = ExecOptions::new(&moved_args, &source, Duration::from_secs(5));
    options.env = Some(&env);
    let moved = executor.execute(options)?;
    assert_eq!(moved.termination, Termination::Exit(0));
    let historical = prepare_command(
        &registry,
        &task,
        &paths,
        None,
        &|_| Ok(fake.clone()),
        "command",
        "fake",
    )?;
    assert_eq!(
        std::fs::read(historical.cwd.join("subject"))?,
        b"verified checkout"
    );
    // An exception that would grant writes into the live checkout cannot be
    // carried into an isolated source view.
    std::fs::create_dir(source.join("mutable"))?;
    let project_state = state
        .join("projects")
        .join(binding.project_id().to_string());
    std::fs::create_dir_all(&project_state)?;
    std::fs::set_permissions(&project_state, std::fs::Permissions::from_mode(0o700))?;
    let policy = project_state.join("read-only.json");
    std::fs::write(&policy, r#"{"readOnlyWritable":["mutable"]}"#)?;
    std::fs::set_permissions(&policy, std::fs::Permissions::from_mode(0o600))?;
    let refused = prepare_command(
        &registry,
        &task,
        &paths,
        None,
        &|_| Ok(fake.clone()),
        "command",
        "fake",
    )
    .err()
    .ok_or("live-source write exception was accepted")?;
    assert!(
        refused
            .to_string()
            .contains("read-only project write policy"),
        "{refused}"
    );
    std::fs::remove_file(policy)?;
    // Replacing the path with another registered worktree at the same head
    // must not silently replace the source selected at admission.
    let moved_path = base.join("moved-source");
    let move_args = vec![
        git.clone().into_os_string(),
        "worktree".into(),
        "move".into(),
        source.clone().into_os_string(),
        moved_path.into_os_string(),
    ];
    let moved = executor.execute(ExecOptions::new(
        &move_args,
        &project,
        Duration::from_secs(5),
    ))?;
    assert_eq!(moved.termination, Termination::Exit(0));
    let added = executor.execute(ExecOptions::new(
        &added_args,
        &project,
        Duration::from_secs(5),
    ))?;
    assert_eq!(added.termination, Termination::Exit(0));
    assert!(
        source_cwd(&registry, &task)
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    Ok(())
}

#[test]
fn writer_preparation_warning_names_setting_and_origin() {
    let mut profile = provenant_records::run::HarnessProfile::default();
    let default = super::dependency_preparation_warning("missing pnpm".into(), true, &profile);
    assert!(default.contains("lanes.prepare_deps=true (compiled default)"));
    assert!(default.contains("pnpm install --frozen-lockfile` in the worktree"));
    profile.name = "focused".into();
    profile
        .knobs
        .insert("lanes.prepare_deps".into(), true.into());
    let configured = super::dependency_preparation_warning("missing pnpm".into(), true, &profile);
    assert!(configured.contains("lanes.prepare_deps=true (sealed profile focused)"));
    assert!(configured.contains("lanes.prepare_deps=false"));
}

#[test]
fn write_grant_limit_reports_configured_value_and_origin() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_run::Providers;
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let checkout = temp.path().join("checkout");
    let state = temp.path().join("state");
    for path in [&home, &checkout, &state] {
        std::fs::create_dir_all(path)?;
    }
    let mut inputs = provenant_core::RootInputs::new(home.clone(), checkout.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let registry = Registry {
        roots: Some(&roots),
        ..Registry::default()
    };
    let default = registry.write_grant_limit();
    assert_eq!(default.bytes, provenant_records::run::MAX_WRITE_GRANT_BYTES);
    assert_eq!(
        default.origin,
        provenant_records::diagnostic::SettingSource::ShippedDefault
    );
    std::fs::write(
        state.join("config.toml"),
        "schema_version=1\n[domains.run]\nwrite_grant_limit_bytes=1048576\n",
    )?;
    let configured = registry.write_grant_limit();
    assert_eq!(configured.bytes, 1048576);
    assert_eq!(
        configured.origin,
        provenant_records::diagnostic::SettingSource::Configuration
    );
    Ok(())
}
