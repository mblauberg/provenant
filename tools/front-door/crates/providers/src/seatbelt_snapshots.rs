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
        cases.push(Case::new(name, "opencode", access, None, false, |plan| {
            plan.network = network;
        })?);
    }
    Ok(cases)
}

#[test]
fn writer_publish_grants_gh_reads_only_with_open_network() -> Result<(), Box<dyn std::error::Error>>
{
    let (mut plan, profile) = prepared("codex", Access::WorktreeWrite, login("codex"), false)?;
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
    assert!(disabled.contains("(deny file-read* (subpath \"/fixture/home/.config/gh\"))"));
    assert!(!disabled.contains("(allow file-read* (subpath \"/fixture/home/.config/gh\"))"));
    Ok(())
}

#[test]
fn seatbelt_profiles_match_snapshots() -> Result<(), Box<dyn std::error::Error>> {
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

#[cfg(target_os = "macos")]
#[test]
fn agy_read_only_compiled_profile_denies_read_root_write() -> Result<(), Box<dyn std::error::Error>>
{
    use provenant_confinement::permissions::snapshot::compile;
    use provenant_confinement::permissions::{AnchoredDirectory, Anchors, required_anchors};
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    for name in ["repo", "scratch", "state", "Library/Keychains", "bin"] {
        std::fs::create_dir_all(root.join(name))?;
    }
    for name in ["scratch", "state"] {
        std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o700))?;
    }
    let executable = root.join("bin/agy");
    std::fs::write(&executable, "fixture")?;
    let repo = root.join("repo");
    std::fs::write(repo.join("input"), "readable")?;
    let plan = super::tests::read_only_plan(root.to_str().unwrap(), Some("keychain"));
    let route = route("agy", "fixture-model");
    let prompt = Prompt::new("fixture".into());
    let input = PrepareInput {
        executable: &executable,
        home: &root,
        ..fixture_input(&route, &plan, &prompt, &repo, None)
    };
    let adapter = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == "agy")
        .unwrap();
    let spec = adapter.adapter.prepare(input)?;
    let directories = required_anchors(plan.permissions(), &spec.profile, None)
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &root,
        directories: &directories,
        worktree: None,
    };
    let compiled = compile(plan.permissions(), &spec.profile, &anchors)?;
    let source = std::str::from_utf8(&compiled.profile)?;
    assert!(source.contains("(deny default)"));
    assert!(source.contains(&format!(
        "(allow file-read* (subpath \"{}\"))",
        repo.display()
    )));
    assert!(source.contains("(allow process-exec (subpath \"/bin\"))"));
    // Deny-by-default leaves the read root unwritable. Check every writable
    // subtree, including ancestors, rather than only the root's exact line.
    for line in source
        .lines()
        .filter(|line| line.starts_with("(allow file-write* (subpath "))
    {
        let path = line
            .strip_prefix("(allow file-write* (subpath \"")
            .unwrap()
            .strip_suffix("\"))")
            .unwrap();
        assert!(
            !repo.starts_with(path) && !Path::new(path).starts_with(&repo),
            "read root is writable via {line}"
        );
    }
    Ok(())
}

#[cfg(target_os = "macos")]
#[test]
#[allow(clippy::disallowed_methods)]
fn agy_read_only_linked_worktree_git_directories_are_not_writable()
-> Result<(), Box<dyn std::error::Error>> {
    use provenant_confinement::permissions::snapshot::compile;
    use provenant_confinement::permissions::{AnchoredDirectory, Anchors, required_anchors};
    use provenant_confinement::{CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest};
    use provenant_core::{ProjectId, WorkScope};
    use provenant_os::exec::{ExecOptions, Executor, SystemExecutor, Termination};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    let temp = tempfile::tempdir()?;
    let root = temp.path().canonicalize()?;
    for name in ["repo", "scratch", "state", "Library/Keychains", "bin"] {
        std::fs::create_dir_all(root.join(name))?;
    }
    for name in ["scratch", "state"] {
        std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(0o700))?;
    }
    let repo = root.join("repo");
    let executor = SystemExecutor::new(1)?;
    let run_git = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
        let argv = std::iter::once("/usr/bin/git")
            .chain(args.iter().copied())
            .map(Into::into)
            .collect::<Vec<_>>();
        let output = executor.execute(ExecOptions::new(&argv, &repo, Duration::from_secs(10)))?;
        assert!(
            output.termination == Termination::Exit(0),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(())
    };
    run_git(&["init"])?;
    run_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "--allow-empty",
        "-m",
        "fixture",
    ])?;
    run_git(&["checkout", "-b", "side"])?;
    run_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "commit",
        "--allow-empty",
        "-m",
        "side",
    ])?;
    run_git(&["checkout", "-"])?;
    run_git(&[
        "-c",
        "user.name=Fixture",
        "-c",
        "user.email=fixture@example.test",
        "merge",
        "--no-ff",
        "side",
        "-m",
        "merge",
    ])?;
    let worktree = root.join("linked");
    run_git(&[
        "worktree",
        "add",
        "-b",
        "fixture",
        worktree.to_str().unwrap(),
    ])?;
    let common = repo.join(".git");
    let private = common.join("worktrees/linked");
    assert!(private.is_dir());
    let executable = root.join("bin/agy");
    std::fs::write(&executable, "fixture")?;
    let base = super::tests::read_only_plan(root.to_str().unwrap(), Some("keychain"));
    let mut permissions = base.permissions().clone();
    permissions.read_roots = vec![PathGrant {
        path: worktree.clone(),
        scope: PathScope::Subtree,
    }];
    permissions.read_only = Some(ReadOnlyGrants::for_test(repo, vec![]));
    let plan = ConfinedPlan::new(PlanRequest {
        scope: WorkScope::Project(ProjectId::from_bytes([1; 16])),
        permissions,
        codex: CodexPolicy::Clean,
        extensions: ExtensionPolicy::Deny,
        agy_state: None,
    })?;
    let route = route("agy", "fixture-model");
    let prompt = Prompt::new("fixture".into());
    let input = PrepareInput {
        executable: &executable,
        home: &root,
        git_context: true,
        ..fixture_input(&route, &plan, &prompt, &worktree, None)
    };
    let adapter = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == "agy")
        .unwrap();
    let spec = adapter.adapter.prepare(input)?;
    for step in &spec.setup {
        match step {
            SetupStep::CreateDir(path) if path.starts_with(root.join("scratch/git-objects")) => {
                std::fs::create_dir_all(path)?
            }
            SetupStep::WriteFile { path, bytes }
                if path.starts_with(root.join("scratch/git-objects")) =>
            {
                std::fs::write(path, bytes)?
            }
            _ => {}
        }
    }
    let object_dir = root.join("scratch/git-objects");
    for args in [
        &["show", "--remerge-diff", "HEAD"][..],
        &["diff", "--merge-base", "HEAD^", "HEAD"][..],
    ] {
        let output = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(&worktree)
            .env("GIT_OBJECT_DIRECTORY", &object_dir)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    // A write goes to scratch, never the host's common object store.
    let output = std::process::Command::new("/usr/bin/git")
        .args(["hash-object", "-w", "--stdin"])
        .current_dir(&worktree)
        .env("GIT_OBJECT_DIRECTORY", &object_dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    let mut child = output;
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"lane scratch object")?;
    let output = child.wait_with_output()?;
    assert!(output.status.success());
    let oid = std::str::from_utf8(&output.stdout)?.trim();
    let relative = format!("{}/{}", &oid[..2], &oid[2..]);
    assert!(object_dir.join(&relative).is_file());
    assert!(!common.join("objects").join(relative).exists());
    let directories = required_anchors(plan.permissions(), &spec.profile, None)
        .iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: &root,
        directories: &directories,
        worktree: None,
    };
    let compiled = compile(plan.permissions(), &spec.profile, &anchors)?;
    let source = std::str::from_utf8(&compiled.profile)?;
    assert!(source.contains(&format!(
        "(allow file-read* (subpath \"{}\"))",
        common.display()
    )));
    for line in source
        .lines()
        .filter(|line| line.starts_with("(allow file-write* (subpath "))
    {
        let path = line
            .strip_prefix("(allow file-write* (subpath \"")
            .unwrap()
            .strip_suffix("\"))")
            .unwrap();
        for git_dir in [&private, &common] {
            assert!(
                !git_dir.starts_with(path) && !Path::new(path).starts_with(git_dir),
                "Git directory is writable via {line}"
            );
        }
    }
    Ok(())
}
