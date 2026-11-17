#[test]
fn configured_grants_cannot_expose_host_indexes() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::WriteGrant;
    for path in [
        "/state/run.sqlite3",
        "/state/run.sqlite3-wal",
        "/state/run.sqlite3-shm",
        "/state/run.sqlite3-journal",
        "/state/run.sqlite3.lock",
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
fn registered_project_inventory_keeps_unsafe_directory_diagnostic_typed()
-> Result<(), Box<dyn std::error::Error>> {
    // Spec: docs/specs/fabric.md#projects-never-merge-by-guess
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    let project = base.join("project");
    let checkout = base.join("checkout");
    for path in [&home, &state, &project, &checkout] {
        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    provenant_os::fixture::run_git(&project, &["init", "--quiet"])?;
    let roots = provenant_core::RootInputs {
        state_dir: Some(state.clone()),
        ..provenant_core::RootInputs::new(home, checkout)
    }
    .resolve(&provenant_os::fs::SystemPathResolver)?;
    let binding = provenant_config::bind_project(
        &roots,
        provenant_config::ProjectLocation::new(project.join(".git"))?,
    )?;
    let projects = state.join("projects");
    std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o755))?;

    // Exercise the real registration inventory so both model and named-command
    // callers receive the same typed refusal from their shared lookup owner.
    let error = registered_project_binding(&roots, binding.project_id())
        .expect_err("unsafe inventory must refuse preparation");
    let refusal = error.into_refusal();
    assert_eq!(
        refusal.terminal_cause,
        provenant_records::run::TerminalCause::ProjectBindingDirectoryUnsafe
    );
    assert_eq!(
        refusal.boundary_class,
        provenant_records::diagnostic::BoundaryClass::Authority
    );
    assert!(
        refusal
            .message
            .contains(&projects.to_string_lossy().to_string())
    );
    assert!(
        refusal
            .terminal_note
            .as_deref()
            .is_some_and(|note| note.contains("chmod 700"))
    );
    assert!(refusal.skipped_routes.iter().all(|route| {
        !route
            .detail
            .contains(&projects.to_string_lossy().to_string())
    }));

    // Other inventory errors keep the pre-existing generic wording and are
    // not promoted to an authority refusal.
    let unrelated = PrepareError::registered_project_inventory(
        provenant_config::ConfigError::InvalidProjectBinding,
    );
    assert_eq!(
        unrelated.to_string(),
        "cannot read registered project inventory: project binding state is invalid"
    );
    assert!(matches!(unrelated, PrepareError::Other(_)));
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
    let scratch = state.join("attempts/attempt/tmp/candidate-0");
    let private_state = state.join("attempts/attempt/provider-state/candidate-0");
    let view = state.join("attempts/attempt/tmp/source-snapshot");
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
                    credential_roots: &provenant_core::CredentialRoots::default(),
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
                InventoryContext::Sandboxed,
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
            let writable = handoff_writable(permissions);
            let brief = run::lane_preamble(
                "policy",
                &Default::default(),
                None,
                writable.then_some(handoff.as_path()),
            );
            assert!(brief.contains("Progress handoff JSON"));
            assert!(rendered.contains(&format!(
                "(allow file-write* (subpath \"{}\"))",
                private_state.display()
            )));
            let state_deny = rendered
                .lines()
                .find(|line| {
                    line.starts_with("(deny file-write* (require-all")
                        && line.contains(state.to_str().unwrap())
                })
                .ok_or("state write denial")?;
            assert!(state_deny.contains(&format!(
                "(require-not (subpath \"{}\"))",
                private_state.display()
            )));
            let mut absent = permissions.clone();
            absent.private_state = None;
            assert!(!handoff_writable(&absent));
            assert!(
                !run::lane_preamble(
                    "policy",
                    &Default::default(),
                    None,
                    handoff_writable(&absent).then_some(handoff.as_path())
                )
                .contains("Progress handoff JSON")
            );

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

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn registered_credential_roots_protect_lane_tokens_for_dedicated_and_shared_roots()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CredentialScope;
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let dedicated = base.join("dedicated");
    let shared = base.join("shared");
    for dir in [&home, &dedicated, &shared] {
        std::fs::create_dir(dir)?;
    }
    let settings: provenant_providers::accounts::Settings = serde_json::from_value(
        serde_json::json!({"entries": {
            "dedicated": {"adapter":"claude", "label":"dedicated", "key":"dedicated", "directory":dedicated},
            "shared": {"adapter":"claude", "label":"shared", "key":"shared", "directory":shared},
            "home": {"adapter":"claude", "label":"home", "key":"home", "directory":home}
        }}),
    )?;
    let lane = shared.join("lane");
    let alias_target = base.join("unselected-endpoint");
    std::fs::write(&alias_target, b"synthetic credential")?;
    std::os::unix::fs::symlink(&alias_target, dedicated.join(provenant_providers::claude::LANE_TOKEN_FILE))?;
    let roots = credential_roots(&Registry::default(), &home, &settings, &[&lane])?;
    assert!(roots.entries().iter().any(|root| root.path == alias_target && root.scope == CredentialScope::Exact && root.source.contains("claude/dedicated")));
    for dir in [&dedicated, &shared, &home] {
        for name in [provenant_providers::claude::LANE_TOKEN_FILE, "undeclared.txt"] {
            assert!(roots.refuses(&dir.join(name), CredentialScope::Exact, true));
        }
        assert!(roots.entries().iter().any(|root| root.path == *dir && root.scope == CredentialScope::Subtree));
        assert!(roots.entries().iter().any(|root| root.path == dir.join(provenant_providers::claude::LANE_TOKEN_FILE) && root.scope == CredentialScope::Exact && root.source.contains("claude/")));
    }
    assert!(!roots.refuses(&lane.join("ordinary.txt"), CredentialScope::Exact, true));
    assert!(roots.refuses(&shared, CredentialScope::Subtree, true));
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn unresolvable_registered_store_names_its_account() -> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let account = base.join("account");
    std::fs::create_dir(&home)?;
    std::fs::create_dir(&account)?;
    std::os::unix::fs::symlink(base.join("missing"), account.join("provenant-lane-token"))?;
    let settings: provenant_providers::accounts::Settings = serde_json::from_value(
        serde_json::json!({"entries": {"work": {"adapter":"claude", "label":"work", "key":"work", "directory":account}}}),
    )?;
    let error = credential_roots(&Registry::default(), &home, &settings, &[])
        .expect_err("dangling store must refuse");
    assert!(error.contains("claude/work"), "{error}");
    assert!(error.contains("provenant-lane-token"), "{error}");
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn host_declared_codex_home_equal_to_home_protects_store_and_admits_project_below_home()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CredentialScope;
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let project = home.join("project");
    let state = base.join("state");
    for dir in [&home, &project, &state] {
        std::fs::create_dir_all(dir)?;
    }
    let host = run::HostLaunchContext {
        credential_roots: vec![provenant_core::CredentialDeclaration {
            inventory: provenant_core::CredentialInventory::Adapter("codex".into()),
            root: provenant_core::CredentialRoot {
                path: home.clone(),
                scope: CredentialScope::Subtree,
                source: "provider environment: CODEX_HOME".into(),
                kind: provenant_core::CredentialKind::Store,
            },
        }],
        ..run::HostLaunchContext::from_environment(&state, &project, PathBuf::from("git"))
    };
    let registry = Registry {
        home: Some(&home),
        cwd: Some(&project),
        state_root: Some(&state),
        host: Some(&host),
        ..Registry::default()
    };
    let settings = provenant_providers::accounts::Settings::default();
    let roots = credential_roots(&registry, &home, &settings, &[&project])?;
    assert!(roots.entries().iter().any(|r| r.path == home && r.scope == CredentialScope::Subtree));
    for name in ["auth.json", "config.toml", "unlisted-secret"] {
        assert!(roots.refuses(&home.join(name), CredentialScope::Exact, true));
    }
    assert!(!roots.refuses(&project.join("ordinary.txt"), CredentialScope::Exact, true));
    let admission = crate::run::admit_source_root(
        &project,
        &state,
        &project,
        &roots,
        crate::run::SourceAdmissionMode::Live,
    );
    assert!(
        admission.is_ok(),
        "project below home must be admitted, got: {admission:?}"
    );
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn dangling_account_directory_that_is_also_the_source_root_refuses_naming_account()
-> Result<(), Box<dyn std::error::Error>> {
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let account = base.join("dangling-account");
    std::fs::create_dir(&home)?;
    std::os::unix::fs::symlink(base.join("nonexistent"), &account)?;
    let settings: provenant_providers::accounts::Settings = serde_json::from_value(
        serde_json::json!({"entries": {"work": {"adapter":"claude", "label":"work", "key":"work", "directory":account}}}),
    )?;
    let error = credential_roots(&Registry::default(), &home, &settings, &[&account])
        .expect_err("dangling account directory that is also source root must refuse");
    assert!(error.contains("claude/work"), "{error}");
    assert!(error.contains("dangling-account"), "{error}");
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn relocated_codex_stores_never_gain_the_builtin_package_exception()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CredentialScope;
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let project = base.join("project");
    let state = base.join("state");
    for dir in [&home, &project, &state] {
        std::fs::create_dir_all(dir)?;
    }
    // Exercise each origin independently: default-directory override, host
    // discovery and registered account. All must retain whole-store protection.
    for origin in ["default", "host", "account"] {
        let store = base.join(origin);
        let mut host = run::HostLaunchContext::from_environment(&state, &project, "git".into());
        host.credential_roots.clear();
        host.default_account_directories.clear();
        host.provider_path_environment.clear();
        let mut settings = provenant_providers::accounts::Settings::default();
        match origin {
            "default" => {
                host.default_account_directories
                    .insert("codex".into(), store.clone());
            }
            "host" => {
                host.credential_roots =
                    provenant_confinement::permissions::host_credential_declarations(|name| {
                        (name == "CODEX_HOME").then(|| store.clone().into_os_string())
                    });
            }
            _ => {
                settings = serde_json::from_value(
                    serde_json::json!({"entries": {"work": {"adapter":"codex", "label":"work", "key":"work", "directory":store}}}),
                )?;
            }
        }
        let registry = Registry {
            home: Some(&home),
            host: Some(&host),
            ..Registry::default()
        };
        let roots = credential_roots(&registry, &home, &settings, &[&project])?;
        let package = store.join("packages/standalone/releases/1.0/bin/codex");
        assert!(
            roots.refuses(&package, CredentialScope::Exact, true),
            "{origin}"
        );
        assert!(!roots.is_codex_standalone_package(&package), "{origin}");
        assert!(roots.is_codex_standalone_package(
            &home.join(".codex/packages/standalone/releases/1.0/bin/codex")
        ));
        let nested = store.join("project");
        std::fs::create_dir_all(&nested)?;
        let shared = credential_roots(&registry, &home, &settings, &[&nested])?;
        assert!(shared.refuses(&store.join("unlisted-secret"), CredentialScope::Exact, false), "{origin}");
        assert!(!shared.refuses(&package, CredentialScope::Exact, true), "{origin}");
        assert!(!shared.refuses(&nested.join("ordinary"), CredentialScope::Exact, true), "{origin}");
        assert!(crate::run::admit_source_root(&project, &state, &nested, &shared, crate::run::SourceAdmissionMode::Live).is_ok(), "{origin}");

    }
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn alternate_sources_inside_builtin_stores_keep_only_actual_launch_carveouts()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CredentialScope;
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let state = base.join("state");
    for (store_name, credential) in [
        (".codex", "auth.json"),
        (".claude", provenant_providers::claude::LANE_TOKEN_FILE),
    ] {
        let store = home.join(store_name);
        let project = store.join("project");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&state)?;
        let mut host = run::HostLaunchContext::from_environment(&state, &project, "git".into());
        host.credential_roots.clear();
        host.default_account_directories.clear();
        host.provider_path_environment.clear();
        let registry = Registry {
            home: Some(&home),
            host: Some(&host),
            ..Registry::default()
        };
        let roots = credential_roots(&registry, &home, &Default::default(), &[&project])?;
        assert!(roots.entries().iter().any(|root| root.path == store && root.scope == CredentialScope::Subtree));
        assert!(roots.refuses(&store.join("unlisted-secret"), CredentialScope::Exact, false));
        // The initial project's table must never be unioned back into this
        // launch's mandatory roots, even when the alternate source is .claude.
        let mut inputs = provenant_core::RootInputs::new(home.clone(), base.join("initial-project"));
        inputs.state_dir = Some(state.clone());
        let host_roots = inputs.resolve(&provenant_os::fs::SystemPathResolver)?;
        let ordinary = project.join("ordinary-file");
        std::fs::write(&ordinary, b"ordinary lane data")?;
        std::os::unix::fs::symlink(&ordinary, store.join("ordinary-link"))?;
        for sandboxed in [false, true] {
            let mandatory = provenant_confinement::permissions::MandatoryRoots::for_host_with_scratch_mode(
                &host_roots, &home, &project, &base.join("scratch"), &roots, &[], &[], sandboxed,
            )?;
            assert!(!mandatory.credential_roots().refuses(&project, CredentialScope::Subtree, true));
            assert!(!mandatory.credential_roots().refuses(&ordinary, CredentialScope::Exact, true));
        }
        assert!(roots.refuses(&store.join(credential), CredentialScope::Exact, true));
        assert!(!roots.refuses(&project, CredentialScope::Subtree, true));
        assert!(
            crate::run::admit_source_root(
                &project,
                &state,
                &project,
                &roots,
                crate::run::SourceAdmissionMode::Live
            )
            .is_ok()
        );
    }
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn shared_host_stores_retain_subtrees_even_without_file_inventories()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::CredentialScope;
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("gcloud");
    let project = home.join("project");
    let state = base.join("state");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&state)?;
    let mut host = run::HostLaunchContext::from_environment(&state, &project, "git".into());
    host.default_account_directories.clear();
    host.provider_path_environment.clear();
    host.credential_roots =
        provenant_confinement::permissions::host_credential_declarations(|name| {
            (name == "GH_CONFIG_DIR").then(|| home.clone().into_os_string())
        });
    // The declared inventory survives the host/guardian custody pipe.
    let host: run::HostLaunchContext = serde_json::from_slice(&serde_json::to_vec(&host)?)?;
    let registry = Registry {
        home: Some(&home),
        host: Some(&host),
        ..Registry::default()
    };
    let roots = credential_roots(&registry, &home, &Default::default(), &[&project])?;
    for name in ["hosts.yml", "config.yml"] {
        assert!(roots.refuses(&home.join(name), CredentialScope::Exact, true));
    }
    assert!(roots.entries().iter().any(|root| root.path == home && root.scope == CredentialScope::Subtree));
    assert!(roots.refuses(&home.join("unlisted-secret"), CredentialScope::Exact, false));
    assert!(
        crate::run::admit_source_root(
            &project,
            &state,
            &project,
            &roots,
            crate::run::SourceAdmissionMode::Live
        )
        .is_ok()
    );
    let mut unknown_host = host.clone();
    unknown_host.credential_roots =
        provenant_confinement::permissions::host_credential_declarations(|name| {
            (name == "XDG_CONFIG_HOME").then(|| base.clone().into_os_string())
        });
    let registry = Registry {
        host: Some(&unknown_host),
        ..registry
    };
    let roots = credential_roots(&registry, &home, &Default::default(), &[&project])?;
    assert!(roots.refuses(&home.join("unlisted-secret"), CredentialScope::Exact, false));
    assert!(!roots.refuses(&project, CredentialScope::Subtree, true));
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[test]
fn shared_store_refuses_declared_files_in_lane_roots_and_names_the_origin()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CredentialDeclaration, CredentialInventory, CredentialKind, CredentialRoot, CredentialScope};
    let temp = tempfile::tempdir()?;
    let base = temp.path().canonicalize()?;
    let home = base.join("home");
    let store = base.join("store");
    let project = store.join("project");
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&project)?;
    let mut host = run::HostLaunchContext::from_environment(&base.join("state"), &project, "git".into());
    host.default_account_directories.clear();
    host.provider_path_environment.clear();
    for scope in [CredentialScope::Subtree, CredentialScope::Exact] {
        let credential = project.join("declared-token");
        host.credential_roots = vec![CredentialDeclaration {
            root: CredentialRoot {
                path: if scope == CredentialScope::Subtree { store.clone() } else { credential.clone() },
                scope,
                kind: CredentialKind::Store,
                source: "host fixture inventory".into(),
            },
            inventory: CredentialInventory::Files(vec!["project/declared-token".into()]),
        }];
        let registry = Registry { host: Some(&host), ..Registry::default() };
        let error = credential_roots(&registry, &home, &Default::default(), &[&project]).unwrap_err();
        assert!(error.contains("host fixture inventory") && error.contains(credential.to_str().ok_or("path")?), "{error}");
    }
    // Both link directions must refuse for exact declarations and inventories:
    // a public path inside the lane cannot escape by pointing outside it.
    for inside_to_outside in [true, false] {
        let (credential, target, name) = if inside_to_outside {
            (project.join("linked-token"), store.join("outside-target"), "project/linked-token")
        } else {
            (store.join("linked-token"), project.join("inside-target"), "linked-token")
        };
        std::fs::write(&target, b"synthetic")?;
        std::os::unix::fs::symlink(&target, &credential)?;
        for scope in [CredentialScope::Subtree, CredentialScope::Exact] {
            host.credential_roots = vec![CredentialDeclaration {
                root: CredentialRoot {
                    path: if scope == CredentialScope::Subtree { store.clone() } else { credential.clone() },
                    scope,
                    kind: CredentialKind::Store,
                    source: "host symlink inventory".into(),
                },
                inventory: CredentialInventory::Files(vec![name.into()]),
            }];
            let registry = Registry { host: Some(&host), ..Registry::default() };
            let error = credential_roots(&registry, &home, &Default::default(), &[&project]).unwrap_err();
            assert!(error.contains("host symlink inventory") && error.contains(credential.to_str().ok_or("path")?), "{error}");
        }
        std::fs::remove_file(credential)?;
    }
    // Adapter-declared files and Provenant-owned per-account files must also
    // refuse after alias resolution, even when absent from the filesystem.
    for name in [".credentials.json", provenant_providers::claude::LANE_TOKEN_FILE] {
        let token = store.join(name);
        std::os::unix::fs::symlink(project.join("token-target"), &token)?;
        // Existing target keeps this a valid alias rather than a dangling link.
        std::fs::write(project.join("token-target"), b"synthetic")?;
        let settings = serde_json::from_value(serde_json::json!({"entries": {"work": {
            "adapter":"claude", "label":"work", "key":"work", "directory":store
        }}}))?;
        host.credential_roots.clear();
        let registry = Registry { host: Some(&host), ..Registry::default() };
        let error = credential_roots(&registry, &home, &settings, &[&project]).unwrap_err();
        assert!(error.contains("claude/work") && error.contains(name), "{error}");
        std::fs::remove_file(token)?;
    }
    // A store cannot be the lane root, nor lie within it.
    for lane in [&store, &base] {
        host.credential_roots = vec![CredentialDeclaration {
            root: CredentialRoot {path: store.clone(), scope: CredentialScope::Subtree, source:"host store".into(), kind:CredentialKind::Store},
            inventory: CredentialInventory::Unknown,
        }];
        let registry = Registry { host: Some(&host), ..Registry::default() };
        assert!(credential_roots(&registry, &home, &Default::default(), &[lane]).is_err());
    }
    Ok(())
}
