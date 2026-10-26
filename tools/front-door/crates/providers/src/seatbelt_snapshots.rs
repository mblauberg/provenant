//! Byte-for-byte golden tests for the Seatbelt profiles the adapters produce.
//!
//! Each profile comes from the adapter's real `prepare`, including the lane's
//! own grants, under fixed fake paths. Regenerate with
//! `PROVENANT_UPDATE_SNAPSHOTS=1 cargo test -p provenant-providers seatbelt_profiles`.
use super::tests::{fixture_access_plan, fixture_input, fixture_token, route};
use super::*;
use provenant_confinement::permissions::snapshot::render;
use provenant_confinement::permissions::{
    NetworkAccess, PathGrant, PathScope, PlanPermissions, ProviderProfile,
};
use provenant_confinement::plan::ReadOnlyGrants;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const UPDATE_ENV: &str = "PROVENANT_UPDATE_SNAPSHOTS";
const HOME: &str = "/fixture/home";

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/seatbelt")
}

fn grant(path: &str, scope: PathScope) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope,
    }
}

/// Grants `prepare` adds only when the host has the path (the macOS-only SSL
/// and Command Line Tools roots, a Homebrew or `/usr/local` prefix). They would
/// make the snapshots differ by machine, so the tests drop them.
fn host_probed(path: &Path) -> bool {
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
                .any(|name| path == Path::new(prefix).join(name))
        })
}

/// An adapter's permissions and profile from its real `prepare`.
fn prepared(
    id: &str,
    access: Access,
    authentication: Option<&str>,
    include_continuation: bool,
) -> Result<(PlanPermissions, ProviderProfile), ProviderError> {
    let confined = fixture_access_plan(access, authentication, include_continuation);
    let mut route = route(id, "fixture-model");
    route.account_key = None;
    let prompt = Prompt::new("fixture prompt".into());
    let token = fixture_token();
    let entry = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == id)
        .ok_or(ProviderError::UnknownIdentity)?;
    let spec = entry.adapter.prepare(fixture_input(
        &route,
        &confined,
        &prompt,
        Path::new("/fixture/repo"),
        (id == "claude").then_some(&token),
    ))?;
    let mut profile = spec.profile;
    for grants in [
        &mut profile.spec.runtime_reads,
        &mut profile.spec.tool_exec_roots,
    ] {
        grants.retain(|grant| !host_probed(&grant.path));
    }
    Ok((confined.permissions().clone(), profile))
}

/// The credential mechanism each adapter's fixture plan selects.
fn login(id: &str) -> Option<&'static str> {
    match id {
        "codex" | "cursor" => Some("login"),
        "agy" => Some("keychain"),
        _ => None,
    }
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
        let (mut plan, profile) = prepared(id, access, authentication, include_continuation)?;
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
        cases.push(Case::new(name, "opencode", access, None, false, |plan| {
            plan.network = network;
        })?);
    }
    Ok(cases)
}

#[test]
fn seatbelt_profiles_match_snapshots() -> Result<(), Box<dyn std::error::Error>> {
    // OpenCode's grants follow these when set, so the snapshots would not hold.
    for key in ["XDG_CONFIG_HOME", "XDG_DATA_HOME"] {
        assert!(
            std::env::var_os(key).is_none(),
            "unset {key} to run the Seatbelt snapshots"
        );
    }
    let update = std::env::var(UPDATE_ENV).is_ok_and(|value| value == "1");
    let dir = snapshot_dir();
    let mut failures = Vec::new();
    let mut names = BTreeSet::new();
    for case in cases()? {
        assert!(
            names.insert(case.name.clone()),
            "duplicate case {}",
            case.name
        );
        let rendered = render(&case.plan, &case.profile, Path::new(HOME))?;
        let path = dir.join(format!("{}.sb", case.name));
        if update {
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&path, &rendered)?;
            continue;
        }
        match std::fs::read(&path) {
            Ok(expected) if expected == rendered.as_bytes() => {}
            Ok(expected) => failures.push(format!(
                "{}: {}",
                case.name,
                first_difference(&String::from_utf8_lossy(&expected), &rendered)
            )),
            Err(error) => failures.push(format!("{}: unreadable snapshot: {error}", case.name)),
        }
    }
    // A snapshot no case renders is stale and must be deleted.
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        let stem = path
            .file_stem()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default();
        if names.contains(stem) {
            continue;
        }
        if update {
            std::fs::remove_file(&path)?;
        } else {
            failures.push(format!("{}: no case renders this snapshot", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "Seatbelt profile snapshots differ (set {UPDATE_ENV}=1 to regenerate):\n{}",
        failures.join("\n")
    );
    Ok(())
}

fn first_difference(expected: &str, actual: &str) -> String {
    let mut expected = expected.lines();
    let mut actual = actual.lines();
    for number in 1.. {
        match (expected.next(), actual.next()) {
            (Some(left), Some(right)) if left == right => {}
            (None, None) => return "differs only in line endings".into(),
            (left, right) => {
                return format!("line {number}: expected {left:?}, rendered {right:?}");
            }
        }
    }
    unreachable!()
}
