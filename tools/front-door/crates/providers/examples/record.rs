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
    PathGrant, PathScope, PlanPermissions, ProviderProfile, WorktreeWriteAuthority,
    required_anchors, system_backend,
};
use provenant_confinement::repo::GitRefName;
use provenant_confinement::{
    CodexPolicy, ConfinedPlan, ExtensionPolicy, PlanRequest, UtilityBudget, UtilityRuntime,
    verify_worktree,
};
use provenant_core::{CancelToken, ProjectId, WorkScope};
use provenant_providers::claude::OauthToken;
use provenant_providers::{
    Adapter, ExitObservation, LaunchSpec, OutcomeStatus, PrepareInput, Prompt, REGISTRY, SetupStep,
    Stdin,
};
use provenant_records::ResolvedRoute;
use provenant_supervisor::exec::{
    ExecOptions, ExecOutcome, Executor, StreamPolicy, SystemExecutor, Termination,
};
use provenant_supervisor::fs::{LocalFs, SafeFs};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

const PROMPT: &str = "Reply with the word ok";
const RESUME_PROMPT: &str = "Reply with the word ok again";
/// Writers must prove an in-sandbox tool edit, not just a reply.
const WRITER_PROMPT: &str = "Create the file note.txt in the current directory containing the word ok, then reply with the word ok";
const BRANCH: &str = "provenant-probe";
const PROBE_TOOLS: [&str; 6] = [
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
    let (binary, endpoint) = match adapter.as_str() {
        "codex" => ("codex", "api.openai.com"),
        "claude" => ("claude", "api.anthropic.com"),
        "opencode" => ("opencode", "opencode.ai"),
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

/// Creates `path` and missing parents with mode 0700, refusing any symbolic
/// link component so neither creation nor chmod can land outside it.
fn private_dir(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err("private directory must be absolute".into());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("symbolic link in {}", current.display()).into());
            }
            Ok(meta) if !meta.is_dir() => return Err("not a directory".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new().mode(0o700).create(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn now_ms() -> Result<u64> {
    Ok(u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis(),
    )?)
}

/// Reads Claude's default login token outside the sandbox, refreshing it
/// first at zero cost when it is close to expiry. Never printed or stored.
fn claude_token(
    executor: &SystemExecutor,
    executable: &Path,
    home: &Path,
    user: &OsStr,
) -> Result<OauthToken> {
    let env: Vec<(OsString, OsString)> = vec![
        ("HOME".into(), home.into()),
        ("USER".into(), user.into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ];
    let read = || -> Result<Vec<u8>> {
        let argv = provenant_providers::claude::token_argv(user);
        let mut options = ExecOptions::new(&argv, home, Duration::from_secs(10));
        options.env = Some(&env);
        options.stdout = StreamPolicy::Capture { limit: 64 * 1024 };
        options.stderr = StreamPolicy::Discard;
        let out = executor.execute(options)?;
        if out.termination != Termination::Exit(0) || out.stdout.truncated {
            return Err("Claude login token is unavailable".into());
        }
        Ok(out.stdout.bytes)
    };
    let token = OauthToken::parse(&read()?, now_ms()?)
        .map_err(|_| "Claude login token is unreadable or expired")?;
    if !token.needs_refresh(now_ms()?) {
        return Ok(token);
    }
    let argv = provenant_providers::claude::refresh_argv(executable);
    let mut options = ExecOptions::new(&argv, home, Duration::from_secs(60));
    options.env = Some(&env);
    options.stdout = StreamPolicy::Discard;
    options.stderr = StreamPolicy::Discard;
    executor.execute(options)?;
    let token = OauthToken::parse(&read()?, now_ms()?)
        .map_err(|_| "Claude login token is unreadable or expired")?;
    if token.needs_refresh(now_ms()?) {
        return Err("Claude login token did not refresh".into());
    }
    Ok(token)
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
            (O::GitHeadWrite, Allowed),
            (O::GitObjectsWrite, Allowed),
            (O::SelectedRefWrite, Allowed),
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

fn route(args: &Args) -> ResolvedRoute {
    ResolvedRoute {
        adapter: args.adapter.clone(),
        model: args.model.clone(),
        effort: args.effort.clone(),
        catalogue_revision: "recording".into(),
        account_key: Some("default".into()),
        host: "recording".into(),
        alternatives: Default::default(),
        exclusions: Vec::new(),
        eligibility: provenant_records::RouteEligibility::Eligible,
        readiness: provenant_records::RouteReadiness::Unavailable {
            reason: provenant_records::RouteExclusionReason::Disabled,
        },
        confidentiality: provenant_records::RouteConfidentiality {
            evidence_ref: "recording".into(),
        },
    }
}

struct Context<'a> {
    executor: &'a SystemExecutor,
    safe_fs: &'a LocalFs,
    backend: &'a dyn provenant_confinement::ConfinementBackend,
    worktree: Option<(&'a provenant_confinement::VerifiedWorktree, &'a GitRefName)>,
}

fn compile(
    context: &Context<'_>,
    plan: &ConfinedPlan,
    profile: &ProviderProfile,
) -> Result<provenant_confinement::permissions::ConfinedLaunch> {
    let directories = required_anchors(plan.permissions(), profile, context.worktree)
        .into_iter()
        .map(|path| AnchoredDirectory::open_canonical(context.safe_fs, path))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let anchors = Anchors {
        directories: &directories,
        safe_fs: context.safe_fs,
        worktree: context.worktree,
    };
    Ok(plan.compile(context.backend, profile, &anchors)?)
}

fn launch(context: &Context<'_>, plan: &ConfinedPlan, spec: &LaunchSpec) -> Result<Run> {
    setup(&spec.setup)?;
    let compiled = compile(context, plan, &spec.profile)?;
    let command = compiled.command(&spec.argv)?;
    let mut options = ExecOptions::new(&command, &spec.cwd, Duration::from_secs(300));
    options.env = Some(&spec.env);
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
    let safe_fs = LocalFs;
    let cancel = CancelToken::new();
    let backend = system_backend(&BackendContext {
        safe_fs: &safe_fs,
        executor: &executor,
        cancel: &cancel,
        env: &[],
    })?;
    let selected_ref = GitRefName::branch(BRANCH)?;
    let fs: Arc<dyn SafeFs> = Arc::new(LocalFs);
    let runtime = UtilityRuntime::new(
        fs,
        std::fs::canonicalize(which("git").ok_or("git")?)?,
        &layout.home_git,
    )?;
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
        executor: &executor,
        safe_fs: &safe_fs,
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
                WorktreeWriteAuthority::for_worktree(tree, &selected_ref)?,
            )),
            None => AccessMode::ReadOnly,
        };
        Ok(ConfinedPlan::new(PlanRequest {
            scope: WorkScope::Project(ProjectId::from_bytes([7; 16])),
            permissions: PlanPermissions {
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
                // Only Codex reads a credential inside the sandbox. Claude's
                // token arrives in its environment; OpenCode's free models
                // need none.
                selected_authentication: (args.adapter == "codex").then(|| "login".into()),
                include_continuation: false,
                network: vec![],
                capabilities: vec![],
                nested_confinement: false,
            },
            codex: CodexPolicy::Clean,
            extensions: ExtensionPolicy::Deny,
            agy_state: None,
        })?)
    };
    let token = match args.adapter.as_str() {
        "claude" => Some(claude_token(&executor, &args.executable, &home, &user)?),
        _ => None,
    };
    let route = route(&args);
    let mut records = Vec::new();
    let mut continuation = None;
    let mut last_profile = None;
    let mut session_ids = Vec::new();
    let fresh_prompt = if args.writer { WRITER_PROMPT } else { PROMPT };
    for (session, text) in [("fresh", fresh_prompt), ("resume", RESUME_PROMPT)] {
        let plan = plan_for(&args.root.join(format!("scratch-{session}")))?;
        let prompt = Prompt::new(text.into());
        let mut nonce = [0u8; 16];
        nonce.copy_from_slice(&Sha256::digest(args.root.as_os_str().as_encoded_bytes())[..16]);
        let mut spec = adapter.prepare(PrepareInput {
            route: &route,
            confined: &plan,
            prompt: &prompt,
            continuation: continuation.as_ref(),
            grant_digest: "recording",
            executable: &args.executable,
            home: &home,
            user: &user,
            token: token.as_ref(),
            cwd,
            nonce,
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
        eprintln!(
            "{session}: exit={:?} status={:?} model={:?} stderr={}",
            run.outcome.termination,
            outcome.status(),
            outcome.model(),
            String::from_utf8_lossy(&run.outcome.stderr.bytes)
                .chars()
                .take(2000)
                .collect::<String>()
        );
        if let Some(id) = outcome.session() {
            session_ids.push(id.to_owned());
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
        records.push((session, run, outcome));
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
    let prefix = compiled.command(&[])?;
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
    for (session, run, outcome) in &records {
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
            "sha256": Sha256::digest(redacted.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            "kind": "live_recording",
            "source": {"kind": "live_confined_run", "recorder": "crates/providers/examples/record.rs"},
            "cli_version": version,
            "platform": platform,
            "argv": argv,
            "session_mode": session,
            "access_mode": access,
            "process_exit": exit(&run.outcome).code,
            "outcome": format!("{:?}", outcome.status()),
            "answering_model": outcome.model(),
            "prompt": if *session == "fresh" { fresh_prompt } else { RESUME_PROMPT },
            "confinement": format!("sandbox-exec deny-default profile, observations in {}/confinement.json", args.adapter),
            "substitution_classes": ["session_identifier", "local_path", "email", "identifier", "token"],
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
