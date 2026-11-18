//! Focused provider grant invariants across access modes and network settings.
use super::tests::{fixture_access_plan, fixture_input, fixture_token, route};
use super::*;
use provenant_confinement::cargo::CargoHome;
use provenant_confinement::permissions::snapshot::render;
use provenant_confinement::permissions::{
    NetworkAccess, PathGrant, PathScope, PlanPermissions, ProviderProfile,
};
use provenant_confinement::plan::ReadOnlyGrants;
use std::collections::BTreeSet;
use std::path::Path;

const HOME: &str = "/fixture/home";

fn grant(path: &str, scope: PathScope) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope,
    }
}

/// Grants `prepare` adds only when the host has the path (the macOS-only SSL
/// and Command Line Tools roots, a Homebrew or `/usr/local` prefix). They would
/// make fixtures differ by machine, so the tests drop them.
fn host_probed(path: &Path) -> bool {
    if path.starts_with("/opt/homebrew/Cellar/provider/1.0") {
        return false;
    }
    const PREFIX_DIRECTORIES: [&str; 10] = [
        "bin",
        "sbin",
        "Cellar",
        "opt",
        "lib",
        "libexec",
        "etc",
        "share",
        "include",
        "Frameworks",
    ];
    let developer = Path::new("/Library/Developer/CommandLineTools");
    path == Path::new("/private/etc/ssl")
        || path.starts_with(developer)
        || ["/opt/homebrew", "/usr/local"].iter().any(|prefix| {
            PREFIX_DIRECTORIES
                .iter()
                .any(|name| path.starts_with(Path::new(prefix).join(name)))
        })
}

/// An adapter's permissions and profile from its real `prepare`.
fn prepared(
    id: &str,
    access: Access,
    authentication: Option<&str>,
    include_continuation: bool,
    network: Option<NetworkAccess>,
) -> Result<(PlanPermissions, ProviderProfile), ProviderError> {
    let base = fixture_access_plan(access, authentication, include_continuation);
    let mut permissions = base.permissions().clone();
    // Provider state is attempt-private, separate from the host Fabric root.
    permissions.private_state = Some(grant("/fixture/provider-state", PathScope::Subtree));
    if let Some(network) = network {
        permissions.network = network;
    }
    let confined = provenant_confinement::ConfinedPlan::new(provenant_confinement::PlanRequest {
        scope: base.scope(),
        permissions,
        codex: provenant_confinement::CodexPolicy::Clean,
        extensions: provenant_confinement::ExtensionPolicy::Deny,
        agy_state: None,
    })
    .expect("snapshot fixture plan");
    let mut route = route(id, if id == "command" { "" } else { "fixture-model" });
    // Agy pins its account; the others launch without one here.
    let agy_account = (id == "agy").then(super::tests::legacy_agy_account);
    if agy_account.is_none() {
        route.account_key = None;
    }
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let entry = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == id)
        .ok_or(ProviderError::UnknownIdentity)?;
    let mut input = fixture_input(
        &route,
        &confined,
        &prompt,
        Path::new("/fixture/repo"),
        (id == "claude").then_some(&token),
    );
    input.account = agy_account.as_ref();
    let command = vec!["fixture-command".into(), "--check".into()];
    let runtimes = vec!["/opt/check/bin/python3".into()];
    if id == "command" {
        input.command = Some(crate::command::Invocation {
            argv: &command,
            runtimes: &runtimes,
            program: input.executable,
        });
    }
    let spec = entry.adapter.prepare(input)?;
    let mut profile = spec.profile;
    for grants in [
        &mut profile.spec.runtime_reads,
        &mut profile.spec.tool_exec_roots,
    ] {
        grants.retain(|grant| !host_probed(&grant.path));
    }
    if let Some(socket) = &mut profile.spec.fabric_socket {
        // Grant fixtures are shared by macOS and Linux.
        socket.path = "/fixture/host/fabric-relay.sock".into();
    }
    Ok((confined.permissions().clone(), profile))
}

/// The credential mechanism each adapter's fixture plan selects.
fn login(id: &str) -> Option<&'static str> {
    provenant_records::AdapterSpec::get(id).and_then(|spec| spec.authentication)
}

struct Case {
    name: String,
    plan: PlanPermissions,
    profile: ProviderProfile,
}

impl Case {
    fn new(
        name: &str,
        id: &str,
        access: Access,
        authentication: Option<&str>,
        include_continuation: bool,
        adjust: impl FnOnce(&mut PlanPermissions),
    ) -> Result<Self, ProviderError> {
        let (mut plan, profile) = prepared(id, access, authentication, include_continuation, None)?;
        adjust(&mut plan);
        Ok(Self {
            name: name.into(),
            plan,
            profile,
        })
    }
}

fn cases() -> Result<Vec<Case>, ProviderError> {
    let mut cases = Vec::new();
    // Each adapter's grant variant, in both lane access modes.
    for entry in REGISTRY {
        let id = entry.adapter.id();
        for (access, mode) in [
            (Access::ReadOnly, "read-only"),
            (Access::WorktreeWrite, "worktree-write"),
        ] {
            if access == Access::WorktreeWrite
                && !provenant_records::AdapterSpec::get(id).is_some_and(|spec| spec.launch.write)
            {
                continue;
            }
            cases.push(Case::new(
                &format!("{id}-{mode}"),
                id,
                access,
                login(id),
                false,
                |_| {},
            )?);
        }
    }
    for (mode, network) in [
        ("open", NetworkAccess::Open),
        ("local", NetworkAccess::Local),
    ] {
        let (plan, profile) = prepared("command", Access::ReadOnly, None, false, Some(network))?;
        assert!(!profile.spec.deny_network);
        cases.push(Case {
            name: format!("command-network-{mode}-read-only"),
            plan,
            profile,
        });
    }
    for (access, mode) in [
        (Access::ReadOnly, "read-only"),
        (Access::WorktreeWrite, "worktree-write"),
    ] {
        let (plan, mut profile) = prepared("codex", access, login("codex"), false, None)?;
        assert!(!profile.spec.allow_database_ipc);
        profile.spec.allow_database_ipc = true;
        cases.push(Case {
            name: format!("database-{mode}"),
            plan,
            profile,
        });
    }
    // Conditional grants: credential unselected, and continuation on.
    cases.push(Case::new(
        "codex-no-auth-read-only",
        "codex",
        Access::ReadOnly,
        login("codex"),
        false,
        |plan| plan.selected_authentication = None,
    )?);
    cases.push(Case::new(
        "claude-continuation-read-only",
        "claude",
        Access::ReadOnly,
        None,
        true,
        |_| {},
    )?);
    // Plan shapes the dispatch adds on top of an adapter's profile.
    cases.push(Case::new(
        "claude-project-read-only",
        "claude",
        Access::ReadOnly,
        None,
        false,
        |plan| {
            plan.read_only = Some(ReadOnlyGrants::for_test(
                "/fixture/project".into(),
                vec![grant("/fixture/project/.agent-run", PathScope::Subtree)],
            ));
            plan.read_roots = vec![grant("/fixture/reference", PathScope::Subtree)];
            plan.protected_roots = vec![grant("/fixture/project/AGENTS.md", PathScope::Exact)];
            plan.secret_roots = vec![grant("/fixture/project/secrets", PathScope::Subtree)];
            plan.write_roots = vec![grant("/fixture/project/locks", PathScope::Subtree)];
        },
    )?);
    // A graph node reads its materialised upstream results, and only those.
    cases.push(Case::new(
        "claude-graph-inputs-read-only",
        "claude",
        Access::ReadOnly,
        None,
        false,
        |plan| {
            plan.read_roots = vec![grant(
                "/fixture/state/fabric/graph-inputs/project/0123abcd",
                PathScope::Subtree,
            )];
        },
    )?);
    cases.push(Case::new(
        "claude-roots-worktree-write",
        "claude",
        Access::WorktreeWrite,
        None,
        false,
        |plan| {
            plan.protected_roots = vec![grant("/fixture/project/AGENTS.md", PathScope::Exact)];
            plan.secret_roots = vec![grant("/fixture/project/secrets", PathScope::Subtree)];
            plan.write_roots = vec![grant("/fixture/project/.agent-run", PathScope::Subtree)];
        },
    )?);
    for (name, access, network) in [
        (
            "network-open-read-only",
            Access::ReadOnly,
            NetworkAccess::Open,
        ),
        (
            "network-local-read-only",
            Access::ReadOnly,
            NetworkAccess::Local,
        ),
        (
            "network-open-worktree-write",
            Access::WorktreeWrite,
            NetworkAccess::Open,
        ),
    ] {
        cases.push(Case::new(
            name,
            "opencode",
            access,
            Some("login"),
            false,
            |plan| {
                plan.network = network;
            },
        )?);
    }
    Ok(cases)
}

#[test]
fn open_network_grants_gh_reads_independently_of_publishing()
-> Result<(), Box<dyn std::error::Error>> {
    let (mut plan, profile) =
        prepared("codex", Access::WorktreeWrite, login("codex"), false, None)?;
    for network in [
        NetworkAccess::Provider,
        NetworkAccess::Local,
        NetworkAccess::Open,
    ] {
        plan.network = network;
        let rendered = render(&plan, &profile, Path::new(HOME))?;
        for path in [".config/gh", "Library/Application Support/GitHub CLI"] {
            let path = Path::new(HOME).join(path);
            let read = format!("(allow file-read* (subpath \"{}\"))", path.display());
            let deny = format!("(deny file-read* (subpath \"{}\"))", path.display());
            assert_eq!(
                rendered.contains(&read),
                network == NetworkAccess::Open,
                "writer gh read grant"
            );
            assert_eq!(
                rendered.contains(&deny),
                network != NetworkAccess::Open,
                "writer gh read denial"
            );
            assert!(rendered.contains(&format!(
                "(deny file-write* (subpath \"{}\"))",
                path.display()
            )));
        }
    }
    if let provenant_confinement::permissions::AccessMode::WorktreeWrite(authority) = &mut plan.mode
    {
        authority.set_publishing(false);
    }
    let disabled = render(&plan, &profile, Path::new(HOME))?;
    assert!(!disabled.contains("(deny file-read* (subpath \"/fixture/home/.config/gh\"))"));
    assert!(disabled.contains("(allow file-read* (subpath \"/fixture/home/.config/gh\"))"));
    Ok(())
}

#[test]
fn fabric_profiles_only_read_database_and_allow_only_the_attempt_socket()
-> Result<(), Box<dyn std::error::Error>> {
    for case in cases()? {
        if case.profile.spec.fabric_socket.is_none() {
            assert!(case.profile.spec.fabric_state_reads.is_empty());
            assert!(
                case.profile
                    .spec
                    .runtime_reads
                    .iter()
                    .all(|grant| !grant.path.starts_with("/fixture/state/fabric"))
            );
            continue;
        }
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let path = format!("/fixture/state/fabric/fabric.sqlite3{suffix}");
            assert!(
                case.profile
                    .spec
                    .runtime_reads
                    .contains(&grant(&path, PathScope::Exact)),
                "{}: {path}",
                case.name
            );
        }
        let fabric = Path::new("/fixture/state/fabric");
        for write in case
            .plan
            .write_roots
            .iter()
            .chain(&case.profile.spec.state_writes)
            .chain(&case.profile.spec.tool_writes)
            .chain(
                case.profile
                    .spec
                    .authentication
                    .iter()
                    .flat_map(|auth| &auth.in_place_writes),
            )
        {
            assert!(
                !(write.path == fabric
                    || write.path.starts_with(fabric)
                    || (write.scope == PathScope::Subtree && fabric.starts_with(&write.path))),
                "{}: write grant {:?}",
                case.name,
                write
            );
        }
        let socket = case
            .profile
            .spec
            .fabric_socket
            .as_ref()
            .ok_or("missing socket")?;
        assert_eq!(socket.scope, PathScope::Exact);
        assert!(socket.path.ends_with("fabric-relay.sock"));
    }
    Ok(())
}

#[test]
fn cargo_permissions_follow_the_declared_access_for_each_profile_case()
-> Result<(), Box<dyn std::error::Error>> {
    let cargo = CargoHome::resolve(Path::new(HOME), |_| None)?;
    assert!(
        cargo
            .tracking()
            .all(|grant| grant.scope == PathScope::Exact)
    );
    let expected_writes: BTreeSet<_> = cargo
        .writes()
        .map(|grant| match grant.scope {
            PathScope::Exact => {
                format!("(allow file-write* (literal \"{}\"))", grant.path.display())
            }
            PathScope::Subtree => {
                format!("(allow file-write* (subpath \"{}\"))", grant.path.display())
            }
        })
        .collect();

    for case in cases()? {
        let rendered = render(&case.plan, &case.profile, Path::new(HOME))?;
        let cargo_home_path = cargo.path.to_string_lossy();
        let writes: BTreeSet<_> = rendered
            .lines()
            .filter(|line| {
                line.starts_with("(allow file-write") && line.contains(cargo_home_path.as_ref())
            })
            .map(str::to_owned)
            .collect();
        assert_eq!(writes, expected_writes, "{}: Cargo write rules", case.name);
        for credential in cargo.credentials() {
            let path = credential.path.display();
            assert!(
                rendered.contains(&format!("(deny file-read* (literal \"{path}\"))")),
                "{}: Cargo credential read: {path}",
                case.name
            );
            assert!(
                rendered.contains(&format!("(deny file-write* (literal \"{path}\"))")),
                "{}: Cargo credential write: {path}",
                case.name
            );
        }
        if let provenant_confinement::permissions::AccessMode::WorktreeWrite(authority) =
            &case.plan.mode
        {
            let cache = authority.cargo_cache().path.display();
            assert!(
                rendered.contains(&format!("(allow file-read* (subpath \"{cache}\"))")),
                "{}: private Cargo cache read: {cache}",
                case.name
            );
            assert!(
                rendered.contains(&format!("(allow file-write* (subpath \"{cache}\"))")),
                "{}: private Cargo cache write: {cache}",
                case.name
            );
        }
    }
    Ok(())
}

// Spec: docs/specs/routing.md#provider-configuration-and-local-diagnostics
#[test]
fn provider_profiles_keep_open_defaults_explicit_network_and_mandatory_boundaries()
-> Result<(), Box<dyn std::error::Error>> {
    for entry in REGISTRY {
        let id = entry.adapter.id();
        for network in [
            NetworkAccess::Open,
            NetworkAccess::Provider,
            NetworkAccess::Local,
        ] {
            let (plan, profile) = prepared(id, Access::ReadOnly, login(id), false, Some(network))?;
            let rendered = render(&plan, &profile, Path::new(HOME))?;
            assert!(rendered.contains("(allow default)"), "{id}");
            assert_eq!(
                rendered.contains("(deny network*)"),
                network != NetworkAccess::Open,
                "{id}"
            );
            let credential = Path::new(HOME).join(".ssh");
            for operation in ["file-read*", "file-write*"] {
                assert!(
                    rendered.contains(&format!(
                        "(deny {operation} (subpath \"{}\"))",
                        credential.display()
                    )),
                    "{id}: {operation} credential boundary"
                );
            }
            for root in &plan.read_roots {
                assert!(
                    rendered.contains(&format!(
                        "(deny file-write* (subpath \"{}\"))",
                        root.path.display()
                    )),
                    "{id}: read-only root"
                );
            }
        }
    }
    Ok(())
}
