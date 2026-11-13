#[test]
fn read_only_cargo_offline_tracks_network() -> Result<(), ProviderError> {
    use provenant_confinement::permissions::NetworkAccess;
    let prompt = Prompt::new("fixture".into());
    let repo = Path::new("/fixture/repo");
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    for network in [
        NetworkAccess::Provider,
        NetworkAccess::Local,
        NetworkAccess::Open,
    ] {
        let plan = fixture_network_plan(Access::ReadOnly, Some("login"), network);
        let spec =
            crate::codex::Codex.prepare(fixture_input(&route, &plan, &prompt, repo, None))?;
        assert_eq!(
            spec.env
                .iter()
                .any(|(key, value)| key == "CARGO_NET_OFFLINE" && value == "true"),
            network != NetworkAccess::Open,
            "{network:?}"
        );
    }
    Ok(())
}

#[test]
fn codex_read_only_toolchain_uses_private_build_and_cargo_home_linked_to_shared_stores() {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = read_only_plan("/fixture", Some("login"));
    let prompt = Prompt::new("fixture".into());
    let spec = crate::codex::Codex
        .prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            std::path::Path::new("/fixture/repo"),
            None,
        ))
        .unwrap();
    let env = |key: &str| spec.env.iter().find(|(k, _)| k == key).unwrap().1.clone();
    assert_eq!(env("CODEX_HOME"), "/fixture/state");
    assert_eq!(env("CARGO_TARGET_DIR"), "/fixture/scratch/target");
    assert_eq!(env("CARGO_BUILD_BUILD_DIR"), "/fixture/scratch/target");
    assert_eq!(env("RUSTUP_HOME"), "/fixture/home/.rustup");
    assert_eq!(env("RUSTUP_AUTO_INSTALL"), "0");
    let paths = |grants: &[provenant_confinement::permissions::PathGrant]| {
        grants
            .iter()
            .map(|g| g.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let reads = paths(&spec.profile.spec.runtime_reads);
    for dir in ["/fixture/home/.cargo", "/fixture/home/.rustup"] {
        assert!(reads.iter().any(|path| path == dir), "{dir}");
    }
    // Cargo downloads over TLS, so the system trust roots are readable.
    #[cfg(target_os = "macos")]
    assert!(reads.iter().any(|path| path == "/private/etc/ssl"));
    // Shared Cargo stores are never writable; Cargo downloads into its own
    // lane-private home. Installed binaries and toolchains stay read-only.
    assert!(spec.profile.spec.tool_writes.is_empty());
    assert_eq!(env("CARGO_HOME"), "/fixture/scratch/cargo-home");
    // The registry and Git checkouts are linked read-only, never granted.
    for name in ["registry", "git"] {
        assert!(
            spec.setup.iter().any(|step| matches!(
                step,
                crate::launch::SetupStep::Symlink { link, target }
                    if link == &std::path::Path::new("/fixture/scratch/cargo-home").join(name)
                        && target == &std::path::Path::new("/fixture/home/.cargo").join(name)
            )),
            "{name}"
        );
    }
    for dir in [
        "/fixture/home/.cargo/bin",
        "/fixture/home/.rustup/toolchains",
        // Scratch holds the target directory and rustdoc's doc-test binaries.
        "/fixture/scratch",
    ] {
        assert!(
            spec.profile
                .spec
                .tool_exec_roots
                .iter()
                .any(|g| g.path == std::path::Path::new(dir))
        );
    }
}

#[test]
fn codex_grants_host_user_skills_read_only_when_present() -> Result<(), Box<dyn std::error::Error>>
{
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let temp = tempfile::tempdir()?;
    let home = temp.path().canonicalize()?;
    let skills = home.join(".agents/skills");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let prepare = || {
            crate::codex::Codex.prepare(PrepareInput {
                home: &home,
                ..fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None)
            })
        };
        // Absent directory: no grant.
        let spec = prepare()?;
        assert!(
            !spec
                .profile
                .spec
                .runtime_reads
                .iter()
                .any(|grant| covers(grant, &skills)),
            "{access:?}"
        );
        std::fs::create_dir_all(&skills)?;
        let spec = prepare()?;
        let staged = spec.user_skills.root.as_ref().expect("staged skills");
        assert!(
            spec.profile
                .spec
                .runtime_reads
                .contains(&crate::launch::subtree(staged))
        );
        assert!(
            !spec
                .profile
                .spec
                .runtime_reads
                .iter()
                .any(|grant| covers(grant, &skills))
        );
        let writable = spec
            .profile
            .spec
            .tool_writes
            .iter()
            .chain(&spec.profile.spec.state_writes)
            .chain(
                spec.profile
                    .spec
                    .authentication
                    .iter()
                    .flat_map(|auth| &auth.in_place_writes),
            )
            .chain(std::iter::once(&plan.permissions().scratch))
            .chain(&plan.permissions().private_state);
        assert!(
            writable
                .into_iter()
                .all(|grant| !overlaps_skill(grant, staged))
        );
        std::fs::remove_dir(&skills)?;
    }
    Ok(())
}

fn covers(grant: &provenant_confinement::permissions::PathGrant, path: &std::path::Path) -> bool {
    use provenant_confinement::permissions::PathScope;
    match grant.scope {
        PathScope::Exact => grant.path == path,
        PathScope::Subtree => path.starts_with(&grant.path),
    }
}

// A write overlaps a skill path if it covers the path or starts inside it.
fn overlaps_skill(grant: &provenant_confinement::permissions::PathGrant, path: &Path) -> bool {
    covers(grant, path) || grant.path.starts_with(path)
}

#[test]
fn skill_write_overlap_detects_ancestors_and_descendants() {
    let skill = Path::new("/fixture/skills/example");
    for grant in [
        crate::launch::subtree("/fixture/skills"),
        crate::launch::subtree(skill),
        crate::launch::subtree(skill.join("nested")),
        crate::launch::exact(skill.join("SKILL.md")),
    ] {
        assert!(overlaps_skill(&grant, skill), "{grant:?}");
    }
    assert!(!overlaps_skill(
        &crate::launch::exact("/fixture/skills"),
        skill
    ));
    assert!(!overlaps_skill(
        &crate::launch::subtree("/fixture/other"),
        skill
    ));
}

/// Probe the startup scan using the launch's HOME and rendered sandbox.
#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::disallowed_methods)]
fn codex_opt_out_skill_scan_succeeds_under_confinement() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::snapshot::render;
    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    let host = root.join("home");
    let state = root.join("state");
    std::fs::create_dir_all(host.join(".agents/skills/host-skill"))?;
    std::fs::write(
        host.join(".agents/skills/host-skill/SKILL.md"),
        "host skill",
    )?;
    std::fs::create_dir_all(state.join(".agents/skills/private-skill"))?;
    std::fs::write(
        state.join(".agents/skills/private-skill/SKILL.md"),
        "fixture skill",
    )?;
    let plan = read_only_plan(root.to_str().unwrap(), Some("login"));
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    route.set_user_skills(false);
    let prompt = Prompt::new("fixture".into());
    let repo = root.join("repo");
    let mut input = fixture_input(&route, &plan, &prompt, &repo, None);
    input.home = &host;
    let launch = crate::codex::Codex.prepare(input)?;
    let profile = render(plan.permissions(), &launch.profile, &host)?;
    let home = &launch.env.iter().find(|(key, _)| key == "HOME").unwrap().1;
    let scan = |home: &std::ffi::OsStr| {
        std::process::Command::new("/usr/bin/sandbox-exec")
            .args(["-p", &profile, "/bin/sh", "-c", "/bin/ls -1 \"$HOME/.agents/skills\" && /bin/cat \"$HOME/.agents/skills\"/*/SKILL.md"])
            .env_clear()
            .env("HOME", home)
            .output()
    };
    let output = scan(home)?;
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    if diagnostic.contains("sandbox_apply: Operation not permitted") {
        provenant_test_support::skip_sandbox_dependent_test(format!(
            "skill scan unavailable under inherited sandbox: {diagnostic}"
        ))?;
        return Ok(());
    }
    assert!(output.status.success(), "private HOME scan: {diagnostic}");
    assert!(output.stdout.ends_with(b"fixture skill"));
    Ok(())
}

#[test]
fn cargo_build_dir_is_pinned_to_lane_target_for_each_access() -> Result<(), ProviderError> {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    for (access, target) in [
        (Access::ReadOnly, "/fixture/scratch/target"),
        (
            Access::WorktreeWrite,
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
        ),
    ] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let launch = crate::codex::Codex.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))?;
        for key in ["CARGO_TARGET_DIR", "CARGO_BUILD_BUILD_DIR"] {
            assert!(
                launch.env.iter().any(|(k, v)| k == key && v == target),
                "{access:?}: {key} must be {target}"
            );
        }
    }
    Ok(())
}

#[test]
fn writer_lint_target_is_a_lane_cache_sibling_of_the_build_target() -> Result<(), ProviderError> {
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture".into());
    let cache = "/fixture/home/Library/Caches/provenant-lane-cargo/fixture";
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let launch = crate::codex::Codex.prepare(fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    // Lint-only cargo runs take their own build directory, so they do not
    // queue on the lock that test and build runs hold.
    let lint = format!("{cache}/target-lint");
    assert!(
        launch
            .env
            .iter()
            .any(|(k, v)| k == "PROVENANT_LANE_CARGO_LINT_TARGET" && v == lint.as_str()),
        "PROVENANT_LANE_CARGO_LINT_TARGET must be {lint}"
    );
    assert!(
        Path::new(&lint).starts_with(cache),
        "the lint target must sit inside the granted lane cache"
    );
    let read_only = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let launch = crate::codex::Codex.prepare(fixture_input(
        &route,
        &read_only,
        &prompt,
        Path::new("/fixture/repo"),
        None,
    ))?;
    assert!(
        launch
            .env
            .iter()
            .all(|(k, _)| k != "PROVENANT_LANE_CARGO_LINT_TARGET"),
        "read-only lanes have no lane cache to hold a lint target"
    );
    Ok(())
}

#[test]
fn no_lane_can_write_shared_toolchain_stores() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let shared = [
        ".cargo/registry",
        ".cargo/registry/src/index/crate-1.0/build.rs",
        ".cargo/git",
        ".cargo/git/checkouts/repo/build.rs",
        ".cargo/.package-cache",
        ".cargo/.global-cache",
        "Library/pnpm",
        "Library/pnpm/pnpm",
        "Library/Caches/pnpm",
        ".npm",
        ".npm/_cacache/content",
        ".cache/node",
        ".cache/node/corepack",
    ];
    for entry in REGISTRY.iter().filter(|entry| {
        provenant_records::AdapterSpec::get(entry.adapter.id())
            .is_some_and(|spec| spec.launch.model)
    }) {
        let id = entry.adapter.id();
        // Claude shares the same launch helpers; its account setup needs
        // identities, so its writer lane is covered by its own test.
        if id != "codex" {
            continue;
        }
        let mut route = route(id, "fixture-model");
        route.account_key = None;
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            let plan = fixture_access_plan(access, (id == "codex").then_some("login"), false);
            let launch = entry.adapter.prepare(fixture_input(
                &route,
                &plan,
                &prompt,
                std::path::Path::new("/fixture/repo"),
                None,
            ))?;
            let spec = &launch.profile.spec;
            let writable: Vec<_> = spec
                .tool_writes
                .iter()
                .chain(&spec.state_writes)
                .chain(
                    spec.authentication
                        .iter()
                        .flat_map(|auth| &auth.in_place_writes),
                )
                .collect();
            for relative in shared {
                let path = std::path::Path::new("/fixture/home").join(relative);
                assert!(
                    !writable.iter().any(|grant| covers(grant, &path)),
                    "{id} {access:?}: {}",
                    path.display()
                );
            }
            let env = |key: &str| {
                launch
                    .env
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| std::path::PathBuf::from(v))
            };
            let cache = match access {
                Access::ReadOnly => std::path::PathBuf::from("/fixture/scratch"),
                _ => "/fixture/home/Library/Caches/provenant-lane-cargo/fixture".into(),
            };
            assert_eq!(env("CARGO_HOME"), Some(cache.join("cargo-home")));
            // Node variables appear only where a Homebrew Node exists. The
            // store stays at the host setting, so a native install's recorded
            // configuration matches; only the caches move into the lane.
            for (key, leaf) in [
                ("npm_config_cache", "npm"),
                ("npm_config_cache_dir", "pnpm-cache"),
                ("pnpm_config_cache_dir", "pnpm-cache"),
            ] {
                if let Some(value) = env(key) {
                    assert_eq!(value, cache.join(leaf), "{id} {access:?} {key}");
                }
            }
            for key in ["PNPM_HOME", "npm_config_store_dir", "pnpm_config_store_dir"] {
                assert!(
                    launch.env.iter().all(|(k, _)| k != key),
                    "{id} {access:?} {key}"
                );
            }
        }
    }
    Ok(())
}

/// pnpm switches to the version a project pins by downloading `@pnpm/exe`, whose
/// launcher is a placeholder until its install step hard-links the native
/// binary over it. Lanes cannot create hard links, so the placeholder stays and
/// running it fails with ENOEXEC. Lanes run the host's pnpm as it is.
#[test]
fn lanes_run_the_installed_pnpm_instead_of_downloading_a_pinned_one() -> Result<(), ProviderError> {
    if !["/opt/homebrew", "/usr/local"]
        .iter()
        .any(|prefix| Path::new(prefix).join("bin/node").exists())
    {
        return Ok(());
    }
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture prompt".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let launch = crate::codex::Codex.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))?;
        for key in [
            "npm_config_manage_package_manager_versions",
            "pnpm_config_manage_package_manager_versions",
            "npm_config_package_manager_strict",
            "pnpm_config_package_manager_strict",
        ] {
            assert!(
                launch.env.iter().any(|(k, v)| k == key && v == "false"),
                "{access:?} {key}"
            );
        }
        // A Corepack shim must not follow a pinned `packageManager` either.
        for key in ["COREPACK_ENABLE_PROJECT_SPEC", "COREPACK_ENABLE_DOWNLOAD"] {
            assert!(
                launch.env.iter().any(|(k, v)| k == key && v == "0"),
                "{access:?} {key}"
            );
        }
    }
    Ok(())
}

/// pnpm records the store directory in `node_modules/.modules.yaml`. A lane
/// override made a worktree the chair had installed natively look stale, and
/// the lane then tried to reinstall into a store it cannot write. Lanes keep the
/// host's store setting and skip the pre-run dependency check, so `pnpm run`
/// uses the native install as it is.
#[test]
fn lanes_run_a_natively_installed_worktree_without_reinstalling() -> Result<(), ProviderError> {
    if !["/opt/homebrew", "/usr/local"]
        .iter()
        .any(|prefix| Path::new(prefix).join("bin/node").exists())
    {
        return Ok(());
    }
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture prompt".into());
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("login"), false);
        let launch = crate::codex::Codex.prepare(fixture_input(
            &route,
            &plan,
            &prompt,
            Path::new("/fixture/repo"),
            None,
        ))?;
        for key in ["PNPM_HOME", "npm_config_store_dir", "pnpm_config_store_dir"] {
            assert!(launch.env.iter().all(|(k, _)| k != key), "{access:?} {key}");
        }
        assert!(
            launch
                .env
                .iter()
                .any(|(k, v)| k == "pnpm_config_verify_deps_before_run" && v == "false"),
            "{access:?}"
        );
    }
    Ok(())
}

/// The host store stays readable so pnpm can resolve the recorded store, and
/// stays outside every write grant: the host later runs what it holds.
#[test]
fn lanes_read_the_host_pnpm_store_but_cannot_write_it() -> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::PathScope;
    if !["/opt/homebrew", "/usr/local"]
        .iter()
        .any(|prefix| Path::new(prefix).join("bin/node").exists())
    {
        return Ok(());
    }
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    let mut requirements = crate::launch::ToolRequirements::default();
    let mut env = Vec::new();
    for store in ["Library/pnpm", ".local/share/pnpm"] {
        std::fs::create_dir_all(home.join(store))?;
    }
    crate::launch::node_tools(
        &home,
        Access::ReadOnly,
        &temp.path().join("cache"),
        &mut requirements,
        &mut env,
    );
    for store in ["Library/pnpm", ".local/share/pnpm"] {
        assert!(
            requirements.reads.iter().any(|grant| {
                grant.path == home.join(store) && grant.scope == PathScope::Subtree
            }),
            "{store}"
        );
    }
    for key in ["PNPM_HOME", "npm_config_store_dir", "pnpm_config_store_dir"] {
        assert!(env.iter().all(|(name, _)| name != key), "{key}");
    }
    Ok(())
}

#[test]
fn provider_recipes_cannot_write_user_configuration() -> Result<(), ProviderError> {
    let cases: &[(&str, &[&str])] = &[
        (
            "claude",
            &[
                ".claude/settings.json",
                ".claude/hooks/run",
                ".claude/CLAUDE.md",
                ".claude/skills/test/SKILL.md",
                ".claude/ide/state",
                ".claude/jobs/job",
                ".claude/state/file",
                ".claude/teams/team",
            ],
        ),
        (
            "codex",
            &[
                ".codex/config.toml",
                ".codex/rules/default.rules",
                ".codex/skills/test/SKILL.md",
                ".codex/hooks/run",
                ".codex/extensions/test",
            ],
        ),
        (
            "opencode",
            &[
                ".config/opencode/opencode.json",
                ".config/opencode/plugins/run.js",
                ".config/opencode/skills/test/SKILL.md",
            ],
        ),
        (
            "agy",
            &[
                ".gemini/settings.json",
                ".gemini/config/mcp_config.json",
                ".gemini/config/plugins/run",
                ".gemini/antigravity-cli/sidecars/run",
                ".gemini/antigravity-cli/plugin_data/file",
                ".gemini/antigravity-cli/knowledge/file",
            ],
        ),
        (
            "cursor",
            &[
                ".cursor/hooks.json",
                ".cursor/extensions/run",
                ".cursor/skills/test/SKILL.md",
                ".cursor/plans/plan",
                ".cursor/ai-tracking/file",
            ],
        ),
    ];
    let prompt = Prompt::new("fixture prompt".into());
    for &(id, forbidden) in cases {
        let adapter = REGISTRY
            .iter()
            .find(|entry| entry.adapter.id() == id)
            .expect("provider")
            .adapter;
        let recording = match id {
            "claude" => fixture!("claude/direct-model.jsonl"),
            "codex" => fixture!("codex/success.jsonl"),
            "cursor" => fixture!("cursor/fresh.jsonl"),
            "agy" => fixture!("agy/success.jsonl"),
            "opencode" => fixture!("opencode/unversioned-success.jsonl"),
            _ => return Err(ProviderError::UnknownIdentity),
        };
        let outcome = replay(adapter, recording, EXIT)?.1;
        let route = route(id, outcome.model().unwrap_or("fixture-model"));
        // An isolated agy account holds its own keychain as a regular file.
        let agy_dir = tempfile::tempdir().unwrap();
        write_agy_keychain(agy_dir.path());
        let account_dir = if id == "agy" {
            agy_dir.path()
        } else {
            std::path::Path::new("/fixture/identities/opaque")
        };
        let account = matches!(id, "claude" | "codex" | "agy")
            .then(|| identity::RegisteredAccount::new(id, "opaque-account", account_dir.into()))
            .transpose()?;
        let account_token =
            (id == "claude").then(|| fixture_token().for_account("opaque-account", account_dir));
        // Synthetic continuation covers every adapter and mode.
        let continuation = Continuation::from_outcome(id, &outcome, &route, "fixture", false)?;
        for access in [Access::ReadOnly, Access::WorktreeWrite] {
            for session in [Session::Fresh, Session::Resume] {
                let plan = fixture_access_plan(
                    access,
                    match id {
                        "codex" | "cursor" | "opencode" => Some("login"),
                        "agy" => Some("keychain"),
                        _ => None,
                    },
                    session == Session::Resume,
                );
                let mut input = fixture_input(
                    &route,
                    &plan,
                    &prompt,
                    std::path::Path::new("/fixture/repo"),
                    account_token.as_ref(),
                );
                input.account = account.as_ref();
                input.continuation = (session == Session::Resume).then_some(&continuation);
                let launch = adapter.prepare(input)?;
                for relative in forbidden
                    .iter()
                    .copied()
                    .chain([".npm/_cacache/content", ".npm/_logs/log"])
                {
                    let path = std::path::Path::new("/fixture/home").join(relative);
                    let writable = launch
                        .profile
                        .spec
                        .state_writes
                        .iter()
                        .chain(
                            launch
                                .profile
                                .spec
                                .authentication
                                .iter()
                                .flat_map(|auth| &auth.in_place_writes),
                        )
                        .chain(std::iter::once(&plan.permissions().scratch))
                        .chain(&plan.permissions().private_state)
                        .any(|grant| covers(grant, &path));
                    assert!(!writable, "{id} {access:?} {session:?}: {}", path.display());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn agy_runs_in_a_private_home_that_links_only_the_login_keychain() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "gemini-3.8-flash");
    let cwd = std::path::Path::new("/fixture/repo");
    for (access, edits) in [(Access::ReadOnly, false), (Access::WorktreeWrite, true)] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        // The user's own login is registered at the home directory itself.
        let account = legacy_agy_account();
        let mut input = fixture_input(&route, &plan, &prompt, cwd, None);
        input.account = Some(&account);
        let spec = agy::Agy.prepare(input)?;
        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            Path::new("/fixture/home"),
        )
        .unwrap();
        // Exercise prepare AND rendering: a declared executable was previously
        // cancelled by an unconditional denial later in the rendered profile.
        assert!(!profile.contains("(deny process-exec (literal \"/usr/bin/security\"))"));
        assert!(
            !profile.contains("(deny mach-lookup (global-name-prefix \"com.apple.securityd\")")
        );
        assert!(!profile.contains("(deny mach-lookup (global-name \"com.apple.securityd\")"));

        assert!(profile.contains(
            "(require-not (literal \"/fixture/home/Library/Keychains/login.keychain-db\"))"
        ));
        assert!(
            !profile.contains("(deny file-read* (subpath \"/fixture/home/Library/Keychains\"))")
        );
        assert!(
            profile.contains("(deny file-write* (subpath \"/fixture/home/Library/Keychains\"))")
        );
        let argv: Vec<_> = spec.argv.iter().map(|arg| arg.to_string_lossy()).collect();
        // Headless lanes auto-approve tools within the outer profile.
        assert!(
            argv.iter()
                .any(|arg| arg == "--dangerously-skip-permissions")
        );
        assert!(!argv.iter().any(|arg| arg == "--sandbox"));
        assert_eq!(spec.stdin, Stdin::Null);
        let home = spec
            .env
            .iter()
            .find(|(key, _)| key == "HOME")
            .map(|(_, value)| std::path::Path::new(value));
        assert_eq!(home, Some(std::path::Path::new("/fixture/state")));
        let keychain = std::path::Path::new("/fixture/home/Library/Keychains/login.keychain-db");
        assert!(spec.setup.contains(&SetupStep::Symlink {
            link: "/fixture/state/Library/Keychains/login.keychain-db".into(),
            target: keychain.into(),
        }));
        // Nothing of the user's Gemini state is granted, and the keychain is read-only.
        let spec = &spec.profile.spec;
        assert_eq!(spec.authentication.len(), 1);
        assert_eq!(
            spec.authentication[0].roots,
            [crate::launch::exact(keychain)]
        );
        assert!(spec.authentication[0].in_place_writes.is_empty());
        assert!(spec.state_writes.is_empty());
        // Writers read only the shared toolchains their commands run.
        assert_eq!(spec.runtime_reads.is_empty(), !edits);
        assert!(
            spec.runtime_reads
                .iter()
                .all(|grant| !grant.path.starts_with("/fixture/home/.gemini"))
        );
    }
    Ok(())
}

#[test]
fn agy_isolated_account_links_its_own_keychain_and_grants_only_that_path()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let directory = root.path().join("home/.agy-second");
    std::fs::create_dir_all(directory.join("Library/Keychains"))?;
    let keychain = directory.join("Library/Keychains/login.keychain-db");
    std::fs::write(&keychain, "")?;
    let account = identity::RegisteredAccount::new("agy", "opaque-account", directory)?;
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "gemini-3.8-flash");
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.account = Some(&account);
        let spec = agy::Agy.prepare(input)?;
        let profile = provenant_confinement::permissions::snapshot::render(
            plan.permissions(),
            &spec.profile,
            Path::new("/fixture/home"),
        )
        .unwrap();
        // Exercise prepare AND rendering: a declared executable was previously
        // cancelled by an unconditional denial later in the rendered profile.
        assert!(!profile.contains("(deny process-exec (literal \"/usr/bin/security\"))"));
        assert!(
            !profile.contains("(deny mach-lookup (global-name-prefix \"com.apple.securityd\")")
        );
        assert!(!profile.contains("(deny mach-lookup (global-name \"com.apple.securityd\")"));

        assert!(spec.env.contains(&("HOME".into(), "/fixture/state".into())));
        assert!(spec.setup.contains(&SetupStep::Symlink {
            link: "/fixture/state/Library/Keychains/login.keychain-db".into(),
            target: keychain.clone(),
        }));
        // The user's login keychain is neither linked nor granted.
        let login = Path::new("/fixture/home/Library/Keychains/login.keychain-db");
        assert!(!spec.setup.iter().any(|step| matches!(
            step,
            SetupStep::Symlink { target, .. } if target == login
        )));
        let auth = &spec.profile.spec.authentication;
        assert_eq!(auth.len(), 1);
        assert_eq!(auth[0].roots, [crate::launch::exact(&keychain)]);
        assert!(auth[0].in_place_writes.is_empty());
        // Another account's route is refused rather than launched.
        let mut other = route.clone();
        other.account_key = Some("another-account".into());
        let mut input = fixture_input(&other, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.account = Some(&account);
        assert!(matches!(
            agy::Agy.prepare(input),
            Err(ProviderError::IdentityMismatch)
        ));
    }
    // The keychain vanishes after registration and observation: the launch
    // fails closed, naming the path, and never links or grants the shared one.
    std::fs::remove_file(&keychain)?;
    for access in [Access::ReadOnly, Access::WorktreeWrite] {
        let plan = fixture_access_plan(access, Some("keychain"), false);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.account = Some(&account);
        match agy::Agy.prepare(input) {
            Err(ProviderError::IsolatedKeychainUnavailable(path)) => {
                assert_eq!(path, keychain.display().to_string());
            }
            Err(other) => panic!("unexpected error {other}"),
            Ok(spec) => panic!(
                "launched without the isolated keychain: {:?} {:?}",
                spec.setup, spec.profile.spec.authentication
            ),
        }
    }
    Ok(())
}

#[test]
fn agy_own_login_keeps_the_shared_keychain_and_an_isolated_directory_never_does()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let prompt = Prompt::new("fixture prompt".into());
    let route = route("agy", "gemini-3.8-flash");
    let plan = fixture_access_plan(Access::ReadOnly, Some("keychain"), false);
    let account = legacy_agy_account();
    let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    input.account = Some(&account);
    let spec = agy::Agy.prepare(input)?;
    let login = Path::new("/fixture/home/Library/Keychains/login.keychain-db");
    assert!(spec.setup.contains(&SetupStep::Symlink {
        link: "/fixture/state/Library/Keychains/login.keychain-db".into(),
        target: login.into(),
    }));
    assert_eq!(
        spec.profile.spec.authentication[0].roots,
        [crate::launch::exact(login)]
    );
    // A directory other than the home is isolated even with no keychain file,
    // or with a directory where the file belongs.
    let directory = root.path().join("home/.gemini");
    for prepare_dir in [false, true] {
        std::fs::create_dir_all(&directory)?;
        if prepare_dir {
            std::fs::create_dir_all(directory.join("Library/Keychains/login.keychain-db"))?;
        }
        let isolated = agy_account_at(&directory);
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        input.account = Some(&isolated);
        assert!(matches!(
            agy::Agy.prepare(input),
            Err(ProviderError::IsolatedKeychainUnavailable(_))
        ));
    }
    // An unregistered route cannot launch an adapter that pins its account.
    let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    assert!(matches!(
        agy::Agy.prepare(input),
        Err(ProviderError::UnknownIdentity)
    ));
    Ok(())
}

const KEYCHAIN_FILE: &str = "Library/Keychains/login.keychain-db";

#[test]
fn agy_keychain_symlink_is_not_an_isolated_account_keychain()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let directory = home.join(".agy-second");
    std::fs::create_dir_all(directory.join("Library/Keychains"))?;
    let elsewhere = root.path().join("elsewhere.keychain-db");
    std::fs::write(&elsewhere, "")?;
    let keychain = directory.join("Library/Keychains/login.keychain-db");
    std::os::unix::fs::symlink(&elsewhere, &keychain)?;
    assert!(matches!(
        agy::account_keychain(&directory, &home),
        Err(ProviderError::IsolatedKeychainUnavailable(path)) if path == keychain.display().to_string()
    ));
    std::fs::remove_file(&keychain)?;
    std::fs::write(&keychain, "")?;
    assert_eq!(agy::account_keychain(&directory, &home)?, Some(keychain));
    // The user's own home is the shared login keychain, however it is spelled.
    assert_eq!(agy::account_keychain(&home, &home)?, None);
    std::os::unix::fs::symlink(&home, root.path().join("alias"))?;
    assert_eq!(
        agy::account_keychain(&root.path().join("alias"), &home)?,
        None
    );
    Ok(())
}

#[test]
fn agy_keychain_behind_a_symlinked_parent_is_not_an_isolated_account_keychain()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    // The user's real keychain directory, holding a regular file.
    std::fs::create_dir_all(home.join("Library/Keychains"))?;
    std::fs::write(home.join(KEYCHAIN_FILE), "")?;
    let refused = |directory: &std::path::Path| {
        matches!(
            agy::account_keychain(directory, &home),
            Err(ProviderError::IsolatedKeychainUnavailable(path))
                if path == directory.join(KEYCHAIN_FILE).display().to_string()
        )
    };
    // `Library/Keychains` is a link to the real one.
    let keychains = root.path().join("keychains-link");
    std::fs::create_dir_all(keychains.join("Library"))?;
    std::os::unix::fs::symlink(
        home.join("Library/Keychains"),
        keychains.join("Library/Keychains"),
    )?;
    assert!(std::fs::symlink_metadata(keychains.join(KEYCHAIN_FILE)).is_ok());
    assert!(refused(&keychains));
    // `Library` itself is a link to the real one.
    let library = root.path().join("library-link");
    std::fs::create_dir_all(&library)?;
    std::os::unix::fs::symlink(home.join("Library"), library.join("Library"))?;
    assert!(std::fs::symlink_metadata(library.join(KEYCHAIN_FILE)).is_ok());
    assert!(refused(&library));
    // A real directory tree is still accepted.
    let own = root.path().join("own");
    std::fs::create_dir_all(own.join("Library/Keychains"))?;
    std::fs::write(own.join(KEYCHAIN_FILE), "")?;
    assert_eq!(
        agy::account_keychain(&own, &home)?,
        Some(own.join(KEYCHAIN_FILE))
    );
    Ok(())
}

#[test]
fn unregistered_agy_launches_on_the_login_keychain_under_the_local_key() {
    let prompt = Prompt::new("fixture prompt".into());
    let mut route = route("agy", "gemini-3.8-flash");
    let plan = fixture_access_plan(Access::ReadOnly, Some("keychain"), false);
    // Preparation's synthetic entry routes on key `local` with no pinned account.
    route.account_key = Some("local".into());
    let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    let spec = agy::Agy.prepare(input).expect("unregistered agy launches");
    let login = Path::new("/fixture/home/Library/Keychains/login.keychain-db");
    assert!(spec.setup.contains(&SetupStep::Symlink {
        link: "/fixture/state/Library/Keychains/login.keychain-db".into(),
        target: login.into(),
    }));
    // The carve-out is only the synthetic key: a registered key needs its account.
    route.account_key = Some("agy-google-fixture".into());
    let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    assert!(matches!(
        agy::Agy.prepare(input),
        Err(ProviderError::UnknownIdentity)
    ));
    // The other registered-account adapters have no such carve-out.
    route.adapter = "codex".into();
    route.account_key = Some("local".into());
    let plan = fixture_access_plan(Access::ReadOnly, Some("login"), false);
    let input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
    assert!(matches!(
        codex::Codex.prepare(input),
        Err(ProviderError::UnknownIdentity)
    ));
}

/// A fake `agy` that signs in as one Google account per `HOME`, recording
/// each probe's `HOME`.
struct AgySignIns {
    accounts: Vec<(std::path::PathBuf, &'static str)>,
    homes: std::sync::Mutex<Vec<std::path::PathBuf>>,
}
impl provenant_os::exec::Executor for AgySignIns {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        let home: std::path::PathBuf = options
            .env
            .unwrap_or_default()
            .iter()
            .find(|(key, _)| key == "HOME")
            .map(|(_, value)| value.into())
            .expect("probe sets HOME");
        let log = options
            .argv
            .iter()
            .position(|arg| arg == "--log-file")
            .map(|index| std::path::PathBuf::from(&options.argv[index + 1]))
            .expect("probe names a log file");
        let email = self
            .accounts
            .iter()
            .find(|(directory, _)| *directory == home)
            .expect("probe HOME is a known sign-in")
            .1;
        std::fs::write(
            log,
            format!("OAuth: authenticated successfully as {email} (test)\n"),
        )
        .unwrap();
        self.homes.lock().unwrap().push(home);
        Ok(provenant_os::exec::ExecOutcome {
            termination: provenant_os::exec::Termination::Exit(0),
            stdout: provenant_os::exec::CapturedStream {
                bytes: br#"{"status":"SUCCESS"}"#.to_vec(),
                ..Default::default()
            },
            stderr: Default::default(),
            timed_out: false,
            drain_truncated: false,
            overflow_stopped: false,
            cancelled: false,
            elapsed_micros: 0,
        })
    }
}

#[test]
fn agy_registration_identity_comes_from_the_directory_it_is_signed_in_under()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let real_home = root.path().join("home");
    let isolated = real_home.join(".agy-second");
    let without_keychain = real_home.join(".gemini");
    std::fs::create_dir_all(isolated.join("Library/Keychains"))?;
    std::fs::write(isolated.join("Library/Keychains/login.keychain-db"), "")?;
    std::fs::create_dir_all(&without_keychain)?;
    let probes = AgySignIns {
        accounts: vec![
            (real_home.clone(), "first@example.test"),
            (isolated.clone(), "second@example.test"),
        ],
        homes: Default::default(),
    };
    let probe_root = root.path().join("probes");
    std::fs::create_dir_all(&probe_root)?;
    let detect = |directory: &Path| {
        crate::accounts::detect(
            &probes,
            "agy",
            Path::new("/opt/homebrew/bin/agy"),
            &real_home,
            OsStr::new("u"),
            directory,
            &probe_root,
        )
    };
    let legacy_key = detect(&real_home)?;
    let isolated_key = detect(&isolated)?;
    assert!(legacy_key.starts_with("agy-google-"));
    assert!(isolated_key.starts_with("agy-google-"));
    assert_ne!(legacy_key, isolated_key);
    assert_eq!(
        *probes.homes.lock().unwrap(),
        [real_home.clone(), isolated.clone()]
    );
    // An isolated directory whose keychain is missing or vanishes is refused
    // without a probe, so it can never record the shared login's key.
    assert!(matches!(
        detect(&without_keychain),
        Err(ProviderError::IsolatedKeychainUnavailable(_))
    ));
    std::fs::remove_file(isolated.join("Library/Keychains/login.keychain-db"))?;
    assert!(matches!(
        detect(&isolated),
        Err(ProviderError::IsolatedKeychainUnavailable(_))
    ));
    assert_eq!(probes.homes.lock().unwrap().len(), 2);
    Ok(())
}

#[test]
fn agy_keychain_hard_linked_to_another_file_or_the_real_login_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let directory = home.join(".agy-second");
    let keychain = directory.join(KEYCHAIN_FILE);
    std::fs::create_dir_all(keychain.parent().ok_or("parent")?)?;
    std::fs::create_dir_all(home.join("Library/Keychains"))?;
    let real = home.join(KEYCHAIN_FILE);
    let refused = |home: &std::path::Path| {
        matches!(
            agy::account_keychain(&directory, home),
            Err(ProviderError::IsolatedKeychainUnavailable(path))
                if path == keychain.display().to_string()
        )
    };

    // A hard link to the real login keychain's inode (also two links).
    std::fs::write(&real, "")?;
    std::fs::hard_link(&real, &keychain)?;
    assert!(refused(&home));
    // Any hard link is refused, even one whose other name is elsewhere.
    std::fs::remove_file(&keychain)?;
    std::fs::write(&keychain, "")?;
    std::fs::hard_link(&keychain, root.path().join("other-name"))?;
    assert!(refused(&home));
    std::fs::remove_file(root.path().join("other-name"))?;
    assert_eq!(
        agy::account_keychain(&directory, &home)?,
        Some(keychain.clone())
    );

    // A single-link file whose device and inode match the injected real
    // home's keychain, here reached through that home's symlink to it.
    let injected = root.path().join("injected-home");
    std::fs::create_dir_all(injected.join("Library/Keychains"))?;
    std::os::unix::fs::symlink(&keychain, injected.join(KEYCHAIN_FILE))?;
    assert!(refused(&injected));
    // A real home without a keychain does not match anything.
    assert!(agy::account_keychain(&directory, &root.path().join("empty"))?.is_some());

    // The probe guard applies the same check to the real home it was given.
    let selected = agy::ProbeHome::select(&directory, &home)?;
    assert!(selected.verify().is_ok());
    std::fs::hard_link(&keychain, root.path().join("late-link"))?;
    assert!(selected.verify().is_err());
    Ok(())
}

/// Runs `inner`, then applies `change` to the keychain, as a competing process
/// would while the probe is running.
struct ChangesKeychain<'a> {
    inner: &'a AgySignIns,
    change: Box<dyn Fn() + Send + Sync>,
}
impl provenant_os::exec::Executor for ChangesKeychain<'_> {
    fn execute(
        &self,
        options: provenant_os::exec::ExecOptions<'_>,
    ) -> Result<provenant_os::exec::ExecOutcome, provenant_os::exec::ExecError> {
        let outcome = self.inner.execute(options);
        (self.change)();
        outcome
    }
}

#[test]
fn agy_probe_refuses_a_keychain_that_changes_between_selection_and_execution()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_os::exec::{ExecOptions, Executor};
    let root = tempfile::tempdir()?;
    let real_home = root.path().join("home");
    let isolated = real_home.join(".agy-second");
    let keychain = isolated.join(KEYCHAIN_FILE);
    std::fs::create_dir_all(keychain.parent().ok_or("parent")?)?;
    let reset = || -> std::io::Result<()> {
        let _ = std::fs::remove_file(&keychain);
        std::fs::write(&keychain, "")
    };
    let probes = AgySignIns {
        accounts: vec![(isolated.clone(), "second@example.test")],
        homes: Default::default(),
    };
    let argv: Vec<std::ffi::OsString> = vec!["agy".into(), "--log-file".into(), "/dev/null".into()];
    let env = [("HOME".into(), isolated.clone().into_os_string())];
    let run = |executor: &dyn Executor| {
        let mut options =
            ExecOptions::new(&argv, Path::new("/"), std::time::Duration::from_secs(5));
        options.env = Some(&env);
        executor.execute(options).map(|_| ())
    };
    let refused = |result: Result<(), provenant_os::exec::ExecError>| {
        matches!(
            result,
            Err(provenant_os::exec::ExecError::Io { operation, .. })
                if operation == agy::KEYCHAIN_OPERATION
        )
    };

    // Gone after selection and before the spawn: nothing is spawned.
    reset()?;
    let selected = agy::ProbeHome::select(&isolated, &real_home)?;
    std::fs::remove_file(&keychain)?;
    assert!(refused(run(&selected.guard(&probes))));
    assert!(probes.homes.lock().unwrap().is_empty());

    // Replaced by another file before the spawn: same path, new identity.
    reset()?;
    let selected = agy::ProbeHome::select(&isolated, &real_home)?;
    // Renamed over the old file, so both inodes exist at once and cannot be
    // the same one.
    let sibling = keychain.with_extension("new");
    std::fs::write(&sibling, "other")?;
    std::fs::rename(&sibling, &keychain)?;
    assert!(refused(run(&selected.guard(&probes))));
    assert!(probes.homes.lock().unwrap().is_empty());

    // Gone, or swapped for a symlink, while the probe runs: the result is refused.
    for swap_in_link in [false, true] {
        reset()?;
        let selected = agy::ProbeHome::select(&isolated, &real_home)?;
        let (path, elsewhere) = (keychain.clone(), root.path().join("elsewhere"));
        let racing = ChangesKeychain {
            inner: &probes,
            change: Box::new(move || {
                std::fs::remove_file(&path).unwrap();
                if swap_in_link {
                    std::fs::write(&elsewhere, "").unwrap();
                    std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
                }
            }),
        };
        assert!(refused(run(&selected.guard(&racing))));
    }
    // The probe did run in those two cases; only its result was refused.
    assert_eq!(probes.homes.lock().unwrap().len(), 2);

    // An unchanged keychain passes, and the user's own home is never guarded.
    reset()?;
    let selected = agy::ProbeHome::select(&isolated, &real_home)?;
    assert!(run(&selected.guard(&probes)).is_ok());
    let own = agy::ProbeHome::select(&real_home, &real_home)?;
    assert_eq!(own.path(), real_home);
    assert!(own.verify().is_ok());

    // Through registration: a keychain removed during the probe is not recorded.
    reset()?;
    let probe_root = root.path().join("probes");
    std::fs::create_dir_all(&probe_root)?;
    let path = keychain.clone();
    let racing = ChangesKeychain {
        inner: &probes,
        change: Box::new(move || std::fs::remove_file(&path).unwrap()),
    };
    assert!(matches!(
        crate::accounts::detect(
            &racing,
            "agy",
            Path::new("/opt/homebrew/bin/agy"),
            &real_home,
            OsStr::new("u"),
            &isolated,
            &probe_root,
        ),
        Err(ProviderError::IsolatedKeychainUnavailable(_))
    ));
    Ok(())
}

#[test]
fn native_and_confined_writers_receive_the_same_seed_environment() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let native =
        |key: &str| (key == provenant_confinement::run_config::SANDBOX_ENV).then(|| "off".into());
    for sandboxed in [true, false] {
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        if !sandboxed {
            input.host_var = &native;
        }
        let launch = crate::codex::Codex.prepare(input)?;
        for (key, expected) in [
            (
                "CARGO_HOME",
                "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/cargo-home",
            ),
            (
                "CARGO_TARGET_DIR",
                "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target",
            ),
            ("RUSTUP_HOME", "/fixture/home/.rustup"),
            ("RUSTUP_AUTO_INSTALL", "0"),
            ("CARGO_CACHE_AUTO_CLEAN_FREQUENCY", "never"),
        ] {
            let values: Vec<_> = launch.env.iter().filter(|(name, _)| name == key).collect();
            assert_eq!(values.len(), 1, "{key} sandboxed={sandboxed}");
            assert_eq!(values[0].1, expected);
        }
        if sandboxed {
            assert!(
                launch
                    .profile
                    .spec
                    .tool_writes
                    .iter()
                    .any(|grant| grant.path
                        == Path::new("/fixture/home/Library/Caches/provenant-cargo-downloads"))
            );
        }
    }
    Ok(())
}

#[test]
fn fresh_codex_home_setup_never_includes_provider_history() -> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    let mut route = route("codex", "fixture-model");
    route.account_key = None;
    let plan = fixture_access_plan(Access::WorktreeWrite, Some("login"), false);
    let native =
        |key: &str| (key == provenant_confinement::run_config::SANDBOX_ENV).then(|| "off".into());
    for sandboxed in [true, false] {
        let mut input = fixture_input(&route, &plan, &prompt, Path::new("/fixture/repo"), None);
        if !sandboxed {
            input.host_var = &native;
        }
        let launch = crate::codex::Codex.prepare(input)?;
        // Setup is declarative. No step can walk a history tree: fresh Codex
        // homes receive only auth/skill links and attempt-local directories.
        for step in &launch.setup {
            let paths = match step {
                crate::launch::SetupStep::CreateDir(path) => vec![path],
                crate::launch::SetupStep::WriteFile { path, .. }
                | crate::launch::SetupStep::WriteExecutable { path, .. } => vec![path],
                crate::launch::SetupStep::Symlink { link, target } => vec![link, target],
            };
            for path in paths {
                assert!(
                    !path.components().any(|part| [
                        "sessions",
                        "archived_sessions",
                        "rollouts",
                        "history.jsonl"
                    ]
                    .iter()
                    .any(|name| part.as_os_str() == *name)),
                    "{}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

#[test]
fn native_adapter_finalisation_supplies_cargo_environment_without_tool_discovery()
-> Result<(), ProviderError> {
    let prompt = Prompt::new("fixture".into());
    let mut route = route("claude", "fixture-model");
    route.account_key = None;
    let plan = fixture_access_plan(Access::WorktreeWrite, None, false);
    let token = fixture_token();
    let native =
        |key: &str| (key == provenant_confinement::run_config::SANDBOX_ENV).then(|| "off".into());
    let mut input = fixture_input(
        &route,
        &plan,
        &prompt,
        Path::new("/fixture/repo"),
        Some(&token),
    );
    input.host_var = &native;
    let mut launch = claude::Claude.prepare(input)?;
    // The CLI finalises every adapter, even those that skip sandbox-only Rust
    // discovery. Repeated finalisation must retain one set of selectors.
    launch.complete_tool_requirements(
        &plan,
        Path::new("/fixture/home"),
        Path::new("/fixture/repo"),
    );
    for (name, expected) in [
        (
            "CARGO_HOME",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/cargo-home",
        ),
        ("RUSTUP_HOME", "/fixture/home/.rustup"),
        (
            "PROVENANT_LANE_CARGO_LINT_TARGET",
            "/fixture/home/Library/Caches/provenant-lane-cargo/fixture/target-lint",
        ),
    ] {
        let values: Vec<_> = launch.env.iter().filter(|(key, _)| key == name).collect();
        assert_eq!(values.len(), 1, "{name}");
        assert_eq!(values[0].1, expected);
    }
    Ok(())
}
