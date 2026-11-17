#[cfg(any(target_os = "macos", feature = "snapshot"))]
use super::grants::write_grants_fixture;
use super::*;
#[cfg(target_os = "macos")]
use crate::repo::GitRefName;
use provenant_core::CancelToken;
use provenant_os::exec::SystemExecutor;
#[cfg(target_os = "macos")]
use provenant_os::exec::{ExecOutcome, Executor, Termination};
#[cfg(any(target_os = "macos", feature = "snapshot"))]
use provenant_records::policy::Policy;
#[cfg(target_os = "macos")]
use std::collections::BTreeMap;
#[cfg(any(target_os = "macos", feature = "snapshot"))]
use std::path::{Path, PathBuf};

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RuleAction {
    Allow,
    Deny,
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[derive(Debug)]
pub(super) struct RuleFact {
    pub(super) action: RuleAction,
    pub(super) operations: Vec<String>,
    pub(super) strings: Vec<String>,
    pub(super) atoms: Vec<String>,
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn rule_facts(source: &str) -> Vec<RuleFact> {
    source
        .lines()
        .filter_map(|line| {
            let expression = line.trim().strip_prefix('(')?;
            let header = expression.split('(').next()?;
            let mut atoms = header.split_whitespace();
            let action = match atoms.next()? {
                "allow" => RuleAction::Allow,
                "deny" => RuleAction::Deny,
                _ => return None,
            };
            let operations = atoms
                .map(|atom| atom.trim_end_matches(')').to_owned())
                .filter(|atom| !atom.is_empty())
                .collect();
            let mut strings = Vec::new();
            let mut remaining = line;
            while let Some((_, quoted)) = remaining.split_once('"') {
                let mut value = String::new();
                let mut chars = quoted.chars();
                while let Some(ch) = chars.next() {
                    match ch {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                value.push(escaped);
                            }
                        }
                        _ => value.push(ch),
                    }
                }
                strings.push(value);
                remaining = chars.as_str();
            }
            let mut atoms = Vec::new();
            let mut token = String::new();
            let mut chars = line.chars().peekable();
            let mut quoted = false;
            while let Some(ch) = chars.next() {
                if quoted {
                    if ch == '\\' {
                        chars.next();
                    } else if ch == '"' {
                        quoted = false;
                    }
                } else if ch == '"' {
                    if !token.is_empty() {
                        atoms.push(std::mem::take(&mut token));
                    }
                    quoted = true;
                } else if ch.is_whitespace() || matches!(ch, '(' | ')') {
                    if !token.is_empty() {
                        atoms.push(std::mem::take(&mut token));
                    }
                } else {
                    token.push(ch);
                }
            }
            if !token.is_empty() {
                atoms.push(token);
            }
            Some(RuleFact {
                action,
                operations,
                strings,
                atoms,
            })
        })
        .collect()
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn rule_index(
    source: &str,
    action: RuleAction,
    operation: &str,
    target: Option<&str>,
) -> Option<usize> {
    match target {
        Some(target) => rule_index_with_targets(source, action, operation, &[target]),
        None => rule_index_with_targets(source, action, operation, &[]),
    }
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn rule_index_with_targets(
    source: &str,
    action: RuleAction,
    operation: &str,
    targets: &[&str],
) -> Option<usize> {
    rule_index_with_facts(source, action, operation, targets, &[])
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
pub(super) fn rule_index_with_facts(
    source: &str,
    action: RuleAction,
    operation: &str,
    targets: &[&str],
    atoms: &[&str],
) -> Option<usize> {
    rule_facts(source).iter().position(|rule| {
        rule.action == action
            && rule.operations.iter().any(|name| name == operation)
            && targets
                .iter()
                .all(|target| rule.strings.iter().any(|value| value == target))
            && atoms
                .iter()
                .all(|atom| rule.atoms.iter().any(|value| value == atom))
    })
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS escape and grant tests call it")
)]
pub(super) fn rule_index_without_strings(
    source: &str,
    action: RuleAction,
    operation: &str,
) -> Option<usize> {
    rule_facts(source).iter().position(|rule| {
        rule.action == action
            && rule.operations.iter().any(|name| name == operation)
            && rule.strings.is_empty()
    })
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[cfg_attr(
    not(target_os = "macos"),
    allow(dead_code, reason = "only the macOS escape and grant tests call it")
)]
pub(super) fn has_rule_atoms(
    source: &str,
    action: RuleAction,
    operation: &str,
    required: &[&str],
) -> bool {
    rule_facts(source).iter().any(|rule| {
        rule.action == action
            && rule.operations.iter().any(|name| name == operation)
            && required
                .iter()
                .all(|atom| rule.atoms.iter().any(|value| value == atom))
    })
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn writer_programs_keep_filesystem_and_credential_boundaries() -> Result<(), PermissionError> {
    let (mut plan, mut profile) = write_grants_fixture();
    plan.read_only = None;
    plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
        git_grants_digest: [0; 32],
        publishing: false,
        common_git_dir: "/fixture/repo/.git".into(),
        cargo_cache: PathGrant {
            path: "/fixture/cache".into(),
            scope: PathScope::Subtree,
        },
    }));
    profile.spec.deny_keychain_access = true;
    let worktree = WorktreeWriteGrants::from_paths(
        Path::new("/fixture/worktrees/lane"),
        Path::new("/fixture/repo/.git/worktrees/lane"),
        Path::new("/fixture/repo/.git"),
        "lane",
    );
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), Some(worktree))?;
    let source = render(&grants)?;
    assert!(rule_index(&source, RuleAction::Allow, "default", None).is_some());
    assert!(rule_index(&source, RuleAction::Deny, "default", None).is_none());
    let exec =
        rule_index(&source, RuleAction::Allow, "process-exec", None).expect("writer exec rule");
    assert!(rule_index(&source, RuleAction::Allow, "process-fork", None).is_some());
    assert!(rule_index(&source, RuleAction::Allow, "pseudo-tty", None).is_some());
    assert!(
        rule_index_with_facts(
            &source,
            RuleAction::Deny,
            "process-exec",
            &["/usr/bin/security"],
            &["literal"],
        )
        .is_some_and(|index| index > exec)
    );
    for (operation, path) in [
        ("file-write*", "/fixture/repo/.git"),
        ("file-read*", "/fixture/home/.ssh"),
        ("file-read*", "/fixture/home/.config/gh"),
        ("file-write*", "/fixture/worktrees/lane/.git"),
    ] {
        assert!(
            rule_index_with_facts(&source, RuleAction::Deny, operation, &[path], &["subpath"],)
                .is_some()
        );
    }
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn profile_path_representability_matches_encoder() {
    use std::os::unix::ffi::OsStringExt;
    let unrepresentable =
        PathBuf::from(std::ffi::OsString::from_vec(b"/fixture/name-\xff".to_vec()));
    for path in [
        Path::new("/fixture/normal"),
        Path::new("/fixture/with\nline"),
        Path::new("/fixture/with\t tab"),
    ] {
        assert!(profile_path_representable(path));
        assert!(sbpl_path(path).is_ok());
    }
    for path in [
        Path::new("/fixture/control-\u{1}"),
        unrepresentable.as_path(),
    ] {
        assert!(!profile_path_representable(path));
        assert!(sbpl_path(path).is_err());
    }
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn expanded_authentication_family_grants_only_selected_credentials() -> Result<(), PermissionError>
{
    let (plan, mut profile) = write_grants_fixture();
    profile.spec.process_executables.push(PathGrant {
        path: "/fixture/provider".into(),
        scope: PathScope::Exact,
    });
    for index in 0..20 {
        profile.spec.authentication.push(AuthenticationProfile {
            keychain: None,
            mechanism: format!("extra-{index}"),
            roots: vec![PathGrant {
                path: format!("/fixture/credential-{index}").into(),
                scope: PathScope::Exact,
            }],
            in_place_writes: vec![],
        });
    }
    validate_profile_spec_with_home(
        &profile.spec,
        Path::new("/fixture/home"),
        &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
        128,
    )?;
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    for auth in &profile.spec.authentication {
        for root in &auth.roots {
            assert_eq!(grants.reads.contains(root), auth.mechanism == "selected");
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn large_profiles_render_without_truncation() -> Result<(), Box<dyn std::error::Error>> {
    let (plan, profile) = write_grants_fixture();
    let mut grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    let root = |index: usize, width: usize| PathGrant {
        path: PathBuf::from(format!("/fixture/reads/{index}/{}", "x".repeat(width))),
        scope: PathScope::Subtree,
    };
    // One pass over the rendered quoted paths, not one substring scan per
    // grant: the check stays linear in the profile size.
    let rendered = |source: &str| {
        source
            .split('"')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect::<std::collections::HashSet<_>>()
    };
    for (count, width) in [(1000, 100), (4000, 200)] {
        grants.reads = (0..count).map(|index| root(index, width)).collect();
        let paths = rendered(&render(&grants)?);
        for grant in &grants.reads {
            assert!(paths.contains(grant.path.to_str().expect("fixture UTF-8")));
        }
    }
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn capacity_policy_controls_warning_off_and_typed_refusal() {
    use provenant_records::policy::{Boundary, Source, Strength};
    use std::collections::BTreeMap;

    let default = Policy::default();
    for code in ["GRANT_COUNT", "PROFILE_GRANT_COUNT", "PROFILE_BYTES"] {
        let rule = default.rule(code);
        assert_eq!(rule.rule.boundary, Boundary::None);
        assert_eq!(rule.rule.strength, Strength::Warn);
    }
    let origin = provenant_records::diagnostic::SettingSource::Configuration;
    assert!(
        check_capacity_policy(
            &default,
            "GRANT_COUNT",
            "domains.run.grant_limit",
            2,
            1,
            origin,
            PermissionError::GrantLimitExceeded {
                family: "plan root grant count",
                limit: 1,
            },
        )
        .expect("warning policy proceeds")
        .is_some()
    );

    let mut off = Policy::default();
    off.overlay(
        &BTreeMap::from([("PROFILE_GRANT_COUNT".to_owned(), Strength::Off)]),
        Source::Host,
    );
    assert!(
        check_capacity_policy(
            &off,
            "PROFILE_GRANT_COUNT",
            "domains.run.grant_limit",
            2,
            1,
            origin,
            PermissionError::GrantLimitExceeded {
                family: "provider profile grant count",
                limit: 1,
            },
        )
        .expect("off policy proceeds")
        .is_none()
    );

    let mut strict = Policy::default();
    strict.overlay(
        &BTreeMap::from([("PROFILE_BYTES".to_owned(), Strength::Refuse)]),
        Source::Dispatch,
    );
    assert!(matches!(
        check_capacity_policy(
            &strict,
            "PROFILE_BYTES",
            "domains.run.profile_limit_bytes",
            2,
            1,
            origin,
            PermissionError::ProfileTooLarge { bytes: 2, limit: 1 },
        ),
        Err(PermissionError::ProfileTooLarge { bytes: 2, limit: 1 })
    ));
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn read_only_extension_guard_uses_immutable_authority_policy() {
    use crate::plan::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use provenant_records::policy::{Boundary, Effect, Facts, Strength};

    let (permissions, _) = write_grants_fixture();
    let error = ConfinedPlan::new(PlanRequest {
        scope: provenant_core::WorkScope::Project(provenant_core::ProjectId::from_bytes([1; 16])),
        permissions,
        codex: CodexPolicy::Inherited,
        extensions: ExtensionPolicy::Unconfined,
        agy_state: None,
    })
    .err()
    .expect("unconfined read-only extensions are refused");
    assert!(matches!(
        error,
        crate::plan::PlanError::UnconfinedExtensions
    ));

    let policy = provenant_records::policy::Policy::default();
    let rule = policy.rule("PLAN_CONFINEDPLAN_NEW_AUTHORITY");
    assert_eq!(rule.rule.boundary, Boundary::Authority);
    assert_eq!(rule.rule.strength, Strength::Refuse);
    assert!(matches!(
        policy.check(
            "PLAN_CONFINEDPLAN_NEW_AUTHORITY",
            Facts::violation("guard failed"),
        ),
        Effect::Refuse(_)
    ));
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn launch_command_allows_only_the_attempts_own_mach_name() -> Result<(), Box<dyn std::error::Error>>
{
    let (plan, profile) = write_grants_fixture();
    let rendered = render(&GrantSet::build(
        &plan,
        &profile,
        Path::new("/fixture/home"),
        None,
    )?)?;
    let launch = ConfinedLaunch {
        enforcement: Enforcement::MacOsSandboxExec,
        profile: rendered.into_bytes(),
        profile_digest: [0; 32],
        native_controls: Vec::new(),
    };
    let attempt = provenant_core::AttemptId::from_bytes([7; 16]);
    let command = launch.command(
        &["tool".into()],
        &attempt,
        &provenant_os::fs::Directory::open_anchor(&std::env::temp_dir())?,
    )?;
    assert_eq!(command[3], "tool");
    assert_eq!(command[1], "-f");
    let profile = std::fs::read_to_string(command[2].clone())?;
    assert!(profile.starts_with("(version 1)\n(allow default)\n"));
    assert!(
        profile.contains("(deny mach-lookup (global-name-prefix \"dev.provenant.attempt.\"))\n")
    );
    assert!(profile.ends_with(&format!(
        "(allow mach-lookup (global-name \"dev.provenant.attempt.{attempt}\"))\n"
    )));
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn worktree_access_read_only_profile_limits_checkout_writes()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::plan::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest, ReadOnlyPolicy};
    use crate::utility::{GitMutation, execute_git_mutation};
    use provenant_core::WorkScope;
    use std::os::unix::fs::PermissionsExt;
    let f = crate::test_support::Fixture::new()?;
    let branch = GitRefName::branch("read-only-profile")?;
    let linked = f._temp.path().join("linked");
    let outcome = execute_git_mutation(
        &f.executor,
        &f.repo,
        &f.runtime,
        GitMutation::WorktreeAdd {
            branch,
            base: Some(f.head()?),
            path: linked.clone(),
            create_branch: true,
        },
        f.budget()?,
        &CancelToken::new(),
    )?;
    assert_eq!(outcome.termination, Termination::Exit(0));
    let repo = f
        .runtime
        .open_read_repo(&f.executor, &linked, f.budget()?, &CancelToken::new())?;
    let root = repo.root_path();
    std::fs::create_dir_all(root.join(".agent-run/locks"))?;
    let scratch = provenant_os::fs::canonicalize(f._temp.path())?.join("private-tmp");
    std::fs::create_dir(&scratch)?;
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))?;
    let policy: ReadOnlyPolicy =
        serde_json::from_str(r#"{"readOnlyWritable":[".agent-run/locks"]}"#)?;
    let binding = f.binding()?;
    let home = provenant_os::fs::canonicalize(&f.home)?;
    let state = provenant_os::fs::canonicalize(&f._temp.path().join("state"))?;
    let mut mandatory = MandatoryRoots::new(&home, &state, root)?;
    for path in [repo.common_git_dir(), repo.private_git_dir()] {
        mandatory.add_git(PathGrant {
            path: path.to_owned(),
            scope: PathScope::Subtree,
        })?;
    }
    let lane_policy = crate::run_config::RunConfig::default().policy_for_launch(&state, mandatory);
    let plan = ConfinedPlan::new(
        PlanRequest {
            scope: WorkScope::Project(binding.project_id()),
            permissions: PlanPermissions {
                credential_roots: provenant_core::CredentialRoots::for_home(&home),
                policy: Some(lane_policy),
                read_only: None,
                mode: AccessMode::ReadOnly,
                scratch: PathGrant {
                    path: scratch.clone(),
                    scope: PathScope::Subtree,
                },
                private_state: None,
                read_roots: vec![],
                protected_roots: vec![],
                secret_roots: vec![],
                selected_authentication: None,
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        }
        .with_read_only_repo(&repo, &binding, &policy)?,
    )?;
    use FixtureOperation as O;
    use FixtureOutcome::{Allowed, Denied};
    for directory in [
        home.join(".cargo/bin"),
        home.join(".cargo/registry"),
        home.join(".rustup/toolchains"),
        scratch.join("target"),
    ] {
        std::fs::create_dir_all(directory)?;
    }
    std::fs::write(
        home.join(".rustup/settings.toml"),
        "default_toolchain = 'fixture'",
    )?;
    std::fs::write(home.join(".cargo/credentials.toml"), "fixture-secret")?;
    // A provider may list a configuration directory without reading its
    // children, as OpenCode does for its XDG config directory.
    let listed = home.join(".config/fixture");
    std::fs::create_dir_all(&listed)?;
    let subtree = |path| PathGrant {
        path,
        scope: PathScope::Subtree,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: "/usr/bin/true".into(),
                scope: PathScope::Exact,
            }],
            runtime_reads: vec![
                subtree(home.join(".cargo/bin")),
                subtree(home.join(".cargo/registry")),
                subtree(home.join(".rustup/toolchains")),
                PathGrant {
                    path: home.join(".rustup/settings.toml"),
                    scope: PathScope::Exact,
                },
                PathGrant {
                    path: listed.clone(),
                    scope: PathScope::Exact,
                },
            ],
            tool_exec_roots: vec![
                subtree(home.join(".cargo/bin")),
                subtree(home.join(".rustup/toolchains")),
                subtree(scratch.join("target")),
            ],
            ..Default::default()
        },
        observations: BTreeMap::from([
            (O::DeclaredRead, Allowed),
            (O::UndeclaredRead, Allowed),
            (O::UndeclaredWrite, Allowed),
            (O::SecretRead, Denied),
            (O::ProtectedWrite, Denied),
            (O::OtherNetwork, Denied),
            (O::DeclaredProcessExec, Allowed),
            (O::UndeclaredProcessExec, Allowed),
            (O::ProcessFork, Allowed),
            (O::RuntimeRead, Allowed),
        ]),
    };
    let paths = required_anchors(plan.permissions(), &profile, None);
    let directories = paths
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &home,
        directories: &directories,

        worktree: None,
    };
    let compiled = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
        policy: Policy::default(),
    }
    .compile(plan.permissions(), &profile, &anchors)?;
    let source = String::from_utf8(compiled.profile)?;
    let over_threshold = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits {
            grant_limit: 1,
            profile_limit_bytes: 1,
            grant_origin: provenant_records::diagnostic::SettingSource::Configuration,
            profile_origin: provenant_records::diagnostic::SettingSource::Configuration,
        },
        policy: Policy::default(),
    }
    .compile(plan.permissions(), &profile, &anchors)?;
    assert_eq!(over_threshold.profile, source.as_bytes());
    // Large/native acceptance: real anchors and compiler, above the effective
    // profile threshold. Exact absent files share an already-held read parent.
    let mut large_profile = ProviderProfile {
        spec: profile.spec.clone(),
        observations: profile.observations.clone(),
    };
    large_profile
        .spec
        .runtime_reads
        .extend((0..3000).map(|index| {
            PathGrant {
                path: listed
                    .parent()
                    .expect("fixture parent")
                    .join(format!("capacity-{index}-{}", "x".repeat(200))),
                scope: PathScope::Exact,
            }
        }));
    let large = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
        policy: Policy::default(),
    }
    .compile(plan.permissions(), &large_profile, &anchors)?;
    assert!(large.profile.len() > ProfileLimits::default().profile_limit_bytes);
    let native = || -> Result<(), Box<dyn std::error::Error>> {
        let env = [("HOME".into(), home.clone().into_os_string())];
        let executor = SystemExecutor::new(1)?;
        let cancel = CancelToken::new();
        let context = BackendContext {
            executor: &executor,
            cancel: &cancel,
            env: &env,
        };
        match system_backend(&context) {
            Err(PermissionError::BackendNotAvailable(reason)) => {
                provenant_test_support::skip_sandbox_dependent_test(format!(
                    "large-profile launch needs native Seatbelt: {reason:?}; ADR 0040 capacity acceptance"
                ))?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let command = large.command(
            &["/usr/bin/true".into()],
            &provenant_core::AttemptId::from_bytes([42; 16]),
            &provenant_os::fs::Directory::open_anchor(&scratch)?,
        )?;
        let mut options = command.options(&scratch, std::time::Duration::from_secs(60))?;
        options.env = Some(&env);
        options.cancel = Some(&cancel);
        let outcome = executor.execute(options)?;
        assert_eq!(outcome.termination, Termination::Exit(0), "{outcome:?}");
        Ok(())
    };
    native()?;
    let mut strict_permissions = plan.permissions().clone();
    strict_permissions.read_only = None;
    strict_permissions.read_roots.clear();
    let strict = ConfinedPlan::new(
        PlanRequest {
            scope: WorkScope::Project(binding.project_id()),
            permissions: strict_permissions,
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        }
        .with_live_read_only_repo(&repo, &binding)?,
    )?;
    let strict_profile = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
        policy: Policy::default(),
    }
    .compile(strict.permissions(), &profile, &anchors)?;
    let strict_source = String::from_utf8(strict_profile.profile)?;
    assert!(strict_source.contains(&format!(
        "(deny file-write* (subpath \"{}\"))",
        root.display()
    )));
    assert!(!strict_source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        root.display()
    )));
    assert!(!strict_source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        root.join(".agent-run/locks").display()
    )));
    let git = binding.common_dir();
    assert!(repo.0.private_git_dir().starts_with(git));
    assert!(source.contains(&format!(
        "(allow file-read* (subpath \"{}\"))",
        git.display()
    )));
    assert!(source.contains(&format!(
        "(deny file-write* (subpath \"{}\"))",
        git.display()
    )));
    assert!(!source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        git.display()
    )));
    assert!(source.contains(&format!(
        "(allow file-write* (subpath \"{}\"))",
        root.join(".agent-run/locks").display()
    )));
    assert!(!source.contains(&format!(
        "(allow process-exec (subpath \"{}\"))",
        root.join(".agent-run/locks").display()
    )));
    for tool in [home.join(".cargo/bin"), home.join(".rustup/toolchains")] {
        assert!(source.contains(&format!(
            "(allow file-read* (subpath \"{}\"))",
            tool.display()
        )));
        assert!(source.contains(&format!(
            "(allow process-exec (subpath \"{}\"))",
            tool.display()
        )));
    }
    // Cargo credentials appear only as hard denies.
    assert!(
        source
            .lines()
            .filter(|line| line.contains("credentials"))
            .all(|line| line.starts_with("(deny "))
    );
    assert!(!source.contains(&format!(
        "(allow file-read* (subpath \"{}\"))",
        home.join(".cargo").display()
    )));
    assert!(source.contains(&format!(
        "(deny file-write* (require-all (subpath \"{}\") (regex #",
        root.join(".agent-run/locks").display()
    )));
    assert!(!source.contains("(deny file-write* (regex #"));
    assert!(source.contains(&crate::plan::read_only::exclusion_regex()));
    assert!(source.contains(&format!("(literal \"{}\")", listed.display())));
    assert!(!source.contains(&format!("(subpath \"{}\")", listed.display())));
    // The listed entry is resolved without following links.
    std::fs::rename(&listed, home.join(".config/elsewhere"))?;
    std::os::unix::fs::symlink(home.join(".config/elsewhere"), &listed)?;
    assert!(
        MacOsSandboxBackend {
            ready: true,
            limits: ProfileLimits::default(),
            policy: Policy::default(),
        }
        .compile(plan.permissions(), &profile, &anchors)
        .is_err()
    );
    std::fs::remove_file(&listed)?;
    std::fs::rename(home.join(".config/elsewhere"), &listed)?;
    // A component swapped for a symlink after resolution cannot be compiled.
    std::fs::rename(root.join(".agent-run"), root.join("moved"))?;
    std::os::unix::fs::symlink(root.join("moved"), root.join(".agent-run"))?;
    assert!(
        MacOsSandboxBackend {
            ready: true,
            limits: ProfileLimits::default(),
            policy: Policy::default(),
        }
        .compile(plan.permissions(), &profile, &anchors)
        .is_err()
    );
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
fn denied_probe_requires_an_operating_system_refusal() {
    let mut outcome = ExecOutcome {
        termination: Termination::Exit(44),
        stdout: Default::default(),
        stderr: provenant_os::exec::CapturedStream {
            bytes: b"security: The specified item could not be found in the keychain.".to_vec(),
            ..Default::default()
        },
        timed_out: false,
        drain_truncated: false,
        overflow_stopped: false,
        cancelled: false,
        elapsed_micros: 0,
    };
    assert!(!denied_probe(&outcome));
    outcome.stderr.bytes =
        b"sandbox-exec: execvp() of '/usr/bin/security' failed: Operation not permitted".to_vec();
    assert!(denied_probe(&outcome));
    outcome.termination = Termination::Exit(0);
    assert!(!denied_probe(&outcome));
    outcome.termination = Termination::Exit(1);
    outcome.cancelled = true;
    assert!(!denied_probe(&outcome));
}

#[cfg(target_os = "macos")]
#[test]
fn sbpl_process_and_pty_authority_is_ambient_without_catalogue_grants() {
    let executable = PathGrant {
        path: PathBuf::from("/usr/bin/true"),
        scope: PathScope::Exact,
    };
    let mut spec = ProviderProfileSpec {
        process_executables: vec![executable],
        allow_process_fork: true,
        ..ProviderProfileSpec::default()
    };
    let profile = ProviderProfile {
        spec: spec.clone(),
        observations: BTreeMap::new(),
    };
    let plan = PlanPermissions {
        credential_roots: provenant_core::CredentialRoots::for_home(std::path::Path::new(
            "/fixture/home",
        )),
        policy: None,
        read_only: None,
        private_state: None,
        mode: AccessMode::ReadOnly,
        scratch: PathGrant {
            path: PathBuf::from("/tmp/attempt-scratch"),
            scope: PathScope::Subtree,
        },
        read_roots: Vec::new(),
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let rendered = String::from_utf8(
        render_profile(&plan, &profile, &[], &[], None, Path::new("/Users/fixture")).expect("SBPL"),
    )
    .expect("UTF-8 SBPL");
    assert!(rendered.contains("(allow process-exec (literal \"/usr/bin/true\"))"));
    assert!(rendered.contains("(allow process-fork)"));
    assert!(rendered.contains("(allow default)"));
    assert!(!rendered.contains("(deny pseudo-tty)"));
    assert!(rendered.contains("(deny file-read* file-write* file-ioctl"));
    assert!(rendered.contains("(require-not (extension \"com.apple.sandbox.pty\"))"));

    spec.allow_process_fork = false;
    spec.loopback_server = true;
    spec.allow_pseudo_tty = true;
    let profile = ProviderProfile {
        spec,
        observations: BTreeMap::new(),
    };
    let rendered = String::from_utf8(
        render_profile(&plan, &profile, &[], &[], None, Path::new("/Users/fixture")).expect("SBPL"),
    )
    .expect("UTF-8 SBPL");
    assert!(rendered.contains("(allow default)"));
    assert!(!rendered.contains("(deny process-fork)"));
    // A private server binds and is reached on loopback only.
    assert!(rendered.contains("(allow network-bind (local ip \"localhost:*\"))"));
    assert!(rendered.contains("(allow network-outbound (remote ip \"localhost:*\"))"));
    assert!(!rendered.contains("\"*:*\""));
    // A pseudo-terminal opens the multiplexer and only its own allocations.
    assert!(rendered.contains("(allow pseudo-tty)"));
    assert!(rendered.contains("(literal \"/dev/ptmx\")"));
    assert!(rendered.contains(
        "(require-all (regex #\"^/dev/ttys[0-9]+$\") (extension \"com.apple.sandbox.pty\"))"
    ));
    assert!(!rendered.contains("(subpath \"/dev\")"));
}

#[cfg(target_os = "macos")]
#[test]
fn network_levels_shared_writes_and_hard_denies_render_in_order() {
    let grant = |path: &str| PathGrant {
        path: PathBuf::from(path),
        scope: PathScope::Subtree,
    };
    let profile = ProviderProfile {
        spec: ProviderProfileSpec {
            process_executables: vec![PathGrant {
                path: PathBuf::from("/usr/bin/true"),
                scope: PathScope::Exact,
            }],
            tool_writes: vec![grant("/Users/fixture/.cargo/registry")],
            ..ProviderProfileSpec::default()
        },
        observations: BTreeMap::new(),
    };
    let mut plan = PlanPermissions {
        credential_roots: provenant_core::CredentialRoots::for_home(std::path::Path::new(
            "/fixture/home",
        )),
        policy: None,
        read_only: None,
        private_state: None,
        mode: AccessMode::ReadOnly,
        scratch: grant("/tmp/attempt-scratch"),
        read_roots: Vec::new(),
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots: vec![grant("/work/repo/.agent-run")],
        network: NetworkAccess::Provider,
        nested_confinement: false,
    };
    let render = |plan: &PlanPermissions| {
        let bytes = render_profile(plan, &profile, &[], &[], None, Path::new("/Users/fixture"))
            .expect("SBPL");
        String::from_utf8(bytes).expect("UTF-8 SBPL")
    };
    let provider = render(&plan);
    // Read-only lanes never exec from write roots.
    assert!(
        rule_index(
            &provider,
            RuleAction::Allow,
            "process-exec",
            Some("/work/repo/.agent-run")
        )
        .is_none()
    );
    assert!(rule_facts(&provider).iter().all(|rule| {
        !rule
            .strings
            .iter()
            .any(|value| value.contains("localhost") || value.contains("PGSQL"))
            && !rule.strings.iter().any(|value| value == "*:*")
    }));
    let last_allow = ["/Users/fixture/.cargo/registry", "/work/repo/.agent-run"]
        .map(|path| {
            rule_index(&provider, RuleAction::Allow, "file-write*", Some(path))
                .expect("declared write grant")
        })
        .into_iter()
        .max()
        .expect("allow rules");
    for (operation, path, scope) in [
        ("file-read*", "/Users/fixture/.ssh", "subpath"),
        ("file-write*", "/Users/fixture/.kube", "subpath"),
        ("file-read*", "/Users/fixture/Monero", "subpath"),
        (
            "file-write*",
            "/Users/fixture/Archives/git-preservation",
            "subpath",
        ),
        (
            "file-read*",
            "/Users/fixture/.cargo/credentials.toml",
            "literal",
        ),
        ("file-read*", "/Users/fixture/.gnupg", "subpath"),
        ("file-read*", "/Users/fixture/.azure", "subpath"),
        ("file-read*", "/Users/fixture/.config/gh", "subpath"),
    ] {
        assert!(
            rule_index_with_facts(&provider, RuleAction::Deny, operation, &[path], &[scope])
                .is_some_and(|index| index > last_allow)
        );
    }
    plan.network = NetworkAccess::Local;
    let local = render(&plan);
    assert!(
        rule_index_with_facts(
            &local,
            RuleAction::Allow,
            "network-outbound",
            &["localhost:*"],
            &["remote", "ip"],
        )
        .is_some()
    );
    assert!(
        rule_index_with_facts(
            &local,
            RuleAction::Allow,
            "network-bind",
            &["localhost:*"],
            &["local", "ip"],
        )
        .is_some()
    );
    assert!(rule_facts(&local).iter().any(|rule| {
        rule.operations
            .iter()
            .any(|name| name == "network-outbound")
            && rule.strings.iter().any(|value| value.contains("PGSQL"))
    }));
    assert!(rule_index(&local, RuleAction::Allow, "network-outbound", Some("*:*")).is_none());
    plan.network = NetworkAccess::Open;
    let open = render(&plan);
    assert!(
        rule_index_with_facts(
            &open,
            RuleAction::Allow,
            "network-outbound",
            &["*:*"],
            &["remote", "ip"],
        )
        .is_some()
    );
    assert!(rule_facts(&open).iter().any(|rule| {
        rule.operations
            .iter()
            .any(|name| name == "network-outbound")
            && rule.strings.iter().any(|value| value.contains("PGSQL"))
    }));
    for path in [
        "/Users/fixture/.config/gh",
        "/Users/fixture/Library/Application Support/GitHub CLI",
    ] {
        assert!(
            rule_index_with_facts(
                &open,
                RuleAction::Allow,
                "file-read*",
                &[path],
                &["subpath"],
            )
            .is_some()
        );
        assert!(
            rule_index_with_facts(&open, RuleAction::Deny, "file-read*", &[path], &["subpath"],)
                .is_none()
        );
        assert!(
            rule_index_with_facts(
                &open,
                RuleAction::Deny,
                "file-write*",
                &[path],
                &["subpath"],
            )
            .is_some()
        );
        assert!(
            rule_index_with_facts(
                &provider,
                RuleAction::Allow,
                "file-read*",
                &[path],
                &["subpath"],
            )
            .is_none()
        );
        assert!(
            rule_index_with_facts(
                &local,
                RuleAction::Allow,
                "file-read*",
                &[path],
                &["subpath"],
            )
            .is_none()
        );
        assert!(
            rule_index_with_facts(
                &provider,
                RuleAction::Deny,
                "file-read*",
                &[path],
                &["subpath"],
            )
            .is_some()
        );
    }
    let ssh_deny = rule_index_with_facts(
        &open,
        RuleAction::Deny,
        "file-read*",
        &["/Users/fixture/.ssh"],
        &["subpath"],
    )
    .expect("ssh credential denial");
    let last_read = rule_facts(&open)
        .iter()
        .rposition(|rule| {
            rule.action == RuleAction::Allow
                && rule.operations.iter().any(|name| name == "file-read*")
        })
        .expect("read grant");
    assert!(ssh_deny > last_read);

    // A secret root overlapping the gh config must be denied after the allow.
    for secret in [
        "/Users/fixture/.config/gh",
        "/Users/fixture/.config/gh/hosts.yml",
        "/Users/fixture/.config",
    ] {
        plan.secret_roots = vec![grant(secret)];
        let hidden = render(&plan);
        let deny = rule_index_with_facts(
            &hidden,
            RuleAction::Deny,
            "file-read*",
            &[secret],
            &["subpath"],
        )
        .expect("secret root denial");
        let allow = rule_index_with_facts(
            &hidden,
            RuleAction::Allow,
            "file-read*",
            &["/Users/fixture/.config/gh"],
            &["subpath"],
        )
        .expect("GitHub CLI credential grant");
        assert!(deny > allow);
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_sandbox_probe_runs_through_executor_with_isolated_environment() {
    let temp = crate::test_support::tempdir().expect("temporary isolated root");
    let root = provenant_os::fs::canonicalize(temp.path()).expect("canonical isolated root");
    let home = root.join("home");
    let state = root.join("state");
    let xdg = root.join("xdg");
    let claude = root.join("claude");
    let codex = root.join("codex");
    for directory in [&home, &state, &xdg, &claude, &codex] {
        std::fs::create_dir_all(directory).expect("isolated process directory");
    }
    let env = [
        ("HOME".into(), home.into_os_string()),
        ("PROVENANT_STATE_DIR".into(), state.into_os_string()),
        ("XDG_CONFIG_HOME".into(), xdg.into_os_string()),
        (
            "GIT_CONFIG_GLOBAL".into(),
            root.join("gitconfig").into_os_string(),
        ),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("CLAUDE_CONFIG_DIR".into(), claude.into_os_string()),
        ("CODEX_HOME".into(), codex.into_os_string()),
    ];

    let executor = SystemExecutor::new(2).expect("bounded utility executor");
    let cancel = CancelToken::new();
    let context = BackendContext {
        executor: &executor,
        cancel: &cancel,
        env: &env,
    };
    match system_backend(&context) {
        Ok(_) | Err(PermissionError::BackendNotAvailable(_)) => {}
        Err(error) => panic!("sandbox probe did not fail closed: {error}"),
    }
}

#[test]
fn linux_has_no_confinement_backend() {
    #[cfg(target_os = "linux")]
    {
        let executor = SystemExecutor::new(1).expect("bounded executor");
        let cancel = CancelToken::new();
        let context = BackendContext {
            executor: &executor,
            cancel: &cancel,
        };
        assert_eq!(
            system_backend(&context)
                .err()
                .map(|error| error.to_string()),
            Some(
                PermissionError::BackendNotAvailable(AvailabilityReason::UnsupportedPlatform)
                    .to_string()
            )
        );
    }
}

#[test]
fn network_endpoints_accept_only_sbpl_hosts_and_checked_ports() {
    for address in ["*", "localhost"] {
        assert!(
            validate_endpoint(&NetworkEndpoint {
                address: address.into(),
                port: 443,
            })
            .is_ok()
        );
    }
    for address in ["127.0.0.1", "::1", "example.test"] {
        assert!(
            validate_endpoint(&NetworkEndpoint {
                address: address.into(),
                port: 443,
            })
            .is_err()
        );
    }
    for port in [0, u64::from(u16::MAX) + 1] {
        assert!(
            validate_endpoint(&NetworkEndpoint {
                address: "localhost".into(),
                port,
            })
            .is_err()
        );
    }
}
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn invalid_provider_endpoints_are_omitted_from_sbpl() -> Result<(), PermissionError> {
    let (mut plan, mut profile) = write_grants_fixture();
    plan.network = NetworkAccess::Open;
    profile.spec.process_executables.push(PathGrant {
        path: "/fixture/provider".into(),
        scope: PathScope::Exact,
    });
    profile.spec.provider_api = vec![
        NetworkEndpoint {
            address: "localhost".into(),
            port: 443,
        },
        NetworkEndpoint {
            address: "invalid\") (allow file-read* (subpath \"/".into(),
            port: 443,
        },
        NetworkEndpoint {
            address: "*".into(),
            port: 0,
        },
    ];
    validate_profile_spec_with_home(
        &profile.spec,
        Path::new("/fixture/home"),
        &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
        128,
    )?;
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    let source = render(&grants)?;
    let policy = provenant_records::policy::Policy::default();
    let rule = policy.rule("RENDER_RENDER_PROFILE_WITH_LIMIT_NONE");
    assert_eq!(
        rule.rule.boundary,
        provenant_records::policy::Boundary::None
    );
    assert_eq!(
        rule.rule.strength,
        provenant_records::policy::Strength::Warn
    );
    assert!(matches!(
        policy.check(
            "RENDER_RENDER_PROFILE_WITH_LIMIT_NONE",
            provenant_records::policy::Facts::violation("predicate observed"),
        ),
        provenant_records::policy::Effect::Warn(_)
    ));
    assert!(
        rule_index_with_facts(
            &source,
            RuleAction::Allow,
            "network-outbound",
            &["localhost:443"],
            &["tcp"],
        )
        .is_some()
    );
    assert!(rule_index(&source, RuleAction::Allow, "network-outbound", Some("*:*")).is_some());
    assert!(
        rule_facts(&source)
            .iter()
            .all(|rule| rule.strings.iter().all(|value| !value.contains("invalid")))
    );
    assert!(rule_index(&source, RuleAction::Allow, "network-outbound", Some("*:0")).is_none());
    Ok(())
}

#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn invalid_discovery_reads_are_omitted_before_anchoring_and_rendering()
-> Result<(), PermissionError> {
    use std::os::unix::ffi::OsStringExt;
    let (plan, mut profile) = write_grants_fixture();
    profile.spec.process_executables.push(PathGrant {
        path: "/fixture/provider".into(),
        scope: PathScope::Exact,
    });
    let invalid_native = PathBuf::from(std::ffi::OsString::from_vec(
        b"/fixture/invalid-\xff/.agents".to_vec(),
    ));
    profile.spec.discovery_reads = vec![
        PathGrant {
            path: "/fixture/repo/.agents".into(),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: "/fixture/home/.config/gh/hosts.yml".into(),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: invalid_native.clone(),
            scope: PathScope::Exact,
        },
        PathGrant {
            path: "/fixture/unsafe/.codex".into(),
            scope: PathScope::Subtree,
        },
    ];
    validate_profile_spec_with_home(
        &profile.spec,
        Path::new("/fixture/home"),
        &provenant_core::CredentialRoots::for_home(Path::new("/fixture/home")),
        128,
    )?;
    let anchors = required_anchors(&plan, &profile, None);
    assert!(!anchors.contains(Path::new("/fixture/home/.config/gh")));
    assert!(!anchors.contains(invalid_native.parent().expect("fixture parent")));
    let grants = GrantSet::build(&plan, &profile, Path::new("/fixture/home"), None)?;
    let source = render(&grants)?;
    let policy = provenant_records::policy::Policy::default();
    let rule = policy.rule("RENDER_RENDER_PROFILE_WITH_LIMIT_NONE_2");
    assert_eq!(
        rule.rule.boundary,
        provenant_records::policy::Boundary::None
    );
    assert_eq!(
        rule.rule.strength,
        provenant_records::policy::Strength::Warn
    );
    assert!(matches!(
        policy.check(
            "RENDER_RENDER_PROFILE_WITH_LIMIT_NONE_2",
            provenant_records::policy::Facts::violation("predicate observed"),
        ),
        provenant_records::policy::Effect::Warn(_)
    ));
    assert!(
        rule_index(
            &source,
            RuleAction::Allow,
            "file-read*",
            Some("/fixture/repo/.agents")
        )
        .is_some()
    );
    assert!(rule_facts(&source).iter().all(|rule| {
        rule.strings
            .iter()
            .all(|value| value != "hosts.yml" && value != "/fixture/unsafe/.codex")
    }));
    Ok(())
}

// Small: mandatory and host denials must survive ambient access and broad grants.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn ambient_policy_renders_complete_mandatory_and_host_denials()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut plan, mut profile) = write_grants_fixture();
    let home = Path::new("/fixture/home");
    let state = Path::new("/fixture/host-state");
    let mut mandatory = MandatoryRoots::new(home, state, Path::new("/fixture/repo"))?;
    mandatory.add_credentials(PathGrant {
        path: "/fixture/relocated/unselected".into(),
        scope: PathScope::Subtree,
    })?;
    mandatory.add_credentials(PathGrant {
        path: "/fixture/agent.sock".into(),
        scope: PathScope::Exact,
    })?;
    mandatory.add_git(PathGrant {
        path: "/fixture/host-git".into(),
        scope: PathScope::Subtree,
    })?;
    let config: crate::run_config::RunConfig = serde_json::from_value(serde_json::json!({
        "host_policy": {
            "deny_read": ["/fixture/host-private", 42, "relative"],
            "deny_write": ["/fixture/host-archive"],
            "network": "open",
            "allow": ["/fixture/home/.ssh"]
        }
    }))?;
    plan.policy = Some(config.policy_for_launch(state, mandatory.clone()));
    plan.network = NetworkAccess::Open;
    plan.read_roots.push(PathGrant {
        path: "/fixture".into(),
        scope: PathScope::Subtree,
    });
    profile.spec.tool_writes.push(PathGrant {
        path: "/fixture/home".into(),
        scope: PathScope::Subtree,
    });
    let grants = GrantSet::build(&plan, &profile, home, None)?;
    let rendered = render(&grants)?;
    assert!(rule_index(&rendered, RuleAction::Allow, "default", None).is_some());
    assert!(rule_index(&rendered, RuleAction::Deny, "file-link", None).is_some());
    assert!(
        rule_index_with_facts(
            &rendered,
            RuleAction::Deny,
            "network-outbound",
            &["/fixture/agent.sock"],
            &["require-all", "unix-socket", "path-literal"],
        )
        .is_some()
    );
    let last_ordinary_write = rule_index_with_facts(
        &rendered,
        RuleAction::Allow,
        "file-write*",
        &["/fixture/home"],
        &["subpath"],
    )
    .expect("broad ordinary grant");
    let gh = gh_config_paths(home);
    for credential in mandatory.credentials() {
        let target = credential.path.to_string_lossy();
        let scope = match credential.scope {
            PathScope::Exact => "literal",
            PathScope::Subtree => "subpath",
        };
        for operation in ["file-write*", "process-exec"] {
            assert!(
                rule_index_with_facts(
                    &rendered,
                    RuleAction::Deny,
                    operation,
                    &[&target],
                    &[scope],
                )
                    .is_some_and(|index| index > last_ordinary_write)
            );
        }
        if !gh.contains(&credential.path) {
            for operation in ["file-read*", "file-clone"] {
                assert!(
                    rule_index_with_facts(
                        &rendered,
                        RuleAction::Deny,
                        operation,
                        &[&target],
                        &[scope],
                    )
                    .is_some_and(|index| index > last_ordinary_write)
                );
            }
        }
    }
    for (operation, path, scope) in [
        ("file-read*", "/fixture/host-state", "subpath"),
        ("file-clone", "/fixture/host-state", "subpath"),
        ("file-write*", "/fixture/host-state", "subpath"),
        ("process-exec", "/fixture/host-state", "subpath"),
        ("file-write*", "/fixture/host-git", "subpath"),
        ("file-read*", "/fixture/host-private", "subpath"),
        ("file-clone", "/fixture/host-private", "subpath"),
        ("process-exec", "/fixture/host-private", "subpath"),
        ("file-write*", "/fixture/host-archive", "subpath"),
    ] {
        assert!(
            rule_index_with_facts(&rendered, RuleAction::Deny, operation, &[path], &[scope],)
                .is_some()
        );
    }
    assert!(!plan.policy.as_ref().expect("policy").warnings().is_empty());
    for root in [state, Path::new("/fixture")] {
        profile.spec.authentication.push(AuthenticationProfile {
            keychain: None,
            mechanism: "whole-state".into(),
            roots: vec![PathGrant {
                path: root.to_owned(),
                scope: PathScope::Subtree,
            }],
            in_place_writes: vec![],
        });
        plan.selected_authentication = Some("whole-state".into());
        assert!(matches!(
            render(&GrantSet::build(&plan, &profile, home, None)?),
            Err(PermissionError::ProtectedOverlap)
        ));
        profile.spec.authentication.pop();
    }
    Ok(())
}

// Small: optional host ceilings cannot widen requests, including offline adapters.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn renderer_applies_all_network_ceiling_pairs_and_offline_denial()
-> Result<(), Box<dyn std::error::Error>> {
    use crate::run_config::RunConfig;
    let home = Path::new("/fixture/home");
    for (setting, ceiling) in [
        ("provider", NetworkAccess::Provider),
        ("local", NetworkAccess::Local),
        ("open", NetworkAccess::Open),
    ] {
        for request in [
            NetworkAccess::Provider,
            NetworkAccess::Local,
            NetworkAccess::Open,
        ] {
            let (mut plan, mut profile) = write_grants_fixture();
            let config: RunConfig = serde_json::from_value(serde_json::json!({
                "host_policy": {"network": setting}
            }))?;
            let policy = config.policy_for_launch(
                Path::new("/fixture/host-state"),
                MandatoryRoots::new(
                    home,
                    Path::new("/fixture/host-state"),
                    Path::new("/fixture/repo"),
                )?,
            );
            let effective = policy.optional().network_for(request);
            assert_eq!(
                effective == NetworkAccess::Open,
                ceiling == NetworkAccess::Open && request == NetworkAccess::Open
            );
            plan.network = request;
            plan.policy = Some(policy);
            let rendered = render(&GrantSet::build(&plan, &profile, home, None)?)?;
            assert_eq!(
                rendered.contains("(allow network-outbound (remote ip \"*:*\"))"),
                effective == NetworkAccess::Open
            );
            assert_eq!(
                rendered.contains("(allow network-outbound (remote ip \"localhost:*\"))"),
                effective != NetworkAccess::Provider
            );
            assert_eq!(
                rendered.contains("(deny network*)"),
                effective != NetworkAccess::Open
            );
            profile.spec.deny_network = true;
            let offline = render(&GrantSet::build(&plan, &profile, home, None)?)?;
            let final_deny = offline.rfind("(deny network*)").expect("offline denial");
            assert!(
                offline
                    .rfind("(allow network")
                    .is_none_or(|allow| final_deny > allow)
            );
        }
    }
    Ok(())
}

// Small: both login and isolated keychains require the selected typed mechanism.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn keychain_ipc_requires_the_selected_typed_mechanism() -> Result<(), PermissionError> {
    let home = Path::new("/fixture/home");
    for (path, selected, typed, explicit_deny, permitted) in [
        (
            "/fixture/isolated/Library/Keychains/login.keychain-db",
            true,
            true,
            false,
            true,
        ),
        (
            "/fixture/isolated/Library/Keychains/login.keychain-db",
            false,
            true,
            false,
            false,
        ),
        (
            "/fixture/isolated/Library/Keychains/login.keychain-db",
            true,
            false,
            false,
            false,
        ),
        (
            "/fixture/home/Library/Keychains/login.keychain-db",
            true,
            true,
            false,
            true,
        ),
        (
            "/fixture/isolated/Library/Keychains/login.keychain-db",
            true,
            true,
            true,
            false,
        ),
    ] {
        let (mut plan, mut profile) = write_grants_fixture();
        plan.selected_authentication = selected.then(|| "keychain".into());
        profile.spec.deny_keychain_access = explicit_deny;
        profile.spec.authentication.push(AuthenticationProfile {
            keychain: typed.then_some(KeychainAccess::SecurityServer),
            mechanism: "keychain".into(),
            roots: vec![PathGrant {
                path: path.into(),
                scope: PathScope::Exact,
            }],
            in_place_writes: vec![],
        });
        let source = render(&GrantSet::build(&plan, &profile, home, None)?)?;
        assert_eq!(
            source.contains("(deny process-exec (literal \"/usr/bin/security\"))"),
            !permitted
        );
        assert_eq!(
            source.contains("(deny mach-lookup (global-name \"com.apple.SecurityServer\"))"),
            explicit_deny
        );
        assert_eq!(
            source.contains("(require-not (global-name \"com.apple.SecurityServer\"))"),
            !explicit_deny
        );
        for prefix in [
            "com.apple.securityd",
            "com.apple.security.cloudkeychain",
            "com.apple.keychainsharing",
            "com.apple.SecurityServer",
        ] {
            assert!(
                source.contains(&format!("(global-name-prefix \"{prefix}\")"))
                    == (prefix == "com.apple.SecurityServer" || !permitted),
                "keychain IPC prefix {prefix}"
            );
        }
    }
    Ok(())
}

// Small: resolving an alias cannot turn a non-exempt store into selected auth.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn mandatory_alias_categories_cannot_become_authentication_exceptions()
-> Result<(), PermissionError> {
    let home = Path::new("/fixture/home");
    for (relative, alias, scope) in [
        (
            ".claude.json",
            "/fixture/relocated/claude-sidecar",
            PathScope::Exact,
        ),
        (
            "Library/Keychains",
            "/fixture/relocated/Library/Keychains",
            PathScope::Subtree,
        ),
        (
            "Archives/git-preservation",
            "/fixture/relocated/git-preservation",
            PathScope::Subtree,
        ),
    ] {
        let (mut plan, mut profile) = write_grants_fixture();
        let mut mandatory = MandatoryRoots::new(
            home,
            Path::new("/fixture/host-state"),
            Path::new("/fixture/repo"),
        )?;
        let original = PathGrant {
            path: home.join(relative),
            scope,
        };
        let alias = PathGrant {
            path: alias.into(),
            scope,
        };
        mandatory.add_credentials_alias(&original, alias.clone())?;
        let exact = PathGrant {
            path: match scope {
                PathScope::Exact => alias.path.clone(),
                PathScope::Subtree => alias.path.join("login.keychain-db"),
            },
            scope: PathScope::Exact,
        };
        profile.spec.authentication.push(AuthenticationProfile {
            keychain: Some(KeychainAccess::SecurityServer),
            mechanism: "selected-alias".into(),
            roots: vec![exact.clone()],
            in_place_writes: vec![exact],
        });
        plan.selected_authentication = Some("selected-alias".into());
        plan.policy = Some(
            crate::run_config::RunConfig::default()
                .policy_for_launch(Path::new("/fixture/host-state"), mandatory),
        );
        let source = render(&GrantSet::build(&plan, &profile, home, None)?)?;
        let filter = match scope {
            PathScope::Exact => "literal",
            PathScope::Subtree => "subpath",
        };
        for operation in [
            "file-read*",
            "file-write*",
            "process-exec",
            "file-map-executable",
            "file-clone",
        ] {
            let denial = format!("(deny {operation} ({filter} \"{}\"))", alias.path.display());
            assert!(
                source.contains(&denial),
                "non-exempt alias {relative}: {denial}"
            );
        }
        assert!(source.contains("(deny process-exec (literal \"/usr/bin/security\"))"));
    }
    Ok(())
}

// Small: enforcing compilation cannot guess the selected state root.
#[cfg(target_os = "macos")]
#[test]
fn enforcing_backend_refuses_missing_host_policy_before_inspecting_grants() {
    let (plan, profile) = write_grants_fixture();
    assert!(plan.policy.is_none());
    let anchors = Anchors {
        home: Path::new("/fixture/home"),
        directories: &[],
        worktree: None,
    };
    let result = MacOsSandboxBackend {
        ready: true,
        limits: ProfileLimits::default(),
        policy: Policy::default(),
    }
    .compile(&plan, &profile, &anchors);
    assert!(matches!(
        result,
        Err(PermissionError::PermissionNotRecorded)
    ));
}

// Small: the host owner's writer roots inside another checkout are carved out
// of that checkout's write denial; the rest of it stays immutable, and only a
// worktree writer receives the exception.
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn host_writer_roots_inside_protected_checkouts_render_as_exceptions() -> Result<(), PermissionError>
{
    let subtree = |path: &str| PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    };
    let home = Path::new("/fixture/home");
    let state = Path::new("/fixture/host-state");
    let mut mandatory =
        MandatoryRoots::new(home, state, Path::new("/fixture/primary/.worktrees/lane"))?;
    mandatory.add_protected(subtree("/fixture/primary"))?;
    mandatory.add_protected(subtree("/fixture/primary/.worktrees/other"))?;
    mandatory.add_protected_git(subtree("/fixture/elsewhere/.git"))?;
    let locks = subtree("/fixture/primary/.agent-run/locks");
    let target = subtree("/fixture/primary/.worktrees/other/target");
    for grant in [&locks, &target] {
        mandatory.add_host_write(grant)?;
        mandatory.validate_extra_write(grant)?;
    }
    let expected_host_writes = [locks.path.clone(), target.path.clone()]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        mandatory
            .host_writes()
            .iter()
            .map(|grant| grant.path.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        expected_host_writes
    );
    // Equal, enclosing, Git and dispatch-only roots are not exceptions.
    for path in [
        "/fixture/primary",
        "/fixture/primary/.worktrees",
        "/fixture/primary/tools/.git",
        "/fixture/elsewhere/.git/hooks",
    ] {
        mandatory.add_host_write(&subtree(path))?;
        assert!(
            matches!(
                mandatory.validate_extra_write(&subtree(path)),
                Err(PermissionError::ProtectedOverlap)
            ),
            "{path}"
        );
    }
    assert!(matches!(
        mandatory.validate_extra_write(&subtree("/fixture/primary/scripts")),
        Err(PermissionError::ProtectedOverlap)
    ));
    assert_eq!(
        mandatory
            .host_writes()
            .iter()
            .map(|grant| grant.path.clone())
            .collect::<std::collections::BTreeSet<_>>(),
        expected_host_writes
    );

    let (mut plan, profile) = write_grants_fixture();
    plan.read_only = None;
    plan.policy =
        Some(crate::run_config::RunConfig::default().policy_for_launch(state, mandatory.clone()));
    plan.mode = AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
        git_grants_digest: [0; 32],
        publishing: false,
        common_git_dir: "/fixture/primary/.git".into(),
        cargo_cache: subtree("/fixture/cache"),
    }));
    plan.write_roots = vec![locks, target];
    let worktree = WorktreeWriteGrants::from_paths(
        Path::new("/fixture/primary/.worktrees/lane"),
        Path::new("/fixture/primary/.git/worktrees/lane"),
        Path::new("/fixture/primary/.git"),
        "lane",
    );
    let writer = render(&GrantSet::build(&plan, &profile, home, Some(worktree))?)?;
    assert!(
        rule_index_with_facts(
            &writer,
            RuleAction::Deny,
            "file-write*",
            &[
                "/fixture/primary",
                "/fixture/primary/.worktrees/lane",
                "/fixture/primary/.agent-run/locks",
                "/fixture/primary/.worktrees/other/target",
            ],
            &["subpath", "require-all", "require-not"],
        )
        .is_some()
    );
    assert!(
        rule_index_with_facts(
            &writer,
            RuleAction::Deny,
            "file-write*",
            &[
                "/fixture/primary/.worktrees/other",
                "/fixture/primary/.worktrees/other/target",
            ],
            &["subpath", "require-all", "require-not"],
        )
        .is_some()
    );
    for path in ["/fixture/elsewhere/.git", "/fixture/primary/.git"] {
        assert!(
            rule_index_with_facts(
                &writer,
                RuleAction::Deny,
                "file-write*",
                &[path],
                &["subpath"],
            )
            .is_some()
        );
    }

    plan.mode = AccessMode::ReadOnly;
    plan.write_roots.clear();
    let reader = render(&GrantSet::build(&plan, &profile, home, None)?)?;
    for path in ["/fixture/primary", "/fixture/primary/.worktrees/other"] {
        assert!(
            rule_index_with_facts(
                &reader,
                RuleAction::Deny,
                "file-write*",
                &[path],
                &["subpath"],
            )
            .is_some()
        );
    }
    assert!(
        rule_index_with_facts(
            &reader,
            RuleAction::Deny,
            "file-write*",
            &["/fixture/primary/.agent-run/locks"],
            &["subpath"],
        )
        .is_none()
    );
    Ok(())
}

// Spec: docs/specs/supervisor.md#plans-refuse-unsafe-roots
#[cfg(any(target_os = "macos", feature = "snapshot"))]
#[test]
fn shared_stores_render_subtree_denials_with_every_lane_root_carved_out()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_core::{CredentialKind, CredentialRoots, CredentialScope};
    let home = Path::new("/fixture/home");
    // A home-rooted account, a relocated CODEX_HOME, and a built-in store
    // containing an alternate graph source all use the same boundary.
    for store in [
        home.to_owned(),
        home.join("relocated-codex"),
        home.join(".claude"),
    ] {
        let project = store.join("project");
        let cwd = project.join("cwd");
        let scratch = store.join("scratch");
        let lanes = vec![project.clone(), cwd, scratch];
        let mut credentials = CredentialRoots::default();
        credentials.add(
            &store,
            CredentialScope::Subtree,
            "shared fixture",
            CredentialKind::Store,
        );
        let credentials = credentials.with_lane_roots(lanes.clone());
        assert!(credentials.refuses(&store.join("unlisted-secret"), CredentialScope::Exact, true));
        assert!(!credentials.refuses(&project.join("ordinary"), CredentialScope::Exact, true));
        let (mut plan, mut profile) = write_grants_fixture();
        plan.credential_roots = credentials;
        plan.network = NetworkAccess::Off;
        plan.read_roots = lanes
            .iter()
            .map(|path| PathGrant {
                path: path.clone(),
                scope: PathScope::Subtree,
            })
            .collect();
        // A selected broad auth grant cannot cancel the shared store denial.
        profile.spec.authentication[1].roots = vec![PathGrant {
            path: store.clone(),
            scope: PathScope::Subtree,
        }];
        let source = render(&GrantSet::build(&plan, &profile, home, None)?)?;
        for operation in [
            "file-read*",
            "file-clone",
            "file-write*",
            "process-exec",
            "file-map-executable",
        ] {
            let rule = source
                .lines()
                .find(|line| {
                    line.starts_with(&format!(
                        "(deny {operation} (require-all (subpath \"{}\")",
                        store.display()
                    ))
                })
                .ok_or("missing shared store denial")?;
            for lane in &lanes {
                assert!(
                    rule.contains(&format!("(require-not (subpath \"{}\"))", lane.display())),
                    "{rule}"
                );
            }
        }
    }
    Ok(())
}
