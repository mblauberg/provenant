#[test]
fn configured_grants_cannot_expose_host_indexes() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::WriteGrant;
    for path in [
        "/state/work-index.sqlite3",
        "/state/work-index.sqlite3-wal",
        "/state/work-index.sqlite3-shm",
        "/state/work-index.sqlite3-journal",
        "/state/work-index.sqlite3.lock",
        "/state/fabric",
        "/state/fabric/fabric.sqlite3",
        "/state/fabric/fabric.sqlite3-wal",
        "/state/fabric/fabric.sqlite3-shm",
        "/state/fabric/fabric.sqlite3-journal",
        "/state/fabric/host.sqlite3",
        "/state/fabric/host.sqlite3-wal",
        "/state/fabric/host.sqlite3-shm",
        "/state/fabric/host.sqlite3-journal",
        "/state/fabric/host.sqlite3.lock",
    ] {
        let grant = WriteGrant {
            source: run::WriteSource::WriteRoot,
            path: path.into(),
            scope: PathScope::Subtree,
        };
        assert!(!host_indexes_protected(Path::new("/state"), &[grant]));
    }
    assert!(host_indexes_protected(Path::new("/state"), &[]));
    let temp = tempfile::tempdir()?;
    let fabric = temp.path().join("fabric");
    std::fs::create_dir(&fabric)?;
    let grant = WriteGrant {
        source: run::WriteSource::StateWrite,
        path: fabric,
        scope: PathScope::Exact,
    };
    assert!(!host_indexes_protected(temp.path(), &[grant]));
    for path in [
        "/state",
        "/state/cargo-target-seeds",
        "/state/cargo-target-seeds/generations/a/target",
    ] {
        let grant = WriteGrant {
            source: run::WriteSource::WriteRoot,
            path: path.into(),
            scope: PathScope::Subtree,
        };
        assert!(!target_seeds_protected(Path::new("/state"), &[grant]));
    }
    assert!(target_seeds_protected(Path::new("/state"), &[]));
    Ok(())
}

#[test]
fn commit_source_lanes_can_read_their_view_without_changing_adapter_writes()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::{AuthenticationProfile, ProviderProfile};

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let source = base.join("repo");
    let assets = base.join("assets");
    let scratch = state.join("runs/run/attempts/attempt/tmp/candidate-0");
    let private_state = state.join("runs/run/attempts/attempt/provider-state/candidate-0");
    let view = state.join("runs/run/attempts/attempt/tmp/source-snapshot");
    for path in [
        &home,
        &state,
        &source,
        &assets,
        &scratch,
        &private_state,
        &view,
    ] {
        provenant_os::fixture::private_state_dir(path)?;
    }
    let mut inputs = provenant_core::RootInputs::new(home.clone(), source.clone());
    inputs.state_dir = Some(state.clone());
    let roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
    let grant = |path: &Path| PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    };
    for adapter in ["codex", "claude", "cursor", "agy", "opencode", "command"] {
        let mut task_writes = None;
        for kind in ["task", "live", "commit"] {
            let mut access = run::LaneAccess::default();
            if kind != "task" {
                access.source_root = Some(source.clone());
                access.source_live = kind == "live";
            }
            let cwd = if kind == "commit" { &view } else { &source };
            let request = confinement_request(
                ProjectId::from_bytes([1; 16]),
                ConfinementRoots {
                    host_roots: Some(&roots),
                    cwd,
                    home: &home,
                    state: &state,
                    credential_roots: &[],
                    github_roots: &[],
                    protected_worktrees: &[],
                    scratch: &scratch,
                    private_state: &private_state,
                    assets: &assets,
                    prompt_file: None,
                },
                None,
                &access,
                &Default::default(),
                adapter,
                false,
            )?;
            let plan = ConfinedPlan::new(request)?;
            let permissions = plan.permissions();
            let mandatory = permissions.policy.as_ref().ok_or("policy")?.mandatory();
            let expected = if kind == "commit" {
                vec![grant(&view)]
            } else {
                vec![]
            };
            assert_eq!(mandatory.state_reads(), expected, "{adapter}/{kind}");
            assert_eq!(mandatory.state_exec(), expected, "{adapter}/{kind}");
            assert_eq!(permissions.private_state, Some(grant(&private_state)));
            assert_eq!(permissions.scratch, grant(&scratch));

            let mut profile = ProviderProfile {
                spec: Default::default(),
                observations: Default::default(),
            };
            if let Some(mechanism) = &permissions.selected_authentication {
                profile.spec.authentication.push(AuthenticationProfile {
                    mechanism: mechanism.clone(),
                    roots: vec![],
                    in_place_writes: vec![],
                    keychain: None,
                });
            }
            let writes = provenant_confinement::write_grants(permissions, &profile);
            if let Some(task) = &task_writes {
                assert_eq!(&writes, task, "{adapter}/{kind}");
            } else {
                task_writes = Some(writes);
            }
            let rendered =
                provenant_confinement::permissions::snapshot::render(permissions, &profile, &home)?;
            let handoff = private_state.join("handoff.json");
            assert!(handoff_writable(permissions, &private_state));
            let brief = run::lane_preamble("policy", false, Some(&handoff));
            assert!(brief.contains(&handoff.display().to_string()));
            assert!(rendered.contains(&format!("(allow file-write* (subpath \"{}\"))", private_state.display())));
            let state_deny = rendered.lines().find(|line| line.starts_with("(deny file-write* (require-all") && line.contains(state.to_str().unwrap())).ok_or("state write denial")?;
            assert!(state_deny.contains(&format!("(require-not (subpath \"{}\"))", private_state.display())));
            let mut absent = permissions.clone();
            absent.private_state = None;
            assert!(!handoff_writable(&absent, &private_state));
            assert!(!run::lane_preamble("policy", false, None).contains("Progress handoff JSON"));

            if kind == "commit" {
                let state_read = rendered
                    .lines()
                    .find(|line| {
                        line.starts_with("(deny file-read* (require-all")
                            && line.contains(state.to_str().unwrap())
                    })
                    .ok_or("state read denial")?;
                assert!(
                    state_read.contains(&format!("(require-not (subpath \"{}\"))", view.display()))
                );
                assert!(rendered.contains(&format!(
                    "(deny file-write* (subpath \"{}\"))",
                    view.display()
                )));
                let exec_deny = rendered
                    .lines()
                    .find(|line| {
                        line.starts_with("(deny process-exec (require-all")
                            && line.contains(state.to_str().unwrap())
                    })
                    .ok_or("state exec denial")?;
                assert!(
                    exec_deny.contains(&format!("(require-not (subpath \"{}\"))", view.display()))
                );
                for line in rendered
                    .lines()
                    .filter(|line| line.starts_with("(deny file-write* (require-all"))
                {
                    assert!(!line.contains(view.to_str().unwrap()), "{line}");
                }
                assert!(rendered.contains(&format!(
                    "(deny file-read* (subpath \"{}/.ssh\"))",
                    home.display()
                )));
            }
        }
    }
    Ok(())
}
