//! Host-only delivery. Credential environment travels on the private handoff
//! pipe, never in provider environment, launch records or the delivery receipt.
use crate::{
    Result, RunError, WriterDeliveryReceipt, WriterDeliveryRequest, WriterDeliveryStage as Stage,
    WriterDeliveryState, WriterRequest,
};
use provenant_confinement::redact::Redactor;
use provenant_confinement::repo::GitRefName;
use provenant_core::GitOid;
use provenant_os::exec::ExecOptions;
use provenant_os::exec::Executor;
use provenant_os::exec::StreamPolicy;
use provenant_os::exec::SystemExecutor;
use provenant_os::exec::Termination;
use provenant_os::fs::Directory;
use serde::{Deserialize, Serialize};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStringExt,
    path::{Path, PathBuf},
    time::Duration,
};

/// Transport-only context. Debug deliberately omits all environment values.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostDeliveryContext {
    environment: Vec<(Vec<u8>, Vec<u8>)>,
    tools: Option<HostTools>,
    bound: Option<BoundDelivery>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundDelivery {
    /// Lane head when delivery was prepared, before the writer ran.
    base_head: GitOid,
    push_url: String,
    lane_branch: String,
    pull_request_repo: Option<String>,
    git_dir: PathBuf,
    host_cwd: PathBuf,
    /// Destination sealed at launch; a later request edit cannot redirect it.
    destination: WriterDeliveryRequest,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HostTools {
    git: PathBuf,
    gh: Option<PathBuf>,
}
impl std::fmt::Debug for HostDeliveryContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostDeliveryContext([private handoff])")
    }
}

impl HostDeliveryContext {
    pub(crate) fn new(environment: Vec<(OsString, OsString)>) -> Self {
        Self {
            environment: environment
                .into_iter()
                .map(|(k, v)| (k.into_vec(), v.into_vec()))
                .collect(),
            bound: None,
            tools: None,
        }
    }
    /// Preserve normal host credential/config discovery without Git's ambient
    /// repository/index overrides or any provider-specific environment.
    pub fn capture() -> Self {
        const KEYS: &[&str] = &[
            "HOME",
            "PATH",
            "XDG_CONFIG_HOME",
            "GH_CONFIG_DIR",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "SSH_AGENT_PID",
            "SSH_ASKPASS",
            "SSH_ASKPASS_REQUIRE",
            "DISPLAY",
            "GIT_ASKPASS",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_TERMINAL_PROMPT",
            "PROVENANT_GIT",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_CONFIG_NOSYSTEM",
            "GCM_INTERACTIVE",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "GIT_SSL_CAINFO",
            "GIT_SSL_CAPATH",
            "HTTPS_PROXY",
            "HTTP_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "https_proxy",
            "http_proxy",
            "all_proxy",
            "no_proxy",
            "TMPDIR",
            "LANG",
            "LC_ALL",
        ];
        Self::new(
            std::env::vars_os()
                .filter(|(key, _)| KEYS.iter().any(|k| key == OsStr::new(k)))
                .collect(),
        )
    }
    /// Under writer custody, before the provider launches. Bind the single
    /// push destination and refresh shared refs with the host's Git authority.
    pub(crate) fn prepare(
        mut self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
    ) -> Result<Self> {
        validate(request)?;
        self.host_environment(writer)?;
        let stage = Stage::Prepare;
        let git = self.executable("git", writer)?;
        let gh = if request.open_pr {
            Some(self.executable("gh", writer)?)
        } else {
            None
        };
        self.tools = Some(HostTools { git, gh });
        let urls = self.command(
            &writer.worktree,
            "git",
            &["remote", "get-url", "--push", "--all", &request.remote],
            stage,
        )?;
        let mut urls = urls.lines();
        let url = urls
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with('-'))
            .ok_or_else(|| prepare_error("remote URL", "no usable push URL"))?;
        if urls.next().is_some() {
            return Err(prepare_error("remote URL", "multiple push URLs"));
        }
        // Embedded HTTP credentials would become process arguments below.
        // Host credential helpers, gh config and the SSH agent remain available.
        if let Some((scheme, rest)) = url.split_once("://")
            && ((scheme != "ssh"
                && rest
                    .split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@')))
                || rest.contains(['?', '#'])
                || scheme == "ssh"
                    && rest
                        .split('@')
                        .next()
                        .is_some_and(|user| user.contains(':')))
        {
            return Err(prepare_error(
                "remote URL",
                "embedded credentials, query or fragment refused",
            ));
        }
        let branch = self.command(
            &writer.worktree,
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            stage,
        )?;
        GitRefName::branch(&branch).map_err(|error| prepare_error("HEAD", error))?;
        let git_dir = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--absolute-git-dir"],
            stage,
        )?;
        let push_url = if !url.contains(':') && !Path::new(url).is_absolute() {
            std::fs::canonicalize(writer.worktree.join(url))
                .map_err(|error| prepare_error("remote URL", error))?
                .to_str()
                .ok_or_else(|| prepare_error("remote URL", "path is not UTF-8"))?
                .to_owned()
        } else {
            url.to_owned()
        };
        let base = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--verify", "HEAD"],
            stage,
        )?;
        self.bound = Some(BoundDelivery {
            base_head: GitOid::parse(&base).map_err(|error| prepare_error("HEAD", error))?,
            push_url,
            lane_branch: branch,
            pull_request_repo: request
                .open_pr
                .then(|| pull_request_repo(url))
                .transpose()
                .map_err(|error| prepare_error("remote URL", error))?,
            git_dir: std::fs::canonicalize(git_dir)
                .map_err(|error| prepare_error("git dir", error))?,
            host_cwd: writer.project.clone(),
            destination: request.clone(),
        });
        // Free-form shell helpers can select writer code. Support normal
        // executable helpers and gh's standard helper, otherwise refuse early.
        let helpers = self.command(
            &writer.worktree,
            "git",
            &[
                "config",
                "--get-regexp",
                "^(credential(\\..*)?\\.helper|core\\.sshcommand)$",
            ],
            stage,
        )?;
        for line in helpers.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            if value.trim().is_empty() {
                continue;
            }
            if key.eq_ignore_ascii_case("core.sshcommand") {
                return Err(prepare_error(
                    "credential helper",
                    "delivery does not support core.sshCommand; use host SSH config",
                ));
            }
            if let Some(command) = value.strip_prefix('!') {
                let parts: Vec<_> = command.split_whitespace().collect();
                if parts.len() != 3
                    || parts[1..] != ["auth", "git-credential"]
                    || (parts[0] != "gh" && !Path::new(parts[0]).is_absolute())
                    || !parts[0]
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/_-.=~:@".contains(&b))
                {
                    return Err(prepare_error(
                        "credential helper",
                        "delivery refuses an opaque shell credential helper",
                    ));
                }
                if Path::new(parts[0]).is_absolute() {
                    credential_helper(Path::new(parts[0]), writer)?;
                } else {
                    self.executable("gh", writer)?;
                }
            } else {
                let parts: Vec<_> = value.split_whitespace().collect();
                if !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b" /_-.=~:@".contains(&b))
                {
                    return Err(prepare_error(
                        "credential helper",
                        "delivery credential helper needs a plain executable",
                    ));
                }
                if Path::new(parts[0]).is_absolute() {
                    credential_helper(Path::new(parts[0]), writer)?;
                } else if parts[0].contains('/') {
                    return Err(prepare_error(
                        "credential helper",
                        "delivery credential helper must resolve outside the lane",
                    ));
                }
            }
        }
        self.command(
            &writer.worktree,
            "git",
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                &request.remote,
            ],
            stage,
        )?;
        Ok(self)
    }
    fn host_environment(&mut self, writer: &WriterRequest) -> Result<()> {
        for (key, value) in &mut self.environment {
            let name = std::str::from_utf8(key).unwrap_or_default();
            if name == "GIT_SSH_COMMAND" && !value.is_empty() {
                return Err(prepare_error(
                    "environment",
                    "GIT_SSH_COMMAND is not supported; use host SSH config",
                ));
            }
            let path = OsString::from_vec(value.clone());
            if name == "PATH" {
                let paths: Vec<_> = std::env::split_paths(&path)
                    .filter(|p| p.is_absolute())
                    .filter_map(|p| std::fs::canonicalize(p).ok())
                    .filter(|p| !p.starts_with(&writer.worktree) && !p.starts_with(&writer.project))
                    .collect();
                *value = std::env::join_paths(paths)
                    .map_err(|_| prepare_error("environment", "PATH is invalid"))?
                    .into_vec();
            } else if matches!(
                name,
                "HOME"
                    | "XDG_CONFIG_HOME"
                    | "GH_CONFIG_DIR"
                    | "GIT_CONFIG_GLOBAL"
                    | "GIT_CONFIG_SYSTEM"
                    | "GIT_SSH"
                    | "GIT_ASKPASS"
                    | "SSH_ASKPASS"
            ) && !value.is_empty()
            {
                *value = host_path(Path::new(&path), writer)
                    .map_err(|error| prepare_error("environment", format!("{name}: {error}")))?
                    .into_os_string()
                    .into_vec();
            }
        }
        Ok(())
    }
    fn executable(&self, tool: &str, writer: &WriterRequest) -> Result<PathBuf> {
        if tool == "git" {
            let override_path = self
                .environment
                .iter()
                .find(|(key, _)| key == b"PROVENANT_GIT")
                .map(|(_, value)| OsString::from_vec(value.clone()));
            let path =
                provenant_os::git::resolve_git_executable(override_path.as_deref().map(Path::new))
                    .ok_or_else(|| prepare_error("git executable", "git is unavailable"))?;
            return host_path(&path, writer)
                .map_err(|error| prepare_error("git executable", error));
        }
        let path = self
            .environment
            .iter()
            .find(|(k, _)| k == b"PATH")
            .map(|(_, v)| OsString::from_vec(v.clone()))
            .ok_or_else(|| {
                prepare_error(
                    "gh executable",
                    format!("{tool} is unavailable: PATH is unset"),
                )
            })?;
        std::env::split_paths(&path)
            .filter(|p| p.is_absolute())
            .map(|p| p.join(tool))
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .find(|p| !p.starts_with(&writer.worktree) && provenant_os::sys::is_executable(p))
            .ok_or_else(|| prepare_error("gh executable", format!("{tool} is unavailable on PATH")))
    }
    fn command(&self, cwd: &Path, tool: &str, args: &[&str], stage: Stage) -> Result<String> {
        let failure = |reason: String| {
            if stage == Stage::Prepare {
                let step = match args.first().copied() {
                    Some("remote") => "remote URL",
                    Some("symbolic-ref") => "HEAD",
                    Some("rev-parse") if args.contains(&"--absolute-git-dir") => "git dir",
                    Some("rev-parse") => "HEAD",
                    Some("config") => "credential helper",
                    Some("fetch") => "fetch",
                    _ => "git executable",
                };
                prepare_error(step, reason)
            } else {
                RunError::Delivery(stage)
            }
        };
        let mut environment: Vec<_> = self
            .environment
            .iter()
            .map(|(k, v)| (OsString::from_vec(k.clone()), OsString::from_vec(v.clone())))
            .collect();
        let cwd = if let Some(bound) = &self.bound {
            environment.push(("GIT_DIR".into(), bound.git_dir.as_os_str().to_owned()));
            environment.push((
                "GIT_WORK_TREE".into(),
                bound.host_cwd.as_os_str().to_owned(),
            ));
            bound.host_cwd.as_path()
        } else {
            cwd
        };
        let tools = self
            .tools
            .as_ref()
            .ok_or_else(|| failure("host tools are unavailable".into()))?;
        let executable = match tool {
            "git" => &tools.git,
            "gh" => tools
                .gh
                .as_ref()
                .ok_or_else(|| failure("gh is unavailable".into()))?,
            _ => return Err(failure(format!("{tool} is unavailable"))),
        };
        let git_options: &[&str] = if tool == "git" {
            &[
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ]
        } else {
            &[]
        };
        let argv: Vec<OsString> = std::iter::once(executable.as_os_str().to_owned())
            .chain(git_options.iter().chain(args).map(OsString::from))
            .collect();
        let executor = SystemExecutor::new(1).map_err(|error| failure(error.to_string()))?;
        let mut redactor = if stage == Stage::Prepare {
            Some(
                Redactor::new(
                    self.environment
                        .iter()
                        .filter(|(key, value)| {
                            matches!(key.as_slice(), b"GH_TOKEN" | b"GITHUB_TOKEN")
                                && !value.is_empty()
                        })
                        .map(|(_, value)| value.clone()),
                )
                .map_err(|_| failure("diagnostic redaction unavailable".into()))?,
            )
        } else {
            None
        };
        let mut options = ExecOptions::new(&argv, cwd, Duration::from_secs(30));
        options.env = Some(&environment);
        options.stdin = Some(b"");
        options.stdout = StreamPolicy::Capture { limit: 16 * 1024 };
        options.stderr = if stage == Stage::Prepare {
            StreamPolicy::Capture { limit: 4096 }
        } else {
            StreamPolicy::Discard
        };
        options.stderr_filter = redactor
            .as_mut()
            .map(|redactor| redactor as &mut dyn provenant_os::exec::CaptureFilter);
        let output = executor
            .execute(options)
            .map_err(|error| failure(format!("{}: {error}", executable.display())))?;
        let absent_helpers = args.starts_with(&["config", "--get-regexp"])
            && output.termination == Termination::Exit(1)
            && output.stdout.bytes.is_empty();
        if (output.termination != Termination::Exit(0) && !absent_helpers)
            || output.timed_out
            || output.stdout.truncated
            || output.stdout.read_failed
            || output.drain_truncated
        {
            let stderr = if output.stderr.filter_failed {
                "diagnostic redaction failed".into()
            } else {
                String::from_utf8_lossy(&output.stderr.bytes).into_owned()
            };
            return Err(failure(format!(
                "{tool} {:?} (timed out: {}): {stderr}",
                output.termination, output.timed_out
            )));
        }
        String::from_utf8(output.stdout.bytes)
            .map(|v| v.trim().into())
            .map_err(|error| failure(error.to_string()))
    }
    fn head(&self, writer: &WriterRequest) -> Result<GitOid> {
        let head = self.command(
            &writer.worktree,
            "git",
            &["rev-parse", "--verify", "HEAD"],
            Stage::Head,
        )?;
        GitOid::parse(&head).map_err(|_| RunError::Delivery(Stage::Head))
    }
    fn push(
        &self,
        writer: &WriterRequest,
        request: &WriterDeliveryRequest,
        head: GitOid,
    ) -> Result<Option<String>> {
        let bound = self
            .bound
            .as_ref()
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        let cwd = &writer.worktree;
        let branch = self.command(
            cwd,
            "git",
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
            Stage::Head,
        )?;
        if branch != bound.lane_branch {
            return Err(RunError::Delivery(Stage::Head));
        }
        let destination = format!("refs/heads/{}", request.branch);
        let refspec = format!("{head}:{destination}");
        self.command(
            cwd,
            "git",
            &[
                "push",
                "--no-follow-tags",
                "--recurse-submodules=no",
                "--",
                &bound.push_url,
                &refspec,
            ],
            Stage::Push,
        )?;
        let tracking = format!("refs/remotes/{}/{}", request.remote, request.branch);
        let fetchspec = format!("+{destination}:{tracking}");
        self.command(
            cwd,
            "git",
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "--",
                &bound.push_url,
                &fetchspec,
            ],
            Stage::Fetch,
        )?;
        let pushed = self.command(
            cwd,
            "git",
            &["rev-parse", "--verify", &tracking],
            Stage::Verify,
        )?;
        if pushed != head.to_string() || self.head(writer)? != head {
            return Err(RunError::Delivery(Stage::Verify));
        }
        for (key, value) in [
            ("remote", request.remote.as_str()),
            ("merge", destination.as_str()),
            ("pushRemote", request.remote.as_str()),
        ] {
            self.command(
                cwd,
                "git",
                &[
                    "config",
                    "--local",
                    &format!("branch.{}.{key}", bound.lane_branch),
                    value,
                ],
                Stage::Upstream,
            )?;
        }
        // Read the upstream back from config: `@{upstream}` and `status -sb`
        // only resolve when the remote's fetch refspec covers the branch, and a
        // narrowed clone (for example main only) is legitimate.
        for (key, value) in [
            ("remote", request.remote.as_str()),
            ("merge", destination.as_str()),
            ("pushRemote", request.remote.as_str()),
        ] {
            let stored = self.command(
                cwd,
                "git",
                &[
                    "config",
                    "--local",
                    "--get",
                    &format!("branch.{}.{key}", bound.lane_branch),
                ],
                Stage::Verify,
            )?;
            if stored != value {
                return Err(RunError::Delivery(Stage::Verify));
            }
        }
        if request.open_pr {
            let url = self.command(
                cwd,
                "gh",
                &[
                    "pr",
                    "create",
                    "--repo",
                    bound
                        .pull_request_repo
                        .as_deref()
                        .ok_or(RunError::Delivery(Stage::PullRequest))?,
                    "--head",
                    &request.branch,
                    "--fill",
                ],
                Stage::PullRequest,
            )?;
            if !url.starts_with("https://") || url.lines().count() != 1 {
                return Err(RunError::Delivery(Stage::PullRequest));
            }
            Ok(Some(url))
        } else {
            Ok(None)
        }
    }
}

fn prepare_error(step: &'static str, detail: impl std::fmt::Display) -> RunError {
    let detail = detail.to_string();
    let safe = provenant_confinement::redact::known_shapes(detail.as_bytes())
        .unwrap_or_else(|_| b"diagnostic redaction failed".to_vec());
    let mut reason = String::from_utf8_lossy(&safe)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if reason.len() > 1024 {
        reason.truncate(reason.floor_char_boundary(1024));
        reason.push_str("...");
    }
    RunError::DeliveryPrepare { step, reason }
}

fn credential_helper(path: &Path, writer: &WriterRequest) -> Result<()> {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    let helper = host_path(path, writer)
        .map_err(|error| prepare_error("credential helper", format!("{name}: {error}")))?;
    if !helper.is_file() || !provenant_os::sys::is_executable(&helper) {
        return Err(prepare_error(
            "credential helper",
            format!("{name} is unavailable ({})", path.display()),
        ));
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
enum HostPathError {
    #[error("not found")]
    NotFound,
    #[error("inside the writer worktree")]
    InsideWorktree,
}

fn host_path(path: &Path, writer: &WriterRequest) -> std::result::Result<PathBuf, HostPathError> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        writer.project.join(path)
    };
    let canonical = match std::fs::canonicalize(&path) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(HostPathError::NotFound)?;
            std::fs::canonicalize(parent)
                .map_err(|_| HostPathError::NotFound)?
                .join(path.file_name().ok_or(HostPathError::NotFound)?)
        }
        Err(_) => return Err(HostPathError::NotFound),
    };
    if canonical.starts_with(&writer.worktree) {
        return Err(HostPathError::InsideWorktree);
    }
    Ok(canonical)
}

fn pull_request_repo(url: &str) -> Result<String> {
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "ssh") {
            return Err(RunError::Delivery(Stage::Prepare));
        }
        let (authority, path) = rest
            .split_once('/')
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        (authority.rsplit('@').next().unwrap_or_default(), path)
    } else {
        let (authority, path) = url
            .split_once(':')
            .ok_or(RunError::Delivery(Stage::Prepare))?;
        (authority.rsplit('@').next().unwrap_or_default(), path)
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty()
        || !host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
        || path.split('/').count() != 2
        || path.split('/').any(|p| {
            p.is_empty()
                || p == "."
                || p == ".."
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        return Err(RunError::Delivery(Stage::Prepare));
    }
    Ok(format!("{host}/{path}"))
}

pub(crate) fn validate(request: &WriterDeliveryRequest) -> Result<()> {
    if request.remote.is_empty()
        || request.remote.len() > 64
        || request.remote.starts_with('-')
        || !request
            .remote
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(RunError::Validation(
            "delivery needs a configured remote name",
        ));
    }
    GitRefName::branch(&request.branch).map_err(|_| RunError::Validation("delivery branch"))?;
    Ok(())
}

/// The live holder calls this only after success and confinement checks. A
/// sealed start marker prevents repeating external effects after interruption.
pub(crate) fn deliver_once(
    host: &HostDeliveryContext,
    writer: &WriterRequest,
    request: &WriterDeliveryRequest,
    attempt: &Directory,
) -> Result<WriterDeliveryReceipt> {
    if let Some(raw) = crate::artifacts::read_sealed(attempt, "delivery.json", crate::RECORD_LIMIT)?
    {
        return crate::db::decode(&raw);
    }
    if host
        .bound
        .as_ref()
        .is_none_or(|bound| bound.destination != *request)
    {
        return Err(RunError::Validation(
            "delivery destination changed after launch",
        ));
    }
    let started =
        crate::artifacts::read_sealed(attempt, "delivery-started.json", crate::RECORD_LIMIT)?;
    let mut receipt = match &started {
        Some(raw) => crate::db::decode(raw)?,
        None => WriterDeliveryReceipt {
            schema_version: 1,
            remote: request.remote.clone(),
            branch: request.branch.clone(),
            head: host.head(writer).ok(),
            state: WriterDeliveryState::Failed {
                stage: Stage::Interrupted,
            },
        },
    };
    if started.is_none() {
        crate::artifacts::seal(
            attempt,
            "delivery-started.json",
            &crate::db::bytes(&receipt)?,
            crate::RECORD_LIMIT,
        )?;
        let unchanged = receipt
            .head
            .zip(host.bound.as_ref())
            .is_some_and(|(head, bound)| head == bound.base_head);
        receipt.state = if unchanged {
            WriterDeliveryState::NoNewCommits
        } else {
            match receipt
                .head
                .ok_or(RunError::Delivery(Stage::Head))
                .and_then(|head| host.push(writer, request, head))
            {
                Ok(pull_request) => WriterDeliveryState::Delivered { pull_request },
                Err(RunError::Delivery(stage)) => WriterDeliveryState::Failed { stage },
                Err(error) => return Err(error),
            }
        };
    }
    crate::artifacts::seal(
        attempt,
        "delivery.json",
        &crate::db::bytes(&receipt)?,
        crate::RECORD_LIMIT,
    )?;
    Ok(receipt)
}
