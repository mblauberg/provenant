//! Activation recorder: runs one provider CLI live under the compiled Rust
//! confinement profile (fresh, then resume), probes every claimed permission
//! observation under the same file and network rules, and with `--write`
//! stores redacted evidence under `tests/fixtures/providers/<adapter>/`.
//!
//! cargo run -p provenant-providers --features recording --example record -- \
//!     <adapter> <read_only|worktree_write> --model MODEL [--effort E] --root DIR [--write]
//!
//! `--root` must be an empty canonical directory. The prompt is fixed and
//! trivial. Provider credentials are opened only by the CLIs themselves; the
//! probes read zero bytes from them.
#![expect(
    clippy::disallowed_methods,
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "developer recorder: sets up a probe repository and reports to the terminal"
)]
use provenant_confinement::permissions::{
    AccessMode, AnchoredDirectory, Anchors, BackendContext, FixtureOperation, FixtureOutcome,
    NetworkAccess, PathGrant, PathScope, PlanPermissions, ProviderProfile, WorktreeWriteAuthority,
    required_anchors, system_backend,
};
use provenant_confinement::repo::GitRefName;
use provenant_confinement::{
    CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest, UtilityBudget, UtilityRuntime,
    verify_worktree,
};
use provenant_core::{CancelToken, ProjectId, WorkScope};
use provenant_os::{
    exec::{ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination},
    fs::Directory,
};
use provenant_providers::claude::{Login, refresh_for_account};
use provenant_providers::{
    Adapter, ExitObservation, LaunchSpec, OutcomeStatus, PrepareInput, Prompt, REGISTRY, SetupStep,
    Stdin,
};
use provenant_records::ResolvedRoute;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const PROMPT: &str = "Reply with the word ok";
const RESUME_PROMPT: &str = "Reply with the word ok again";
const SHELL_PROOF_COMMAND: &str = r#"test -z "${CLAUDE_CODE_OAUTH_TOKEN+x}" && test -z "${CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR+x}" && printf provenant-shell-proof"#;
const SHELL_RESUME_PROMPT: &str = "Use Bash to run git status --short. Then make a separate Bash call running test -z \"${CLAUDE_CODE_OAUTH_TOKEN+x}\" && test -z \"${CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR+x}\" && printf provenant-shell-proof. Reply with the word ok again";
/// Writers must prove an in-sandbox tool edit, not just a reply.
const WRITER_PROMPT: &str = "Create the file note.txt in the current directory containing the word ok, then reply with the word ok";
/// Agy's headless mode ends the turn on a denied command, so its writer is
/// told to edit without a shell.
const AGY_WRITER_PROMPT: &str = "Using only your file-writing tool, with no shell commands and no listing or reading of other files, create the file note.txt in the current directory containing the word ok, then reply with the word ok";
const SHELL_WRITER_PROMPT: &str = "Use Bash to run printf ok > note.txt in the current directory and git status --short. Then make a separate Bash call running test -z \"${CLAUDE_CODE_OAUTH_TOKEN+x}\" && test -z \"${CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR+x}\" && printf provenant-shell-proof. Reply with the word ok";
const BRANCH: &str = "provenant-probe";
const PROBE_TOOLS: [&str; 7] = [
    "/bin/ln",
    "/bin/cat",
    "/usr/bin/touch",
    "/bin/dd",
    "/usr/bin/xargs",
    "/usr/bin/true",
    "/usr/bin/nc",
];

struct Args {
    adapter: String,
    writer: bool,
    model: String,
    effort: Option<String>,
    root: PathBuf,
    write: bool,
    skip_live: bool,
    executable: PathBuf,
    provenant: PathBuf,
    assets: PathBuf,
    endpoint: String,
}

fn args() -> Result<Args> {
    let mut raw = std::env::args().skip(1);
    let adapter = raw.next().ok_or("adapter")?;
    let writer = match raw.next().as_deref() {
        Some("read_only") => false,
        Some("worktree_write") => true,
        _ => return Err("access mode".into()),
    };
    let mut flags = BTreeMap::new();
    let mut write = false;
    let mut skip_live = false;
    while let Some(flag) = raw.next() {
        if flag == "--skip-live" {
            skip_live = true;
        } else if flag == "--write" {
            write = true;
        } else {
            flags.insert(flag, raw.next().ok_or("flag value")?);
        }
    }
    if write && skip_live {
        return Err("--write needs live evidence".into());
    }
    if adapter == "claude" && flags.remove("--account").as_deref() != Some("pro") {
        return Err("Claude recording requires --account pro and chair quota approval; no other identity is supported".into());
    }
    let (binary, endpoint) = match adapter.as_str() {
        "codex" => ("codex", "api.openai.com"),
        "claude" => ("claude", "api.anthropic.com"),
        "opencode" => ("opencode", "opencode.ai"),
        "agy" => ("agy", "generativelanguage.googleapis.com"),
        _ => return Err("no recorder form for this adapter".into()),
    };
    let found = which(binary).ok_or("executable not on PATH")?;
    Ok(Args {
        adapter,
        writer,
        model: flags.remove("--model").ok_or("--model")?,
        effort: flags.remove("--effort"),
        root: std::fs::canonicalize(flags.remove("--root").ok_or("--root")?)?,
        write,
        skip_live,
        executable: std::fs::canonicalize(found)?,
        provenant: std::fs::canonicalize(
            flags
                .remove("--provenant")
                .map(PathBuf::from)
                .or_else(|| which("provenant"))
                .ok_or("--provenant or provenant on PATH")?,
        )?,
        assets: std::fs::canonicalize(
            flags
                .remove("--assets")
                .map(PathBuf::from)
                .unwrap_or(std::env::current_dir()?),
        )?,
        endpoint: endpoint.into(),
    })
}

fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    })
}

/// Creates `path` and missing parents by descriptor-relative traversal from
/// `/`, never following a symbolic link. Only directories created here get
/// mode 0700; existing ones are never chmodded, so a swapped entry cannot
/// redirect either step.
fn private_dir(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err("private directory must be absolute".into());
    }
    let mut dir = Directory::root(true)?;
    for component in path.components() {
        let name = match component {
            Component::RootDir => continue,
            Component::Normal(name) => name,
            _ => return Err("private directory must be normalised".into()),
        };
        dir = match dir.open_directory(name) {
            Ok(child) => child,
            Err(error) if error.is_not_found() => {
                dir.mkdir(name, 0o700)?;
                dir.open_directory(name)?
            }
            Err(error) => return Err(error.into()),
        };
    }
    Ok(())
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?)
}

fn subtree(path: &Path) -> PathGrant {
    PathGrant {
        path: path.to_owned(),
        scope: PathScope::Subtree,
    }
}

fn exact(path: &Path) -> PathGrant {
    PathGrant {
        path: path.to_owned(),
        scope: PathScope::Exact,
    }
}

fn git(cwd: &Path, home: &Path, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/opt/homebrew/bin:/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()?;
    if !status.success() {
        return Err(format!("git {args:?} failed").into());
    }
    Ok(())
}

struct Layout {
    home_git: PathBuf,
    repo: PathBuf,
    worktree: PathBuf,
    outside: PathBuf,
    outside_bin: PathBuf,
    secret: PathBuf,
    state: PathBuf,
}

fn layout(root: &Path, writer: bool) -> Result<Layout> {
    let layout = Layout {
        home_git: root.join("git-home"),
        repo: root.join("repo"),
        worktree: root.join("worktree"),
        outside: root.join("outside"),
        outside_bin: root.join("outside-bin"),
        // Plans refuse secrets overlapping any read or write grant.
        secret: root.join("secret"),
        state: root.join("state"),
    };
    private_dir(&layout.home_git)?;
    private_dir(&layout.state)?;
    for dir in [
        &layout.repo,
        &layout.outside,
        &layout.outside_bin,
        &layout.secret,
    ] {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(layout.outside.join("file"), "outside")?;
    std::fs::copy("/usr/bin/true", layout.outside_bin.join("true"))?;
    std::fs::write(layout.repo.join("AGENTS.md"), "Protected instructions.\n")?;
    std::fs::write(layout.repo.join("README.md"), "Probe repository.\n")?;
    let home = &layout.home_git;
    git(&layout.repo, home, &["init", "-q", "-b", "main"])?;
    git(&layout.repo, home, &["config", "user.name", "Probe"])?;
    git(
        &layout.repo,
        home,
        &["config", "user.email", "probe@example.invalid"],
    )?;
    git(&layout.repo, home, &["add", "."])?;
    git(&layout.repo, home, &["commit", "-q", "-m", "probe"])?;
    if writer {
        let path = layout.worktree.to_str().ok_or("path")?;
        git(
            &layout.repo,
            home,
            &["worktree", "add", "-q", "-b", BRANCH, path],
        )?;
    }
    std::fs::write(layout.secret.join("file"), "secret")?;
    Ok(layout)
}

/// The claimed observations every enabled mode must hold; each is probed.
fn expected(profile: &ProviderProfile, writer: bool) -> BTreeMap<FixtureOperation, FixtureOutcome> {
    use FixtureOperation as O;
    use FixtureOutcome::{Allowed, Denied};
    let spec = &profile.spec;
    let mut map = BTreeMap::from([
        (O::DeclaredRead, Allowed),
        (O::UndeclaredRead, Denied),
        (O::UndeclaredWrite, Denied),
        (O::SecretRead, Denied),
        (O::ProtectedWrite, Denied),
        (O::ProtectedInstructionRead, Allowed),
        (O::OtherNetwork, Denied),
        (O::ProviderEndpoint, Allowed),
        (O::DeclaredProcessExec, Allowed),
        (O::UndeclaredProcessExec, Denied),
        (
            O::ProcessFork,
            if spec.allow_process_fork {
                Allowed
            } else {
                Denied
            },
        ),
    ]);
    if spec.deny_keychain_access {
        map.extend([
            (O::ShellExec, Allowed),
            (O::KeychainTool, Denied),
            (O::KeychainRead, Denied),
        ]);
    }
    if !spec.authentication.is_empty() {
        map.insert(O::AuthenticationRead, Allowed);
    }
    if !spec.runtime_reads.is_empty() {
        map.insert(O::RuntimeRead, Allowed);
    }
    if !spec.state_writes.is_empty() {
        map.insert(O::StateWrite, Allowed);
    }
    // Proved by the live run itself: the CLI cannot answer without it.
    if spec.loopback_server {
        map.insert(O::LoopbackServer, Allowed);
    }
    if writer {
        map.extend([
            (O::WorktreeWrite, Allowed),
            (O::GitIndexWrite, Allowed),
            (O::GitHeadWrite, Denied),
            (O::GitObjectsWrite, Denied),
            (O::SelectedRefWrite, Denied),
            (O::GitConfigWrite, Denied),
            (O::OtherRefWrite, Denied),
        ]);
    }
    map
}

fn setup(steps: &[SetupStep]) -> Result<()> {
    for step in steps {
        match step {
            SetupStep::CreateDir(path) => private_dir(path)?,
            SetupStep::WriteFile { path, bytes } | SetupStep::WriteExecutable { path, bytes } => {
                use std::{io::Write, os::unix::fs::OpenOptionsExt};
                let executable = matches!(step, SetupStep::WriteExecutable { .. });
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(if executable { 0o700 } else { 0o600 })
                    .open(path)?;
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            SetupStep::Symlink { link, target } => match std::fs::read_link(link) {
                Ok(existing) if &existing == target => {}
                Ok(_) => return Err("conflicting link".into()),
                Err(_) => std::os::unix::fs::symlink(target, link)?,
            },
        }
    }
    Ok(())
}

struct Run {
    outcome: ExecOutcome,
    argv: Vec<OsString>,
}

fn route(args: &Args, account_key: &str) -> ResolvedRoute {
    ResolvedRoute {
        extra: Default::default(),
        adapter: args.adapter.clone(),
        model: args.model.clone(),
        effort: args.effort.clone(),
        catalogue_revision: "recording".into(),
        account_key: Some(account_key.into()),
        host: "recording".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Unavailable {
            extra: Default::default(),
            reason: provenant_records::RouteExclusionReason::Disabled,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            extra: Default::default(),
            evidence_ref: "recording".into(),
        },
        pool_selection: None,
    }
}

/// Fingerprint of the signed-in Claude identity, from `auth status --json`.
fn claude_account_key(
    executor: &dyn Executor,
    executable: &Path,
    home: &Path,
    user: &std::ffi::OsStr,
    directory: &Path,
    root: &Path,
) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Status {
        #[serde(rename = "loggedIn")]
        logged_in: bool,
        email: String,
    }
    let argv: Vec<OsString> = vec![
        executable.into(),
        "auth".into(),
        "status".into(),
        "--json".into(),
    ];
    let env = [
        ("HOME".into(), home.into()),
        ("USER".into(), user.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("CLAUDE_CONFIG_DIR".into(), directory.into()),
    ];
    let mut options = ExecOptions::new(&argv, root, Duration::from_secs(10));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
    options.stderr = StreamPolicy::Discard;
    let out = executor.execute(options)?;
    if out.termination != Termination::Exit(0) || out.stdout.truncated {
        return Err("claude auth status failed".into());
    }
    let status: Status = serde_json::from_slice(&out.stdout.bytes)?;
    if !status.logged_in {
        return Err("claude is not logged in".into());
    }
    provenant_providers::identity::fingerprint(&status.email)
        .ok_or_else(|| "unusable claude identity".into())
}

struct Context<'a> {
    home: &'a Path,
    executor: &'a SystemExecutor,
    backend: &'a dyn provenant_confinement::ConfinementBackend,
    worktree: Option<(&'a provenant_confinement::VerifiedWorktree, &'a GitRefName)>,
}

fn compile(
    context: &Context<'_>,
    plan: &ConfinedPlan,
    profile: &ProviderProfile,
) -> Result<provenant_confinement::permissions::ConfinedLaunch> {
    if let (AccessMode::WorktreeWrite(authority), Some((worktree, selected_ref))) =
        (&plan.permissions().mode, context.worktree)
    {
        provenant_confinement::permissions::prepare_writer_directories(
            worktree,
            selected_ref,
            authority,
            context.home,
        )?;
    }
    let directories = required_anchors(plan.permissions(), profile, context.worktree)
        .into_iter()
        .map(AnchoredDirectory::open_canonical)
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        home: context.home,
        directories: &directories,
        worktree: context.worktree,
    };
    Ok(plan.compile(context.backend, profile, &anchors)?)
}

fn launch(context: &Context<'_>, plan: &ConfinedPlan, spec: &LaunchSpec) -> Result<Run> {
    setup(&spec.setup)?;
    let compiled = compile(context, plan, &spec.profile)?;
    let command = compiled.command(&spec.argv, &provenant_core::AttemptId::from_bytes([0; 16]))?;
    let mut options = ExecOptions::new(&command, &spec.cwd, Duration::from_secs(300));
    options.env = Some(&spec.env);
    options.secret_input = spec.secret_input.as_ref();
    options.stdin = match &spec.stdin {
        Stdin::Bytes(bytes) => Some(bytes),
        Stdin::Null => None,
    };
    options.stdout = StreamPolicy::Capture { limit: 8 << 20 };
    options.stderr = StreamPolicy::Capture { limit: 1 << 20 };
    Ok(Run {
        outcome: context.executor.execute(options)?,
        argv: spec.argv.clone(),
    })
}

fn exit(outcome: &ExecOutcome) -> ExitObservation {
    ExitObservation {
        code: match outcome.termination {
            Termination::Exit(code) => Some(code),
            Termination::Signal(_) => None,
        },
        timed_out: outcome.timed_out,
    }
}

/// One probe: a tool, its arguments and optional stdin.
struct Probe {
    operation: FixtureOperation,
    argv: Vec<OsString>,
    stdin: Option<&'static [u8]>,
    cleanup: Option<PathBuf>,
}

fn probe(operation: FixtureOperation, tool: &str, args: &[&Path]) -> Probe {
    let mut argv = vec![OsString::from(tool)];
    argv.extend(args.iter().map(|path| path.as_os_str().to_owned()));
    Probe {
        operation,
        argv,
        stdin: None,
        cleanup: None,
    }
}

/// Opens the file and copies zero blocks: proves access, reads no bytes.
fn read(operation: FixtureOperation, path: &Path) -> Probe {
    let mut source = OsString::from("if=");
    source.push(path);
    Probe {
        operation,
        argv: vec![
            "/bin/dd".into(),
            source,
            "of=/dev/null".into(),
            "count=0".into(),
        ],
        stdin: None,
        cleanup: None,
    }
}

fn touch(operation: FixtureOperation, path: PathBuf) -> Probe {
    let existed = path.exists();
    let mut item = probe(operation, "/usr/bin/touch", &[&path]);
    item.cleanup = (!existed).then_some(path);
    item
}

fn first_file(grant: &PathGrant) -> Option<PathBuf> {
    match grant.scope {
        PathScope::Exact => Some(grant.path.clone()),
        PathScope::Subtree => std::fs::read_dir(&grant.path)
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .find(|path| path.is_file()),
    }
}

fn probes(
    args: &Args,
    layout: &Layout,
    plan: &ConfinedPlan,
    profile: &ProviderProfile,
) -> Result<Vec<Probe>> {
    use FixtureOperation as O;
    let spec = &profile.spec;
    let tree = if args.writer {
        &layout.worktree
    } else {
        &layout.repo
    };
    let mut list = vec![
        read(O::DeclaredRead, &tree.join("README.md")),
        read(O::UndeclaredRead, &layout.outside.join("file")),
        touch(O::UndeclaredWrite, layout.outside.join("new")),
        read(O::SecretRead, &layout.secret.join("file")),
        touch(O::ProtectedWrite, tree.join("AGENTS.md")),
        read(O::ProtectedInstructionRead, &tree.join("AGENTS.md")),
        probe(
            O::OtherNetwork,
            "/usr/bin/nc",
            &[
                Path::new("-vz"),
                Path::new("-G3"),
                Path::new("example.com"),
                Path::new("80"),
            ],
        ),
        probe(
            O::ProviderEndpoint,
            "/usr/bin/nc",
            &[
                Path::new("-vz"),
                Path::new("-G5"),
                Path::new(&args.endpoint),
                Path::new("443"),
            ],
        ),
        probe(
            O::DeclaredProcessExec,
            args.executable.to_str().ok_or("path")?,
            &[Path::new("--version")],
        ),
        probe(
            O::UndeclaredProcessExec,
            layout.outside_bin.join("true").to_str().ok_or("path")?,
            &[],
        ),
    ];
    let mut fork = probe(
        O::ProcessFork,
        "/usr/bin/xargs",
        &[Path::new("/usr/bin/true")],
    );
    fork.stdin = Some(b"x\n");
    list.push(fork);
    if !spec.authentication.is_empty() {
        let auth = spec
            .authentication
            .iter()
            .find(|auth| {
                Some(&auth.mechanism) == plan.permissions().selected_authentication.as_ref()
            })
            .and_then(|auth| auth.roots.first())
            .ok_or("selected authentication")?;
        list.push(read(O::AuthenticationRead, &auth.path));
    }
    if let Some(grant) = spec.runtime_reads.first() {
        let file = first_file(grant).ok_or("runtime read file")?;
        list.push(read(O::RuntimeRead, &file));
    }
    if spec.deny_keychain_access {
        list.extend([
            probe(
                O::ShellExec,
                "/bin/sh",
                &[Path::new("-c"), Path::new("printf ok")],
            ),
            probe(O::KeychainTool, "/usr/bin/security", &[Path::new("help")]),
            read(
                O::KeychainRead,
                &PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?)
                    .join("Library/Keychains/login.keychain-db"),
            ),
        ]);
    }
    if let Some(grant) = spec.state_writes.first() {
        let path = match grant.scope {
            PathScope::Exact => grant.path.clone(),
            PathScope::Subtree => grant.path.join("probe"),
        };
        list.push(touch(O::StateWrite, path));
    }
    if args.writer {
        let common = layout.repo.join(".git");
        let private = common.join("worktrees/worktree");
        list.extend([
            touch(O::WorktreeWrite, layout.worktree.join("probe")),
            touch(O::GitIndexWrite, private.join("index.lock")),
            touch(O::GitHeadWrite, private.join("HEAD.lock")),
            touch(O::GitObjectsWrite, common.join("objects/probe")),
            touch(
                O::SelectedRefWrite,
                common.join(format!("refs/heads/{BRANCH}.lock")),
            ),
            touch(O::GitConfigWrite, common.join("config")),
            touch(O::OtherRefWrite, common.join("refs/heads/other.lock")),
        ]);
    }
    Ok(list)
}

/// Narrowed-grant denials (#927, #1044), probed but not stored: no symlink can
/// be created in writable state, and no user configuration, hook, extension or
/// plugin directory takes a new entry. Each must be denied.
fn narrowed_denials(home: &Path, plan: &ConfinedPlan) -> Vec<(String, Probe)> {
    let permissions = plan.permissions();
    let writable = permissions
        .private_state
        .as_ref()
        .unwrap_or(&permissions.scratch);
    let mut list = vec![(
        "symlink in writable state".to_owned(),
        probe(
            FixtureOperation::StateWrite,
            "/bin/ln",
            &[
                Path::new("-s"),
                Path::new("/usr/bin/true"),
                &writable.path.join("probe-link"),
            ],
        ),
    )];
    let configuration = [
        ".gemini/config",
        ".gemini/antigravity-cli/plugins",
        ".gemini/antigravity-cli/plugin_data",
        ".gemini/antigravity-cli/bin",
        ".claude/hooks",
        ".codex/rules",
        ".cursor/extensions",
    ];
    for relative in configuration {
        let directory = home.join(relative);
        if directory.is_dir() {
            list.push((
                format!("new entry in {relative}"),
                touch(
                    FixtureOperation::UndeclaredWrite,
                    directory.join("provenant-probe"),
                ),
            ));
        }
    }
    list
}

fn observe(
    context: &Context<'_>,
    command_prefix: &[OsString],
    cwd: &Path,
    env: &[(OsString, OsString)],
    item: &Probe,
) -> Result<(FixtureOutcome, String)> {
    let mut argv = command_prefix.to_vec();
    argv.extend(item.argv.iter().cloned());
    let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(20));
    options.env = Some(env);
    options.stdin = item.stdin;
    options.stdout = StreamPolicy::Capture { limit: 4096 };
    options.stderr = StreamPolicy::Capture { limit: 4096 };
    let outcome = context.executor.execute(options)?;
    let stderr = String::from_utf8_lossy(&outcome.stderr.bytes).into_owned();
    let result = match outcome.termination {
        Termination::Exit(0) if !outcome.timed_out => FixtureOutcome::Allowed,
        // The provider ran: exec was allowed even if `--version` then failed
        // under the probe's bare environment.
        Termination::Exit(_)
            if item.operation == FixtureOperation::DeclaredProcessExec
                && !stderr.contains("sandbox-exec:") =>
        {
            FixtureOutcome::Allowed
        }
        _ if stderr.contains("Operation not permitted") => FixtureOutcome::Denied,
        _ => {
            return Err(format!(
                "{:?} was inconclusive: {:?} {stderr}",
                item.operation, outcome.termination
            )
            .into());
        }
    };
    if let Some(path) = &item.cleanup {
        let _ = std::fs::remove_file(path);
    }
    Ok((result, stderr))
}

/// Replace session ids, local paths, emails and token-like strings.
struct Redactor {
    literals: Vec<(String, String)>,
}
impl Redactor {
    fn text(&self, value: &str) -> String {
        let mut out = value.to_owned();
        for (from, to) in &self.literals {
            out = out.replace(from.as_str(), to);
        }
        out.split_inclusive(|c: char| c.is_whitespace() || "\"'(),;<>[]{}".contains(c))
            .map(|piece| {
                let token = piece
                    .trim_end_matches(|c: char| c.is_whitespace() || "\"'(),;<>[]{}".contains(c));
                let tail = &piece[token.len()..];
                let class = if !token.contains('/')
                    && token
                        .split_once('@')
                        .is_some_and(|(_, domain)| domain.contains('.'))
                {
                    Some("FIXTURE_EMAIL")
                } else if (token.len() == 36 && token.chars().filter(|c| *c == '-').count() == 4)
                    || token.split_once('_').is_some_and(|(prefix, id)| {
                        prefix.chars().all(|c| c.is_ascii_lowercase())
                            && id.len() >= 16
                            && id.chars().all(|c| c.is_ascii_alphanumeric())
                    })
                {
                    Some("FIXTURE_IDENTIFIER")
                } else if token.len() >= 32
                    && !token.starts_with(['/', '-'])
                    && token.chars().filter(char::is_ascii_digit).count() >= 4
                    && token
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_+/=.".contains(c))
                {
                    Some("FIXTURE_TOKEN")
                } else {
                    None
                };
                match class {
                    Some(class) => format!("{class}{tail}"),
                    None => piece.to_owned(),
                }
            })
            .collect()
    }
    fn value(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.text(text),
            Value::Array(items) => items.iter_mut().for_each(|item| self.value(item)),
            Value::Object(map) => {
                let entries = std::mem::take(map);
                for (key, mut item) in entries {
                    self.value(&mut item);
                    map.insert(self.text(&key), item);
                }
            }
            _ => {}
        }
    }
    fn stream(&self, bytes: &[u8]) -> Result<String> {
        let mut out = String::new();
        for line in String::from_utf8(bytes.to_vec())?
            .lines()
            .filter(|line| !line.is_empty())
        {
            let mut value: Value = serde_json::from_str(line)?;
            self.value(&mut value);
            out.push_str(&serde_json::to_string(&value)?);
            out.push('\n');
        }
        Ok(out)
    }
}

fn platform() -> Result<String> {
    let version = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()?;
    let arch = std::process::Command::new("/usr/bin/uname")
        .arg("-m")
        .output()?;
    Ok(format!(
        "macOS {} {}",
        String::from_utf8(version.stdout)?.trim(),
        String::from_utf8(arch.stdout)?.trim()
    ))
}

fn successful_bash(stream: &[u8]) -> bool {
    let mut requests = std::collections::BTreeSet::new();
    for event in stream
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
    {
        let Some(content) = event
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for item in content {
            match (
                event.get("type").and_then(Value::as_str),
                item.get("type").and_then(Value::as_str),
            ) {
                (Some("assistant"), Some("tool_use"))
                    if item.get("name").and_then(Value::as_str) == Some("Bash")
                        && item
                            .get("input")
                            .and_then(|input| input.get("command"))
                            .and_then(Value::as_str)
                            .is_some_and(|command| command == SHELL_PROOF_COMMAND) =>
                {
                    if let Some(id) = item.get("id").and_then(Value::as_str) {
                        requests.insert(id.to_owned());
                    }
                }
                (Some("user"), Some("tool_result"))
                    if item.get("is_error").and_then(Value::as_bool) != Some(true)
                        && item
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .is_some_and(|id| requests.contains(id))
                        && item
                            .get("content")
                            .and_then(Value::as_str)
                            .is_some_and(|text| text.trim() == "provenant-shell-proof") =>
                {
                    return true;
                }
                _ => {}
            }
        }
    }
    false
}

/// Codex streams carry no answering model: only its private rollout under the
/// recording's `CODEX_HOME` can establish one. Other adapters report it in-stream.
fn answering_model(
    adapter: &str,
    session: Option<&str>,
    stream_model: Option<&str>,
    codex_home: &Path,
) -> Option<String> {
    if adapter == "codex" {
        session
            .and_then(|thread| provenant_providers::codex_rollout::observe(codex_home, thread).ok())
            .map(|turn| turn.model)
    } else {
        stream_model.map(str::to_owned)
    }
}

#[test]
fn codex_answering_model_comes_from_the_private_rollout() {
    let home = std::env::temp_dir().join(format!("provenant-record-{}", std::process::id()));
    let day = home.join("sessions/2026/10/24");
    std::fs::create_dir_all(&day).unwrap();
    let thread = "01234567-89ab-cdef-0123-456789abcdef";
    std::fs::write(
        day.join(format!("rollout-2026-10-24T00-00-00-{thread}.jsonl")),
        format!(
            "{}\n{}\n",
            json!({"type":"session_meta","payload":{"id":thread}}),
            json!({"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"low"}}),
        ),
    )
    .unwrap();
    let observed = answering_model("codex", Some(thread), None, &home);
    let unbound = answering_model("codex", None, Some("argv-model"), &home);
    let claude = answering_model("claude", None, Some("claude-sonnet-5"), &home);
    std::fs::remove_dir_all(&home).unwrap();
    assert_eq!(observed.as_deref(), Some("gpt-6-luna"));
    assert_eq!(unbound, None);
    assert_eq!(claude.as_deref(), Some("claude-sonnet-5"));
}

#[test]
fn bash_proof_needs_a_matching_successful_tool_result() {
    let request = json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":SHELL_PROOF_COMMAND}}]}}).to_string();
    let unchecked = json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"Bash","input":{"command":"printf provenant-shell-proof"}}]}}).to_string();
    let result = |id: &str, error: bool, output: &str| {
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":id,"is_error":error,"content":output}]}}).to_string()
    };
    assert!(!successful_bash(request.as_bytes()));
    assert!(!successful_bash(
        format!(
            "{unchecked}\n{}",
            result("b", false, "provenant-shell-proof")
        )
        .as_bytes()
    ));
    for (id, error, output) in [
        ("other", false, "provenant-shell-proof"),
        ("b", true, "provenant-shell-proof"),
        ("b", false, "permission denied"),
    ] {
        assert!(!successful_bash(
            format!("{request}\n{}", result(id, error, output)).as_bytes()
        ));
    }
    assert!(successful_bash(
        format!("{request}\n{}", result("b", false, "provenant-shell-proof")).as_bytes()
    ));
}

fn main() -> Result<()> {
    let args = args()?;
    if std::fs::read_dir(&args.root)?.next().is_some() {
        return Err("--root must be empty".into());
    }
    let adapter: &dyn Adapter = REGISTRY
        .iter()
        .map(|entry| entry.adapter)
        .find(|adapter| adapter.id() == args.adapter)
        .ok_or("adapter")?;
    let layout = layout(&args.root, args.writer)?;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let user = std::env::var_os("USER").ok_or("USER")?;
    let executor = SystemExecutor::new(4)?;
    let cancel = CancelToken::new();
    let backend = system_backend(&BackendContext {
        executor: &executor,
        cancel: &cancel,
        #[cfg(target_os = "macos")]
        env: &[],
    })?;
    let selected_ref = GitRefName::branch(BRANCH)?;
    let git = std::fs::canonicalize(which("git").ok_or("git")?)?;
    let runtime = UtilityRuntime::new(git.clone(), &layout.home_git)?;
    let worktree = if args.writer {
        let repo = runtime.open_repo(
            &executor,
            &layout.repo,
            UtilityBudget::bounded(Duration::from_secs(10))?,
            &cancel,
        )?;
        Some(verify_worktree(&repo, &layout.worktree)?)
    } else {
        None
    };
    let context = Context {
        home: &home,
        executor: &executor,
        backend: backend.as_ref(),
        worktree: worktree.as_ref().map(|tree| (tree, &selected_ref)),
    };
    let cwd = if args.writer {
        &layout.worktree
    } else {
        &layout.repo
    };
    let plan_for = |scratch: &Path| -> Result<ConfinedPlan> {
        private_dir(scratch)?;
        let mode = match &worktree {
            Some(tree) => AccessMode::WorktreeWrite(Box::new(
                WorktreeWriteAuthority::for_worktree(tree, &selected_ref, &home)?,
            )),
            None => AccessMode::ReadOnly,
        };
        Ok(ConfinedPlan::new(PlanRequest {
            scope: WorkScope::Project(ProjectId::from_bytes([7; 16])),
            permissions: PlanPermissions {
                read_only: None,
                mode,
                scratch: subtree(scratch),
                private_state: Some(subtree(&layout.state)),
                read_roots: if args.writer {
                    vec![]
                } else {
                    vec![subtree(&layout.repo)]
                },
                protected_roots: vec![exact(&cwd.join("AGENTS.md"))],
                secret_roots: vec![subtree(&layout.secret)],
                // Codex reads its login file and Agy the login keychain inside
                // the sandbox. Claude's token arrives in its environment;
                // OpenCode's free models need none.
                selected_authentication: match args.adapter.as_str() {
                    "codex" => Some("login".into()),
                    "agy" => Some("keychain".into()),
                    _ => None,
                },
                include_continuation: false,
                write_roots: vec![],
                network: NetworkAccess::Provider,
                nested_confinement: false,
            },
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })?)
    };
    // Claude registrations live at the fixed ~/.claude-<label> directory (the
    // recorder accepts only the pro account); rotating ~/.claude is never one.
    let directory = home.join(if args.adapter == "codex" {
        ".codex"
    } else {
        ".claude-pro"
    });
    // Claude binds the token to the signed-in identity, so the key is that
    // identity's fingerprint; Codex recording uses a fixed label.
    // Host probes get a fresh private cwd under the run root, not the
    // supervisor's `/`.
    let probe = provenant_providers::host::HostProbeExecutor::new(&executor, &args.root)?;
    let account_key = if args.adapter == "claude" {
        claude_account_key(
            &probe,
            &args.executable,
            &home,
            &user,
            &directory,
            &args.root,
        )?
    } else {
        "recording".into()
    };
    // Agy has no signed-in identity source, so it records without a registered
    // account; its route carries a fixed opaque key only to bind a continuation.
    let account = (args.adapter != "agy")
        .then(|| {
            provenant_providers::accounts::Registration {
                adapter: args.adapter.clone(),
                label: "recording".into(),
                key: account_key.clone(),
                org_fingerprint: None,
                directory,
                plan: None,
                priority: None,
                billing_anchor: None,
                plans: Vec::new(),
                directories: Vec::new(),
                limits: None,
                display_name: None,
                hidden: false,
                order: None,
                headline: None,
                settings: Default::default(),
            }
            .registered()
        })
        .transpose()?;
    let token = match (args.adapter.as_str(), &account) {
        ("claude", Some(account)) => {
            let login = Login {
                executable: &args.executable,
                home: &home,
                user: &user,
                config_dir: account.directory(),
            };
            let now = || now_ms().unwrap_or(u64::MAX);
            let deadline = now().saturating_add(90_000);
            Some(refresh_for_account(
                &probe, &login, account, &now, deadline,
            )?)
        }
        _ => None,
    };
    let state = args.root.join("provenant-state");
    private_dir(&state)?;
    for child in ["fabric", "projects", "runs", "workflow"] {
        private_dir(&state.join(child))?;
    }
    let route = route(&args, &account_key);
    let mut records = Vec::new();
    let mut continuation = None;
    let mut last_profile = None;
    let mut session_ids = Vec::new();
    let fresh_prompt = if args.writer && args.adapter == "claude" {
        SHELL_WRITER_PROMPT
    } else if args.writer && args.adapter == "agy" {
        AGY_WRITER_PROMPT
    } else if args.writer {
        WRITER_PROMPT
    } else {
        PROMPT
    };
    let resume_prompt = if args.writer && args.adapter == "claude" {
        SHELL_RESUME_PROMPT
    } else {
        RESUME_PROMPT
    };
    for (session, text) in [("fresh", fresh_prompt), ("resume", resume_prompt)] {
        let plan = plan_for(&args.root.join(format!("scratch-{session}")))?;
        let prompt = Prompt::new(text.into());
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&Sha256::digest(args.root.as_os_str().as_encoded_bytes())[..16]);
        let mut spec = adapter.prepare(PrepareInput {
            command: None,
            route: &route,
            confined: &plan,
            prompt: &prompt,
            continuation: continuation.as_ref(),
            grant_digest: "recording",
            executable: &args.executable,
            home: &home,
            user: &user,
            host_var: &|key| std::env::var_os(key),
            account: account.as_ref(),
            token: token.as_ref(),
            cwd,
            git_context: false,
            nonce,
            provenant: provenant_providers::LaneRuntime {
                executable: &args.provenant,
                git: &git,
                state: &args.root.join("provenant-state"),
                child_state: &args.root.join("provenant-state/children/recording"),
                home: &home,
                depth: 1,
                attempt: provenant_core::AttemptId::from_bytes(nonce),
                assets: &args.assets,
            },
        })?;
        spec.profile.observations = expected(&spec.profile, args.writer);
        if args.skip_live {
            // Probe debugging only: compile and probe without calling the CLI.
            setup(&spec.setup)?;
            last_profile = Some((plan, spec.profile));
            break;
        }
        let run = launch(&context, &plan, &spec)?;
        // Raw stream stays in the private recording root, never in fixtures.
        std::fs::write(
            args.root.join(format!("raw-{session}.jsonl")),
            &run.outcome.stdout.bytes,
        )?;
        let mut stream = spec.stream;
        adapter.observe(&mut stream, &run.outcome.stdout.bytes)?;
        let outcome = adapter.finish(stream, exit(&run.outcome))?;
        let model = answering_model(
            &args.adapter,
            outcome.session(),
            outcome.model(),
            &layout.state,
        );
        eprintln!(
            "{session}: exit={:?} status={:?} model={model:?} stderr={}",
            run.outcome.termination,
            outcome.status(),
            String::from_utf8_lossy(&run.outcome.stderr.bytes)
                .chars()
                .take(2000)
                .collect::<String>()
        );
        if let Some(id) = outcome.session() {
            session_ids.push(id.to_owned());
        }
        let writer_shell_proof =
            args.writer && args.adapter == "claude" && successful_bash(&run.outcome.stdout.bytes);
        if args.writer && args.adapter == "claude" && !writer_shell_proof {
            return Err("Claude writer did not invoke Bash".into());
        }
        if session == "fresh" && args.writer {
            let note = std::fs::read_to_string(layout.worktree.join("note.txt"))?;
            if note.trim() != "ok" {
                return Err("writer did not create note.txt".into());
            }
            std::fs::remove_file(layout.worktree.join("note.txt"))?;
        }
        if session == "fresh" {
            continuation = Some(adapter.continuation(&outcome, &route, "recording")?);
        }
        let subprocess_env_scrub = spec
            .env
            .iter()
            .any(|(key, value)| key == "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB" && value == "1");
        records.push((
            session,
            run,
            outcome,
            model,
            writer_shell_proof,
            subprocess_env_scrub,
        ));
        last_profile = Some((plan, spec.profile));
    }

    let (plan, mut profile) = last_profile.ok_or("no run")?;
    let mut probe_profile_spec = profile.spec.clone();
    for tool in PROBE_TOOLS {
        probe_profile_spec
            .process_executables
            .push(exact(Path::new(tool)));
    }
    let probe_profile = ProviderProfile {
        spec: probe_profile_spec,
        observations: profile.observations.clone(),
    };
    let compiled = compile(&context, &plan, &probe_profile)?;
    let prefix = compiled.command(&[], &provenant_core::AttemptId::from_bytes([0; 16]))?;
    if args.skip_live {
        // Debugging aid: the probe profile, for ad hoc sandbox-exec checks.
        std::fs::write(
            args.root.join("probe-profile.sb"),
            prefix[2].as_encoded_bytes(),
        )?;
    }
    let env = [
        ("HOME".into(), home.clone().into_os_string()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let mut observed = BTreeMap::new();
    for item in probes(&args, &layout, &plan, &profile)? {
        let (result, stderr) = observe(&context, &prefix, cwd, &env, &item)?;
        let expected = profile.observations.get(&item.operation).copied();
        eprintln!(
            "probe {:?}: {result:?} (expected {expected:?}) {}",
            item.operation,
            stderr.trim()
        );
        if expected != Some(result) {
            return Err(format!(
                "{:?} observed {result:?}, expected {expected:?}",
                item.operation
            )
            .into());
        }
        observed.insert(item.operation, result);
    }
    for (label, item) in narrowed_denials(&home, &plan) {
        let (result, stderr) = observe(&context, &prefix, cwd, &env, &item)?;
        eprintln!("narrowed {label}: {result:?} {}", stderr.trim());
        if result != FixtureOutcome::Denied {
            return Err(format!("{label} was {result:?}, expected Denied").into());
        }
    }
    if profile.spec.loopback_server {
        observed.insert(FixtureOperation::LoopbackServer, FixtureOutcome::Allowed);
    }
    profile.observations = observed;

    let mut literals: Vec<(String, String)> = session_ids
        .iter()
        .map(|id| (id.clone(), "FIXTURE_SESSION_IDENTIFIER".into()))
        .collect();
    literals.push((
        args.root.to_string_lossy().into_owned(),
        "FIXTURE_ROOT".into(),
    ));
    literals.push((home.to_string_lossy().into_owned(), "FIXTURE_HOME".into()));
    let redactor = Redactor { literals };
    let access = if args.writer {
        "worktree_write"
    } else {
        "read_only"
    };
    let version_probe = std::process::Command::new(&args.executable)
        .arg("--version")
        .env_clear()
        .env("HOME", &home)
        .output()?;
    let version = String::from_utf8_lossy(&version_probe.stdout)
        .trim()
        .to_owned();
    let platform = platform()?;
    for (session, run, outcome, model, writer_shell_proof, subprocess_env_scrub) in &records {
        let redacted = redactor.stream(&run.outcome.stdout.bytes)?;
        let argv: Vec<String> = run
            .argv
            .iter()
            .map(|arg| redactor.text(&arg.to_string_lossy()))
            .collect();
        let id = format!("{}-live-{access}-{session}", args.adapter);
        let file = format!("{}/live-{access}-{session}.jsonl", args.adapter);
        let entry = json!({
            "id": id,
            "fixture": file,
            "sha256": provenant_core::hex(&Sha256::digest(redacted.as_bytes())),
            "kind": "live_recording",
            "source": {"kind": "live_confined_run", "recorder": "crates/providers/examples/record.rs"},
            "cli_version": version,
            "platform": platform,
            "argv": argv,
            "session_mode": session,
            "access_mode": access,
            "process_exit": exit(&run.outcome).code,
            "outcome": format!("{:?}", outcome.status()),
            "answering_model": model,
            "prompt": if *session == "fresh" { fresh_prompt } else { resume_prompt },
            "writer_shell_proof": writer_shell_proof,
            "subprocess_env_scrub": subprocess_env_scrub,
            "confinement": format!("sandbox-exec deny-default profile, observations in {}/confinement.json", args.adapter),
            "substitution_classes": ["session_identifier", "local_path", "email", "identifier", "token"],
            "keychain_ipc_proof": false,
            // Claude's token arrives through a pipe, so a successful run with the
            // keychain denied is the evidence, not a disqualifier.
            "activation_eligible": outcome.status() == OutcomeStatus::Success,
        });
        println!("{}", serde_json::to_string_pretty(&entry)?);
        if args.write {
            let fixtures =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/providers");
            std::fs::write(fixtures.join(&file), &redacted)?;
            // The recorder owns each adapter's recordings.json manifest.
            let manifest_path = fixtures.join(format!("{}/recordings.json", args.adapter));
            let mut captures: Vec<Value> = match std::fs::read_to_string(&manifest_path) {
                Ok(text) => serde_json::from_str(&text)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(error) => return Err(error.into()),
            };
            captures.retain(|capture| capture["id"] != entry["id"]);
            captures.push(entry);
            captures.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
            std::fs::write(
                &manifest_path,
                serde_json::to_string_pretty(&captures)? + "\n",
            )?;
        }
    }
    if args.write {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../tests/fixtures/providers/{}/confinement.json",
            args.adapter
        ));
        let mut current: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        current[access] = serde_json::to_value(&profile.observations)?;
        std::fs::write(&path, serde_json::to_string_pretty(&current)? + "\n")?;
    }
    Ok(())
}
