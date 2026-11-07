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
        // Golden profiles are shared by macOS and Linux.
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
        cases.push(Case::new(name, "opencode", access, None, false, |plan| {
            plan.network = network;
        })?);
    }
    Ok(cases)
}

#[test]
fn writer_publish_grants_gh_reads_only_with_open_network() -> Result<(), Box<dyn std::error::Error>>
{
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
    assert!(disabled.contains("(deny file-read* (subpath \"/fixture/home/.config/gh\"))"));
    assert!(!disabled.contains("(allow file-read* (subpath \"/fixture/home/.config/gh\"))"));
    Ok(())
}

#[test]
fn fabric_profiles_only_read_database_and_allow_only_the_attempt_socket()
-> Result<(), Box<dyn std::error::Error>> {
    for case in cases()? {
        let rendered = render(&case.plan, &case.profile, Path::new(HOME))?;
        if case.profile.spec.fabric_socket.is_none() {
            assert!(!rendered.contains("fabric.sqlite3"));
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
            assert!(
                rendered.contains(&format!("(allow file-read* (literal \"{path}\"))")),
                "{}: {path}",
                case.name
            );
            assert!(
                !rendered
                    .lines()
                    .any(|line| line.contains("allow file-write")
                        && (line.contains("/fixture/state/fabric")
                            || line.contains("(subpath \"/fixture/state\")"))),
                "{}",
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
        assert!(rendered.contains(&format!(
            "(allow network-outbound (remote unix-socket (path-literal \"{}\")))",
            socket.path.display()
        )));
        assert!(!rendered.contains("provenant-peer.sock"));
    }
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
        let snapshot_name = if case.name == "claude-continuation-read-only" {
            "claude-read-only"
        } else {
            &case.name
        };
        let path = dir.join(format!("{snapshot_name}.sb"));
        if update && case.name != "claude-continuation-read-only" {
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
    for name in [
        "repo",
        "scratch/read-only-git",
        "state",
        "Library/Keychains",
        "bin",
    ] {
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
    let account = super::tests::agy_account_at(&root);
    let prompt = Prompt::new("fixture".into());
    let mut input = PrepareInput {
        executable: &executable,
        home: &root,
        account: Some(&account),
        ..fixture_input(&route, &plan, &prompt, &repo, None)
    };
    // Read-only plans grant the host Git, which anchors must find on disk.
    input.provenant.git = Path::new("/usr/bin/git");
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
    let clt_git = Path::new("/Library/Developer/CommandLineTools/usr/bin/git");
    let git = if clt_git.is_file() {
        clt_git
    } else {
        Path::new("/usr/bin/git")
    };
    let run_git = |args: &[&str]| -> Result<(), Box<dyn std::error::Error>> {
        let argv = std::iter::once(git.as_os_str())
            .chain(args.iter().map(std::ffi::OsStr::new))
            .map(Into::into)
            .collect::<Vec<_>>();
        let env = provenant_os::fixture::git_env();
        let mut options = ExecOptions::new(&argv, &repo, Duration::from_secs(10));
        options.env = Some(&env);
        let output = executor.execute(options)?;
        assert!(
            output.termination == Termination::Exit(0),
            "git failed: {}",
            String::from_utf8_lossy(&output.stderr.bytes)
        );
        Ok(())
    };
    run_git(&["init"])?;
    std::fs::write(repo.join("file"), "base\n")?;
    run_git(&["add", "file"])?;
    run_git(&["commit", "--allow-empty", "-m", "fixture"])?;
    run_git(&["checkout", "-b", "side"])?;
    std::fs::write(repo.join("file"), "side\n")?;
    run_git(&["add", "file"])?;
    run_git(&["commit", "--allow-empty", "-m", "side"])?;
    run_git(&["checkout", "-"])?;
    std::fs::write(repo.join("file"), "main\n")?;
    run_git(&["add", "file"])?;
    run_git(&["commit", "-m", "main"])?;
    run_git(&["merge", "--no-ff", "-s", "ours", "side", "-m", "merge"])?;
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
    let private_common = root.join("scratch/git-common");
    std::fs::create_dir_all(private_common.join("objects"))?;
    std::fs::create_dir_all(private_common.join("refs"))?;
    std::fs::copy(common.join("config"), private_common.join("config"))?;
    std::fs::copy(private.join("index"), private_common.join("index"))?;
    let head = std::process::Command::new(git)
        .current_dir(&worktree)
        .args(["rev-parse", "HEAD"])
        .output()?;
    assert!(head.status.success());
    std::fs::write(private_common.join("HEAD"), head.stdout)?;
    std::fs::create_dir_all(root.join("scratch/git-objects/info"))?;
    std::fs::write(
        root.join("scratch/git-objects/info/alternates"),
        format!("{}\n", common.join("objects").display()),
    )?;
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
    let account = super::tests::agy_account_at(&root);
    let prompt = Prompt::new("fixture".into());
    let mut input = PrepareInput {
        executable: &executable,
        home: &root,
        account: Some(&account),
        git_context: true,
        ..fixture_input(&route, &plan, &prompt, &worktree, None)
    };
    input.provenant.git = git;
    let adapter = REGISTRY
        .iter()
        .find(|entry| entry.adapter.id() == "agy")
        .unwrap();
    let spec = adapter.adapter.prepare(input)?;
    for step in &spec.setup {
        match step {
            SetupStep::CreateDir(path) if path.starts_with(root.join("scratch")) => {
                std::fs::create_dir_all(path)?
            }
            SetupStep::WriteFile { path, bytes }
                if path.starts_with(root.join("scratch/git-objects"))
                    || path == &root.join("scratch/gitconfig") =>
            {
                std::fs::write(path, bytes)?
            }
            SetupStep::WriteExecutable { path, bytes }
                if path == &root.join("scratch/bin/git")
                    || path == &root.join("scratch/read-only-git/git") =>
            {
                std::fs::write(path, bytes)?;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
            }
            _ => {}
        }
    }
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
    let sandboxed =
        |argv: &[std::ffi::OsString]| -> Result<std::process::Command, Box<dyn std::error::Error>> {
            let command =
                compiled.command(argv, &provenant_core::AttemptId::from_bytes([1; 16]))?;
            let mut process = std::process::Command::new(&command[0]);
            process
                .args(&command[1..])
                .current_dir(&worktree)
                .env_clear()
                .envs(spec.env.iter().cloned());
            Ok(process)
        };
    for args in [&["log", "-1"][..], &["diff", "HEAD~1"][..]] {
        let argv: Vec<std::ffi::OsString> =
            std::iter::once(root.join("scratch/bin/git").into_os_string())
                .chain(args.iter().map(Into::into))
                .collect();
        let output = sandboxed(&argv)?.output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(worktree.join("file"), "changed\n")?;
    let output = sandboxed(&[git.into(), "add".into(), "file".into()])?.output()?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let input = root.join("scratch/object-input");
    std::fs::write(&input, b"unique lane object")?;
    let output =
        sandboxed(&[git.into(), "hash-object".into(), "-w".into(), input.into()])?.output()?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("Operation not permitted"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
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
