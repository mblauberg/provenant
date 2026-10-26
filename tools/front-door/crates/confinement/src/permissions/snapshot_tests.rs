//! Byte-for-byte golden tests for the rendered Seatbelt profiles.
//!
//! Regenerate with `PROVENANT_UPDATE_SNAPSHOTS=1 cargo test -p provenant-confinement seatbelt_profiles`.
use super::*;

const UPDATE_ENV: &str = "PROVENANT_UPDATE_SNAPSHOTS";
const HOME: &str = "/fixture/home";

fn snapshot_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots/seatbelt")
}

fn exact(path: &str) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope: PathScope::Exact,
    }
}

fn subtree(path: &str) -> PathGrant {
    PathGrant {
        path: path.into(),
        scope: PathScope::Subtree,
    }
}

fn https() -> Vec<NetworkEndpoint> {
    vec![NetworkEndpoint {
        address: "*".into(),
        port: 443,
    }]
}

fn read_only_plan() -> PlanPermissions {
    PlanPermissions {
        read_only: None,
        mode: AccessMode::ReadOnly,
        scratch: subtree("/fixture/attempt/tmp/candidate-0"),
        private_state: None,
        read_roots: Vec::new(),
        protected_roots: Vec::new(),
        secret_roots: Vec::new(),
        selected_authentication: None,
        include_continuation: false,
        write_roots: Vec::new(),
        network: NetworkAccess::Provider,
        nested_confinement: false,
    }
}

fn writer_plan() -> PlanPermissions {
    PlanPermissions {
        mode: AccessMode::WorktreeWrite(Box::new(WorktreeWriteAuthority {
            git_grants_digest: [7; 32],
            cargo_cache: subtree("/fixture/cache/lane"),
        })),
        ..read_only_plan()
    }
}

fn git_grants() -> WorktreeWriteGrants {
    let git = |name: &str| exact(&format!("/fixture/repo/.git/{name}"));
    let private = |name: &str| exact(&format!("/fixture/repo/.git/worktrees/lane/{name}"));
    WorktreeWriteGrants {
        worktree: subtree("/fixture/worktrees/lane"),
        private_git_dir: subtree("/fixture/repo/.git/worktrees/lane"),
        common_git_dir: subtree("/fixture/repo/.git"),
        index: ["index", "index.lock"].map(private),
        next_index: private(NEXT_INDEX_SAMPLE),
        common_exclude: git("info/exclude"),
        fetch_heads: [private("FETCH_HEAD"), git("FETCH_HEAD")],
        packed_refs_lock: git("packed-refs.lock"),
        head: [
            "HEAD",
            "HEAD.lock",
            "logs/HEAD",
            "logs/HEAD.lock",
            "COMMIT_EDITMSG",
            "AUTO_MERGE",
            "AUTO_MERGE.lock",
            "MERGE_HEAD",
            "MERGE_MSG",
            "MERGE_MODE",
            "ORIG_HEAD",
            "ORIG_HEAD.lock",
            "MERGE_RR",
            "MERGE_RR.lock",
        ]
        .map(private),
        objects: subtree("/fixture/repo/.git/objects"),
        selected_ref: [
            "refs/heads/lane",
            "refs/heads/lane.lock",
            "logs/refs/heads/lane",
            "logs/refs/heads/lane.lock",
        ]
        .map(git),
    }
}

/// The Provenant lane's own grants, as `lane.rs` adds them to every adapter.
fn with_lane_grants(spec: &mut ProviderProfileSpec, hooks: bool) {
    spec.process_executables
        .extend([exact("/fixture/bin/provenant"), exact("/fixture/bin/git")]);
    spec.runtime_reads.extend([
        exact("/fixture/bin/provenant"),
        exact("/fixture/state/config.toml"),
        subtree("/fixture/state/projects"),
    ]);
    spec.state_writes.extend([
        subtree("/fixture/state/child"),
        subtree("/fixture/state/fabric/locks"),
        exact("/fixture/state/fabric/fabric.sqlite3"),
    ]);
    if hooks {
        spec.process_executables
            .extend(["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash"].map(exact));
        spec.state_writes.push(subtree("/fixture/state/workflow"));
    }
}

/// An adapter with nothing but an executable, fork and the lane's grants.
fn lane_only() -> ProviderProfileSpec {
    let mut spec = ProviderProfileSpec {
        process_executables: vec![exact("/fixture/bin/adapter")],
        allow_process_fork: true,
        provider_api: https(),
        ..ProviderProfileSpec::default()
    };
    with_lane_grants(&mut spec, false);
    spec
}

fn claude(shell: bool) -> ProviderProfileSpec {
    let mut spec = ProviderProfileSpec {
        process_executables: vec![exact("/fixture/home/.local/bin/claude")],
        allow_process_fork: true,
        deny_keychain_access: shell,
        tool_exec_roots: if shell {
            vec![subtree("/bin"), subtree("/usr/bin")]
        } else {
            Vec::new()
        },
        tool_writes: if shell {
            vec![subtree("/fixture/home/.cargo/registry")]
        } else {
            Vec::new()
        },
        runtime_reads: vec![exact("/fixture/home/.claude/settings.json")],
        state_writes: if shell {
            Vec::new()
        } else {
            vec![
                exact("/fixture/home/.claude/projects/fixture/session.jsonl"),
                subtree("/fixture/home/.claude/projects/fixture/session"),
            ]
        },
        provider_api: https(),
        ..ProviderProfileSpec::default()
    };
    with_lane_grants(&mut spec, true);
    spec
}

fn codex() -> ProviderProfileSpec {
    let credential = exact("/fixture/home/.codex/auth.json");
    ProviderProfileSpec {
        process_executables: vec![exact("/fixture/home/.codex/bin/codex")],
        allow_process_fork: true,
        runtime_reads: vec![subtree("/fixture/home/.codex/packages/standalone")],
        authentication: vec![AuthenticationProfile {
            mechanism: "login".into(),
            roots: vec![credential.clone()],
            in_place_writes: vec![credential],
        }],
        tool_exec_roots: vec![subtree("/bin"), subtree("/usr/bin")],
        tool_writes: vec![subtree("/fixture/home/.cargo/registry")],
        provider_api: https(),
        ..ProviderProfileSpec::default()
    }
}

fn cursor() -> ProviderProfileSpec {
    let package = "/fixture/home/.local/share/cursor-agent/versions/1";
    ProviderProfileSpec {
        process_executables: vec![exact("/fixture/home/.local/bin/cursor-agent")],
        allow_process_fork: true,
        runtime_reads: vec![subtree(package)],
        authentication: vec![AuthenticationProfile {
            mechanism: "login".into(),
            roots: vec![exact("/fixture/home/.cursor/cli-config.json")],
            in_place_writes: Vec::new(),
        }],
        state_writes: vec![subtree("/fixture/home/.cursor/chats")],
        tool_exec_roots: vec![subtree("/bin"), subtree("/usr/bin"), subtree(package)],
        provider_api: https(),
        ..ProviderProfileSpec::default()
    }
}

fn opencode() -> ProviderProfileSpec {
    ProviderProfileSpec {
        process_executables: vec![exact("/fixture/home/.opencode/bin/opencode")],
        allow_process_fork: true,
        runtime_reads: vec![exact("/fixture/home/.config/opencode/opencode.json")],
        state_writes: vec![subtree("/fixture/home/.local/share/opencode")],
        discovery_reads: ["/fixture/worktrees/lane", "/fixture/worktrees", "/fixture"]
            .iter()
            .flat_map(|parent| {
                [".agents", ".claude", ".codex"].map(|name| exact(&format!("{parent}/{name}")))
            })
            .collect(),
        provider_api: https(),
        loopback_server: true,
        ..ProviderProfileSpec::default()
    }
}

fn agy() -> ProviderProfileSpec {
    ProviderProfileSpec {
        process_executables: vec![
            exact("/fixture/home/.local/bin/agy"),
            exact("/usr/bin/security"),
        ],
        allow_process_fork: true,
        allow_pseudo_tty: true,
        authentication: vec![AuthenticationProfile {
            mechanism: "keychain".into(),
            roots: vec![exact("/fixture/home/Library/Keychains/login.keychain-db")],
            in_place_writes: Vec::new(),
        }],
        provider_api: https(),
        loopback_server: true,
        ..ProviderProfileSpec::default()
    }
}

struct Case {
    name: &'static str,
    plan: PlanPermissions,
    spec: ProviderProfileSpec,
    /// What `compile_sbpl` gathers for the profile's read grants. Continuation
    /// ("context") reads are in here when the plan includes them.
    reads: Vec<PathGrant>,
}

impl Case {
    fn new(name: &'static str, plan: PlanPermissions, spec: ProviderProfileSpec) -> Self {
        Self {
            name,
            plan,
            spec,
            reads: Vec::new(),
        }
    }

    fn reads(mut self, reads: &[&str]) -> Self {
        self.reads = reads.iter().map(|path| subtree(path)).collect();
        self
    }

    fn render(&self) -> Result<String, PermissionError> {
        let profile = ProviderProfile {
            spec: self.spec.clone(),
            observations: BTreeMap::new(),
        };
        // The selected mechanism's in-place writes, as `compile_sbpl` picks.
        let credential_writes: Vec<PathGrant> = profile
            .spec
            .authentication
            .iter()
            .filter(|item| self.plan.selected_authentication.as_ref() == Some(&item.mechanism))
            .flat_map(|item| item.in_place_writes.iter().cloned())
            .collect();
        let git = git_grants();
        let worktree = matches!(self.plan.mode, AccessMode::WorktreeWrite(_)).then_some(&git);
        let bytes = render_profile(
            &self.plan,
            &profile,
            &self.reads,
            &credential_writes,
            worktree,
            Path::new(HOME),
        )?;
        String::from_utf8(bytes).map_err(|_| PermissionError::InvalidFixture)
    }
}

fn cases() -> Vec<Case> {
    let selected = |plan: PlanPermissions, mechanism: &str| PlanPermissions {
        selected_authentication: Some(mechanism.into()),
        ..plan
    };
    let network = |plan: PlanPermissions, network| PlanPermissions { network, ..plan };
    let project = PlanPermissions {
        read_only: Some(crate::plan::ReadOnlyGrants::for_test(
            "/fixture/project".into(),
            vec![subtree("/fixture/project/.agent-run")],
        )),
        private_state: Some(subtree("/fixture/attempt/private")),
        read_roots: vec![subtree("/fixture/reference")],
        protected_roots: vec![exact("/fixture/project/AGENTS.md")],
        secret_roots: vec![subtree("/fixture/project/secrets")],
        write_roots: vec![subtree("/fixture/project/locks")],
        ..read_only_plan()
    };
    let writer_roots = PlanPermissions {
        private_state: Some(subtree("/fixture/attempt/private")),
        protected_roots: vec![exact("/fixture/project/AGENTS.md")],
        secret_roots: vec![subtree("/fixture/project/secrets")],
        write_roots: vec![subtree("/fixture/project/.agent-run")],
        ..writer_plan()
    };
    vec![
        // Each lane access mode, with only the lane's own provider grants.
        Case::new("read-only-bare", read_only_plan(), lane_only()),
        Case::new("read-only-project", project, lane_only()),
        Case::new("worktree-write-bare", writer_plan(), lane_only()),
        Case::new("worktree-write-roots", writer_roots, lane_only()),
        // Each network level.
        Case::new(
            "network-local",
            network(read_only_plan(), NetworkAccess::Local),
            lane_only(),
        ),
        Case::new(
            "network-open",
            network(writer_plan(), NetworkAccess::Open),
            lane_only(),
        ),
        // Context (continuation) grants off and on.
        Case::new("claude-context-off", read_only_plan(), claude(false)),
        Case::new("claude-context-on", read_only_plan(), claude(false)).reads(&[
            "/fixture/home/.claude/projects/fixture",
            "/fixture/home/.claude/history",
        ]),
        // Each adapter's grant variant.
        Case::new("claude-shell-worktree-write", writer_plan(), claude(true)),
        Case::new(
            "codex-worktree-write",
            selected(writer_plan(), "login"),
            codex(),
        ),
        Case::new("codex-no-auth-read-only", read_only_plan(), codex()),
        Case::new(
            "cursor-read-only",
            selected(read_only_plan(), "login"),
            cursor(),
        ),
        Case::new("cursor-worktree-write", writer_plan(), cursor()),
        Case::new("opencode-read-only", read_only_plan(), opencode()),
        Case::new(
            "opencode-worktree-write",
            network(writer_plan(), NetworkAccess::Local),
            opencode(),
        ),
        Case::new(
            "agy-read-only",
            selected(read_only_plan(), "keychain"),
            agy(),
        ),
    ]
}

#[test]
fn seatbelt_profiles_match_snapshots() {
    let update = std::env::var(UPDATE_ENV).is_ok_and(|value| value == "1");
    let dir = snapshot_dir();
    let mut failures = Vec::new();
    let mut names = BTreeSet::new();
    for case in cases() {
        assert!(names.insert(case.name), "duplicate case {}", case.name);
        let rendered = case.render().expect("render");
        let path = dir.join(format!("{}.sb", case.name));
        if update {
            std::fs::create_dir_all(&dir).expect("snapshot directory");
            std::fs::write(&path, &rendered).expect("write snapshot");
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
    for entry in std::fs::read_dir(&dir).expect("snapshot directory") {
        let path = entry.expect("snapshot entry").path();
        let stem = path.file_stem().and_then(OsStr::to_str).unwrap_or_default();
        if names.contains(stem) {
            continue;
        }
        if update {
            std::fs::remove_file(&path).expect("remove stale snapshot");
        } else {
            failures.push(format!("{}: no case renders this snapshot", path.display()));
        }
    }
    assert!(
        failures.is_empty(),
        "Seatbelt profile snapshots differ (set {UPDATE_ENV}=1 to regenerate):\n{}",
        failures.join("\n")
    );
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
